use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

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

pub const WALLET_FILE_NAME: &str = "wallet.dat";

pub const WALLET_FILE_VERSION: u32 = 10;

const DEFAULT_KEYPOOL_SIZE: usize = 100;

#[derive(Clone)]
struct KeyPoolEntry {
    key: WalletKey,
    created_at: u64,
}

#[derive(Clone)]
struct SaplingKeyEntry {
    extsk: [u8; 169],
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

    fn p2pkh_key_hash(&self) -> Result<[u8; 20], WalletError> {
        let pubkey_bytes = self.pubkey_bytes()?;
        Ok(hash160(&pubkey_bytes))
    }

    fn p2pkh_script_pubkey(&self) -> Result<Vec<u8>, WalletError> {
        let key_hash = self.p2pkh_key_hash()?;
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
    tx_history: BTreeSet<Hash256>,
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
                tx_history: BTreeSet::new(),
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

    pub fn record_txids(
        &mut self,
        txids: impl IntoIterator<Item = Hash256>,
    ) -> Result<usize, WalletError> {
        let prev_len = self.tx_history.len();
        for txid in txids {
            self.tx_history.insert(txid);
        }
        let added = self.tx_history.len().saturating_sub(prev_len);
        if added > 0 {
            self.save()?;
            self.revision = self.revision.saturating_add(1);
        }
        Ok(added)
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

    pub fn script_pubkey_is_watchonly(&self, script_pubkey: &[u8]) -> bool {
        self.watch_scripts
            .iter()
            .any(|spk| spk.as_slice() == script_pubkey)
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
            let mut rng = rand::rngs::OsRng;
            let mut seed = [0u8; 32];
            rng.fill_bytes(&mut seed);
            let extsk = ExtendedSpendingKey::master(&seed);
            self.sapling_keys.push(SaplingKeyEntry {
                extsk: extsk.to_bytes(),
                next_diversifier_index: [0u8; 11],
            });
            added_key = true;
        }

        let entry = self
            .sapling_keys
            .first_mut()
            .ok_or(WalletError::InvalidData("sapling key state missing"))?;
        let prev_entry = entry.clone();

        let extsk = ExtendedSpendingKey::from_bytes(&entry.extsk)
            .map_err(|_| WalletError::InvalidData("invalid sapling spending key encoding"))?;
        let dfvk = extsk.to_diversifiable_full_viewing_key();

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
            let extsk = ExtendedSpendingKey::from_bytes(&entry.extsk)
                .map_err(|_| WalletError::InvalidData("invalid sapling spending key encoding"))?;
            let dfvk = extsk.to_diversifiable_full_viewing_key();

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
            let extsk = ExtendedSpendingKey::from_bytes(&entry.extsk)
                .map_err(|_| WalletError::InvalidData("invalid sapling spending key encoding"))?;
            let dfvk = extsk.to_diversifiable_full_viewing_key();
            if dfvk.decrypt_diversifier(&addr).is_some() {
                return Ok(Some(entry.extsk));
            }
        }

        Ok(None)
    }

