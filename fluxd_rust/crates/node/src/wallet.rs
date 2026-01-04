use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::{
    Algorithm as Argon2Algorithm, Argon2, Params as Argon2Params, Version as Argon2Version,
};
use bech32::{Bech32, Hrp};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use fluxd_chainstate::state::ChainState;
use rand::RngCore;
use sapling_crypto::keys::{NullifierDerivingKey, PreparedIncomingViewingKey};
use sapling_crypto::note::{ExtractedNoteCommitment, Rseed};
use sapling_crypto::note_encryption::{
    try_sapling_note_decryption, SaplingDomain, Zip212Enforcement,
};
use sapling_crypto::{
    zip32::ExtendedSpendingKey, CommitmentTree as SaplingCommitmentTree,
    IncrementalWitness as SaplingIncrementalWitness, Node as SaplingNode, PaymentAddress,
};
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use zcash_note_encryption::{EphemeralKeyBytes, ShieldedOutput, ENC_CIPHERTEXT_SIZE};
use zcash_primitives::merkle_tree::{
    read_commitment_tree, read_incremental_witness, write_commitment_tree,
    write_incremental_witness,
};
use zip32::DiversifierIndex;

use fluxd_consensus::params::Network;
use fluxd_consensus::Hash256;
use fluxd_primitives::block::Block;
use fluxd_primitives::encoding::{DecodeError, Decoder, Encoder};
use fluxd_primitives::hash::hash160;
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::{script_pubkey_to_address, secret_key_to_wif, wif_to_secret_key};
use fluxd_script::message::signed_message_hash;
use fluxd_storage::KeyValueStore;
use zeroize::Zeroize;

pub const WALLET_FILE_NAME: &str = "wallet.dat";

pub const WALLET_FILE_VERSION: u32 = 14;

const WALLET_DUMP_EPOCH: &str = "1970-01-01T00:00:00Z";
const DEFAULT_KEYPOOL_SIZE: usize = 100;
const WALLET_SECRETS_VERSION: u32 = 1;
const WALLET_ENCRYPTION_VERSION: u8 = 1;
const WALLET_ENCRYPTION_SALT_BYTES: usize = 16;
const WALLET_ENCRYPTION_NONCE_BYTES: usize = 12;

#[derive(Clone)]
struct WalletKdfParams {
    mem_kib: u32,
    iters: u32,
    parallelism: u32,
    salt: [u8; WALLET_ENCRYPTION_SALT_BYTES],
}

#[derive(Clone)]
struct WalletEncryptedSecrets {
    kdf: WalletKdfParams,
    nonce: [u8; WALLET_ENCRYPTION_NONCE_BYTES],
    ciphertext: Vec<u8>,
}

#[derive(Clone)]
struct KeyPoolEntry {
    key: WalletKey,
    created_at: u64,
}

#[derive(Clone)]
struct SaplingKeyEntry {
    extfvk: [u8; 169],
    extsk: Option<[u8; 169]>,
    next_diversifier_index: [u8; 11],
}

#[derive(Clone)]
struct SaplingViewingKeyEntry {
    extfvk: [u8; 169],
    next_diversifier_index: [u8; 11],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SaplingRseedBytes {
    BeforeZip212([u8; 32]),
    AfterZip212([u8; 32]),
}

impl SaplingRseedBytes {
    pub(crate) fn from_rseed(rseed: &Rseed) -> Self {
        match *rseed {
            Rseed::BeforeZip212(rcm) => SaplingRseedBytes::BeforeZip212(rcm.to_bytes()),
            Rseed::AfterZip212(bytes) => SaplingRseedBytes::AfterZip212(bytes),
        }
    }

    pub(crate) fn to_rseed(self) -> Option<Rseed> {
        match self {
            SaplingRseedBytes::BeforeZip212(bytes) => {
                Option::from(jubjub::Fr::from_bytes(&bytes)).map(Rseed::BeforeZip212)
            }
            SaplingRseedBytes::AfterZip212(bytes) => Some(Rseed::AfterZip212(bytes)),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SaplingNoteRecord {
    pub(crate) address: [u8; 43],
    pub(crate) value: i64,
    pub(crate) height: i32,
    pub(crate) position: u64,
    pub(crate) nullifier: [u8; 32],
    pub(crate) rseed: Option<SaplingRseedBytes>,
}

pub(crate) type SaplingNoteKey = (Hash256, u32);

#[derive(Debug)]
pub enum WalletError {
    Io(std::io::Error),
    Decode(DecodeError),
    InvalidData(&'static str),
    ChainState(String),
    NetworkMismatch { expected: Network, found: Network },
    InvalidSecretKey,
    WalletLocked,
    WalletNotEncrypted,
    WalletAlreadyEncrypted,
    IncorrectPassphrase,
}

impl std::fmt::Display for WalletError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WalletError::Io(err) => write!(f, "{err}"),
            WalletError::Decode(err) => write!(f, "{err}"),
            WalletError::InvalidData(msg) => write!(f, "{msg}"),
            WalletError::ChainState(message) => write!(f, "{message}"),
            WalletError::NetworkMismatch { expected, found } => write!(
                f,
                "wallet network mismatch (expected {expected:?}, found {found:?})"
            ),
            WalletError::InvalidSecretKey => write!(f, "invalid secret key"),
            WalletError::WalletLocked => write!(f, "wallet is locked"),
            WalletError::WalletNotEncrypted => write!(f, "wallet is not encrypted"),
            WalletError::WalletAlreadyEncrypted => write!(f, "wallet is already encrypted"),
            WalletError::IncorrectPassphrase => write!(f, "incorrect wallet passphrase"),
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
    key_hash: [u8; 20],
    secret: Option<[u8; 32]>,
    compressed: bool,
    pubkey_bytes: Vec<u8>,
}

impl WalletKey {
    fn from_secret(secret: [u8; 32], compressed: bool) -> Result<Self, WalletError> {
        let secret_key =
            SecretKey::from_slice(&secret).map_err(|_| WalletError::InvalidSecretKey)?;
        let pubkey = PublicKey::from_secret_key(secp(), &secret_key);
        let pubkey_bytes = if compressed {
            pubkey.serialize().to_vec()
        } else {
            pubkey.serialize_uncompressed().to_vec()
        };
        Ok(Self {
            key_hash: hash160(&pubkey_bytes),
            secret: Some(secret),
            compressed,
            pubkey_bytes,
        })
    }

    fn secret_key(&self) -> Result<SecretKey, WalletError> {
        let secret = self.secret.ok_or(WalletError::WalletLocked)?;
        SecretKey::from_slice(&secret).map_err(|_| WalletError::InvalidSecretKey)
    }

    fn pubkey(&self) -> Result<PublicKey, WalletError> {
        let secret = self.secret_key()?;
        Ok(PublicKey::from_secret_key(secp(), &secret))
    }

    fn ensure_pubkey_bytes(&mut self) -> Result<(), WalletError> {
        if !self.pubkey_bytes.is_empty() {
            return Ok(());
        }
        if self.secret.is_none() {
            return Ok(());
        }
        let pubkey = self.pubkey()?;
        self.pubkey_bytes = if self.compressed {
            pubkey.serialize().to_vec()
        } else {
            pubkey.serialize_uncompressed().to_vec()
        };
        Ok(())
    }

    fn validate_pubkey_bytes(&self) -> Result<(), WalletError> {
        if self.pubkey_bytes.is_empty() {
            return Ok(());
        }
        let expected_len = if self.compressed { 33 } else { 65 };
        if self.pubkey_bytes.len() != expected_len {
            return Err(WalletError::InvalidData("wallet pubkey length mismatch"));
        }
        PublicKey::from_slice(&self.pubkey_bytes)
            .map_err(|_| WalletError::InvalidData("wallet contains invalid pubkey bytes"))?;
        if hash160(&self.pubkey_bytes) != self.key_hash {
            return Err(WalletError::InvalidData("wallet pubkey hash mismatch"));
        }
        Ok(())
    }

    fn pubkey_bytes(&self) -> Result<Vec<u8>, WalletError> {
        if !self.pubkey_bytes.is_empty() {
            return Ok(self.pubkey_bytes.clone());
        }
        let pubkey = self.pubkey()?;
        Ok(if self.compressed {
            pubkey.serialize().to_vec()
        } else {
            pubkey.serialize_uncompressed().to_vec()
        })
    }

    fn p2pkh_key_hash(&self) -> Result<[u8; 20], WalletError> {
        Ok(self.key_hash)
    }

    fn p2pkh_script_pubkey(&self) -> Result<Vec<u8>, WalletError> {
        Ok(p2pkh_script(&self.key_hash))
    }

    fn address(&self, network: Network) -> Result<String, WalletError> {
        let script_pubkey = self.p2pkh_script_pubkey()?;
        script_pubkey_to_address(&script_pubkey, network)
            .ok_or(WalletError::InvalidData("failed to encode address"))
    }

    fn wif(&self, network: Network) -> Result<String, WalletError> {
        let secret = self.secret.ok_or(WalletError::WalletLocked)?;
        Ok(secret_key_to_wif(&secret, network, self.compressed))
    }
}

pub struct Wallet {
    path: PathBuf,
    network: Network,
    keys: Vec<WalletKey>,
    watch_scripts: Vec<Vec<u8>>,
    redeem_scripts: BTreeMap<[u8; 20], Vec<u8>>,
    tx_history: BTreeSet<Hash256>,
    tx_received_at: BTreeMap<Hash256, u64>,
    tx_store: BTreeMap<Hash256, Vec<u8>>,
    keypool: VecDeque<KeyPoolEntry>,
    sapling_keys: Vec<SaplingKeyEntry>,
    sapling_viewing_keys: Vec<SaplingViewingKeyEntry>,
    change_key_hashes: BTreeSet<[u8; 20]>,
    sapling_scan_height: i32,
    sapling_scan_hash: Hash256,
    sapling_next_position: u64,
    sapling_notes: BTreeMap<SaplingNoteKey, SaplingNoteRecord>,
    sapling_tree: SaplingCommitmentTree,
    sapling_witnesses: BTreeMap<SaplingNoteKey, SaplingIncrementalWitness>,
    revision: u64,
    locked_outpoints: HashSet<OutPoint>,
    pay_tx_fee_per_kb: i64,
    encrypted_secrets: Option<WalletEncryptedSecrets>,
    unlocked_key: Option<[u8; 32]>,
    unlocked_until: u64,
    unlock_generation: u64,
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
                redeem_scripts: BTreeMap::new(),
                tx_history: BTreeSet::new(),
                tx_received_at: BTreeMap::new(),
                tx_store: BTreeMap::new(),
                keypool: VecDeque::new(),
                sapling_keys: Vec::new(),
                sapling_viewing_keys: Vec::new(),
                change_key_hashes: BTreeSet::new(),
                sapling_scan_height: -1,
                sapling_scan_hash: [0u8; 32],
                sapling_next_position: 0,
                sapling_notes: BTreeMap::new(),
                sapling_tree: SaplingCommitmentTree::empty(),
                sapling_witnesses: BTreeMap::new(),
                revision: 0,
                locked_outpoints: HashSet::new(),
                pay_tx_fee_per_kb: 0,
                encrypted_secrets: None,
                unlocked_key: None,
                unlocked_until: 0,
                unlock_generation: 0,
            }),
            Err(err) => Err(WalletError::Io(err)),
        }
    }

    pub fn pay_tx_fee_per_kb(&self) -> i64 {
        self.pay_tx_fee_per_kb
    }

    pub fn is_encrypted(&self) -> bool {
        self.encrypted_secrets.is_some()
    }

    pub fn unlocked_until(&mut self) -> u64 {
        self.lock_if_expired();
        self.unlocked_until
    }

    pub(crate) fn unlock_generation(&self) -> u64 {
        self.unlock_generation
    }

    fn can_generate_keys(&mut self) -> bool {
        self.lock_if_expired();
        !self.is_encrypted() || self.unlocked_key.is_some()
    }

    fn require_unlocked(&mut self) -> Result<(), WalletError> {
        self.lock_if_expired();
        if !self.is_encrypted() {
            return Ok(());
        }
        if self.unlocked_key.is_none() || self.unlocked_until == 0 {
            return Err(WalletError::WalletLocked);
        }
        Ok(())
    }

