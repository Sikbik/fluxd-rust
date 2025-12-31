use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rand::RngCore;
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};

use fluxd_consensus::params::Network;
use fluxd_primitives::encoding::{DecodeError, Decoder, Encoder};
use fluxd_primitives::hash::hash160;
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::{script_pubkey_to_address, secret_key_to_wif, wif_to_secret_key};
use fluxd_script::message::signed_message_hash;

pub const WALLET_FILE_NAME: &str = "wallet.dat";

pub const WALLET_FILE_VERSION: u32 = 2;

#[derive(Debug)]
pub enum WalletError {
    Io(std::io::Error),
    Decode(DecodeError),
    InvalidData(&'static str),
    NetworkMismatch { expected: Network, found: Network },
    InvalidSecretKey,
}

impl std::fmt::Display for WalletError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WalletError::Io(err) => write!(f, "{err}"),
            WalletError::Decode(err) => write!(f, "{err}"),
            WalletError::InvalidData(msg) => write!(f, "{msg}"),
            WalletError::NetworkMismatch { expected, found } => write!(
                f,
                "wallet network mismatch (expected {expected:?}, found {found:?})"
            ),
            WalletError::InvalidSecretKey => write!(f, "invalid secret key"),
        }
    }
}

impl std::error::Error for WalletError {}

impl From<std::io::Error> for WalletError {
    fn from(err: std::io::Error) -> Self {
        WalletError::Io(err)
    }
}

impl From<DecodeError> for WalletError {
    fn from(err: DecodeError) -> Self {
        WalletError::Decode(err)
    }
}

#[derive(Clone)]
struct WalletKey {
    secret: [u8; 32],
    compressed: bool,
}

impl WalletKey {
    fn secret_key(&self) -> Result<SecretKey, WalletError> {
        SecretKey::from_slice(&self.secret).map_err(|_| WalletError::InvalidSecretKey)
    }

    fn pubkey(&self) -> Result<PublicKey, WalletError> {
        let secret = self.secret_key()?;
        Ok(PublicKey::from_secret_key(secp(), &secret))
    }

    fn pubkey_bytes(&self) -> Result<Vec<u8>, WalletError> {
        let pubkey = self.pubkey()?;
        if self.compressed {
            Ok(pubkey.serialize().to_vec())
        } else {
            Ok(pubkey.serialize_uncompressed().to_vec())
        }
    }

    fn p2pkh_script_pubkey(&self) -> Result<Vec<u8>, WalletError> {
        let pubkey_bytes = self.pubkey_bytes()?;
        let key_hash = hash160(&pubkey_bytes);
        Ok(p2pkh_script(&key_hash))
    }

    fn address(&self, network: Network) -> Result<String, WalletError> {
        let script_pubkey = self.p2pkh_script_pubkey()?;
        script_pubkey_to_address(&script_pubkey, network)
            .ok_or(WalletError::InvalidData("failed to encode address"))
    }

    fn wif(&self, network: Network) -> String {
        secret_key_to_wif(&self.secret, network, self.compressed)
    }
}

pub struct Wallet {
    path: PathBuf,
    network: Network,
    keys: Vec<WalletKey>,
    watch_scripts: Vec<Vec<u8>>,
    revision: u64,
    locked_outpoints: HashSet<OutPoint>,
    pay_tx_fee_per_kb: i64,
}