    pub fn sapling_extfvk_for_address(
        &self,
        bytes: &[u8; 43],
    ) -> Result<Option<[u8; 169]>, WalletError> {
        if let Some(extsk_bytes) = self.sapling_extsk_for_address(bytes)? {
            let extsk = ExtendedSpendingKey::from_bytes(&extsk_bytes)
                .map_err(|_| WalletError::InvalidData("invalid sapling spending key encoding"))?;
            #[allow(deprecated)]
            let extfvk = extsk.to_extended_full_viewing_key();

            let mut buf = Vec::with_capacity(169);
            extfvk
                .write(&mut buf)
                .map_err(|_| WalletError::InvalidData("invalid sapling viewing key encoding"))?;

            let extfvk_bytes: [u8; 169] = buf
                .as_slice()
                .try_into()
                .map_err(|_| WalletError::InvalidData("invalid sapling viewing key encoding"))?;
            return Ok(Some(extfvk_bytes));
        }

        let Some(addr) = PaymentAddress::from_bytes(bytes) else {
            return Ok(None);
        };

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
        let _ = ExtendedSpendingKey::from_bytes(&extsk)
            .map_err(|_| WalletError::InvalidData("invalid sapling spending key encoding"))?;

        if self.sapling_keys.iter().any(|entry| entry.extsk == extsk) {
            return Ok(false);
        }

        let prev_len = self.sapling_keys.len();
        self.sapling_keys.push(SaplingKeyEntry {
            extsk,
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
            let extsk = ExtendedSpendingKey::from_bytes(&entry.extsk)
                .map_err(|_| WalletError::InvalidData("invalid sapling spending key encoding"))?;
            let dfvk = extsk.to_diversifiable_full_viewing_key();
            if dfvk.decrypt_diversifier(&addr).is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn sapling_address_is_watchonly(&self, bytes: &[u8; 43]) -> Result<bool, WalletError> {
        if self.sapling_extsk_for_address(bytes)?.is_some() {
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
            None => self.generate_unique_key(compressed)?,
        };

        let added_keypool = self.ensure_keypool_minimum(DEFAULT_KEYPOOL_SIZE)?;

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
            if SecretKey::from_slice(&seed).is_err() {
                continue;
            }
            if self
                .keys
                .iter()
                .any(|key| key.secret.as_slice() == seed.as_slice())
            {
                continue;
            }
            if self
                .keypool
                .iter()
                .any(|entry| entry.key.secret.as_slice() == seed.as_slice())
            {
                continue;
            }
            return Ok(WalletKey {
                secret: seed,
                compressed,
            });
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
                    key: WalletKey { secret, compressed },
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
                sapling_keys.push(SaplingKeyEntry {
                    extsk,
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
                let witness = read_incremental_witness(Cursor::new(witness_bytes))
                    .map_err(|_| WalletError::InvalidData("invalid sapling witness encoding"))?;
                sapling_witnesses.insert((txid, out_index), witness);
            }
        }

        if !decoder.is_empty() {
            return Err(WalletError::InvalidData("wallet file has trailing bytes"));
        }
        Ok(Self {
            path: path.to_path_buf(),
            network,
            keys,
            watch_scripts,
            tx_history,
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

        encoder.write_varint(self.tx_history.len() as u64);
        for txid in &self.tx_history {
            encoder.write_bytes(txid);
        }

        encoder.write_varint(self.keypool.len() as u64);
        for entry in &self.keypool {
            encoder.write_bytes(&entry.key.secret);
            encoder.write_u8(if entry.key.compressed { 1 } else { 0 });
            encoder.write_u64_le(entry.created_at);
        }

        encoder.write_varint(self.sapling_keys.len() as u64);
        for entry in &self.sapling_keys {
            encoder.write_bytes(&entry.extsk);
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
    fn sapling_scan_keys(&self) -> Result<Vec<SaplingScanKey>, WalletError> {
        let mut keys =
            Vec::with_capacity(self.sapling_keys.len() + self.sapling_viewing_keys.len());

        for entry in &self.sapling_keys {
            let extsk = ExtendedSpendingKey::from_bytes(&entry.extsk)
                .map_err(|_| WalletError::InvalidData("invalid sapling spending key encoding"))?;
            let dfvk = extsk.to_diversifiable_full_viewing_key();
            let fvk = dfvk.fvk().clone();
            let ivk = fvk.vk.ivk();
            keys.push(SaplingScanKey {
                ivk: PreparedIncomingViewingKey::new(&ivk),
                nk: fvk.vk.nk,
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
        let key_bytes = wallet.sapling_keys[0].extsk;
        drop(wallet);

        let mut wallet =
            Wallet::load_or_create(&data_dir, Network::Regtest).expect("wallet reload");
        assert_eq!(wallet.sapling_key_count(), 1);
        assert_eq!(wallet.sapling_viewing_key_count(), 0);
        assert_eq!(wallet.sapling_keys[0].extsk, key_bytes);
        let addr2 = wallet
            .generate_new_sapling_address_bytes()
            .expect("generate sapling address");
        assert_ne!(addr1, addr2);
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