    fn lock_if_expired(&mut self) {
        if !self.is_encrypted() || self.unlocked_until == 0 {
            return;
        }
        if current_unix_seconds() >= self.unlocked_until {
            self.lock_inner();
        }
    }

    fn lock_inner(&mut self) {
        self.unlocked_until = 0;
        self.unlock_generation = self.unlock_generation.saturating_add(1);
        if let Some(key) = self.unlocked_key.as_mut() {
            key.zeroize();
        }
        self.unlocked_key = None;
        for key in &mut self.keys {
            if let Some(secret) = key.secret.as_mut() {
                secret.zeroize();
            }
            key.secret = None;
        }
        for entry in &mut self.keypool {
            if let Some(secret) = entry.key.secret.as_mut() {
                secret.zeroize();
            }
            entry.key.secret = None;
        }
        for entry in &mut self.sapling_keys {
            if let Some(extsk) = entry.extsk.as_mut() {
                extsk.zeroize();
            }
            entry.extsk = None;
        }
    }

    pub fn walletlock(&mut self) -> Result<(), WalletError> {
        if !self.is_encrypted() {
            return Err(WalletError::WalletNotEncrypted);
        }
        self.lock_inner();
        Ok(())
    }

    pub fn encryptwallet(&mut self, passphrase: &str) -> Result<(), WalletError> {
        if self.is_encrypted() {
            return Err(WalletError::WalletAlreadyEncrypted);
        }
        let mut secrets = self.encode_secrets_blob()?;

        let (kdf, key) = new_wallet_kdf(passphrase)?;
        let mut nonce = [0u8; WALLET_ENCRYPTION_NONCE_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ciphertext = encrypt_wallet_secrets(self.network, &key, &nonce, &secrets)?;

        self.encrypted_secrets = Some(WalletEncryptedSecrets {
            kdf,
            nonce,
            ciphertext,
        });
        self.lock_inner();
        if let Err(err) = self.save() {
            self.encrypted_secrets = None;
            self.apply_secrets_blob(&secrets)?;
            secrets.zeroize();
            let mut key = key;
            key.zeroize();
            return Err(err);
        }
        secrets.zeroize();
        let mut key = key;
        key.zeroize();
        Ok(())
    }

    pub fn walletpassphrase(
        &mut self,
        passphrase: &str,
        timeout: u64,
    ) -> Result<(u64, u64), WalletError> {
        if !self.is_encrypted() {
            return Err(WalletError::WalletNotEncrypted);
        }
        self.lock_if_expired();
        let needs_pubkey_persist = self.keys.iter().any(|key| key.pubkey_bytes.is_empty())
            || self
                .keypool
                .iter()
                .any(|entry| entry.key.pubkey_bytes.is_empty());
        let encrypted = self
            .encrypted_secrets
            .as_ref()
            .ok_or(WalletError::WalletNotEncrypted)?;
        let key = derive_wallet_key(passphrase, &encrypted.kdf)?;
        let mut plaintext =
            decrypt_wallet_secrets(self.network, &key, &encrypted.nonce, &encrypted.ciphertext)?;
        self.apply_secrets_blob(&plaintext)?;
        plaintext.zeroize();

        let unlocked_until = current_unix_seconds().saturating_add(timeout);
        self.unlocked_until = unlocked_until;
        self.unlock_generation = self.unlock_generation.saturating_add(1);
        if let Some(existing) = self.unlocked_key.as_mut() {
            existing.zeroize();
        }
        self.unlocked_key = Some(key);
        if needs_pubkey_persist {
            self.save()?;
        }
        let mut key = key;
        key.zeroize();
        Ok((unlocked_until, self.unlock_generation))
    }

    pub fn walletpassphrasechange(
        &mut self,
        old_passphrase: &str,
        new_passphrase: &str,
    ) -> Result<(), WalletError> {
        if !self.is_encrypted() {
            return Err(WalletError::WalletNotEncrypted);
        }
        let prev = self
            .encrypted_secrets
            .clone()
            .ok_or(WalletError::WalletNotEncrypted)?;
        let old_key = derive_wallet_key(old_passphrase, &prev.kdf)?;
        let mut plaintext =
            decrypt_wallet_secrets(self.network, &old_key, &prev.nonce, &prev.ciphertext)?;

        let (kdf, new_key) = new_wallet_kdf(new_passphrase)?;
        let mut nonce = [0u8; WALLET_ENCRYPTION_NONCE_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ciphertext = encrypt_wallet_secrets(self.network, &new_key, &nonce, &plaintext)?;
        plaintext.zeroize();

        self.encrypted_secrets = Some(WalletEncryptedSecrets {
            kdf,
            nonce,
            ciphertext,
        });
        if self.unlocked_key.is_some() {
            if let Some(key) = self.unlocked_key.as_mut() {
                key.zeroize();
            }
            self.unlocked_key = Some(new_key);
        }

        if let Err(err) = self.save() {
            self.encrypted_secrets = Some(prev);
            return Err(err);
        }
        let mut old_key = old_key;
        old_key.zeroize();
        let mut new_key = new_key;
        new_key.zeroize();
        Ok(())
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

    pub fn keypool_size(&self) -> usize {
        self.keypool.len()
    }

    #[cfg(test)]
    pub fn sapling_key_count(&self) -> usize {
        self.sapling_keys.len()
    }

    #[cfg(test)]
    pub fn sapling_viewing_key_count(&self) -> usize {
        self.sapling_viewing_keys.len()
    }

    pub(crate) fn has_sapling_keys(&self) -> bool {
        !self.sapling_keys.is_empty() || !self.sapling_viewing_keys.is_empty()
    }

    pub fn keypool_oldest(&self) -> u64 {
        self.keypool
            .front()
            .map(|entry| entry.created_at)
            .unwrap_or(0)
    }

    pub fn tx_count(&self) -> usize {
        self.tx_history.len()
    }

    pub fn tx_received_time(&self, txid: &Hash256) -> Option<u64> {
        self.tx_received_at.get(txid).copied()
    }

    pub fn transaction_bytes(&self, txid: &Hash256) -> Option<&[u8]> {
        self.tx_store.get(txid).map(|bytes| bytes.as_slice())
    }

    pub fn record_txids(
        &mut self,
        txids: impl IntoIterator<Item = Hash256>,
    ) -> Result<usize, WalletError> {
        let now = current_unix_seconds();
        let prev_len = self.tx_history.len();
        for txid in txids {
            if self.tx_history.insert(txid) {
                self.tx_received_at.entry(txid).or_insert(now);
            }
        }
        let added = self.tx_history.len().saturating_sub(prev_len);
        if added > 0 {
            self.save()?;
            self.revision = self.revision.saturating_add(1);
        }
        Ok(added)
    }

    pub fn record_transactions(
        &mut self,
        transactions: impl IntoIterator<Item = (Hash256, Vec<u8>)>,
    ) -> Result<usize, WalletError> {
        let now = current_unix_seconds();
        let prev_len = self.tx_history.len();
        let mut changed = false;

        for (txid, raw) in transactions {
            if self.tx_history.insert(txid) {
                self.tx_received_at.entry(txid).or_insert(now);
                changed = true;
            }
            match self.tx_store.get(&txid) {
                Some(existing) if existing.as_slice() == raw.as_slice() => {}
                _ => {
                    self.tx_store.insert(txid, raw);
                    changed = true;
                }
            }
        }

        let added = self.tx_history.len().saturating_sub(prev_len);
        if changed {
            self.save()?;
            self.revision = self.revision.saturating_add(1);
        }
        Ok(added)
    }

    pub fn record_transaction(&mut self, txid: Hash256, raw: Vec<u8>) -> Result<(), WalletError> {
        let now = current_unix_seconds();
        let mut changed = false;

        if self.tx_history.insert(txid) {
            self.tx_received_at.entry(txid).or_insert(now);
            changed = true;
        }
        match self.tx_store.get(&txid) {
            Some(existing) if existing.as_slice() == raw.as_slice() => {}
            _ => {
                self.tx_store.insert(txid, raw);
                changed = true;
            }
        }

        if changed {
            self.save()?;
            self.revision = self.revision.saturating_add(1);
        }
        Ok(())
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

    pub fn can_spend_script_pubkey(&self, script_pubkey: &[u8]) -> bool {
        if let Some(key_hash) = p2pkh_key_hash_from_script_pubkey(script_pubkey) {
            return self.has_key_hash(&key_hash);
        }
        let Some(script_hash) = p2sh_hash_from_script_pubkey(script_pubkey) else {
            return false;
        };
        let Some(redeem_script) = self.redeem_scripts.get(&script_hash) else {
            return false;
        };
        self.can_spend_redeem_script(redeem_script)
    }

    pub fn all_script_pubkeys(&self) -> Result<Vec<Vec<u8>>, WalletError> {
        let mut out = Vec::with_capacity(self.keys.len() + self.redeem_scripts.len());
        for key in &self.keys {
            out.push(key.p2pkh_script_pubkey()?);
        }
        for redeem_script in self.redeem_scripts.values() {
            if !self.can_spend_redeem_script(redeem_script) {
                continue;
            }
            out.push(p2sh_script_pubkey_from_redeem_script(redeem_script));
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    pub fn all_script_pubkeys_including_watchonly(&self) -> Result<Vec<Vec<u8>>, WalletError> {
        let mut out = self.all_script_pubkeys()?;
        out.extend(self.watch_scripts.iter().cloned());
        for redeem_script in self.redeem_scripts.values() {
            out.push(p2sh_script_pubkey_from_redeem_script(redeem_script));
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    pub fn script_pubkey_is_watchonly(&self, script_pubkey: &[u8]) -> bool {
        if self.can_spend_script_pubkey(script_pubkey) {
            return false;
        }
        if let Some(script_hash) = p2sh_hash_from_script_pubkey(script_pubkey) {
            if self.redeem_scripts.contains_key(&script_hash) {
                return true;
            }
        }
        self.watch_scripts
            .iter()
            .any(|spk| spk.as_slice() == script_pubkey)
    }

    pub fn pubkey_bytes_for_p2pkh_script_pubkey(
        &self,
        script_pubkey: &[u8],
    ) -> Result<Option<Vec<u8>>, WalletError> {
        let Some(key_hash) = p2pkh_key_hash_from_script_pubkey(script_pubkey) else {
            return Ok(None);
        };

        for key in &self.keys {
            if key.key_hash != key_hash {
                continue;
            }
            if !key.pubkey_bytes.is_empty() {
                return Ok(Some(key.pubkey_bytes.clone()));
            }
            if key.secret.is_some() {
                return Ok(Some(key.pubkey_bytes()?));
            }
            return Ok(None);
        }

        for entry in &self.keypool {
            if entry.key.key_hash != key_hash {
                continue;
            }
            if !entry.key.pubkey_bytes.is_empty() {
                return Ok(Some(entry.key.pubkey_bytes.clone()));
            }
            if entry.key.secret.is_some() {
                return Ok(Some(entry.key.pubkey_bytes()?));
            }
            return Ok(None);
        }

        Ok(None)
    }

    pub fn redeem_script_for_p2sh_script_pubkey(&self, script_pubkey: &[u8]) -> Option<Vec<u8>> {
        let script_hash = p2sh_hash_from_script_pubkey(script_pubkey)?;
        self.redeem_scripts.get(&script_hash).cloned()
    }

    pub fn signing_key_for_pubkey(
        &self,
        pubkey: &PublicKey,
    ) -> Result<Option<SecretKey>, WalletError> {
        for key in &self.keys {
            let secret = key.secret_key()?;
            let candidate = PublicKey::from_secret_key(secp(), &secret);
            if &candidate == pubkey {
                return Ok(Some(secret));
            }
        }
        for entry in &self.keypool {
            let secret = entry.key.secret_key()?;
            let candidate = PublicKey::from_secret_key(secp(), &secret);
            if &candidate == pubkey {
                return Ok(Some(secret));
            }
        }
        Ok(None)
    }

    pub fn import_wif(&mut self, wif: &str) -> Result<(), WalletError> {
        self.require_unlocked()?;
        let (secret, compressed) = wif_to_secret_key(wif, self.network)
            .map_err(|_| WalletError::InvalidData("invalid wif"))?;
        let key = WalletKey::from_secret(secret, compressed)?;
        if self
            .keys
            .iter()
            .any(|existing| existing.key_hash == key.key_hash)
            || self
                .keypool
                .iter()
                .any(|entry| entry.key.key_hash == key.key_hash)
        {
            return Ok(());
        }
        let prev_len = self.keys.len();
        self.keys.push(key);
        if let Err(err) = self.save() {
            self.keys.truncate(prev_len);
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

    pub fn import_redeem_script(&mut self, redeem_script: Vec<u8>) -> Result<(), WalletError> {
        let hash = hash160(&redeem_script);
        if self
            .redeem_scripts
            .get(&hash)
            .is_some_and(|existing| existing.as_slice() == redeem_script.as_slice())
        {
            return Ok(());
        }

        let prev = self.redeem_scripts.insert(hash, redeem_script);
        if let Err(err) = self.save() {
            match prev {
                Some(existing) => {
                    self.redeem_scripts.insert(hash, existing);
                }
                None => {
                    self.redeem_scripts.remove(&hash);
                }
            }
            return Err(err);
        }
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn dump_wif_for_address(&self, address: &str) -> Result<Option<String>, WalletError> {
        for key in &self.keys {
            if key.address(self.network)? == address {
                return Ok(Some(key.wif(self.network)?));
            }
        }
        Ok(None)
    }

    pub fn export_wallet_dump(&self, include_sapling_keys: bool) -> Result<String, WalletError> {
        #[derive(Clone, Copy)]
        enum TransparentDumpFlag {
            Default,
            Change,
            Reserve,
        }

        struct TransparentDumpEntry {
            address: String,
            wif: String,
            flag: TransparentDumpFlag,
        }

        let mut out = String::new();
        out.push_str("# Wallet dump created by fluxd-rust\n");
        out.push_str(&format!("# * Created on {WALLET_DUMP_EPOCH}\n"));
        out.push('\n');

        let mut transparent = Vec::new();

        for key in &self.keys {
            let address = key.address(self.network)?;
            let wif = key.wif(self.network)?;
            let key_hash = key.p2pkh_key_hash()?;
            let flag = if self.change_key_hashes.contains(&key_hash) {
                TransparentDumpFlag::Change
            } else {
                TransparentDumpFlag::Default
            };
            transparent.push(TransparentDumpEntry { address, wif, flag });
        }

        for entry in &self.keypool {
            let address = entry.key.address(self.network)?;
            let wif = entry.key.wif(self.network)?;
            transparent.push(TransparentDumpEntry {
                address,
                wif,
                flag: TransparentDumpFlag::Reserve,
            });
        }

        transparent.sort_by(|a, b| a.address.cmp(&b.address).then_with(|| a.wif.cmp(&b.wif)));

        for entry in transparent {
            let flag = match entry.flag {
                TransparentDumpFlag::Reserve => "reserve=1",
                TransparentDumpFlag::Change => "change=1",
                TransparentDumpFlag::Default => "label=",
            };
            out.push_str(&format!(
                "{} {} {} # addr={}\n",
                entry.wif, WALLET_DUMP_EPOCH, flag, entry.address
            ));
        }

        if include_sapling_keys {
            out.push('\n');
            out.push_str("# Sapling keys\n");
            out.push('\n');

            let addr_hrp = match self.network {
                Network::Mainnet => "za",
                Network::Testnet => "ztestacadia",
                Network::Regtest => "zregtestsapling",
            };
            let addr_hrp = Hrp::parse(addr_hrp)
                .map_err(|_| WalletError::InvalidData("invalid sapling address hrp"))?;

            let sk_hrp = match self.network {
                Network::Mainnet => "secret-extended-key-main",
                Network::Testnet => "secret-extended-key-test",
                Network::Regtest => "secret-extended-key-regtest",
            };
            let sk_hrp = Hrp::parse(sk_hrp)
                .map_err(|_| WalletError::InvalidData("invalid sapling spending key hrp"))?;

            let mut sapling_lines = BTreeSet::new();
            for entry in &self.sapling_keys {
                let extsk_bytes = entry.extsk.ok_or(WalletError::WalletLocked)?;
                let extsk = ExtendedSpendingKey::from_bytes(&extsk_bytes).map_err(|_| {
                    WalletError::InvalidData("invalid sapling spending key encoding")
                })?;
                let dfvk = extsk.to_diversifiable_full_viewing_key();
                let (_, addr) = dfvk.find_address(DiversifierIndex::from([0u8; 11])).ok_or(
                    WalletError::InvalidData("sapling diversifier space exhausted"),
                )?;
                let addr_bytes = addr.to_bytes();
                let zaddr = bech32::encode::<Bech32>(addr_hrp, addr_bytes.as_slice())
                    .map_err(|_| WalletError::InvalidData("failed to encode sapling address"))?;
                let zkey =
                    bech32::encode::<Bech32>(sk_hrp, extsk_bytes.as_slice()).map_err(|_| {
                        WalletError::InvalidData("failed to encode sapling spending key")
                    })?;
                sapling_lines.insert((zkey, zaddr));
            }

            for (zkey, zaddr) in sapling_lines {
                out.push_str(&format!(
                    "{} {} # zaddr={}\n",
                    zkey, WALLET_DUMP_EPOCH, zaddr
                ));
            }

            out.push('\n');
        }

        out.push_str("# End of dump\n");
        Ok(out)
    }

    pub fn generate_new_address(&mut self, compressed: bool) -> Result<String, WalletError> {
        self.reserve_from_keypool_or_generate(compressed, false)
    }

    pub fn generate_new_change_address(&mut self, compressed: bool) -> Result<String, WalletError> {
        self.reserve_from_keypool_or_generate(compressed, true)
    }

    #[cfg(test)]
    pub fn is_change_script_pubkey(&self, script_pubkey: &[u8]) -> bool {
        let Some(key_hash) = extract_p2pkh_hash(script_pubkey) else {
            return false;
        };
        self.change_key_hashes.contains(&key_hash)
    }

    pub fn change_key_hashes(&self) -> Vec<[u8; 20]> {
        self.change_key_hashes.iter().copied().collect()
    }

    pub(crate) fn sapling_note_map(&self) -> &BTreeMap<SaplingNoteKey, SaplingNoteRecord> {
        &self.sapling_notes
    }

    pub(crate) fn sapling_witness_map(
        &self,
    ) -> &BTreeMap<SaplingNoteKey, SaplingIncrementalWitness> {
        &self.sapling_witnesses
    }

    pub(crate) fn sapling_tree(&self) -> &SaplingCommitmentTree {
        &self.sapling_tree
    }

    pub(crate) fn backfill_sapling_note_rseed(
        &mut self,
        key: SaplingNoteKey,
        rseed: SaplingRseedBytes,
    ) -> Result<bool, WalletError> {
        let Some(note) = self.sapling_notes.get(&key) else {
            return Ok(false);
        };
        if note.rseed.is_some() {
            return Ok(false);
        }

        if let Some(note) = self.sapling_notes.get_mut(&key) {
            note.rseed = Some(rseed);
        }
        if let Err(err) = self.save() {
            if let Some(note) = self.sapling_notes.get_mut(&key) {
                note.rseed = None;
            }
            return Err(err);
        }
        self.revision = self.revision.saturating_add(1);
        Ok(true)
    }

    pub fn reset_sapling_scan(&mut self) -> Result<(), WalletError> {
        let prev_scan_height = self.sapling_scan_height;
        let prev_scan_hash = self.sapling_scan_hash;
        let prev_next_position = self.sapling_next_position;
        let prev_notes = self.sapling_notes.clone();
        let prev_tree = self.sapling_tree.clone();
        let prev_witnesses = self.sapling_witnesses.clone();

        self.sapling_scan_height = -1;
        self.sapling_scan_hash = [0u8; 32];
        self.sapling_next_position = 0;
        self.sapling_notes.clear();
        self.sapling_tree = SaplingCommitmentTree::empty();
        self.sapling_witnesses.clear();

        if let Err(err) = self.save() {
            self.sapling_scan_height = prev_scan_height;
            self.sapling_scan_hash = prev_scan_hash;
            self.sapling_next_position = prev_next_position;
            self.sapling_notes = prev_notes;
            self.sapling_tree = prev_tree;
            self.sapling_witnesses = prev_witnesses;
            return Err(err);
        }

        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn ensure_sapling_scan_initialized_to_tip<S: KeyValueStore>(
        &mut self,
        chainstate: &ChainState<S>,
    ) -> Result<(), WalletError> {
        if self.sapling_scan_height >= 0 {
            return Ok(());
        }
        if !self.sapling_notes.is_empty() {
            return Ok(());
        }

        let tip = chainstate
            .best_block()
            .map_err(|err| WalletError::ChainState(err.to_string()))?;
        let Some(tip) = tip else {
            return Ok(());
        };
        let sapling_count = chainstate
            .sapling_commitment_count()
            .map_err(|err| WalletError::ChainState(err.to_string()))?;
        let tree_bytes = chainstate
            .sapling_tree_bytes()
            .map_err(|err| WalletError::ChainState(err.to_string()))?;
        let sapling_tree = read_commitment_tree(Cursor::new(tree_bytes))
            .map_err(|_| WalletError::InvalidData("invalid sapling tree encoding"))?;

        let prev_scan_height = self.sapling_scan_height;
        let prev_scan_hash = self.sapling_scan_hash;
        let prev_next_position = self.sapling_next_position;
        let prev_tree = self.sapling_tree.clone();
        let prev_witnesses = self.sapling_witnesses.clone();

        self.sapling_scan_height = tip.height;
        self.sapling_scan_hash = tip.hash;
        self.sapling_next_position = sapling_count;
        self.sapling_tree = sapling_tree;
        self.sapling_witnesses.clear();

        if let Err(err) = self.save() {
            self.sapling_scan_height = prev_scan_height;
            self.sapling_scan_hash = prev_scan_hash;
            self.sapling_next_position = prev_next_position;
            self.sapling_tree = prev_tree;
            self.sapling_witnesses = prev_witnesses;
            return Err(err);
        }
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    pub fn sync_sapling_notes<S: KeyValueStore>(
        &mut self,
        chainstate: &ChainState<S>,
    ) -> Result<(), WalletError> {
        if self.sapling_keys.is_empty() && self.sapling_viewing_keys.is_empty() {
            return Ok(());
        }

        let tip = chainstate
            .best_block()
            .map_err(|err| WalletError::ChainState(err.to_string()))?;
        let Some(tip) = tip else {
            return Ok(());
        };

        let prev_scan_height = self.sapling_scan_height;
        let prev_scan_hash = self.sapling_scan_hash;
        let prev_next_position = self.sapling_next_position;
        let prev_notes = self.sapling_notes.clone();
        let prev_tree = self.sapling_tree.clone();
        let prev_witnesses = self.sapling_witnesses.clone();

        if self.sapling_scan_height < 0 {
            self.sapling_scan_hash = [0u8; 32];
            self.sapling_next_position = 0;
            self.sapling_tree = SaplingCommitmentTree::empty();
            self.sapling_witnesses.clear();
        }

        let scan_keys = self.sapling_scan_keys()?;

        let mut needs_full_rescan = false;
        if self.sapling_scan_height >= 0 {
            let tree_size = self.sapling_tree.size() as u64;
            if tree_size != self.sapling_next_position
                || self.sapling_witnesses.len() != self.sapling_notes.len()
            {
                needs_full_rescan = true;
            }
        }

        if self.sapling_scan_height >= 0 && !needs_full_rescan {
            let best_hash = chainstate
                .height_hash(self.sapling_scan_height)
                .map_err(|err| WalletError::ChainState(err.to_string()))?;
            if best_hash != Some(self.sapling_scan_hash) {
                needs_full_rescan = true;
            }
        }

        if needs_full_rescan {
            self.sapling_scan_height = -1;
            self.sapling_scan_hash = [0u8; 32];
            self.sapling_next_position = 0;
            self.sapling_notes.clear();
            self.sapling_tree = SaplingCommitmentTree::empty();
            self.sapling_witnesses.clear();
        }

        let mut height = self.sapling_scan_height.saturating_add(1);
        while height <= tip.height {
            let hash = chainstate
                .height_hash(height)
                .map_err(|err| WalletError::ChainState(err.to_string()))?
                .ok_or(WalletError::InvalidData(
                    "missing block hash for wallet scan",
                ))?;
            let block = read_block_by_hash(chainstate, &hash)?;

            for tx in &block.transactions {
                let txid = tx
                    .txid()
                    .map_err(|_| WalletError::InvalidData("invalid transaction encoding"))?;
                for (out_index, output) in tx.shielded_outputs.iter().enumerate() {
                    let position = self.sapling_next_position;
                    let cmu = Option::from(ExtractedNoteCommitment::from_bytes(&output.cm))
                        .ok_or(WalletError::InvalidData("invalid sapling note commitment"))?;
                    let node = SaplingNode::from_cmu(&cmu);
                    self.sapling_tree
                        .append(node.clone())
                        .map_err(|_| WalletError::InvalidData("sapling tree append failed"))?;
                    for witness in self.sapling_witnesses.values_mut() {
                        witness.append(node.clone()).map_err(|_| {
                            WalletError::InvalidData("sapling witness append failed")
                        })?;
                    }
                    self.sapling_next_position = self.sapling_next_position.saturating_add(1);
                    if let Some(note_record) =
                        scan_sapling_output(&scan_keys, output, position, height)
                    {
                        let note_key = (txid, out_index as u32);
                        let witness =
                            SaplingIncrementalWitness::from_tree(self.sapling_tree.clone())
                                .ok_or(WalletError::InvalidData("sapling witness state missing"))?;
                        match self.sapling_notes.entry(note_key) {
                            std::collections::btree_map::Entry::Vacant(entry) => {
                                entry.insert(note_record);
                                self.sapling_witnesses.insert(note_key, witness);
                            }
                            std::collections::btree_map::Entry::Occupied(_) => {
                                if !self.sapling_witnesses.contains_key(&note_key) {
                                    self.sapling_witnesses.insert(note_key, witness);
                                }
                            }
                        }
                    }
                }
            }

            self.sapling_scan_height = height;
            self.sapling_scan_hash = hash;
            height = height.saturating_add(1);
        }

        let changed = self.sapling_scan_height != prev_scan_height
            || self.sapling_scan_hash != prev_scan_hash
            || self.sapling_next_position != prev_next_position
            || self.sapling_notes != prev_notes
            || self.sapling_tree != prev_tree
            || self.sapling_witnesses.len() != prev_witnesses.len()
            || !self.sapling_witnesses.keys().eq(prev_witnesses.keys());

        if changed {
            if let Err(err) = self.save() {
                self.sapling_scan_height = prev_scan_height;
                self.sapling_scan_hash = prev_scan_hash;
                self.sapling_next_position = prev_next_position;
                self.sapling_notes = prev_notes;
                self.sapling_tree = prev_tree;
                self.sapling_witnesses = prev_witnesses;
                return Err(err);
            }
            self.revision = self.revision.saturating_add(1);
        }

        Ok(())
    }

    pub fn generate_new_sapling_address_bytes(&mut self) -> Result<[u8; 43], WalletError> {
        let mut added_key = false;
        if self.sapling_keys.is_empty() {
            self.require_unlocked()?;
            let mut rng = rand::rngs::OsRng;
            let mut seed = [0u8; 32];
            rng.fill_bytes(&mut seed);
            let extsk = ExtendedSpendingKey::master(&seed);
            let extsk_bytes = extsk.to_bytes();
            let extfvk = sapling_extfvk_from_extsk(&extsk_bytes)?;
            self.sapling_keys.push(SaplingKeyEntry {
                extfvk,
                extsk: Some(extsk_bytes),
                next_diversifier_index: [0u8; 11],
            });
            added_key = true;
        }

        let entry = self
            .sapling_keys
            .first_mut()
            .ok_or(WalletError::InvalidData("sapling key state missing"))?;
        let prev_entry = entry.clone();

        let extfvk =
            sapling_crypto::zip32::ExtendedFullViewingKey::read(entry.extfvk.as_slice())
                .map_err(|_| WalletError::InvalidData("invalid sapling viewing key encoding"))?;
        let dfvk = extfvk.to_diversifiable_full_viewing_key();

        let start_index = DiversifierIndex::from(entry.next_diversifier_index);
        let (found_index, address) =
            dfvk.find_address(start_index)
                .ok_or(WalletError::InvalidData(
                    "sapling diversifier space exhausted",
                ))?;

        let mut next_index = found_index;
        next_index
            .increment()
            .map_err(|_| WalletError::InvalidData("sapling diversifier index overflow"))?;
        entry.next_diversifier_index = *next_index.as_bytes();

        if let Err(err) = self.save() {
            if let Some(first) = self.sapling_keys.first_mut() {
                *first = prev_entry;
            }
            if added_key {
                self.sapling_keys.clear();
            }
            return Err(err);
        }
        self.revision = self.revision.saturating_add(1);
        Ok(address.to_bytes())
    }

    pub fn sapling_addresses_bytes(&self) -> Result<Vec<[u8; 43]>, WalletError> {
        let mut out = Vec::new();

        for entry in &self.sapling_keys {
            let extfvk =
                sapling_crypto::zip32::ExtendedFullViewingKey::read(entry.extfvk.as_slice())
                    .map_err(|_| {
                        WalletError::InvalidData("invalid sapling viewing key encoding")
                    })?;
            let dfvk = extfvk.to_diversifiable_full_viewing_key();

            let mut index = DiversifierIndex::from([0u8; 11]);
            let stop = DiversifierIndex::from(entry.next_diversifier_index);

            while index < stop {
                let Some((found_index, address)) = dfvk.find_address(index) else {
                    return Err(WalletError::InvalidData(
                        "sapling diversifier space exhausted",
                    ));
                };
                if found_index >= stop {
                    break;
                }
                out.push(address.to_bytes());
                index = found_index;
                index
                    .increment()
                    .map_err(|_| WalletError::InvalidData("sapling diversifier index overflow"))?;
            }
        }

        Ok(out)
    }

    pub fn sapling_viewing_addresses_bytes(&self) -> Result<Vec<[u8; 43]>, WalletError> {
        let mut out = Vec::new();

        for entry in &self.sapling_viewing_keys {
            let extfvk =
                sapling_crypto::zip32::ExtendedFullViewingKey::read(entry.extfvk.as_slice())
                    .map_err(|_| {
                        WalletError::InvalidData("invalid sapling viewing key encoding")
                    })?;
            let dfvk = extfvk.to_diversifiable_full_viewing_key();

            let mut index = DiversifierIndex::from([0u8; 11]);
            let stop = DiversifierIndex::from(entry.next_diversifier_index);

            while index < stop {
                let Some((found_index, address)) = dfvk.find_address(index) else {
                    return Err(WalletError::InvalidData(
                        "sapling diversifier space exhausted",
                    ));
                };
                if found_index >= stop {
                    break;
                }
                out.push(address.to_bytes());
                index = found_index;
                index
                    .increment()
                    .map_err(|_| WalletError::InvalidData("sapling diversifier index overflow"))?;
            }
        }

        Ok(out)
    }

    pub fn sapling_extsk_for_address(
        &self,
        bytes: &[u8; 43],
    ) -> Result<Option<[u8; 169]>, WalletError> {
        let Some(addr) = PaymentAddress::from_bytes(bytes) else {
            return Ok(None);
        };

        for entry in &self.sapling_keys {
            let extfvk =
                sapling_crypto::zip32::ExtendedFullViewingKey::read(entry.extfvk.as_slice())
                    .map_err(|_| {
                        WalletError::InvalidData("invalid sapling viewing key encoding")
                    })?;
            let dfvk = extfvk.to_diversifiable_full_viewing_key();
            if dfvk.decrypt_diversifier(&addr).is_some() {
                let extsk = entry.extsk.ok_or(WalletError::WalletLocked)?;
                return Ok(Some(extsk));
            }
        }

        Ok(None)
    }

    pub fn sapling_extfvk_for_address(
        &self,
        bytes: &[u8; 43],
    ) -> Result<Option<[u8; 169]>, WalletError> {
        let Some(addr) = PaymentAddress::from_bytes(bytes) else {
            return Ok(None);
        };

        for entry in &self.sapling_keys {
            let extfvk =
                sapling_crypto::zip32::ExtendedFullViewingKey::read(entry.extfvk.as_slice())
                    .map_err(|_| {
                        WalletError::InvalidData("invalid sapling viewing key encoding")
                    })?;
            let dfvk = extfvk.to_diversifiable_full_viewing_key();
            if dfvk.decrypt_diversifier(&addr).is_some() {
                return Ok(Some(entry.extfvk));
            }
        }

        for entry in &self.sapling_viewing_keys {
            let extfvk =
                sapling_crypto::zip32::ExtendedFullViewingKey::read(entry.extfvk.as_slice())
                    .map_err(|_| {
                        WalletError::InvalidData("invalid sapling viewing key encoding")
                    })?;
            let dfvk = extfvk.to_diversifiable_full_viewing_key();
            if dfvk.decrypt_diversifier(&addr).is_some() {
                return Ok(Some(entry.extfvk));
            }
        }

        Ok(None)
    }

    pub fn import_sapling_extsk(&mut self, extsk: [u8; 169]) -> Result<bool, WalletError> {
        self.require_unlocked()?;
        let _ = ExtendedSpendingKey::from_bytes(&extsk)
            .map_err(|_| WalletError::InvalidData("invalid sapling spending key encoding"))?;
        let extfvk = sapling_extfvk_from_extsk(&extsk)?;

        if self.sapling_keys.iter().any(|entry| entry.extfvk == extfvk) {
            return Ok(false);
        }

        let prev_len = self.sapling_keys.len();
        self.sapling_keys.push(SaplingKeyEntry {
            extfvk,
            extsk: Some(extsk),
            next_diversifier_index: [0u8; 11],
        });

        if let Err(err) = self.save() {
            self.sapling_keys.truncate(prev_len);
            return Err(err);
        }
        self.revision = self.revision.saturating_add(1);
        Ok(true)
    }

    pub fn import_sapling_extfvk(&mut self, extfvk: [u8; 169]) -> Result<bool, WalletError> {
        let extfvk_parsed = sapling_crypto::zip32::ExtendedFullViewingKey::read(extfvk.as_slice())
            .map_err(|_| WalletError::InvalidData("invalid sapling viewing key encoding"))?;

        if self
            .sapling_viewing_keys
            .iter()
            .any(|entry| entry.extfvk == extfvk)
        {
            return Ok(false);
        }

        let (default_index, _) = extfvk_parsed.default_address();
        let mut next_index = default_index;
        next_index
            .increment()
            .map_err(|_| WalletError::InvalidData("sapling diversifier index overflow"))?;

        let next_diversifier_index = *next_index.as_bytes();

        let prev_len = self.sapling_viewing_keys.len();
        self.sapling_viewing_keys.push(SaplingViewingKeyEntry {
            extfvk,
            next_diversifier_index,
        });

        if let Err(err) = self.save() {
            self.sapling_viewing_keys.truncate(prev_len);
            return Err(err);
        }

        self.revision = self.revision.saturating_add(1);
        Ok(true)
    }

    pub fn sapling_address_is_mine(&self, bytes: &[u8; 43]) -> Result<bool, WalletError> {
        let Some(addr) = PaymentAddress::from_bytes(bytes) else {
            return Ok(false);
        };

        for entry in &self.sapling_keys {
            let extfvk =
                sapling_crypto::zip32::ExtendedFullViewingKey::read(entry.extfvk.as_slice())
                    .map_err(|_| {
                        WalletError::InvalidData("invalid sapling viewing key encoding")
                    })?;
            let dfvk = extfvk.to_diversifiable_full_viewing_key();
            if dfvk.decrypt_diversifier(&addr).is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn sapling_address_is_watchonly(&self, bytes: &[u8; 43]) -> Result<bool, WalletError> {
        if self.sapling_address_is_mine(bytes)? {
            return Ok(false);
        }

        let Some(addr) = PaymentAddress::from_bytes(bytes) else {
            return Ok(false);
        };

        for entry in &self.sapling_viewing_keys {
            let extfvk =
                sapling_crypto::zip32::ExtendedFullViewingKey::read(entry.extfvk.as_slice())
                    .map_err(|_| {
                        WalletError::InvalidData("invalid sapling viewing key encoding")
                    })?;
            let dfvk = extfvk.to_diversifiable_full_viewing_key();
            if dfvk.decrypt_diversifier(&addr).is_some() {
                return Ok(true);
            }
        }

        Ok(false)
    }

    pub fn refill_keypool(&mut self, newsize: usize) -> Result<(), WalletError> {
        self.require_unlocked()?;
        if self.keypool.len() >= newsize {
            return Ok(());
        }
        let prev_len = self.keypool.len();
        while self.keypool.len() < newsize {
            let key = self.generate_unique_key(true)?;
            let created_at = current_unix_seconds();
            self.keypool.push_back(KeyPoolEntry { key, created_at });
        }
        if let Err(err) = self.save() {
            while self.keypool.len() > prev_len {
                self.keypool.pop_back();
            }
            return Err(err);
        }
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    fn reserve_from_keypool_or_generate(
        &mut self,
        compressed: bool,
        is_change: bool,
    ) -> Result<String, WalletError> {
        let prev_key_len = self.keys.len();
        let mut change_added = None;

        let popped = if compressed {
            self.keypool.pop_front()
        } else {
            None
        };
        let reserved = match popped.as_ref() {
            Some(entry) => entry.key.clone(),
            None => {
                self.require_unlocked()?;
                self.generate_unique_key(compressed)?
            }
        };

        let added_keypool = if self.can_generate_keys() {
            self.ensure_keypool_minimum(DEFAULT_KEYPOOL_SIZE)?
        } else {
            0
        };

        let address = reserved.address(self.network)?;
        if is_change {
            let key_hash = reserved.p2pkh_key_hash()?;
            if self.change_key_hashes.insert(key_hash) {
                change_added = Some(key_hash);
            }
        }
        self.keys.push(reserved);
        if let Err(err) = self.save() {
            self.keys.truncate(prev_key_len);
            for _ in 0..added_keypool {
                self.keypool.pop_back();
            }
            if let Some(entry) = popped {
                self.keypool.push_front(entry);
            }
            if let Some(key_hash) = change_added {
                self.change_key_hashes.remove(&key_hash);
            }
            return Err(err);
        }
        self.revision = self.revision.saturating_add(1);
        Ok(address)
    }

    fn ensure_keypool_minimum(&mut self, target: usize) -> Result<usize, WalletError> {
        let mut added = 0usize;
        while self.keypool.len() < target {
            let key = self.generate_unique_key(true)?;
            self.keypool.push_back(KeyPoolEntry {
                key,
                created_at: current_unix_seconds(),
            });
            added = added.saturating_add(1);
        }
        Ok(added)
    }

    fn generate_unique_key(&self, compressed: bool) -> Result<WalletKey, WalletError> {
        let mut rng = rand::rngs::OsRng;
        let mut seed = [0u8; 32];
        for _ in 0..100 {
            rng.fill_bytes(&mut seed);
            let key = match WalletKey::from_secret(seed, compressed) {
                Ok(key) => key,
                Err(WalletError::InvalidSecretKey) => continue,
                Err(err) => return Err(err),
            };
            if self
                .keys
                .iter()
                .any(|existing| existing.key_hash == key.key_hash)
            {
                continue;
            }
            if self
                .keypool
                .iter()
                .any(|entry| entry.key.key_hash == key.key_hash)
            {
                continue;
            }
            return Ok(key);
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
        for redeem_script in self.redeem_scripts.values() {
            let script_pubkey = p2sh_script_pubkey_from_redeem_script(redeem_script);
            if let Some(addr) = script_pubkey_to_address(&script_pubkey, self.network) {
                if allow.contains(addr.as_str()) {
                    out.push(script_pubkey);
                }
            }
        }
        for script_pubkey in &self.watch_scripts {
            if let Some(addr) = script_pubkey_to_address(script_pubkey, self.network) {
                if allow.contains(addr.as_str()) {
                    out.push(script_pubkey.clone());
                }
            }
        }
        out.sort();
        out.dedup();
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

    pub fn backup_to(&mut self, destination: &Path) -> Result<(), WalletError> {
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

    fn encode_secrets_blob(&self) -> Result<Vec<u8>, WalletError> {
        let mut transparent: Vec<([u8; 32], bool)> = Vec::new();
        for key in &self.keys {
            let secret = key.secret.ok_or(WalletError::WalletLocked)?;
            transparent.push((secret, key.compressed));
        }
        for entry in &self.keypool {
            let secret = entry.key.secret.ok_or(WalletError::WalletLocked)?;
            transparent.push((secret, entry.key.compressed));
        }
        transparent.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        transparent.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);

        let mut sapling: Vec<[u8; 169]> = Vec::new();
        for entry in &self.sapling_keys {
            let extsk = entry.extsk.ok_or(WalletError::WalletLocked)?;
            sapling.push(extsk);
        }
        sapling.sort();
        sapling.dedup();

        let mut encoder = Encoder::new();
        encoder.write_u32_le(WALLET_SECRETS_VERSION);
        encoder.write_varint(transparent.len() as u64);
        for (secret, compressed) in transparent {
            encoder.write_bytes(&secret);
            encoder.write_u8(if compressed { 1 } else { 0 });
        }
        encoder.write_varint(sapling.len() as u64);
        for extsk in sapling {
            encoder.write_bytes(&extsk);
        }
        Ok(encoder.into_inner())
    }

    fn apply_secrets_blob(&mut self, blob: &[u8]) -> Result<(), WalletError> {
        let mut decoder = Decoder::new(blob);
        let version = decoder.read_u32_le()?;
        if version != WALLET_SECRETS_VERSION {
            return Err(WalletError::InvalidData(
                "unsupported wallet secrets version",
            ));
        }

        let transparent_count = decoder.read_varint()?;
        let transparent_count = usize::try_from(transparent_count)
            .map_err(|_| WalletError::InvalidData("wallet secrets key count too large"))?;
        let mut transparent =
            HashMap::<[u8; 20], ([u8; 32], bool)>::with_capacity(transparent_count.min(8192));
        for _ in 0..transparent_count {
            let secret = decoder.read_fixed::<32>()?;
            let compressed = decoder.read_bool()?;
            let key = WalletKey::from_secret(secret, compressed)?;
            transparent.insert(key.key_hash, (secret, compressed));
        }

        let sapling_count = decoder.read_varint()?;
        let sapling_count = usize::try_from(sapling_count)
            .map_err(|_| WalletError::InvalidData("wallet secrets sapling count too large"))?;
        let mut sapling = HashMap::<[u8; 169], [u8; 169]>::with_capacity(sapling_count.min(64));
        for _ in 0..sapling_count {
            let extsk_bytes = decoder.read_fixed::<169>()?;
            let extfvk = sapling_extfvk_from_extsk(&extsk_bytes)?;
            sapling.insert(extfvk, extsk_bytes);
        }

        if !decoder.is_empty() {
            return Err(WalletError::InvalidData(
                "wallet secrets blob has trailing bytes",
            ));
        }

        for key in &mut self.keys {
            let Some((secret, compressed)) = transparent.get(&key.key_hash) else {
                return Err(WalletError::InvalidData("missing wallet secret key"));
            };
            if key.compressed != *compressed {
                return Err(WalletError::InvalidData(
                    "wallet secret compression mismatch",
                ));
            }
            key.secret = Some(*secret);
            key.ensure_pubkey_bytes()?;
            key.validate_pubkey_bytes()?;
        }

        for entry in &mut self.keypool {
            let Some((secret, compressed)) = transparent.get(&entry.key.key_hash) else {
                return Err(WalletError::InvalidData("missing keypool secret key"));
            };
            if entry.key.compressed != *compressed {
                return Err(WalletError::InvalidData(
                    "wallet keypool compression mismatch",
                ));
            }
            entry.key.secret = Some(*secret);
            entry.key.ensure_pubkey_bytes()?;
            entry.key.validate_pubkey_bytes()?;
        }

        for entry in &mut self.sapling_keys {
            let Some(extsk) = sapling.get(&entry.extfvk) else {
                return Err(WalletError::InvalidData("missing sapling spending key"));
            };
            entry.extsk = Some(*extsk);
        }

        Ok(())
    }

    fn decode(path: &Path, expected_network: Network, bytes: &[u8]) -> Result<Self, WalletError> {
        let mut decoder = Decoder::new(bytes);
        let version = decoder.read_u32_le()?;
        if version == 0 || version > WALLET_FILE_VERSION {
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

        if version <= 10 {
            let key_count = decoder.read_varint()?;
            let key_count = usize::try_from(key_count)
                .map_err(|_| WalletError::InvalidData("wallet key count too large"))?;
            let mut keys = Vec::with_capacity(key_count.min(4096));
            for _ in 0..key_count {
                let secret = decoder.read_fixed::<32>()?;
                let compressed = decoder.read_bool()?;
                keys.push(WalletKey::from_secret(secret, compressed)?);
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

            let mut tx_history = BTreeSet::new();
            if version >= 3 {
                let count = decoder.read_varint()?;
                let count = usize::try_from(count)
                    .map_err(|_| WalletError::InvalidData("tx history count too large"))?;
                for _ in 0..count {
                    let txid = decoder.read_fixed::<32>()?;
                    tx_history.insert(txid);
                }
            }

            let mut keypool = VecDeque::new();
            if version >= 4 {
                let count = decoder.read_varint()?;
                let count = usize::try_from(count)
                    .map_err(|_| WalletError::InvalidData("keypool count too large"))?;
                for _ in 0..count {
                    let secret = decoder.read_fixed::<32>()?;
                    let compressed = decoder.read_bool()?;
                    let created_at = decoder.read_u64_le()?;
                    keypool.push_back(KeyPoolEntry {
                        key: WalletKey::from_secret(secret, compressed)?,
                        created_at,
                    });
                }
            }

            let mut sapling_keys = Vec::new();
            if version >= 5 {
                let count = decoder.read_varint()?;
                let count = usize::try_from(count)
                    .map_err(|_| WalletError::InvalidData("sapling key count too large"))?;
                sapling_keys = Vec::with_capacity(count.min(16));
                for _ in 0..count {
                    let extsk = decoder.read_fixed::<169>()?;
                    let next_diversifier_index = decoder.read_fixed::<11>()?;
                    let extfvk = sapling_extfvk_from_extsk(&extsk)?;
                    sapling_keys.push(SaplingKeyEntry {
                        extfvk,
                        extsk: Some(extsk),
                        next_diversifier_index,
                    });
                }
            }

            let mut sapling_viewing_keys = Vec::new();
            if version >= 6 {
                let count = decoder.read_varint()?;
                let count = usize::try_from(count)
                    .map_err(|_| WalletError::InvalidData("sapling viewing key count too large"))?;
                sapling_viewing_keys = Vec::with_capacity(count.min(16));
                for _ in 0..count {
                    let extfvk = decoder.read_fixed::<169>()?;
                    let next_diversifier_index = decoder.read_fixed::<11>()?;
                    sapling_viewing_keys.push(SaplingViewingKeyEntry {
                        extfvk,
                        next_diversifier_index,
                    });
                }
            }

            let mut change_key_hashes = BTreeSet::new();
            if version >= 7 {
                let count = decoder.read_varint()?;
                let count = usize::try_from(count)
                    .map_err(|_| WalletError::InvalidData("change key hash count too large"))?;
                for _ in 0..count {
                    let key_hash = decoder.read_fixed::<20>()?;
                    change_key_hashes.insert(key_hash);
                }
            }

            let mut sapling_scan_height = -1i32;
            let mut sapling_scan_hash = [0u8; 32];
            let mut sapling_next_position = 0u64;
            let mut sapling_notes: BTreeMap<SaplingNoteKey, SaplingNoteRecord> = BTreeMap::new();
            if version >= 8 {
                sapling_scan_height = decoder.read_i32_le()?;
                sapling_scan_hash = decoder.read_fixed::<32>()?;
                sapling_next_position = decoder.read_u64_le()?;
                let count = decoder.read_varint()?;
                let count = usize::try_from(count)
                    .map_err(|_| WalletError::InvalidData("sapling note count too large"))?;
                sapling_notes = BTreeMap::new();
                for _ in 0..count {
                    let txid = decoder.read_fixed::<32>()?;
                    let out_index = decoder.read_u32_le()?;
                    let height = decoder.read_i32_le()?;
                    let position = decoder.read_u64_le()?;
                    let value = decoder.read_i64_le()?;
                    let address = decoder.read_fixed::<43>()?;
                    let nullifier = decoder.read_fixed::<32>()?;
                    let rseed = if version >= 10 {
                        match decoder.read_u8()? {
                            0 => None,
                            1 => Some(SaplingRseedBytes::BeforeZip212(decoder.read_fixed::<32>()?)),
                            2 => Some(SaplingRseedBytes::AfterZip212(decoder.read_fixed::<32>()?)),
                            _ => {
                                return Err(WalletError::InvalidData(
                                    "invalid sapling note rseed encoding",
                                ))
                            }
                        }
                    } else {
                        None
                    };
                    sapling_notes.insert(
                        (txid, out_index),
                        SaplingNoteRecord {
                            address,
                            value,
                            height,
                            position,
                            nullifier,
                            rseed,
                        },
                    );
                }
            }

            let mut sapling_tree = SaplingCommitmentTree::empty();
            let mut sapling_witnesses: BTreeMap<SaplingNoteKey, SaplingIncrementalWitness> =
                BTreeMap::new();
            if version >= 9 {
                let tree_bytes = decoder.read_var_bytes()?;
                if !tree_bytes.is_empty() {
                    sapling_tree = read_commitment_tree(Cursor::new(tree_bytes))
                        .map_err(|_| WalletError::InvalidData("invalid sapling tree encoding"))?;
                }

                let count = decoder.read_varint()?;
                let count = usize::try_from(count)
                    .map_err(|_| WalletError::InvalidData("sapling witness count too large"))?;
                sapling_witnesses = BTreeMap::new();
                for _ in 0..count {
                    let txid = decoder.read_fixed::<32>()?;
                    let out_index = decoder.read_u32_le()?;
                    let witness_bytes = decoder.read_var_bytes()?;
                    let witness =
                        read_incremental_witness(Cursor::new(witness_bytes)).map_err(|_| {
                            WalletError::InvalidData("invalid sapling witness encoding")
                        })?;
                    sapling_witnesses.insert((txid, out_index), witness);
                }
            }

            if !decoder.is_empty() {
                return Err(WalletError::InvalidData("wallet file has trailing bytes"));
            }

            return Ok(Self {
                path: path.to_path_buf(),
                network,
                keys,
                watch_scripts,
                redeem_scripts: BTreeMap::new(),
                tx_history,
                tx_received_at: BTreeMap::new(),
                tx_store: BTreeMap::new(),
                keypool,
                sapling_keys,
                sapling_viewing_keys,
                change_key_hashes,
                sapling_scan_height,
                sapling_scan_hash,
                sapling_next_position,
                sapling_notes,
                sapling_tree,
                sapling_witnesses,
                revision: 0,
                locked_outpoints: HashSet::new(),
                pay_tx_fee_per_kb,
                encrypted_secrets: None,
                unlocked_key: None,
                unlocked_until: 0,
                unlock_generation: 0,
            });
        }

        let encrypted_flag = decoder.read_bool()?;
        let mut encrypted_secrets = None;
        if encrypted_flag {
            let enc_version = decoder.read_u8()?;
            if enc_version != WALLET_ENCRYPTION_VERSION {
                return Err(WalletError::InvalidData(
                    "unsupported wallet encryption version",
                ));
            }
            let mem_kib = decoder.read_u32_le()?;
            let iters = decoder.read_u32_le()?;
            let parallelism = decoder.read_u32_le()?;
            let salt = decoder.read_fixed::<WALLET_ENCRYPTION_SALT_BYTES>()?;
            let nonce = decoder.read_fixed::<WALLET_ENCRYPTION_NONCE_BYTES>()?;
            encrypted_secrets = Some(WalletEncryptedSecrets {
                kdf: WalletKdfParams {
                    mem_kib,
                    iters,
                    parallelism,
                    salt,
                },
                nonce,
                ciphertext: Vec::new(),
            });
        }

        let key_count = decoder.read_varint()?;
        let key_count = usize::try_from(key_count)
            .map_err(|_| WalletError::InvalidData("wallet key count too large"))?;
        let mut keys = Vec::with_capacity(key_count.min(4096));
        for _ in 0..key_count {
            let key_hash = decoder.read_fixed::<20>()?;
            let compressed = decoder.read_bool()?;
            let pubkey_bytes = if version >= 12 {
                decoder.read_var_bytes()?
            } else {
                Vec::new()
            };
            let key = WalletKey {
                key_hash,
                secret: None,
                compressed,
                pubkey_bytes,
            };
            key.validate_pubkey_bytes()?;
            keys.push(key);
        }

        let watch_count = decoder.read_varint()?;
        let watch_count = usize::try_from(watch_count)
            .map_err(|_| WalletError::InvalidData("watch script count too large"))?;
        let mut watch_scripts = Vec::with_capacity(watch_count.min(4096));
        for _ in 0..watch_count {
            let script = decoder.read_var_bytes()?;
            watch_scripts.push(script);
        }

        let mut redeem_scripts: BTreeMap<[u8; 20], Vec<u8>> = BTreeMap::new();
        if version >= 12 {
            let redeem_count = decoder.read_varint()?;
            let redeem_count = usize::try_from(redeem_count)
                .map_err(|_| WalletError::InvalidData("redeem script count too large"))?;
            for _ in 0..redeem_count {
                let hash = decoder.read_fixed::<20>()?;
                let script = decoder.read_var_bytes()?;
                if hash160(&script) != hash {
                    return Err(WalletError::InvalidData("redeemScript hash mismatch"));
                }
                redeem_scripts.insert(hash, script);
            }
        }

        let tx_count = decoder.read_varint()?;
        let tx_count = usize::try_from(tx_count)
            .map_err(|_| WalletError::InvalidData("tx history count too large"))?;
        let mut tx_history = BTreeSet::new();
        for _ in 0..tx_count {
            let txid = decoder.read_fixed::<32>()?;
            tx_history.insert(txid);
        }

        let mut tx_received_at = BTreeMap::new();
        if version >= 13 {
            let count = decoder.read_varint()?;
            let count = usize::try_from(count)
                .map_err(|_| WalletError::InvalidData("tx received time count too large"))?;
            for _ in 0..count {
                let txid = decoder.read_fixed::<32>()?;
                let received_at = decoder.read_u64_le()?;
                tx_received_at.insert(txid, received_at);
            }
        }

        let mut tx_store: BTreeMap<Hash256, Vec<u8>> = BTreeMap::new();
        if version >= 14 {
            let count = decoder.read_varint()?;
            let count = usize::try_from(count)
                .map_err(|_| WalletError::InvalidData("tx store count too large"))?;
            for _ in 0..count {
                let txid = decoder.read_fixed::<32>()?;
                let raw = decoder.read_var_bytes()?;
                tx_store.insert(txid, raw);
            }
        }

        let keypool_count = decoder.read_varint()?;
        let keypool_count = usize::try_from(keypool_count)
            .map_err(|_| WalletError::InvalidData("keypool count too large"))?;
        let mut keypool = VecDeque::new();
        for _ in 0..keypool_count {
            let key_hash = decoder.read_fixed::<20>()?;
            let compressed = decoder.read_bool()?;
            let pubkey_bytes = if version >= 12 {
                decoder.read_var_bytes()?
            } else {
                Vec::new()
            };
            let created_at = decoder.read_u64_le()?;
            let key = WalletKey {
                key_hash,
                secret: None,
                compressed,
                pubkey_bytes,
            };
            key.validate_pubkey_bytes()?;
            keypool.push_back(KeyPoolEntry { key, created_at });
        }

        let sapling_count = decoder.read_varint()?;
        let sapling_count = usize::try_from(sapling_count)
            .map_err(|_| WalletError::InvalidData("sapling key count too large"))?;
        let mut sapling_keys = Vec::with_capacity(sapling_count.min(16));
        for _ in 0..sapling_count {
            let extfvk = decoder.read_fixed::<169>()?;
            let next_diversifier_index = decoder.read_fixed::<11>()?;
            sapling_keys.push(SaplingKeyEntry {
                extfvk,
                extsk: None,
                next_diversifier_index,
            });
        }

        let viewing_count = decoder.read_varint()?;
        let viewing_count = usize::try_from(viewing_count)
            .map_err(|_| WalletError::InvalidData("sapling viewing key count too large"))?;
        let mut sapling_viewing_keys = Vec::with_capacity(viewing_count.min(16));
        for _ in 0..viewing_count {
            let extfvk = decoder.read_fixed::<169>()?;
            let next_diversifier_index = decoder.read_fixed::<11>()?;
            sapling_viewing_keys.push(SaplingViewingKeyEntry {
                extfvk,
                next_diversifier_index,
            });
        }

        let change_count = decoder.read_varint()?;
        let change_count = usize::try_from(change_count)
            .map_err(|_| WalletError::InvalidData("change key hash count too large"))?;
        let mut change_key_hashes = BTreeSet::new();
        for _ in 0..change_count {
            let key_hash = decoder.read_fixed::<20>()?;
            change_key_hashes.insert(key_hash);
        }

        let sapling_scan_height = decoder.read_i32_le()?;
        let sapling_scan_hash = decoder.read_fixed::<32>()?;
        let sapling_next_position = decoder.read_u64_le()?;
        let note_count = decoder.read_varint()?;
        let note_count = usize::try_from(note_count)
            .map_err(|_| WalletError::InvalidData("sapling note count too large"))?;
        let mut sapling_notes: BTreeMap<SaplingNoteKey, SaplingNoteRecord> = BTreeMap::new();
        for _ in 0..note_count {
            let txid = decoder.read_fixed::<32>()?;
            let out_index = decoder.read_u32_le()?;
            let height = decoder.read_i32_le()?;
            let position = decoder.read_u64_le()?;
            let value = decoder.read_i64_le()?;
            let address = decoder.read_fixed::<43>()?;
            let nullifier = decoder.read_fixed::<32>()?;
            let rseed = match decoder.read_u8()? {
                0 => None,
                1 => Some(SaplingRseedBytes::BeforeZip212(decoder.read_fixed::<32>()?)),
                2 => Some(SaplingRseedBytes::AfterZip212(decoder.read_fixed::<32>()?)),
                _ => {
                    return Err(WalletError::InvalidData(
                        "invalid sapling note rseed encoding",
                    ))
                }
            };
            sapling_notes.insert(
                (txid, out_index),
                SaplingNoteRecord {
                    address,
                    value,
                    height,
                    position,
                    nullifier,
                    rseed,
                },
            );
        }

        let tree_bytes = decoder.read_var_bytes()?;
        let mut sapling_tree = SaplingCommitmentTree::empty();
        if !tree_bytes.is_empty() {
            sapling_tree = read_commitment_tree(Cursor::new(tree_bytes))
                .map_err(|_| WalletError::InvalidData("invalid sapling tree encoding"))?;
        }

        let witness_count = decoder.read_varint()?;
        let witness_count = usize::try_from(witness_count)
            .map_err(|_| WalletError::InvalidData("sapling witness count too large"))?;
        let mut sapling_witnesses: BTreeMap<SaplingNoteKey, SaplingIncrementalWitness> =
            BTreeMap::new();
        for _ in 0..witness_count {
            let txid = decoder.read_fixed::<32>()?;
            let out_index = decoder.read_u32_le()?;
            let witness_bytes = decoder.read_var_bytes()?;
            let witness = read_incremental_witness(Cursor::new(witness_bytes))
                .map_err(|_| WalletError::InvalidData("invalid sapling witness encoding"))?;
            sapling_witnesses.insert((txid, out_index), witness);
        }

        let secrets_payload = decoder.read_var_bytes()?;

        if !decoder.is_empty() {
            return Err(WalletError::InvalidData("wallet file has trailing bytes"));
        }

        let mut wallet = Self {
            path: path.to_path_buf(),
            network,
            keys,
            watch_scripts,
            redeem_scripts,
            tx_history,
            tx_received_at,
            tx_store,
            keypool,
            sapling_keys,
            sapling_viewing_keys,
            change_key_hashes,
            sapling_scan_height,
            sapling_scan_hash,
            sapling_next_position,
            sapling_notes,
            sapling_tree,
            sapling_witnesses,
            revision: 0,
            locked_outpoints: HashSet::new(),
            pay_tx_fee_per_kb,
            encrypted_secrets,
            unlocked_key: None,
            unlocked_until: 0,
            unlock_generation: 0,
        };

        if let Some(enc) = wallet.encrypted_secrets.as_mut() {
            enc.ciphertext = secrets_payload;
        } else {
            wallet.apply_secrets_blob(&secrets_payload)?;
        }

        Ok(wallet)
    }

    fn save(&mut self) -> Result<(), WalletError> {
        let mut encoder = Encoder::new();
        encoder.write_u32_le(WALLET_FILE_VERSION);
        encoder.write_u8(encode_network(self.network));
        encoder.write_i64_le(self.pay_tx_fee_per_kb);

        let encrypted_flag = self.encrypted_secrets.is_some();
        encoder.write_u8(if encrypted_flag { 1 } else { 0 });
        if let Some(enc) = self.encrypted_secrets.as_mut() {
            encoder.write_u8(WALLET_ENCRYPTION_VERSION);
            encoder.write_u32_le(enc.kdf.mem_kib);
            encoder.write_u32_le(enc.kdf.iters);
            encoder.write_u32_le(enc.kdf.parallelism);
            encoder.write_bytes(&enc.kdf.salt);
            encoder.write_bytes(&enc.nonce);
        }

        encoder.write_varint(self.keys.len() as u64);
        for key in &self.keys {
            encoder.write_bytes(&key.key_hash);
            encoder.write_u8(if key.compressed { 1 } else { 0 });
            encoder.write_var_bytes(&key.pubkey_bytes);
        }

        encoder.write_varint(self.watch_scripts.len() as u64);
        for script in &self.watch_scripts {
            encoder.write_var_bytes(script);
        }

        encoder.write_varint(self.redeem_scripts.len() as u64);
        for (hash, script) in &self.redeem_scripts {
            encoder.write_bytes(hash);
            encoder.write_var_bytes(script);
        }

        encoder.write_varint(self.tx_history.len() as u64);
        for txid in &self.tx_history {
            encoder.write_bytes(txid);
        }

        self.tx_received_at
            .retain(|txid, _| self.tx_history.contains(txid));
        encoder.write_varint(self.tx_received_at.len() as u64);
        for (txid, received_at) in &self.tx_received_at {
            encoder.write_bytes(txid);
            encoder.write_u64_le(*received_at);
        }

        self.tx_store
            .retain(|txid, _| self.tx_history.contains(txid));
        encoder.write_varint(self.tx_store.len() as u64);
        for (txid, raw) in &self.tx_store {
            encoder.write_bytes(txid);
            encoder.write_var_bytes(raw);
        }

        encoder.write_varint(self.keypool.len() as u64);
        for entry in &self.keypool {
            encoder.write_bytes(&entry.key.key_hash);
            encoder.write_u8(if entry.key.compressed { 1 } else { 0 });
            encoder.write_var_bytes(&entry.key.pubkey_bytes);
            encoder.write_u64_le(entry.created_at);
        }

        encoder.write_varint(self.sapling_keys.len() as u64);
        for entry in &self.sapling_keys {
            encoder.write_bytes(&entry.extfvk);
            encoder.write_bytes(&entry.next_diversifier_index);
        }

        encoder.write_varint(self.sapling_viewing_keys.len() as u64);
        for entry in &self.sapling_viewing_keys {
            encoder.write_bytes(&entry.extfvk);
            encoder.write_bytes(&entry.next_diversifier_index);
        }

        encoder.write_varint(self.change_key_hashes.len() as u64);
        for key_hash in &self.change_key_hashes {
            encoder.write_bytes(key_hash);
        }

        encoder.write_i32_le(self.sapling_scan_height);
        encoder.write_bytes(&self.sapling_scan_hash);
        encoder.write_u64_le(self.sapling_next_position);
        encoder.write_varint(self.sapling_notes.len() as u64);
        for ((txid, out_index), note) in &self.sapling_notes {
            encoder.write_bytes(txid);
            encoder.write_u32_le(*out_index);
            encoder.write_i32_le(note.height);
            encoder.write_u64_le(note.position);
            encoder.write_i64_le(note.value);
            encoder.write_bytes(&note.address);
            encoder.write_bytes(&note.nullifier);
            match note.rseed {
                None => encoder.write_u8(0),
                Some(SaplingRseedBytes::BeforeZip212(bytes)) => {
                    encoder.write_u8(1);
                    encoder.write_bytes(&bytes);
                }
                Some(SaplingRseedBytes::AfterZip212(bytes)) => {
                    encoder.write_u8(2);
                    encoder.write_bytes(&bytes);
                }
            }
        }
        let mut sapling_tree_bytes = Vec::new();
        write_commitment_tree(&self.sapling_tree, &mut sapling_tree_bytes)
            .map_err(|_| WalletError::InvalidData("invalid sapling tree state"))?;
        encoder.write_var_bytes(&sapling_tree_bytes);

        encoder.write_varint(self.sapling_witnesses.len() as u64);
        for ((txid, out_index), witness) in &self.sapling_witnesses {
            encoder.write_bytes(txid);
            encoder.write_u32_le(*out_index);
            let mut bytes = Vec::new();
            write_incremental_witness(witness, &mut bytes)
                .map_err(|_| WalletError::InvalidData("invalid sapling witness state"))?;
            encoder.write_var_bytes(&bytes);
        }

        let mut secrets_payload = if encrypted_flag {
            if let Some(key) = self.unlocked_key.as_ref() {
                let mut plaintext = self.encode_secrets_blob()?;
                let mut nonce = [0u8; WALLET_ENCRYPTION_NONCE_BYTES];
                rand::rngs::OsRng.fill_bytes(&mut nonce);
                let ciphertext = encrypt_wallet_secrets(self.network, key, &nonce, &plaintext)?;
                plaintext.zeroize();
                if let Some(enc) = self.encrypted_secrets.as_mut() {
                    enc.nonce = nonce;
                    enc.ciphertext = ciphertext;
                }
            }
            self.encrypted_secrets
                .as_ref()
                .map(|enc| enc.ciphertext.clone())
                .unwrap_or_default()
        } else {
            self.encode_secrets_blob()?
        };

        encoder.write_var_bytes(&secrets_payload);
        secrets_payload.zeroize();

        let bytes = encoder.into_inner();
        write_file_atomic(&self.path, &bytes)?;
        Ok(())
    }
}

struct SaplingScanKey {
    ivk: PreparedIncomingViewingKey,
    nk: NullifierDerivingKey,
}

impl Wallet {
    fn has_key_hash(&self, key_hash: &[u8; 20]) -> bool {
        self.keys.iter().any(|key| &key.key_hash == key_hash)
            || self
                .keypool
                .iter()
                .any(|entry| &entry.key.key_hash == key_hash)
    }

    fn has_pubkey(&self, target: &PublicKey) -> bool {
        for key in &self.keys {
            if wallet_key_matches_pubkey(key, target) {
                return true;
            }
        }
        for entry in &self.keypool {
            if wallet_key_matches_pubkey(&entry.key, target) {
                return true;
            }
        }
        false
    }

    fn can_spend_redeem_script(&self, redeem_script: &[u8]) -> bool {
        if let Some((required, pubkeys)) = parse_multisig_redeem_script_with_required(redeem_script)
        {
            let mut available = 0usize;
            for pubkey in pubkeys {
                if self.has_pubkey(&pubkey) {
                    available = available.saturating_add(1);
                    if available >= required {
                        return true;
                    }
                }
            }
            return false;
        }

        if let Some(key_hash) = p2pkh_key_hash_from_script_pubkey(redeem_script) {
            return self.has_key_hash(&key_hash);
        }

        false
    }

    fn sapling_scan_keys(&self) -> Result<Vec<SaplingScanKey>, WalletError> {
        let mut keys =
            Vec::with_capacity(self.sapling_keys.len() + self.sapling_viewing_keys.len());

        for entry in &self.sapling_keys {
            let extfvk =
                sapling_crypto::zip32::ExtendedFullViewingKey::read(entry.extfvk.as_slice())
                    .map_err(|_| {
                        WalletError::InvalidData("invalid sapling viewing key encoding")
                    })?;
            let ivk = extfvk.fvk.vk.ivk();
            keys.push(SaplingScanKey {
                ivk: PreparedIncomingViewingKey::new(&ivk),
                nk: extfvk.fvk.vk.nk,
            });
        }

        for entry in &self.sapling_viewing_keys {
            let extfvk =
                sapling_crypto::zip32::ExtendedFullViewingKey::read(entry.extfvk.as_slice())
                    .map_err(|_| {
                        WalletError::InvalidData("invalid sapling viewing key encoding")
                    })?;
            let ivk = extfvk.fvk.vk.ivk();
            keys.push(SaplingScanKey {
                ivk: PreparedIncomingViewingKey::new(&ivk),
                nk: extfvk.fvk.vk.nk,
            });
        }

        Ok(keys)
    }
}

struct SaplingOutputRef<'a> {
    output: &'a fluxd_primitives::transaction::OutputDescription,
}

impl ShieldedOutput<SaplingDomain, ENC_CIPHERTEXT_SIZE> for SaplingOutputRef<'_> {
    fn ephemeral_key(&self) -> EphemeralKeyBytes {
        EphemeralKeyBytes(self.output.ephemeral_key)
    }

    fn cmstar_bytes(
        &self,
    ) -> <SaplingDomain as zcash_note_encryption::Domain>::ExtractedCommitmentBytes {
        self.output.cm
    }

    fn enc_ciphertext(&self) -> &[u8; ENC_CIPHERTEXT_SIZE] {
        &self.output.enc_ciphertext
    }
}

fn scan_sapling_output(
    keys: &[SaplingScanKey],
    output: &fluxd_primitives::transaction::OutputDescription,
    position: u64,
    height: i32,
) -> Option<SaplingNoteRecord> {
    let output_ref = SaplingOutputRef { output };
    for key in keys {
        let (note, recipient, _memo) =
            try_sapling_note_decryption(&key.ivk, &output_ref, Zip212Enforcement::GracePeriod)?;
        let value_u64 = note.value().inner();
        let value = i64::try_from(value_u64).ok()?;
        let nullifier = note.nf(&key.nk, position).0;
        return Some(SaplingNoteRecord {
            address: recipient.to_bytes(),
            value,
            height,
            position,
            nullifier,
            rseed: Some(SaplingRseedBytes::from_rseed(note.rseed())),
        });
    }
    None
}

fn read_block_by_hash<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    hash: &Hash256,
) -> Result<Block, WalletError> {
    let location = chainstate
        .block_location(hash)
        .map_err(|err| WalletError::ChainState(err.to_string()))?
        .ok_or(WalletError::InvalidData(
            "missing block location for wallet scan",
        ))?;
    let bytes = chainstate
        .read_block(location)
        .map_err(|err| WalletError::ChainState(err.to_string()))?;
    Block::consensus_decode(&bytes).map_err(|_| WalletError::InvalidData("invalid block bytes"))
}

fn count_sapling_outputs<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    hash: &Hash256,
) -> Result<usize, WalletError> {
    let block = read_block_by_hash(chainstate, hash)?;
    Ok(block
        .transactions
        .iter()
        .map(|tx| tx.shielded_outputs.len())
        .sum())
}

#[cfg(test)]
fn extract_p2pkh_hash(script_pubkey: &[u8]) -> Option<[u8; 20]> {
    if script_pubkey.len() != 25
        || script_pubkey[0] != 0x76
        || script_pubkey[1] != 0xa9
        || script_pubkey[2] != 0x14
        || script_pubkey[23] != 0x88
        || script_pubkey[24] != 0xac
    {
        return None;
    }
    let mut hash = [0u8; 20];
    hash.copy_from_slice(&script_pubkey[3..23]);
    Some(hash)
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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

fn p2pkh_key_hash_from_script_pubkey(script_pubkey: &[u8]) -> Option<[u8; 20]> {
    if script_pubkey.len() != 25 {
        return None;
    }
    if script_pubkey[0] != 0x76 {
        return None;
    }
    if script_pubkey[1] != 0xa9 || script_pubkey[2] != 0x14 {
        return None;
    }
    if script_pubkey[23] != 0x88 || script_pubkey[24] != 0xac {
        return None;
    }
    script_pubkey.get(3..23)?.try_into().ok()
}

fn p2sh_hash_from_script_pubkey(script_pubkey: &[u8]) -> Option<[u8; 20]> {
    if script_pubkey.len() != 23 {
        return None;
    }
    if script_pubkey[0] != 0xa9 || script_pubkey[1] != 0x14 {
        return None;
    }
    if script_pubkey[22] != 0x87 {
        return None;
    }
    script_pubkey.get(2..22)?.try_into().ok()
}

fn p2sh_script_pubkey_from_redeem_script(redeem_script: &[u8]) -> Vec<u8> {
    let hash = hash160(redeem_script);
    let mut script = Vec::with_capacity(23);
    script.push(0xa9);
    script.push(0x14);
    script.extend_from_slice(&hash);
    script.push(0x87);
    script
}

fn wallet_key_matches_pubkey(key: &WalletKey, target: &PublicKey) -> bool {
    if !key.pubkey_bytes.is_empty() {
        let Ok(pubkey) = PublicKey::from_slice(&key.pubkey_bytes) else {
            return false;
        };
        return &pubkey == target;
    }
    let Ok(secret) = key.secret_key() else {
        return false;
    };
    let pubkey = PublicKey::from_secret_key(secp(), &secret);
    &pubkey == target
}

fn decode_small_int_opcode(opcode: u8) -> Option<usize> {
    match opcode {
        0x00 => Some(0),
        0x51..=0x60 => Some((opcode - 0x50) as usize),
        _ => None,
    }
}

fn parse_multisig_redeem_script_with_required(script: &[u8]) -> Option<(usize, Vec<PublicKey>)> {
    const OP_CHECKMULTISIG: u8 = 0xae;
    const OP_PUSHDATA1: u8 = 0x4c;
    const OP_PUSHDATA2: u8 = 0x4d;
    const OP_PUSHDATA4: u8 = 0x4e;

    if script.len() < 3 {
        return None;
    }
    if script.last().copied() != Some(OP_CHECKMULTISIG) {
        return None;
    }
    let n = decode_small_int_opcode(*script.get(script.len().saturating_sub(2))?)?;
    let m = decode_small_int_opcode(*script.first()?)?;
    if m == 0 || n == 0 || m > n || n > 16 {
        return None;
    }

    let mut pubkeys = Vec::new();
    let mut cursor = 1usize;
    let end = script.len().saturating_sub(2);
    while cursor < end {
        let opcode = *script.get(cursor)?;
        let (len, advance) = match opcode {
            len @ 1..=75 => (len as usize, 1usize),
            OP_PUSHDATA1 => (*script.get(cursor + 1)? as usize, 2usize),
            OP_PUSHDATA2 => {
                let bytes: [u8; 2] = script.get(cursor + 1..cursor + 3)?.try_into().ok()?;
                (u16::from_le_bytes(bytes) as usize, 3usize)
            }
            OP_PUSHDATA4 => {
                let bytes: [u8; 4] = script.get(cursor + 1..cursor + 5)?.try_into().ok()?;
                (u32::from_le_bytes(bytes) as usize, 5usize)
            }
            _ => return None,
        };
        cursor = cursor.saturating_add(advance);
        if len != 33 && len != 65 {
            return None;
        }
        let start = cursor;
        let stop = start.saturating_add(len);
        if stop > end {
            return None;
        }
        let pubkey = PublicKey::from_slice(script.get(start..stop)?).ok()?;
        pubkeys.push(pubkey);
        cursor = stop;
    }
    if cursor != end {
        return None;
    }
    if pubkeys.len() != n {
        return None;
    }
    Some((m, pubkeys))
}

fn sapling_extfvk_from_extsk(extsk: &[u8; 169]) -> Result<[u8; 169], WalletError> {
    let extsk = ExtendedSpendingKey::from_bytes(extsk)
        .map_err(|_| WalletError::InvalidData("invalid sapling spending key encoding"))?;
    #[allow(deprecated)]
    let extfvk = extsk.to_extended_full_viewing_key();

    let mut buf = Vec::with_capacity(169);
    extfvk
        .write(&mut buf)
        .map_err(|_| WalletError::InvalidData("invalid sapling viewing key encoding"))?;
    buf.as_slice()
        .try_into()
        .map_err(|_| WalletError::InvalidData("invalid sapling viewing key encoding"))
}

fn wallet_secrets_aad(network: Network) -> Vec<u8> {
    const PREFIX: &[u8] = b"fluxd-wallet-secrets-v1:";
    let mut out = Vec::with_capacity(PREFIX.len() + 1);
    out.extend_from_slice(PREFIX);
    out.push(encode_network(network));
    out
}

fn derive_wallet_key(passphrase: &str, kdf: &WalletKdfParams) -> Result<[u8; 32], WalletError> {
    let params = Argon2Params::new(kdf.mem_kib, kdf.iters, kdf.parallelism, Some(32))
        .map_err(|_| WalletError::InvalidData("invalid wallet kdf parameters"))?;
    let argon2 = Argon2::new(Argon2Algorithm::Argon2id, Argon2Version::V0x13, params);
    let mut out = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), &kdf.salt, &mut out)
        .map_err(|_| WalletError::InvalidData("wallet key derivation failed"))?;
    Ok(out)
}

fn new_wallet_kdf(passphrase: &str) -> Result<(WalletKdfParams, [u8; 32]), WalletError> {
    const MEM_KIB: u32 = 64 * 1024;
    const ITERS: u32 = 3;
    const PAR: u32 = 1;

    let mut salt = [0u8; WALLET_ENCRYPTION_SALT_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let kdf = WalletKdfParams {
        mem_kib: MEM_KIB,
        iters: ITERS,
        parallelism: PAR,
        salt,
    };
    let key = derive_wallet_key(passphrase, &kdf)?;
    Ok((kdf, key))
}

fn encrypt_wallet_secrets(
    network: Network,
    key: &[u8; 32],
    nonce: &[u8; WALLET_ENCRYPTION_NONCE_BYTES],
    plaintext: &[u8],
) -> Result<Vec<u8>, WalletError> {
    let cipher = ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(key));
    let nonce = chacha20poly1305::Nonce::from_slice(nonce);
    let aad = wallet_secrets_aad(network);
    cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| WalletError::InvalidData("wallet encryption failed"))
}

fn decrypt_wallet_secrets(
    network: Network,
    key: &[u8; 32],
    nonce: &[u8; WALLET_ENCRYPTION_NONCE_BYTES],
    ciphertext: &[u8],
) -> Result<Vec<u8>, WalletError> {
    let cipher = ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(key));
    let nonce = chacha20poly1305::Nonce::from_slice(nonce);
    let aad = wallet_secrets_aad(network);
    cipher
        .decrypt(
            nonce,
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| WalletError::IncorrectPassphrase)
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

    #[test]
    fn sapling_key_persists_across_restart() {
        let data_dir = temp_data_dir("fluxd-wallet-sapling-test");
        fs::create_dir_all(&data_dir).expect("create data dir");

        let mut wallet = Wallet::load_or_create(&data_dir, Network::Regtest).expect("wallet");
        assert_eq!(wallet.sapling_viewing_key_count(), 0);
        let addr1 = wallet
            .generate_new_sapling_address_bytes()
            .expect("generate sapling address");
        assert_eq!(wallet.sapling_key_count(), 1);
        let key_bytes = wallet.sapling_keys[0].extsk.expect("sapling spending key");
        let fvk_bytes = wallet.sapling_keys[0].extfvk;
        drop(wallet);

        let mut wallet =
            Wallet::load_or_create(&data_dir, Network::Regtest).expect("wallet reload");
        assert_eq!(wallet.sapling_key_count(), 1);
        assert_eq!(wallet.sapling_viewing_key_count(), 0);
        assert_eq!(
            wallet.sapling_keys[0].extsk.expect("sapling spending key"),
            key_bytes
        );
        assert_eq!(wallet.sapling_keys[0].extfvk, fvk_bytes);
        let addr2 = wallet
            .generate_new_sapling_address_bytes()
            .expect("generate sapling address");
        assert_ne!(addr1, addr2);
    }

    #[test]
    fn tx_received_time_persists_across_restart() {
        let data_dir = temp_data_dir("fluxd-wallet-tx-time-test");
        fs::create_dir_all(&data_dir).expect("create data dir");

        let mut wallet = Wallet::load_or_create(&data_dir, Network::Regtest).expect("wallet");
        let txid = [0x11u8; 32];
        wallet
            .record_txids(std::iter::once(txid))
            .expect("record txids");
        let before = wallet.tx_received_time(&txid).expect("before timestamp");
        assert!(before > 0);
        drop(wallet);

        let wallet = Wallet::load_or_create(&data_dir, Network::Regtest).expect("wallet reload");
        let after = wallet.tx_received_time(&txid).expect("after timestamp");
        assert_eq!(before, after);
    }

    #[test]
    fn encrypted_wallet_roundtrips_and_locks() {
        let data_dir = temp_data_dir("fluxd-wallet-encrypt-test");
        fs::create_dir_all(&data_dir).expect("create data dir");

        let mut wallet = Wallet::load_or_create(&data_dir, Network::Regtest).expect("wallet");
        let wif_a = secret_key_to_wif(&[9u8; 32], Network::Regtest, true);
        wallet.import_wif(&wif_a).expect("import wif");
        wallet
            .generate_new_sapling_address_bytes()
            .expect("generate sapling address");

        let address = wallet
            .default_address()
            .expect("default address")
            .expect("address");

        wallet
            .encryptwallet("test-passphrase")
            .expect("encryptwallet");
        assert!(wallet.is_encrypted());
        assert!(matches!(
            wallet.dump_wif_for_address(&address),
            Err(WalletError::WalletLocked)
        ));

        drop(wallet);

        let mut wallet = Wallet::load_or_create(&data_dir, Network::Regtest).expect("reload");
        assert!(wallet.is_encrypted());
        let address2 = wallet
            .default_address()
            .expect("default address")
            .expect("address");
        assert_eq!(address, address2);
        assert!(matches!(
            wallet.dump_wif_for_address(&address),
            Err(WalletError::WalletLocked)
        ));

        wallet
            .walletpassphrase("test-passphrase", 60)
            .expect("walletpassphrase");
        assert!(wallet
            .dump_wif_for_address(&address)
            .expect("dump wif")
            .is_some());

        wallet.walletlock().expect("walletlock");
        assert!(matches!(
            wallet.dump_wif_for_address(&address),
            Err(WalletError::WalletLocked)
        ));
    }

    #[test]
    fn change_address_tracked_and_persists_across_restart() {
        let data_dir = temp_data_dir("fluxd-wallet-change-test");
        fs::create_dir_all(&data_dir).expect("create data dir");

        let mut wallet = Wallet::load_or_create(&data_dir, Network::Regtest).expect("wallet");
        let receive = wallet.generate_new_address(true).expect("receive address");
        let change = wallet
            .generate_new_change_address(true)
            .expect("change address");
        assert_ne!(receive, change);

        let scripts = wallet.all_script_pubkeys().expect("scripts");
        let change_script = scripts
            .iter()
            .find(|spk| script_pubkey_to_address(spk, Network::Regtest).as_deref() == Some(&change))
            .expect("change script");
        assert!(wallet.is_change_script_pubkey(change_script));

        let receive_script = scripts
            .iter()
            .find(|spk| {
                script_pubkey_to_address(spk, Network::Regtest).as_deref() == Some(&receive)
            })
            .expect("receive script");
        assert!(!wallet.is_change_script_pubkey(receive_script));

        drop(wallet);

        let wallet = Wallet::load_or_create(&data_dir, Network::Regtest).expect("wallet reload");
        assert!(wallet.is_change_script_pubkey(change_script));
        assert!(!wallet.is_change_script_pubkey(receive_script));
    }
}