impl Wallet {
    pub fn load_or_create(data_dir: &Path, network: Network) -> Result<Self, WalletError> {
        let path = data_dir.join(WALLET_FILE_NAME);
        match fs::read(&path) {
            Ok(bytes) => {
                let wallet = Self::decode(&path, network, &bytes)?;
                Ok(wallet)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                path,
                network,
                keys: Vec::new(),
                watch_scripts: Vec::new(),
                revision: 0,
                locked_outpoints: HashSet::new(),
                pay_tx_fee_per_kb: 0,
            }),
            Err(err) => Err(WalletError::Io(err)),
        }
    }

    pub fn pay_tx_fee_per_kb(&self) -> i64 {
        self.pay_tx_fee_per_kb
    }

    pub fn set_pay_tx_fee_per_kb(&mut self, fee: i64) -> Result<(), WalletError> {
        let prev = self.pay_tx_fee_per_kb;
        self.pay_tx_fee_per_kb = fee;
        if let Err(err) = self.save() {
            self.pay_tx_fee_per_kb = prev;
            return Err(err);
        }
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn key_count(&self) -> usize {
        self.keys.len()
    }

    pub fn lock_outpoint(&mut self, outpoint: OutPoint) {
        self.locked_outpoints.insert(outpoint);
    }

    pub fn unlock_outpoint(&mut self, outpoint: &OutPoint) {
        self.locked_outpoints.remove(outpoint);
    }

    pub fn unlock_all_outpoints(&mut self) {
        self.locked_outpoints.clear();
    }

    pub fn locked_outpoints(&self) -> Vec<OutPoint> {
        self.locked_outpoints.iter().cloned().collect()
    }

    pub fn default_address(&self) -> Result<Option<String>, WalletError> {
        let Some(key) = self.keys.first() else {
            return Ok(None);
        };
        Ok(Some(key.address(self.network)?))
    }

    pub fn all_script_pubkeys(&self) -> Result<Vec<Vec<u8>>, WalletError> {
        let mut out = Vec::with_capacity(self.keys.len());
        for key in &self.keys {
            out.push(key.p2pkh_script_pubkey()?);
        }
        Ok(out)
    }

    pub fn all_script_pubkeys_including_watchonly(&self) -> Result<Vec<Vec<u8>>, WalletError> {
        let mut out = self.all_script_pubkeys()?;
        out.extend(self.watch_scripts.iter().cloned());
        Ok(out)
    }

    pub fn import_wif(&mut self, wif: &str) -> Result<(), WalletError> {
        let (secret, compressed) = wif_to_secret_key(wif, self.network)
            .map_err(|_| WalletError::InvalidData("invalid wif"))?;
        if self.keys.iter().any(|key| key.secret == secret) {
            return Ok(());
        }
        let key = WalletKey { secret, compressed };
        self.keys.push(key);
        if let Err(err) = self.save() {
            self.keys.retain(|k| k.secret != secret);
            return Err(err);
        }
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn import_watch_script_pubkey(
        &mut self,
        script_pubkey: Vec<u8>,
    ) -> Result<(), WalletError> {
        let owned = self
            .all_script_pubkeys()?
            .iter()
            .any(|spk| spk.as_slice() == script_pubkey.as_slice());
        if owned {
            return Ok(());
        }
        if self
            .watch_scripts
            .iter()
            .any(|spk| spk.as_slice() == script_pubkey.as_slice())
        {
            return Ok(());
        }
        self.watch_scripts.push(script_pubkey);
        if let Err(err) = self.save() {
            self.watch_scripts.pop();
            return Err(err);
        }
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn dump_wif_for_address(&self, address: &str) -> Result<Option<String>, WalletError> {
        for key in &self.keys {
            if key.address(self.network)? == address {
                return Ok(Some(key.wif(self.network)));
            }
        }
        Ok(None)
    }

    pub fn generate_new_address(&mut self, compressed: bool) -> Result<String, WalletError> {
        let mut rng = rand::rngs::OsRng;
        let mut seed = [0u8; 32];
        for _ in 0..100 {
            rng.fill_bytes(&mut seed);
            if SecretKey::from_slice(&seed).is_ok() {
                if self.keys.iter().any(|key| key.secret == seed) {
                    continue;
                }
                let key = WalletKey {
                    secret: seed,
                    compressed,
                };
                let address = key.address(self.network)?;
                self.keys.push(key);
                if let Err(err) = self.save() {
                    self.keys.retain(|k| k.secret != seed);
                    return Err(err);
                }
                self.revision = self.revision.saturating_add(1);
                return Ok(address);
            }
        }
        Err(WalletError::InvalidData("failed to generate secret key"))
    }

    pub fn scripts_for_filter(&self, addresses: &[String]) -> Result<Vec<Vec<u8>>, WalletError> {
        if addresses.is_empty() {
            return self.all_script_pubkeys();
        }
        let allow: HashSet<&str> = addresses.iter().map(|s| s.as_str()).collect();
        let mut out = Vec::new();
        for key in &self.keys {
            let addr = key.address(self.network)?;
            if allow.contains(addr.as_str()) {
                out.push(key.p2pkh_script_pubkey()?);
            }
        }
        for script_pubkey in &self.watch_scripts {
            if let Some(addr) = script_pubkey_to_address(script_pubkey, self.network) {
                if allow.contains(addr.as_str()) {
                    out.push(script_pubkey.clone());
                }
            }
        }
        Ok(out)
    }

    pub fn signing_key_for_script_pubkey(
        &self,
        script_pubkey: &[u8],
    ) -> Result<Option<(SecretKey, Vec<u8>)>, WalletError> {
        for key in &self.keys {
            if key.p2pkh_script_pubkey()?.as_slice() != script_pubkey {
                continue;
            }
            let secret = key.secret_key()?;
            let pubkey = key.pubkey_bytes()?;
            return Ok(Some((secret, pubkey)));
        }
        Ok(None)
    }

    pub fn sign_message(
        &self,
        address: &str,
        message: &[u8],
    ) -> Result<Option<Vec<u8>>, WalletError> {
        for key in &self.keys {
            if key.address(self.network)? != address {
                continue;
            }
            let secret = key.secret_key()?;
            let digest = signed_message_hash(message);
            let msg = Message::from_digest_slice(&digest)
                .map_err(|_| WalletError::InvalidData("invalid message digest"))?;
            let sig = secp().sign_ecdsa_recoverable(&msg, &secret);
            let (rec_id, bytes) = sig.serialize_compact();
            let mut out = [0u8; 65];
            let header = 27u8
                .saturating_add(rec_id.to_i32() as u8)
                .saturating_add(if key.compressed { 4 } else { 0 });
            out[0] = header;
            out[1..].copy_from_slice(&bytes);
            return Ok(Some(out.to_vec()));
        }
        Ok(None)
    }

    pub fn backup_to(&self, destination: &Path) -> Result<(), WalletError> {
        if destination == self.path.as_path() {
            return Err(WalletError::InvalidData(
                "backup destination must differ from wallet.dat",
            ));
        }
        if let Ok(meta) = fs::metadata(destination) {
            if meta.is_dir() {
                return Err(WalletError::InvalidData(
                    "backup destination is a directory",
                ));
            }
        }

        if fs::metadata(&self.path).is_err() {
            self.save()?;
        }
        let bytes = fs::read(&self.path)?;
        write_file_atomic(destination, &bytes)?;
        Ok(())
    }

    fn decode(path: &Path, expected_network: Network, bytes: &[u8]) -> Result<Self, WalletError> {
        let mut decoder = Decoder::new(bytes);
        let version = decoder.read_u32_le()?;
        if version != 1 && version != WALLET_FILE_VERSION {
            return Err(WalletError::InvalidData("unsupported wallet file version"));
        }
        let network = decode_network(decoder.read_u8()?)?;
        if network != expected_network {
            return Err(WalletError::NetworkMismatch {
                expected: expected_network,
                found: network,
            });
        }
        let pay_tx_fee_per_kb = if version >= 2 {
            decoder.read_i64_le()?
        } else {
            0
        };

        let key_count = decoder.read_varint()?;
        let key_count = usize::try_from(key_count)
            .map_err(|_| WalletError::InvalidData("wallet key count too large"))?;
        let mut keys = Vec::with_capacity(key_count.min(4096));
        for _ in 0..key_count {
            let secret = decoder.read_fixed::<32>()?;
            let compressed = decoder.read_bool()?;
            keys.push(WalletKey { secret, compressed });
        }

        let watch_scripts = if version >= 2 {
            let count = decoder.read_varint()?;
            let count = usize::try_from(count)
                .map_err(|_| WalletError::InvalidData("watch script count too large"))?;
            let mut out = Vec::with_capacity(count.min(4096));
            for _ in 0..count {
                let script = decoder.read_var_bytes()?;
                out.push(script);
            }
            out
        } else {
            Vec::new()
        };

        if !decoder.is_empty() {
            return Err(WalletError::InvalidData("wallet file has trailing bytes"));
        }
        Ok(Self {
            path: path.to_path_buf(),
            network,
            keys,
            watch_scripts,
            revision: 0,
            locked_outpoints: HashSet::new(),
            pay_tx_fee_per_kb,
        })
    }

    fn save(&self) -> Result<(), WalletError> {
        let mut encoder = Encoder::new();
        encoder.write_u32_le(WALLET_FILE_VERSION);
        encoder.write_u8(encode_network(self.network));
        encoder.write_i64_le(self.pay_tx_fee_per_kb);

        encoder.write_varint(self.keys.len() as u64);
        for key in &self.keys {
            encoder.write_bytes(&key.secret);
            encoder.write_u8(if key.compressed { 1 } else { 0 });
        }
        encoder.write_varint(self.watch_scripts.len() as u64);
        for script in &self.watch_scripts {
            encoder.write_var_bytes(script);
        }
        let bytes = encoder.into_inner();
        write_file_atomic(&self.path, &bytes)?;
        Ok(())
    }
}

fn encode_network(network: Network) -> u8 {
    match network {
        Network::Mainnet => 0,
        Network::Testnet => 1,
        Network::Regtest => 2,
    }
}

fn decode_network(value: u8) -> Result<Network, WalletError> {
    match value {
        0 => Ok(Network::Mainnet),
        1 => Ok(Network::Testnet),
        2 => Ok(Network::Regtest),
        _ => Err(WalletError::InvalidData("unknown wallet network")),
    }
}

fn p2pkh_script(key_hash: &[u8; 20]) -> Vec<u8> {
    const OP_DUP: u8 = 0x76;
    const OP_HASH160: u8 = 0xa9;
    const OP_EQUALVERIFY: u8 = 0x88;
    const OP_CHECKSIG: u8 = 0xac;

    let mut script = Vec::with_capacity(25);
    script.push(OP_DUP);
    script.push(OP_HASH160);
    script.push(0x14);
    script.extend_from_slice(key_hash);
    script.push(OP_EQUALVERIFY);
    script.push(OP_CHECKSIG);
    script
}

fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<(), WalletError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    if fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(path);
        fs::rename(&tmp, path)?;
    }
    Ok(())
}

fn secp() -> &'static Secp256k1<secp256k1::All> {
    static SECP: OnceLock<Secp256k1<secp256k1::All>> = OnceLock::new();
    SECP.get_or_init(Secp256k1::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_data_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn default_address_stable_across_restart() {
        let data_dir = temp_data_dir("fluxd-wallet-test");
        fs::create_dir_all(&data_dir).expect("create data dir");

        let mut wallet = Wallet::load_or_create(&data_dir, Network::Regtest).expect("wallet");
        let wif_a = secret_key_to_wif(&[2u8; 32], Network::Regtest, true);
        let wif_b = secret_key_to_wif(&[1u8; 32], Network::Regtest, true);
        wallet.import_wif(&wif_a).expect("import wif a");
        wallet.import_wif(&wif_b).expect("import wif b");
        let before = wallet.default_address().expect("default").expect("address");
        drop(wallet);

        let wallet = Wallet::load_or_create(&data_dir, Network::Regtest).expect("wallet reload");
        let after = wallet.default_address().expect("default").expect("address");
        assert_eq!(before, after);
    }
}
