use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fluxd_consensus::constants::{
    max_reorg_depth, COINBASE_MATURITY, FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V1,
    FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V2, FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V3,
    FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V4, FLUXNODE_DOS_REMOVE_AMOUNT,
    FLUXNODE_DOS_REMOVE_AMOUNT_V2, FLUXNODE_MIN_CONFIRMATION_DETERMINISTIC, MAX_SCRIPT_SIZE,
    MIN_BLOCK_VERSION, MIN_PON_BLOCK_VERSION,
};
use fluxd_consensus::money::MAX_MONEY;
use fluxd_consensus::upgrades::{current_epoch_branch_id, network_upgrade_active, UpgradeIndex};
use fluxd_consensus::{
    block_subsidy, exchange_fund_amount, foundation_fund_amount, is_swap_pool_interval,
    min_dev_fund_amount, swap_pool_amount, ChainParams, ConsensusParams, Hash256,
};
use fluxd_fluxnode::cache::{apply_fluxnode_tx, lookup_operator_pubkey};
use fluxd_fluxnode::storage::{FluxnodeRecord, KeyId};
use fluxd_primitives::address_to_script_pubkey;
use fluxd_primitives::block::Block;
use fluxd_primitives::encoding::{DecodeError, Decoder, Encoder};
use fluxd_primitives::hash::hash160;
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::transaction::{
    FluxnodeConfirmTx, FluxnodeStartVariantV6, FluxnodeTx, FluxnodeTxV5, FluxnodeTxV6, Transaction,
    TransactionEncodeError,
};
use fluxd_storage::{Column, KeyValueStore, StoreError, WriteBatch, WriteOp};
use rayon::prelude::*;

use crate::address_index::AddressIndex;
use crate::anchors::{AnchorSet, NullifierSet};
use crate::blockindex::{BlockIndexEntry, STATUS_HAVE_DATA, STATUS_HAVE_UNDO};
use crate::filemeta::{
    block_file_info_key, parse_block_file_info_key, parse_undo_file_info_key, undo_file_info_key,
    FlatFileInfo, META_BLOCK_FILES_LAST_FILE_KEY, META_BLOCK_FILES_LAST_LEN_KEY,
    META_UNDO_FILES_LAST_FILE_KEY, META_UNDO_FILES_LAST_LEN_KEY,
};
use crate::flatfiles::{FileLocation, FlatFileError, FlatFileStore};
use crate::index::{status_with_block, status_with_header, ChainIndex, ChainTip, HeaderEntry};
use crate::metrics::ConnectMetrics;
use crate::shielded::{
    empty_sapling_tree, empty_sprout_tree, sapling_empty_root_hash, sapling_node_from_hash,
    sapling_root_hash, sapling_tree_from_bytes, sapling_tree_to_bytes, sprout_empty_root_hash,
    sprout_root_hash, sprout_tree_from_bytes, sprout_tree_to_bytes, SaplingTree, SproutTree,
};
use crate::txindex::{TxIndex, TxLocation};
use crate::undo::{BlockUndo, FluxnodeUndo, SpentOutput};
use crate::utxo::{outpoint_key_bytes, OutPointKey, UtxoEntry, UtxoSet};
use crate::validation::{validate_block, ValidationError, ValidationFlags};
use fluxd_pon::validation as pon_validation;
use fluxd_pow::difficulty::{block_proof, HeaderInfo};
use fluxd_pow::validation as pow_validation;
use fluxd_script::interpreter::{verify_script, BLOCK_SCRIPT_VERIFY_FLAGS};
use fluxd_script::message::verify_signed_message;

struct ScriptCheck {
    tx_index: usize,
    input_index: usize,
    script_sig: Vec<u8>,
    script_pubkey: Vec<u8>,
    value: i64,
}

#[derive(Debug)]
pub enum ChainStateError {
    Validation(ValidationError),
    Store(StoreError),
    FlatFile(FlatFileError),
    MissingInput,
    MissingHeader,
    ValueOutOfRange,
    CorruptIndex(&'static str),
    InvalidHeader(&'static str),
}

impl std::fmt::Display for ChainStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChainStateError::Validation(err) => write!(f, "{err}"),
            ChainStateError::Store(err) => write!(f, "{err}"),
            ChainStateError::FlatFile(err) => write!(f, "{err}"),
            ChainStateError::MissingInput => write!(f, "missing input"),
            ChainStateError::MissingHeader => write!(f, "missing header"),
            ChainStateError::ValueOutOfRange => write!(f, "value out of range"),
            ChainStateError::CorruptIndex(message) => write!(f, "{message}"),
            ChainStateError::InvalidHeader(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ChainStateError {}

impl From<ValidationError> for ChainStateError {
    fn from(err: ValidationError) -> Self {
        ChainStateError::Validation(err)
    }
}

impl From<StoreError> for ChainStateError {
    fn from(err: StoreError) -> Self {
        ChainStateError::Store(err)
    }
}

impl From<FlatFileError> for ChainStateError {
    fn from(err: FlatFileError) -> Self {
        ChainStateError::FlatFile(err)
    }
}

impl From<TransactionEncodeError> for ChainStateError {
    fn from(err: TransactionEncodeError) -> Self {
        ChainStateError::Validation(ValidationError::from(err))
    }
}

impl From<pow_validation::PowError> for ChainStateError {
    fn from(err: pow_validation::PowError) -> Self {
        ChainStateError::Validation(ValidationError::from(err))
    }
}

impl From<pon_validation::PonError> for ChainStateError {
    fn from(err: pon_validation::PonError) -> Self {
        ChainStateError::Validation(ValidationError::from(err))
    }
}

const HEADER_CACHE_CAPACITY: usize = 200_000;
const UTXO_CACHE_CAPACITY: usize = 200_000;
const MTP_WINDOW_SIZE: usize = 11;

struct HeaderCache {
    entries: HashMap<Hash256, HeaderEntry>,
    order: VecDeque<Hash256>,
    capacity: usize,
}

impl HeaderCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    fn get(&self, hash: &Hash256) -> Option<HeaderEntry> {
        self.entries.get(hash).cloned()
    }

    fn insert(&mut self, hash: Hash256, entry: HeaderEntry) {
        if self.entries.insert(hash, entry).is_some() {
            return;
        }
        self.order.push_back(hash);
        if self.entries.len() > self.capacity {
            while let Some(evicted) = self.order.pop_front() {
                if self.entries.remove(&evicted).is_some() {
                    break;
                }
            }
        }
    }
}

struct UtxoCacheEntry {
    bytes: Vec<u8>,
    stamp: u64,
}

struct UtxoCache {
    entries: HashMap<OutPointKey, UtxoCacheEntry>,
    order: VecDeque<(OutPointKey, u64)>,
    capacity: usize,
    clock: u64,
}

impl UtxoCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity,
            clock: 0,
        }
    }

    fn get(&mut self, key: &OutPointKey) -> Option<&[u8]> {
        if self.capacity == 0 {
            return None;
        }
        let stamp = self.bump_stamp();
        let entry = self.entries.get_mut(key)?;
        entry.stamp = stamp;
        self.order.push_back((*key, stamp));
        Some(entry.bytes.as_slice())
    }

    fn insert(&mut self, key: OutPointKey, bytes: Vec<u8>) {
        if self.capacity == 0 {
            return;
        }
        let stamp = self.bump_stamp();
        self.entries.insert(key, UtxoCacheEntry { bytes, stamp });
        self.order.push_back((key, stamp));
        self.evict();
    }

    fn remove(&mut self, key: &OutPointKey) {
        self.entries.remove(key);
    }

    fn bump_stamp(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    fn evict(&mut self) {
        while self.entries.len() > self.capacity {
            let Some((key, stamp)) = self.order.pop_front() else {
                break;
            };
            let Some(entry) = self.entries.get(&key) else {
                continue;
            };
            if entry.stamp != stamp {
                continue;
            }
            self.entries.remove(&key);
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum FlatFileKind {
    Blocks,
    Undo,
}

#[derive(Clone, Copy, Debug)]
struct TrackedFlatFile {
    file_id: u32,
    info: FlatFileInfo,
}

#[derive(Default)]
struct FlatFileMetaCache {
    blocks: Option<TrackedFlatFile>,
    undo: Option<TrackedFlatFile>,
}

struct DifficultyWindow {
    tip_hash: Hash256,
    window: VecDeque<HeaderInfo>,
    window_len: usize,
}

struct MtpWindow {
    tip_hash: Hash256,
    window: VecDeque<HeaderInfo>,
}

#[derive(Default)]
pub struct HeaderValidationCache {
    difficulty_window: Option<DifficultyWindow>,
    mtp_window: Option<MtpWindow>,
}

impl DifficultyWindow {
    fn bootstrap<S: KeyValueStore>(
        state: &ChainState<S>,
        tip_hash: Hash256,
        params: &ConsensusParams,
        pending: Option<&HashMap<Hash256, HeaderEntry>>,
    ) -> Option<Self> {
        let lwma_window = params.zawy_lwma_averaging_window.saturating_add(1);
        let window_len = params.digishield_averaging_window.max(lwma_window) as usize;
        if window_len == 0 {
            return None;
        }
        let chain = collect_headers(state, &tip_hash, window_len, pending).ok()?;
        if chain.is_empty() {
            return None;
        }
        Some(Self {
            tip_hash,
            window: VecDeque::from(chain),
            window_len,
        })
    }

    fn expected_bits(
        &mut self,
        next_time: i64,
        params: &ConsensusParams,
    ) -> Result<u32, ChainStateError> {
        let window = self.window.make_contiguous();
        fluxd_pow::difficulty::get_next_work_required(window, Some(next_time), params)
            .map_err(|_| ChainStateError::InvalidHeader("difficulty calculation failed"))
    }

    fn advance(&mut self, hash: Hash256, height: i32, time: u32, bits: u32) {
        self.tip_hash = hash;
        self.window.push_back(HeaderInfo {
            height: height as i64,
            time: time as i64,
            bits,
        });
        while self.window.len() > self.window_len {
            self.window.pop_front();
        }
    }
}

impl MtpWindow {
    fn bootstrap<S: KeyValueStore>(
        state: &ChainState<S>,
        tip_hash: Hash256,
        pending: Option<&HashMap<Hash256, HeaderEntry>>,
    ) -> Option<Self> {
        let chain = collect_headers(state, &tip_hash, MTP_WINDOW_SIZE, pending).ok()?;
        if chain.is_empty() {
            return None;
        }
        Some(Self {
            tip_hash,
            window: VecDeque::from(chain),
        })
    }

    fn median_time_past(&self) -> i64 {
        let mut times: Vec<i64> = self.window.iter().map(|header| header.time).collect();
        times.sort_unstable();
        times[times.len() / 2]
    }

    fn advance(&mut self, hash: Hash256, height: i32, time: u32, bits: u32) {
        self.tip_hash = hash;
        self.window.push_back(HeaderInfo {
            height: height as i64,
            time: time as i64,
            bits,
        });
        while self.window.len() > MTP_WINDOW_SIZE {
            self.window.pop_front();
        }
    }
}

pub struct ChainState<S> {
    store: Arc<S>,
    utxos: UtxoSet<Arc<S>>,
    anchors_sprout: AnchorSet<Arc<S>>,
    anchors_sapling: AnchorSet<Arc<S>>,
    nullifiers_sprout: NullifierSet<Arc<S>>,
    nullifiers_sapling: NullifierSet<Arc<S>>,
    address_index: AddressIndex<Arc<S>>,
    tx_index: TxIndex<Arc<S>>,
    index: ChainIndex<S>,
    blocks: FlatFileStore,
    undo: FlatFileStore,
    header_cache: Mutex<HeaderCache>,
    utxo_cache: Mutex<UtxoCache>,
    shielded_cache: Mutex<Option<ShieldedTreesCache>>,
    file_meta: Mutex<FlatFileMetaCache>,
}

impl<S: KeyValueStore> ChainState<S> {
    pub fn new(store: Arc<S>, blocks: FlatFileStore, undo: FlatFileStore) -> Self {
        Self::new_with_utxo_cache_capacity(store, blocks, undo, UTXO_CACHE_CAPACITY)
    }

    pub fn new_with_utxo_cache_capacity(
        store: Arc<S>,
        blocks: FlatFileStore,
        undo: FlatFileStore,
        utxo_cache_capacity: usize,
    ) -> Self {
        Self {
            utxos: UtxoSet::new(Arc::clone(&store)),
            anchors_sprout: AnchorSet::new(Arc::clone(&store), Column::AnchorSprout),
            anchors_sapling: AnchorSet::new(Arc::clone(&store), Column::AnchorSapling),
            nullifiers_sprout: NullifierSet::new(Arc::clone(&store), Column::NullifierSprout),
            nullifiers_sapling: NullifierSet::new(Arc::clone(&store), Column::NullifierSapling),
            address_index: AddressIndex::new(Arc::clone(&store)),
            tx_index: TxIndex::new(Arc::clone(&store)),
            index: ChainIndex::new(Arc::clone(&store)),
            store,
            blocks,
            undo,
            header_cache: Mutex::new(HeaderCache::new(HEADER_CACHE_CAPACITY)),
            utxo_cache: Mutex::new(UtxoCache::new(utxo_cache_capacity)),
            shielded_cache: Mutex::new(None),
            file_meta: Mutex::new(FlatFileMetaCache::default()),
        }
    }

    pub fn best_header(&self) -> Result<Option<ChainTip>, ChainStateError> {
        Ok(self.index.best_header()?)
    }

    pub fn best_block(&self) -> Result<Option<ChainTip>, ChainStateError> {
        Ok(self.index.best_block()?)
    }

    pub fn header_entry(
        &self,
        hash: &fluxd_consensus::Hash256,
    ) -> Result<Option<HeaderEntry>, ChainStateError> {
        if let Ok(cache) = self.header_cache.lock() {
            if let Some(entry) = cache.get(hash) {
                return Ok(Some(entry));
            }
        }
        let entry = self.index.get_header(hash)?;
        if let Some(entry) = entry.clone() {
            if let Ok(mut cache) = self.header_cache.lock() {
                cache.insert(*hash, entry);
            }
        }
        Ok(entry)
    }

    pub fn insert_header(
        &self,
        header: &fluxd_primitives::block::BlockHeader,
        params: &ConsensusParams,
        batch: &mut WriteBatch,
    ) -> Result<HeaderEntry, ChainStateError> {
        let mut pending = HashMap::new();
        let mut best = self.index.best_header()?.map(|tip| {
            (
                tip.hash,
                primitive_types::U256::from_big_endian(&tip.chainwork),
            )
        });
        let mut difficulty_window = None;
        self.insert_header_with_pending(
            header,
            params,
            batch,
            &mut pending,
            &mut best,
            true,
            &mut difficulty_window,
        )
    }

    pub fn insert_headers_batch(
        &self,
        headers: &[fluxd_primitives::block::BlockHeader],
        params: &ConsensusParams,
        batch: &mut WriteBatch,
    ) -> Result<Vec<(Hash256, HeaderEntry)>, ChainStateError> {
        self.insert_headers_batch_with_pow(headers, params, batch, true)
    }

    pub fn insert_headers_batch_with_pow(
        &self,
        headers: &[fluxd_primitives::block::BlockHeader],
        params: &ConsensusParams,
        batch: &mut WriteBatch,
        check_pow: bool,
    ) -> Result<Vec<(Hash256, HeaderEntry)>, ChainStateError> {
        let mut pending = HashMap::new();
        let mut best = self.index.best_header()?.map(|tip| {
            (
                tip.hash,
                primitive_types::U256::from_big_endian(&tip.chainwork),
            )
        });
        let mut difficulty_window = headers
            .first()
            .and_then(|header| DifficultyWindow::bootstrap(self, header.prev_block, params, None));
        let mut results = Vec::with_capacity(headers.len());
        for header in headers {
            let entry = self.insert_header_with_pending(
                header,
                params,
                batch,
                &mut pending,
                &mut best,
                check_pow,
                &mut difficulty_window,
            )?;
            results.push((header.hash(), entry));
        }
        Ok(results)
    }

    pub fn validate_headers_batch_with_cache(
        &self,
        headers: &[fluxd_primitives::block::BlockHeader],
        params: &ConsensusParams,
        pending: &mut HashMap<Hash256, HeaderEntry>,
        check_pow: bool,
        cache: &mut HeaderValidationCache,
    ) -> Result<Vec<(Hash256, HeaderEntry)>, ChainStateError> {
        if headers.is_empty() {
            return Ok(Vec::new());
        }
        if cache.difficulty_window.is_none() {
            cache.difficulty_window = headers.first().and_then(|header| {
                DifficultyWindow::bootstrap(self, header.prev_block, params, Some(pending))
            });
        }
        if cache.mtp_window.is_none() {
            cache.mtp_window = headers
                .first()
                .and_then(|header| MtpWindow::bootstrap(self, header.prev_block, Some(pending)));
        }
        let mut results = Vec::with_capacity(headers.len());
        for header in headers {
            let entry = self.validate_header_with_pending(
                header,
                params,
                pending,
                check_pow,
                &mut cache.difficulty_window,
                &mut cache.mtp_window,
            )?;
            results.push((header.hash(), entry));
        }
        Ok(results)
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_header_with_pending(
        &self,
        header: &fluxd_primitives::block::BlockHeader,
        params: &ConsensusParams,
        batch: &mut WriteBatch,
        pending: &mut HashMap<Hash256, HeaderEntry>,
        best: &mut Option<(Hash256, primitive_types::U256)>,
        check_pow: bool,
        difficulty_window: &mut Option<DifficultyWindow>,
    ) -> Result<HeaderEntry, ChainStateError> {
        let hash = header.hash();
        if let Some(existing) = pending.get(&hash) {
            return Ok(existing.clone());
        }
        if let Some(existing) = self.index.get_header(&hash)? {
            batch.put(
                Column::BlockHeader,
                hash.to_vec(),
                header.consensus_encode(),
            );
            if header.is_pon() {
                let prev_hash = header.prev_block;
                let is_genesis = prev_hash == [0u8; 32] && hash == params.hash_genesis_block;
                if !is_genesis {
                    if let Some(prev_entry) = pending
                        .get(&prev_hash)
                        .cloned()
                        .or_else(|| self.index.get_header(&prev_hash).ok().flatten())
                    {
                        let prev_chainwork = prev_entry.chainwork_value();
                        let work = primitive_types::U256::from(1u64 << 40);
                        let expected_work = prev_chainwork + work;
                        if existing.chainwork_value() != expected_work {
                            let mut updated = existing.clone();
                            updated.chainwork = expected_work.to_big_endian();
                            self.index.put_header(batch, &hash, &updated);
                            pending.insert(hash, updated.clone());
                            if let Ok(mut cache) = self.header_cache.lock() {
                                cache.insert(hash, updated.clone());
                            }
                            let should_update_best = match best {
                                Some((_, best_work)) => expected_work > *best_work,
                                None => true,
                            };
                            if should_update_best {
                                *best = Some((hash, expected_work));
                                self.index.set_best_header(batch, &hash);
                            }
                            return Ok(updated);
                        }
                    }
                }
            }
            let existing_work = existing.chainwork_value();
            let should_update_best = match best {
                Some((_, best_work)) => existing_work > *best_work,
                None => true,
            };
            if should_update_best {
                *best = Some((hash, existing_work));
                self.index.set_best_header(batch, &hash);
            }
            return Ok(existing);
        }

        let prev_hash = header.prev_block;
        let is_genesis = prev_hash == [0u8; 32] && hash == params.hash_genesis_block;
        let prev_entry = if is_genesis {
            None
        } else {
            Some(match pending.get(&prev_hash) {
                Some(entry) => entry.clone(),
                None => self
                    .index
                    .get_header(&prev_hash)?
                    .ok_or(ChainStateError::MissingHeader)?,
            })
        };
        let (height, prev_chainwork) = match prev_entry.as_ref() {
            Some(entry) => (entry.height + 1, entry.chainwork_value()),
            None => (0, primitive_types::U256::zero()),
        };

        if let Some(checkpoint) = params
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.height == height)
        {
            if checkpoint.hash != hash {
                return Err(ChainStateError::InvalidHeader("checkpoint mismatch"));
            }
        }

        if let Some(best_block) = self.index.best_block()? {
            let reorg_depth = best_block.height as i64 - (height as i64 - 1);
            if reorg_depth >= max_reorg_depth(best_block.height as i64) {
                return Err(ChainStateError::InvalidHeader(
                    "forked chain older than max reorganization depth",
                ));
            }

            if let Some(checkpoint) = last_checkpoint_on_chain(self, params, best_block.height) {
                if height < checkpoint.height {
                    return Err(ChainStateError::InvalidHeader(
                        "forked chain older than last checkpoint",
                    ));
                }
            }
        }

        let pon_active = network_upgrade_active(height, &params.upgrades, UpgradeIndex::Pon);
        if header.version < MIN_BLOCK_VERSION {
            return Err(ChainStateError::InvalidHeader("block version too low"));
        }
        if pon_active && header.version < MIN_PON_BLOCK_VERSION {
            return Err(ChainStateError::InvalidHeader("pon block version too low"));
        }
        for upgrade in &params.upgrades {
            if height == upgrade.activation_height {
                if let Some(expected_hash) = upgrade.hash_activation_block {
                    if hash != expected_hash {
                        return Err(ChainStateError::InvalidHeader(
                            "activation block hash mismatch",
                        ));
                    }
                }
            }
        }
        if pon_active && !header.is_pon() {
            return Err(ChainStateError::InvalidHeader(
                "pon upgrade active but header is not pon",
            ));
        }
        if !pon_active && header.is_pon() {
            return Err(ChainStateError::InvalidHeader(
                "pon upgrade inactive but header is pon",
            ));
        }
        if let Some(prev_entry) = prev_entry.as_ref() {
            let now = current_time_secs();
            let lwma_active = network_upgrade_active(height, &params.upgrades, UpgradeIndex::Lwma);
            let max_future = if !lwma_active {
                2 * 60 * 60
            } else if header.is_pon() {
                300
            } else {
                360
            };
            if header.time as i64 > now + max_future {
                return Err(ChainStateError::InvalidHeader(
                    "block timestamp too far in the future",
                ));
            }
            if header.is_pon() {
                if header.time as i64 <= prev_entry.time as i64 {
                    return Err(ChainStateError::InvalidHeader(
                        "pon block timestamp too early",
                    ));
                }
            } else {
                let mtp_headers = collect_headers(self, &prev_hash, 11, Some(pending))?;
                let mtp = median_time_past(&mtp_headers);
                if header.time as i64 <= mtp {
                    return Err(ChainStateError::InvalidHeader("block timestamp too early"));
                }
            }
        }

        if !is_genesis {
            let expected_bits = if !pon_active {
                match difficulty_window.as_mut() {
                    Some(window) if window.tip_hash == prev_hash => {
                        window.expected_bits(header.time as i64, params)?
                    }
                    _ => self.expected_bits(
                        &prev_hash,
                        height,
                        header.time as i64,
                        params,
                        Some(pending),
                    )?,
                }
            } else {
                self.expected_bits(
                    &prev_hash,
                    height,
                    header.time as i64,
                    params,
                    Some(pending),
                )?
            };
            if header.bits != expected_bits {
                eprintln!(
                    "unexpected difficulty bits at height {}: expected {:#x}, got {:#x}",
                    height, expected_bits, header.bits
                );
                return Err(ChainStateError::InvalidHeader("unexpected difficulty bits"));
            }
        }

        if header.is_pon() {
            pon_validation::validate_pon_header(header, height, params)?;
        } else if check_pow {
            pow_validation::validate_pow_header(header, height, params)?;
        }

        let chainwork = if header.is_pon() {
            let work = primitive_types::U256::from(1u64 << 40);
            let work = prev_chainwork + work;
            work.to_big_endian()
        } else {
            let work = block_proof(header.bits)
                .map_err(|_| ChainStateError::InvalidHeader("invalid difficulty target"))?;
            let work = prev_chainwork + work;
            work.to_big_endian()
        };

        let entry = HeaderEntry {
            prev_hash,
            height,
            time: header.time,
            bits: header.bits,
            chainwork,
            status: status_with_header(0),
        };

        self.index.put_header(batch, &hash, &entry);
        batch.put(
            Column::BlockHeader,
            hash.to_vec(),
            header.consensus_encode(),
        );
        pending.insert(hash, entry.clone());
        if let Ok(mut cache) = self.header_cache.lock() {
            cache.insert(hash, entry.clone());
        }

        let new_work = primitive_types::U256::from_big_endian(&entry.chainwork);
        let should_update_best = match best {
            Some((_, best_work)) => new_work > *best_work,
            None => true,
        };
        if should_update_best {
            *best = Some((hash, new_work));
            self.index.set_best_header(batch, &hash);
        }

        if !pon_active {
            if let Some(window) = difficulty_window.as_mut() {
                if window.tip_hash == prev_hash {
                    window.advance(hash, height, header.time, header.bits);
                } else {
                    *difficulty_window = None;
                }
            }
        } else {
            *difficulty_window = None;
        }

        Ok(entry)
    }

    fn validate_header_with_pending(
        &self,
        header: &fluxd_primitives::block::BlockHeader,
        params: &ConsensusParams,
        pending: &mut HashMap<Hash256, HeaderEntry>,
        check_pow: bool,
        difficulty_window: &mut Option<DifficultyWindow>,
        mtp_window: &mut Option<MtpWindow>,
    ) -> Result<HeaderEntry, ChainStateError> {
        let hash = header.hash();
        if let Some(existing) = pending.get(&hash) {
            return Ok(existing.clone());
        }
        if let Some(existing) = self.index.get_header(&hash)? {
            if header.is_pon() {
                let prev_hash = header.prev_block;
                let is_genesis = prev_hash == [0u8; 32] && hash == params.hash_genesis_block;
                if !is_genesis {
                    if let Some(prev_entry) = pending
                        .get(&prev_hash)
                        .cloned()
                        .or_else(|| self.index.get_header(&prev_hash).ok().flatten())
                    {
                        let prev_chainwork = prev_entry.chainwork_value();
                        let work = primitive_types::U256::from(1u64 << 40);
                        let expected_work = prev_chainwork + work;
                        if existing.chainwork_value() != expected_work {
                            let mut updated = existing.clone();
                            updated.chainwork = expected_work.to_big_endian();
                            return Ok(updated);
                        }
                    }
                }
            }
            return Ok(existing);
        }

        let prev_hash = header.prev_block;
        let is_genesis = prev_hash == [0u8; 32] && hash == params.hash_genesis_block;
        let prev_entry = if is_genesis {
            None
        } else {
            Some(match pending.get(&prev_hash) {
                Some(entry) => entry.clone(),
                None => self
                    .index
                    .get_header(&prev_hash)?
                    .ok_or(ChainStateError::MissingHeader)?,
            })
        };
        let (height, prev_chainwork) = match prev_entry.as_ref() {
            Some(entry) => (entry.height + 1, entry.chainwork_value()),
            None => (0, primitive_types::U256::zero()),
        };

        if let Some(checkpoint) = params
            .checkpoints
            .iter()
            .find(|checkpoint| checkpoint.height == height)
        {
            if checkpoint.hash != hash {
                return Err(ChainStateError::InvalidHeader("checkpoint mismatch"));
            }
        }

        if let Some(best_block) = self.index.best_block()? {
            let reorg_depth = best_block.height as i64 - (height as i64 - 1);
            if reorg_depth >= max_reorg_depth(best_block.height as i64) {
                return Err(ChainStateError::InvalidHeader("reorg too deep"));
            }
        }

        let pon_active = network_upgrade_active(height, &params.upgrades, UpgradeIndex::Pon);
        if header.is_pon() && !pon_active {
            return Err(ChainStateError::InvalidHeader(
                "pon upgrade inactive but header is pon",
            ));
        }
        if let Some(prev_entry) = prev_entry.as_ref() {
            let now = current_time_secs();
            let lwma_active = network_upgrade_active(height, &params.upgrades, UpgradeIndex::Lwma);
            let max_future = if !lwma_active {
                2 * 60 * 60
            } else if header.is_pon() {
                300
            } else {
                360
            };
            if header.time as i64 > now + max_future {
                return Err(ChainStateError::InvalidHeader(
                    "block timestamp too far in the future",
                ));
            }
            if header.is_pon() {
                if header.time as i64 <= prev_entry.time as i64 {
                    return Err(ChainStateError::InvalidHeader(
                        "pon block timestamp too early",
                    ));
                }
            } else {
                let mtp = match mtp_window.as_mut() {
                    Some(window) if window.tip_hash == prev_hash => window.median_time_past(),
                    _ => {
                        *mtp_window = None;
                        let mtp_headers =
                            collect_headers(self, &prev_hash, MTP_WINDOW_SIZE, Some(pending))?;
                        median_time_past(&mtp_headers)
                    }
                };
                if header.time as i64 <= mtp {
                    return Err(ChainStateError::InvalidHeader("block timestamp too early"));
                }
            }
        }

        if !is_genesis {
            let expected_bits = if !pon_active {
                match difficulty_window.as_mut() {
                    Some(window) if window.tip_hash == prev_hash => {
                        window.expected_bits(header.time as i64, params)?
                    }
                    _ => self.expected_bits(
                        &prev_hash,
                        height,
                        header.time as i64,
                        params,
                        Some(pending),
                    )?,
                }
            } else {
                self.expected_bits(
                    &prev_hash,
                    height,
                    header.time as i64,
                    params,
                    Some(pending),
                )?
            };
            if header.bits != expected_bits {
                eprintln!(
                    "unexpected difficulty bits at height {}: expected {:#x}, got {:#x}",
                    height, expected_bits, header.bits
                );
                return Err(ChainStateError::InvalidHeader("unexpected difficulty bits"));
            }
        }

        if header.is_pon() {
            pon_validation::validate_pon_header(header, height, params)?;
        } else if check_pow {
            pow_validation::validate_pow_header(header, height, params)?;
        }

        let chainwork = if header.is_pon() {
            let work = primitive_types::U256::from(1u64 << 40);
            let work = prev_chainwork + work;
            work.to_big_endian()
        } else {
            let work = block_proof(header.bits)
                .map_err(|_| ChainStateError::InvalidHeader("invalid difficulty target"))?;
            let work = prev_chainwork + work;
            work.to_big_endian()
        };

        let entry = HeaderEntry {
            prev_hash,
            height,
            time: header.time,
            bits: header.bits,
            chainwork,
            status: status_with_header(0),
        };

        pending.insert(hash, entry.clone());
        if !pon_active {
            if let Some(window) = difficulty_window.as_mut() {
                if window.tip_hash == prev_hash {
                    window.advance(hash, height, header.time, header.bits);
                } else {
                    *difficulty_window = None;
                }
            }
            if let Some(window) = mtp_window.as_mut() {
                if window.tip_hash == prev_hash {
                    window.advance(hash, height, header.time, header.bits);
                } else {
                    *mtp_window = None;
                }
            }
        } else {
            *difficulty_window = None;
            *mtp_window = None;
        }

        Ok(entry)
    }

    fn expected_bits(
        &self,
        prev_hash: &fluxd_consensus::Hash256,
        height: i32,
        next_time: i64,
        params: &ConsensusParams,
        pending: Option<&HashMap<Hash256, HeaderEntry>>,
    ) -> Result<u32, ChainStateError> {
        if height == 0 {
            return Ok(block_bits_from_params(params));
        }

        if network_upgrade_active(height, &params.upgrades, UpgradeIndex::Pon) {
            return expected_pon_bits(self, prev_hash, height, params, pending);
        }

        // LWMA/LWMA3 require the previous block (height - N) for the first solvetime.
        let lwma_window = params.zawy_lwma_averaging_window.saturating_add(1);
        let window = params.digishield_averaging_window.max(lwma_window) as usize;
        let chain = collect_headers(self, prev_hash, window, pending)?;
        fluxd_pow::difficulty::get_next_work_required(&chain, Some(next_time), params)
            .map_err(|_| ChainStateError::InvalidHeader("difficulty calculation failed"))
    }

    fn validate_fluxnode_tx(
        &self,
        tx: &Transaction,
        txid: &Hash256,
        height: i32,
        params: &ChainParams,
        created_utxos: &HashMap<OutPointKey, UtxoEntry>,
        operator_pubkeys: &HashMap<OutPoint, Vec<u8>>,
    ) -> Result<(), ChainStateError> {
        let Some(fluxnode) = tx.fluxnode.as_ref() else {
            return Ok(());
        };

        let message_hex = hash256_to_hex(txid);
        let message = message_hex.as_bytes();

        match fluxnode {
            FluxnodeTx::V5(FluxnodeTxV5::Start(start)) => {
                if start.collateral == OutPoint::null() {
                    return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                        "fluxnode start has null collateral",
                    )));
                }
                if operator_pubkeys.contains_key(&start.collateral)
                    || !self.fluxnode_start_allowed(&start.collateral, height, params)?
                {
                    if let Ok(Some(record)) = self.fluxnode_record(&start.collateral) {
                        eprintln!(
                            "fluxnode start rejected at height {}: tx {} collateral {} (start_height {} last_confirmed {})",
                            height,
                            hash256_to_hex(txid),
                            outpoint_to_string(&start.collateral),
                            record.start_height,
                            record.last_confirmed_height
                        );
                    }
                    return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                        "fluxnode start collateral already registered",
                    )));
                }

                let collateral =
                    self.lookup_fluxnode_collateral(&start.collateral, created_utxos)?;
                ensure_fluxnode_collateral_mature(collateral.height, height)?;
                validate_fluxnode_collateral_script(
                    &collateral.script_pubkey,
                    height,
                    &start.collateral_pubkey,
                    start.sig_time,
                    params,
                    None,
                )?;

                verify_signed_message(&start.collateral_pubkey, &start.sig, message).map_err(
                    |_| {
                        ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode start signature invalid",
                        ))
                    },
                )?;
            }
            FluxnodeTx::V6(FluxnodeTxV6::Start(start)) => match &start.variant {
                FluxnodeStartVariantV6::Normal {
                    collateral,
                    collateral_pubkey,
                    sig_time,
                    sig,
                    ..
                } => {
                    if *collateral == OutPoint::null() {
                        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode start has null collateral",
                        )));
                    }
                    if operator_pubkeys.contains_key(collateral)
                        || !self.fluxnode_start_allowed(collateral, height, params)?
                    {
                        if let Ok(Some(record)) = self.fluxnode_record(collateral) {
                            eprintln!(
                                "fluxnode start rejected at height {}: tx {} collateral {} (start_height {} last_confirmed {})",
                                height,
                                hash256_to_hex(txid),
                                outpoint_to_string(collateral),
                                record.start_height,
                                record.last_confirmed_height
                            );
                        }
                        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode start collateral already registered",
                        )));
                    }

                    let collateral_entry =
                        self.lookup_fluxnode_collateral(collateral, created_utxos)?;
                    ensure_fluxnode_collateral_mature(collateral_entry.height, height)?;
                    validate_fluxnode_collateral_script(
                        &collateral_entry.script_pubkey,
                        height,
                        collateral_pubkey,
                        *sig_time,
                        params,
                        None,
                    )?;

                    if script_p2pkh_hash(&collateral_entry.script_pubkey).is_none() {
                        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode normal collateral not p2pkh",
                        )));
                    }

                    verify_signed_message(collateral_pubkey, sig, message).map_err(|_| {
                        ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode start signature invalid",
                        ))
                    })?;
                }
                FluxnodeStartVariantV6::P2sh {
                    collateral,
                    redeem_script,
                    sig_time,
                    sig,
                    ..
                } => {
                    if *collateral == OutPoint::null() {
                        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode start has null collateral",
                        )));
                    }
                    if redeem_script.len() > MAX_SCRIPT_SIZE {
                        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode redeem script too large",
                        )));
                    }
                    if operator_pubkeys.contains_key(collateral)
                        || !self.fluxnode_start_allowed(collateral, height, params)?
                    {
                        if let Ok(Some(record)) = self.fluxnode_record(collateral) {
                            eprintln!(
                                "fluxnode start rejected at height {}: tx {} collateral {} (start_height {} last_confirmed {})",
                                height,
                                hash256_to_hex(txid),
                                outpoint_to_string(collateral),
                                record.start_height,
                                record.last_confirmed_height
                            );
                        }
                        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode start collateral already registered",
                        )));
                    }

                    let pubkeys = parse_multisig_redeem_script(redeem_script).ok_or_else(|| {
                        ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode redeem script not multisig",
                        ))
                    })?;

                    let collateral_entry =
                        self.lookup_fluxnode_collateral(collateral, created_utxos)?;
                    ensure_fluxnode_collateral_mature(collateral_entry.height, height)?;
                    validate_fluxnode_collateral_script(
                        &collateral_entry.script_pubkey,
                        height,
                        &[],
                        *sig_time,
                        params,
                        Some(redeem_script),
                    )?;

                    if script_p2sh_hash(&collateral_entry.script_pubkey).is_none() {
                        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode p2sh collateral not p2sh",
                        )));
                    }

                    let mut ok = false;
                    for pubkey in pubkeys {
                        if verify_signed_message(&pubkey, sig, message).is_ok() {
                            ok = true;
                            break;
                        }
                    }
                    if !ok {
                        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode start signature invalid",
                        )));
                    }
                }
            },
            FluxnodeTx::V5(FluxnodeTxV5::Confirm(confirm))
            | FluxnodeTx::V6(FluxnodeTxV6::Confirm(confirm)) => {
                if confirm.collateral == OutPoint::null() {
                    return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                        "fluxnode confirm has null collateral",
                    )));
                }
                if confirm.sig.is_empty() || confirm.benchmark_sig.is_empty() {
                    return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                        "fluxnode confirm missing signatures",
                    )));
                }
                if !(1..=3).contains(&confirm.benchmark_tier) {
                    return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                        "fluxnode confirm has invalid benchmarking tier",
                    )));
                }
                if confirm.update_type > 1 {
                    return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                        "fluxnode confirm has invalid update type",
                    )));
                }

                let max_ip_len = if confirm.benchmark_sig_time >= 1_647_262_800 {
                    60
                } else {
                    40
                };
                if confirm.ip.len() > max_ip_len {
                    return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                        "fluxnode confirm ip too large",
                    )));
                }

                let operator_pubkey = if let Some(pubkey) =
                    operator_pubkeys.get(&confirm.collateral)
                {
                    pubkey.clone()
                } else {
                    lookup_operator_pubkey(&self.store, &confirm.collateral)?.ok_or_else(|| {
                        ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode confirm missing start record",
                        ))
                    })?
                };

                let msg = fluxnode_confirm_message(confirm);
                verify_signed_message(&operator_pubkey, &confirm.sig, &msg).map_err(|_| {
                    ChainStateError::Validation(ValidationError::Fluxnode(
                        "fluxnode confirm signature invalid",
                    ))
                })?;

                let benchmark_key = select_timed_pubkey(
                    params.fluxnode.benchmarking_public_keys,
                    confirm.benchmark_sig_time,
                )
                .ok_or_else(|| {
                    ChainStateError::Validation(ValidationError::Fluxnode(
                        "fluxnode benchmark key missing",
                    ))
                })?;
                let benchmark_pubkey = hex_to_bytes(benchmark_key.key).ok_or_else(|| {
                    ChainStateError::Validation(ValidationError::Fluxnode(
                        "invalid benchmark pubkey",
                    ))
                })?;

                let benchmark_msg = fluxnode_benchmark_message(confirm);
                verify_signed_message(&benchmark_pubkey, &confirm.benchmark_sig, &benchmark_msg)
                    .map_err(|_| {
                        ChainStateError::Validation(ValidationError::Fluxnode(
                            "fluxnode benchmark signature invalid",
                        ))
                    })?;
            }
        }

        Ok(())
    }

    fn lookup_fluxnode_collateral(
        &self,
        outpoint: &OutPoint,
        created_utxos: &HashMap<OutPointKey, UtxoEntry>,
    ) -> Result<UtxoEntry, ChainStateError> {
        let key = outpoint_key_bytes(outpoint);
        if let Some(entry) = created_utxos.get(&key) {
            return Ok(entry.clone());
        }
        self.utxo_entry_cached(key)?.ok_or_else(|| {
            ChainStateError::Validation(ValidationError::Fluxnode("fluxnode collateral not found"))
        })
    }

    fn fluxnode_record(
        &self,
        outpoint: &OutPoint,
    ) -> Result<Option<FluxnodeRecord>, ChainStateError> {
        let key = outpoint_key_bytes(outpoint);
        let Some(bytes) = self.store.get(Column::Fluxnode, key.as_bytes())? else {
            return Ok(None);
        };
        FluxnodeRecord::decode(&bytes)
            .map(Some)
            .map_err(|_| ChainStateError::CorruptIndex("invalid fluxnode record"))
    }

    fn fluxnode_start_allowed(
        &self,
        outpoint: &OutPoint,
        height: i32,
        params: &ChainParams,
    ) -> Result<bool, ChainStateError> {
        let Ok(height_u32) = u32::try_from(height) else {
            return Ok(true);
        };
        let Some(record) = self.fluxnode_record(outpoint)? else {
            return Ok(true);
        };

        if record.last_confirmed_height == record.start_height {
            if record.start_height == height_u32 {
                return Ok(true);
            }
            let pon_active =
                network_upgrade_active(height, &params.consensus.upgrades, UpgradeIndex::Pon);
            let dos_remove = if pon_active {
                FLUXNODE_DOS_REMOVE_AMOUNT_V2
            } else {
                FLUXNODE_DOS_REMOVE_AMOUNT
            };
            let dos_remove = u32::try_from(dos_remove).unwrap_or_default();
            let removal_height = record.start_height.saturating_add(dos_remove);
            return Ok(height_u32 > removal_height);
        }

        let expiration = fluxnode_confirm_expiration_count(height, &params.consensus);
        let expire_height = record
            .last_confirmed_height
            .saturating_add(expiration)
            .saturating_add(1);
        Ok(height_u32 > expire_height)
    }

    pub fn connect_block(
        &self,
        block: &Block,
        height: i32,
        params: &ChainParams,
        flags: &ValidationFlags,
        prevalidated: bool,
        connect_metrics: Option<&ConnectMetrics>,
    ) -> Result<WriteBatch, ChainStateError> {
        let consensus = &params.consensus;
        let mut batch = WriteBatch::new();
        let header_entry = self.insert_header(&block.header, consensus, &mut batch)?;
        if header_entry.height != height {
            return Err(ChainStateError::InvalidHeader(
                "block height does not match header index",
            ));
        }

        if let Some(best) = self.index.best_block()? {
            if block.header.prev_block != best.hash {
                return Err(ChainStateError::InvalidHeader(
                    "block does not extend best block tip",
                ));
            }
        } else if height != 0 {
            return Err(ChainStateError::InvalidHeader(
                "missing best block for non-genesis height",
            ));
        }

        if !prevalidated {
            validate_block(block, height, consensus, flags)?;
        }
        if block.header.is_pon()
            && network_upgrade_active(height, &consensus.upgrades, UpgradeIndex::Pon)
        {
            let operator_pubkey =
                lookup_operator_pubkey(&self.store, &block.header.nodes_collateral)?.ok_or(
                    ChainStateError::InvalidHeader("missing fluxnode entry for pon signature"),
                )?;
            pon_validation::validate_pon_signature(&block.header, consensus, &operator_pubkey)?;
        }
        check_coinbase_funding(&block.transactions[0], height, params)?;

        let mut utxo_stats = self.utxo_stats_or_compute()?;
        let mut value_pools = self.value_pools_or_compute()?;
        let mut utxos_created = 0u64;
        let mut utxos_spent = 0u64;
        let mut value_created = 0i64;
        let mut value_spent = 0i64;
        let mut sprout_pool_delta = 0i64;
        let mut sapling_pool_delta = 0i64;

        let mut utxo_time = Duration::ZERO;
        let mut index_time = Duration::ZERO;
        let mut anchor_time = Duration::ZERO;
        let mut flatfile_time = Duration::ZERO;
        let (prev_sprout_root, prev_sprout_tree, prev_sapling_root, prev_sapling_tree) =
            self.shielded_cache_snapshot()?;
        let mut sprout_tree: Option<SproutTree> = None;
        let mut sapling_tree: Option<SaplingTree> = None;
        let mut undo = BlockUndo {
            prev_sprout_tree,
            prev_sapling_tree,
            spent: Vec::new(),
            fluxnode: Vec::new(),
        };
        let mut seen_sprout_nullifiers = HashSet::new();
        let mut seen_sapling_nullifiers = HashSet::new();
        let mut txids = Vec::with_capacity(block.transactions.len());
        let estimated_inputs = block
            .transactions
            .iter()
            .skip(1)
            .map(|tx| tx.vin.len())
            .sum::<usize>();
        let estimated_outputs = block
            .transactions
            .iter()
            .map(|tx| tx.vout.len())
            .sum::<usize>();
        let mut created_utxos: HashMap<OutPointKey, UtxoEntry> =
            HashMap::with_capacity(estimated_outputs);
        let mut spent_outpoints: HashSet<OutPointKey> = HashSet::with_capacity(estimated_inputs);
        let mut block_script_checks: Vec<ScriptCheck> = Vec::new();
        let branch_id = current_epoch_branch_id(height, &consensus.upgrades);
        let flux_rebrand_active =
            network_upgrade_active(height, &consensus.upgrades, UpgradeIndex::Flux);
        let mut total_fees = 0i64;
        let mut fluxnode_operator_pubkeys: HashMap<OutPoint, Vec<u8>> = HashMap::new();
        for (index, tx) in block.transactions.iter().enumerate() {
            let is_coinbase = index == 0;
            let txid = tx.txid()?;
            self.validate_fluxnode_tx(
                tx,
                &txid,
                height,
                params,
                &created_utxos,
                &fluxnode_operator_pubkeys,
            )?;
            txids.push(txid);
            let tx_value_out = tx_value_out(tx)?;
            let mut tx_value_in = tx_shielded_value_in(tx)?;
            let mut sprout_intermediates: HashMap<Hash256, SproutTree> = HashMap::new();
            sapling_pool_delta = sapling_pool_delta
                .checked_sub(tx.value_balance)
                .ok_or(ChainStateError::ValueOutOfRange)?;

            for joinsplit in &tx.join_splits {
                sprout_pool_delta = sprout_pool_delta
                    .checked_add(joinsplit.vpub_old)
                    .and_then(|value| value.checked_sub(joinsplit.vpub_new))
                    .ok_or(ChainStateError::ValueOutOfRange)?;
                for nullifier in &joinsplit.nullifiers {
                    if !seen_sprout_nullifiers.insert(*nullifier) {
                        return Err(ChainStateError::Validation(
                            ValidationError::InvalidTransaction(
                                "duplicate sprout nullifier in block",
                            ),
                        ));
                    }
                    if self.nullifiers_sprout.contains(nullifier)? {
                        return Err(ChainStateError::Validation(
                            ValidationError::InvalidTransaction("sprout nullifier already spent"),
                        ));
                    }
                }

                let mut tree = match sprout_intermediates.get(&joinsplit.anchor) {
                    Some(tree) => tree.clone(),
                    None => match self.sprout_anchor_tree(&joinsplit.anchor)? {
                        Some(tree) => tree,
                        None => {
                            return Err(ChainStateError::Validation(
                                ValidationError::InvalidTransaction("sprout anchor not found"),
                            ))
                        }
                    },
                };

                for commitment in &joinsplit.commitments {
                    tree.append(crate::shielded::SproutNode::from_hash(commitment))
                        .map_err(|_| {
                            ChainStateError::Validation(ValidationError::InvalidTransaction(
                                "sprout tree append failed",
                            ))
                        })?;
                }

                let next_root = sprout_root_hash(&tree);
                sprout_intermediates.insert(next_root, tree);
            }

            for spend in &tx.shielded_spends {
                if !seen_sapling_nullifiers.insert(spend.nullifier) {
                    return Err(ChainStateError::Validation(
                        ValidationError::InvalidTransaction("duplicate sapling nullifier in block"),
                    ));
                }
                if self.nullifiers_sapling.contains(&spend.nullifier)? {
                    return Err(ChainStateError::Validation(
                        ValidationError::InvalidTransaction("sapling nullifier already spent"),
                    ));
                }
                if !self.sapling_anchor_exists(&spend.anchor)? {
                    return Err(ChainStateError::Validation(
                        ValidationError::InvalidTransaction("sapling anchor not found"),
                    ));
                }
            }

            if !is_coinbase {
                let mut transparent_in = 0i64;
                for (input_index, input) in tx.vin.iter().enumerate() {
                    let outpoint_key = outpoint_key_bytes(&input.prevout);
                    if !spent_outpoints.insert(outpoint_key) {
                        eprintln!(
                            "missing input for tx {} input {} prevout {}:{} at height {}",
                            hash256_to_hex(&txid),
                            input_index,
                            hash256_to_hex(&input.prevout.hash),
                            input.prevout.index,
                            height
                        );
                        return Err(ChainStateError::MissingInput);
                    }

                    let entry = match created_utxos.remove(&outpoint_key) {
                        Some(entry) => entry,
                        None => {
                            let utxo_start = Instant::now();
                            let entry = self.utxo_entry_cached(outpoint_key)?;
                            utxo_time += utxo_start.elapsed();
                            match entry {
                                Some(entry) => entry,
                                None => {
                                    eprintln!(
                                        "missing input for tx {} input {} prevout {}:{} at height {}",
                                        hash256_to_hex(&txid),
                                        input_index,
                                        hash256_to_hex(&input.prevout.hash),
                                        input.prevout.index,
                                        height
                                    );
                                    return Err(ChainStateError::MissingInput);
                                }
                            }
                        }
                    };
                    utxos_spent = utxos_spent
                        .checked_add(1)
                        .ok_or(ChainStateError::ValueOutOfRange)?;
                    value_spent = value_spent
                        .checked_add(entry.value)
                        .ok_or(ChainStateError::ValueOutOfRange)?;
                    if entry.is_coinbase {
                        let spend_height = height as i64 - entry.height as i64;
                        if spend_height < COINBASE_MATURITY as i64 {
                            return Err(ChainStateError::Validation(
                                ValidationError::InvalidTransaction("premature spend of coinbase"),
                            ));
                        }
                        if consensus.coinbase_must_be_protected
                            && !flux_rebrand_active
                            && !tx.vout.is_empty()
                        {
                            return Err(ChainStateError::Validation(
                                ValidationError::InvalidTransaction(
                                    "coinbase spend has transparent outputs",
                                ),
                            ));
                        }
                    }
                    if flags.check_script {
                        block_script_checks.push(ScriptCheck {
                            tx_index: index,
                            input_index,
                            script_sig: input.script_sig.clone(),
                            script_pubkey: entry.script_pubkey.clone(),
                            value: entry.value,
                        });
                    }
                    transparent_in = transparent_in
                        .checked_add(entry.value)
                        .ok_or(ChainStateError::ValueOutOfRange)?;
                    let prevout = input.prevout.clone();
                    let utxo_start = Instant::now();
                    self.utxos.delete(&mut batch, &prevout);
                    utxo_time += utxo_start.elapsed();
                    let index_start = Instant::now();
                    self.address_index
                        .delete(&mut batch, &entry.script_pubkey, &prevout);
                    index_time += index_start.elapsed();
                    undo.spent.push(SpentOutput {
                        outpoint: prevout,
                        entry,
                    });
                }
                tx_value_in = tx_value_in
                    .checked_add(transparent_in)
                    .ok_or(ChainStateError::ValueOutOfRange)?;
                if tx_value_in < tx_value_out {
                    return Err(ChainStateError::ValueOutOfRange);
                }
                let fee = tx_value_in - tx_value_out;
                total_fees = total_fees
                    .checked_add(fee)
                    .ok_or(ChainStateError::ValueOutOfRange)?;
            }

            if let Some(entry) = fluxnode_undo_entry(&self.store, tx)? {
                undo.fluxnode.push(entry);
            }
            apply_fluxnode_tx(&self.store, &mut batch, tx, height as u32)?;
            if let Some((collateral, operator_pubkey)) = fluxnode_start_operator_pubkey(tx) {
                fluxnode_operator_pubkeys.insert(collateral, operator_pubkey);
            }

            if !(tx.join_splits.is_empty() && tx.shielded_spends.is_empty()) {
                let anchor_start = Instant::now();
                for joinsplit in &tx.join_splits {
                    for nullifier in &joinsplit.nullifiers {
                        self.nullifiers_sprout.insert(&mut batch, nullifier);
                    }
                }
                for spend in &tx.shielded_spends {
                    self.nullifiers_sapling.insert(&mut batch, &spend.nullifier);
                }
                anchor_time += anchor_start.elapsed();
            }

            if !(tx.join_splits.is_empty() && tx.shielded_outputs.is_empty()) {
                let anchor_start = Instant::now();
                if !tx.join_splits.is_empty() {
                    if sprout_tree.is_none() {
                        sprout_tree = Some(self.shielded_cache_sprout_tree()?);
                    }
                    let sprout_tree = sprout_tree
                        .as_mut()
                        .ok_or(ChainStateError::CorruptIndex("missing sprout tree cache"))?;
                    for joinsplit in &tx.join_splits {
                        for commitment in &joinsplit.commitments {
                            sprout_tree
                                .append(crate::shielded::SproutNode::from_hash(commitment))
                                .map_err(|_| {
                                    ChainStateError::Validation(
                                        ValidationError::InvalidTransaction(
                                            "sprout tree append failed",
                                        ),
                                    )
                                })?;
                        }
                    }
                }
                if !tx.shielded_outputs.is_empty() {
                    if sapling_tree.is_none() {
                        sapling_tree = Some(self.shielded_cache_sapling_tree()?);
                    }
                    let sapling_tree = sapling_tree
                        .as_mut()
                        .ok_or(ChainStateError::CorruptIndex("missing sapling tree cache"))?;
                    for output in &tx.shielded_outputs {
                        let node = sapling_node_from_hash(&output.cm).ok_or(
                            ChainStateError::Validation(ValidationError::InvalidTransaction(
                                "sapling note commitment invalid",
                            )),
                        )?;
                        sapling_tree.append(node).map_err(|_| {
                            ChainStateError::Validation(ValidationError::InvalidTransaction(
                                "sapling tree append failed",
                            ))
                        })?;
                    }
                }
                anchor_time += anchor_start.elapsed();
            }

            for (out_index, output) in tx.vout.iter().enumerate() {
                let outpoint = OutPoint {
                    hash: txid,
                    index: out_index as u32,
                };
                let entry = UtxoEntry {
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    height: height as u32,
                    is_coinbase,
                };
                utxos_created = utxos_created
                    .checked_add(1)
                    .ok_or(ChainStateError::ValueOutOfRange)?;
                value_created = value_created
                    .checked_add(output.value)
                    .ok_or(ChainStateError::ValueOutOfRange)?;
                let utxo_start = Instant::now();
                self.utxos.put(&mut batch, &outpoint, &entry);
                utxo_time += utxo_start.elapsed();
                created_utxos.insert(outpoint_key_bytes(&outpoint), entry);
                let index_start = Instant::now();
                self.address_index
                    .insert(&mut batch, &output.script_pubkey, &outpoint);
                index_time += index_start.elapsed();
            }
        }

        if flags.check_script && !block_script_checks.is_empty() {
            let script_start = Instant::now();
            let result = block_script_checks.par_iter().try_for_each(|check| {
                let tx = &block.transactions[check.tx_index];
                verify_script(
                    &check.script_sig,
                    &check.script_pubkey,
                    tx,
                    check.input_index,
                    check.value,
                    BLOCK_SCRIPT_VERIFY_FLAGS,
                    branch_id,
                )
                .map_err(|err| (check.tx_index, check.input_index, err))
            });
            if let Some(metrics) = flags.metrics.as_ref() {
                metrics.record_script(script_start.elapsed());
            }
            if let Err((tx_index, input_index, err)) = result {
                if let Ok(txid) = block.transactions[tx_index].txid() {
                    eprintln!(
                        "script validation failed for tx {} input {}: {}",
                        hash256_to_hex(&txid),
                        input_index,
                        err
                    );
                } else {
                    eprintln!(
                        "script validation failed for input {}: {}",
                        input_index, err
                    );
                }
                return Err(ChainStateError::Validation(
                    ValidationError::InvalidTransaction("script validation failed"),
                ));
            }
        }

        let anchor_finalize_start = Instant::now();
        let (sprout_root, sprout_changed) = match sprout_tree.as_ref() {
            Some(tree) => {
                let root = sprout_root_hash(tree);
                (root, root != prev_sprout_root)
            }
            None => (prev_sprout_root, false),
        };
        let (sapling_root, sapling_changed) = match sapling_tree.as_ref() {
            Some(tree) => {
                let root = sapling_root_hash(tree);
                (root, root != prev_sapling_root)
            }
            None => (prev_sapling_root, false),
        };
        if network_upgrade_active(height, &consensus.upgrades, UpgradeIndex::Acadia)
            && block.header.final_sapling_root != sapling_root
        {
            return Err(ChainStateError::Validation(ValidationError::InvalidBlock(
                "sapling root mismatch",
            )));
        }

        if sprout_changed {
            let tree = sprout_tree
                .as_ref()
                .ok_or(ChainStateError::CorruptIndex("missing sprout tree state"))?;
            let sprout_bytes = sprout_tree_to_bytes(tree)
                .map_err(|_| ChainStateError::CorruptIndex("invalid sprout tree"))?;
            self.anchors_sprout
                .insert(&mut batch, &sprout_root, sprout_bytes.clone());
            batch.put(Column::Meta, SPROUT_TREE_KEY, sprout_bytes);
        }
        if sapling_changed {
            let tree = sapling_tree
                .as_ref()
                .ok_or(ChainStateError::CorruptIndex("missing sapling tree state"))?;
            let sapling_bytes = sapling_tree_to_bytes(tree)
                .map_err(|_| ChainStateError::CorruptIndex("invalid sapling tree"))?;
            self.anchors_sapling
                .insert(&mut batch, &sapling_root, Vec::new());
            batch.put(Column::Meta, SAPLING_TREE_KEY, sapling_bytes);
        }
        anchor_time += anchor_finalize_start.elapsed();

        let block_bytes = block.consensus_encode()?;
        let flatfile_start = Instant::now();
        let location = self.blocks.append(&block_bytes)?;
        flatfile_time += flatfile_start.elapsed();
        let block_file_len = location
            .offset
            .checked_add(4)
            .and_then(|value| value.checked_add(location.len as u64))
            .ok_or(ChainStateError::ValueOutOfRange)?;
        self.update_flatfile_meta(
            &mut batch,
            FlatFileKind::Blocks,
            location.file_id,
            block_file_len,
            height,
            block.header.time,
        )?;
        let block_hash = block.header.hash();
        let mut logical_ts = block.header.time;
        if block.header.prev_block != [0u8; 32] {
            match self.block_logical_time(&block.header.prev_block) {
                Ok(Some(prev_ts)) => {
                    if logical_ts <= prev_ts {
                        logical_ts = prev_ts.saturating_add(1);
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    eprintln!("failed to read previous block logical timestamp: {}", err);
                }
            }
        }
        let mut ts_key = Vec::with_capacity(36);
        ts_key.extend_from_slice(&logical_ts.to_be_bytes());
        ts_key.extend_from_slice(&block_hash);
        batch.put(Column::TimestampIndex, ts_key, Vec::new());
        batch.put(
            Column::BlockTimestamp,
            block_hash.to_vec(),
            logical_ts.to_be_bytes().to_vec(),
        );
        batch.put(
            Column::BlockHeader,
            block_hash.to_vec(),
            block.header.consensus_encode(),
        );

        let index_start = Instant::now();
        for (index, txid) in txids.iter().enumerate() {
            let tx_location = TxLocation {
                block: location,
                index: index as u32,
            };
            self.tx_index.insert(&mut batch, txid, tx_location);
        }
        index_time += index_start.elapsed();

        let mut entry = header_entry;
        entry.status = status_with_block(entry.status);
        let index_start = Instant::now();
        self.index.put_header(&mut batch, &block_hash, &entry);
        self.index.set_best_block(&mut batch, &block_hash);
        self.index.set_height_hash(&mut batch, height, &block_hash);
        index_time += index_start.elapsed();

        let block_value = block_subsidy(height, consensus);
        let exchange_fund = exchange_fund_amount(height, &params.funding);
        let foundation_fund = foundation_fund_amount(height, &params.funding);
        let swap_pool = swap_pool_amount(height as i64, &params.swap_pool);
        let coinbase_value = tx_value_out(&block.transactions[0])?;
        let max_reward = block_value
            .checked_add(total_fees)
            .and_then(|value| value.checked_add(exchange_fund))
            .and_then(|value| value.checked_add(foundation_fund))
            .and_then(|value| value.checked_add(swap_pool))
            .ok_or(ChainStateError::ValueOutOfRange)?;
        if height > 2 && coinbase_value > max_reward {
            return Err(ChainStateError::Validation(ValidationError::InvalidBlock(
                "coinbase pays too much",
            )));
        }

        utxo_stats.txouts = utxo_stats
            .txouts
            .checked_add(utxos_created)
            .and_then(|value| value.checked_sub(utxos_spent))
            .ok_or(ChainStateError::CorruptIndex("utxo stats mismatch"))?;
        utxo_stats.total_amount = utxo_stats
            .total_amount
            .checked_add(value_created)
            .and_then(|value| value.checked_sub(value_spent))
            .ok_or(ChainStateError::ValueOutOfRange)?;
        batch.put(Column::Meta, UTXO_STATS_KEY, utxo_stats.encode());

        value_pools.sprout = value_pools
            .sprout
            .checked_add(sprout_pool_delta)
            .ok_or(ChainStateError::ValueOutOfRange)?;
        value_pools.sapling = value_pools
            .sapling
            .checked_add(sapling_pool_delta)
            .ok_or(ChainStateError::ValueOutOfRange)?;
        if value_pools.sprout < 0 || value_pools.sapling < 0 {
            return Err(ChainStateError::CorruptIndex(
                "negative shielded value pool",
            ));
        }
        batch.put(Column::Meta, VALUE_POOLS_KEY, value_pools.encode());

        let undo_bytes = undo.encode();
        let undo_flatfile_start = Instant::now();
        let undo_location = self.undo.append(&undo_bytes)?;
        flatfile_time += undo_flatfile_start.elapsed();
        let undo_file_len = undo_location
            .offset
            .checked_add(4)
            .and_then(|value| value.checked_add(undo_location.len as u64))
            .ok_or(ChainStateError::ValueOutOfRange)?;
        self.update_flatfile_meta(
            &mut batch,
            FlatFileKind::Undo,
            undo_location.file_id,
            undo_file_len,
            height,
            block.header.time,
        )?;

        batch.put(
            Column::BlockIndex,
            block_hash.to_vec(),
            BlockIndexEntry {
                block: location,
                undo: Some(undo_location),
                tx_count: block.transactions.len() as u32,
                status: STATUS_HAVE_DATA | STATUS_HAVE_UNDO,
            }
            .encode(),
        );
        batch.put(
            Column::BlockUndo,
            block_hash.to_vec(),
            undo_location.encode(),
        );
        self.prune_block_undo(height, &mut batch)?;

        if let Some(metrics) = connect_metrics {
            metrics.record_utxo(utxo_time);
            metrics.record_index(index_time);
            metrics.record_anchor(anchor_time);
            metrics.record_flatfile(flatfile_time);
        }

        Ok(batch)
    }

    pub fn disconnect_block(&self, hash: &Hash256) -> Result<WriteBatch, ChainStateError> {
        let best_block = self
            .index
            .best_block()?
            .ok_or(ChainStateError::InvalidHeader(
                "missing best block for disconnect",
            ))?;
        if best_block.hash != *hash {
            return Err(ChainStateError::InvalidHeader(
                "block does not match best block tip",
            ));
        }
        let entry = self
            .index
            .get_header(hash)?
            .ok_or(ChainStateError::MissingHeader)?;
        let location = self
            .block_location(hash)?
            .ok_or(ChainStateError::CorruptIndex("missing block index entry"))?;
        let bytes = self.read_block(location)?;
        let block = Block::consensus_decode(&bytes)
            .map_err(|_| ChainStateError::CorruptIndex("invalid block bytes"))?;
        let mut undo = self.block_undo(hash)?.ok_or(ChainStateError::CorruptIndex(
            "missing block undo entry; resync required",
        ))?;

        let mut batch = WriteBatch::new();
        let mut utxo_stats = self.utxo_stats_or_compute()?;
        let mut value_pools = self.value_pools_or_compute()?;
        let (sprout_pool_delta, sapling_pool_delta) = value_pool_deltas(&block)?;
        let mut utxos_removed = 0u64;
        let mut utxos_restored = 0u64;
        let mut value_removed = 0i64;
        let mut value_restored = 0i64;

        for tx in &block.transactions {
            for joinsplit in &tx.join_splits {
                for nullifier in &joinsplit.nullifiers {
                    self.nullifiers_sprout.remove(&mut batch, nullifier);
                }
            }
            for spend in &tx.shielded_spends {
                self.nullifiers_sapling.remove(&mut batch, &spend.nullifier);
            }
        }

        for (tx_index, tx) in block.transactions.iter().enumerate().rev() {
            let txid = tx.txid()?;
            for (output_index, output) in tx.vout.iter().enumerate() {
                let outpoint = OutPoint {
                    hash: txid,
                    index: output_index as u32,
                };
                self.utxos.delete(&mut batch, &outpoint);
                self.address_index
                    .delete(&mut batch, &output.script_pubkey, &outpoint);
                utxos_removed = utxos_removed
                    .checked_add(1)
                    .ok_or(ChainStateError::ValueOutOfRange)?;
                value_removed = value_removed
                    .checked_add(output.value)
                    .ok_or(ChainStateError::ValueOutOfRange)?;
            }
            self.tx_index.delete(&mut batch, &txid);

            if tx_index != 0 {
                for input in tx.vin.iter().rev() {
                    let spent = undo
                        .spent
                        .pop()
                        .ok_or(ChainStateError::CorruptIndex("block undo input mismatch"))?;
                    if spent.outpoint != input.prevout {
                        return Err(ChainStateError::CorruptIndex(
                            "block undo outpoint mismatch",
                        ));
                    }
                    self.utxos.put(&mut batch, &spent.outpoint, &spent.entry);
                    self.address_index.insert(
                        &mut batch,
                        &spent.entry.script_pubkey,
                        &spent.outpoint,
                    );
                    utxos_restored = utxos_restored
                        .checked_add(1)
                        .ok_or(ChainStateError::ValueOutOfRange)?;
                    value_restored = value_restored
                        .checked_add(spent.entry.value)
                        .ok_or(ChainStateError::ValueOutOfRange)?;
                }
            }

            if let Some(collateral) = fluxnode_collateral(tx) {
                let entry = undo.fluxnode.pop().ok_or(ChainStateError::CorruptIndex(
                    "block undo fluxnode mismatch",
                ))?;
                if &entry.collateral != collateral {
                    return Err(ChainStateError::CorruptIndex(
                        "block undo fluxnode collateral mismatch",
                    ));
                }
                let key = outpoint_key_bytes(&entry.collateral);
                match &entry.prev {
                    Some(record) => {
                        batch.put(Column::Fluxnode, key.as_bytes(), record.encode());
                    }
                    None => {
                        batch.delete(Column::Fluxnode, key.as_bytes());
                    }
                }
            }
        }

        if !undo.spent.is_empty() {
            return Err(ChainStateError::CorruptIndex(
                "block undo has extra spent entries",
            ));
        }
        if !undo.fluxnode.is_empty() {
            return Err(ChainStateError::CorruptIndex(
                "block undo has extra fluxnode entries",
            ));
        }

        if let Some(ts) = self.block_logical_time(hash)? {
            let mut ts_key = Vec::with_capacity(36);
            ts_key.extend_from_slice(&ts.to_be_bytes());
            ts_key.extend_from_slice(hash);
            batch.delete(Column::TimestampIndex, ts_key);
            batch.delete(Column::BlockTimestamp, hash.to_vec());
        }

        let (
            current_sprout_root,
            current_sprout_bytes,
            current_sapling_root,
            current_sapling_bytes,
        ) = self.shielded_cache_snapshot()?;
        let prev_sprout_root = if undo.prev_sprout_tree == current_sprout_bytes {
            current_sprout_root
        } else {
            let prev_sprout_tree = sprout_tree_from_bytes(&undo.prev_sprout_tree)
                .map_err(|_| ChainStateError::CorruptIndex("invalid sprout undo tree"))?;
            sprout_root_hash(&prev_sprout_tree)
        };
        let prev_sapling_root = if undo.prev_sapling_tree == current_sapling_bytes {
            current_sapling_root
        } else {
            let prev_sapling_tree = sapling_tree_from_bytes(&undo.prev_sapling_tree)
                .map_err(|_| ChainStateError::CorruptIndex("invalid sapling undo tree"))?;
            sapling_root_hash(&prev_sapling_tree)
        };

        self.anchors_sprout.remove(&mut batch, &current_sprout_root);
        self.anchors_sapling
            .remove(&mut batch, &current_sapling_root);
        self.anchors_sprout
            .insert(&mut batch, &prev_sprout_root, undo.prev_sprout_tree.clone());
        self.anchors_sapling
            .insert(&mut batch, &prev_sapling_root, Vec::new());
        batch.put(Column::Meta, SPROUT_TREE_KEY, undo.prev_sprout_tree);
        batch.put(Column::Meta, SAPLING_TREE_KEY, undo.prev_sapling_tree);

        self.index.clear_height_hash(&mut batch, entry.height);
        self.index.set_best_block(&mut batch, &entry.prev_hash);
        batch.delete(Column::BlockUndo, hash.to_vec());
        self.clear_block_index_undo(hash, &mut batch)?;

        utxo_stats.txouts = utxo_stats
            .txouts
            .checked_add(utxos_restored)
            .and_then(|value| value.checked_sub(utxos_removed))
            .ok_or(ChainStateError::CorruptIndex("utxo stats mismatch"))?;
        utxo_stats.total_amount = utxo_stats
            .total_amount
            .checked_sub(value_removed)
            .and_then(|value| value.checked_add(value_restored))
            .ok_or(ChainStateError::ValueOutOfRange)?;
        batch.put(Column::Meta, UTXO_STATS_KEY, utxo_stats.encode());

        value_pools.sprout = value_pools
            .sprout
            .checked_sub(sprout_pool_delta)
            .ok_or(ChainStateError::ValueOutOfRange)?;
        value_pools.sapling = value_pools
            .sapling
            .checked_sub(sapling_pool_delta)
            .ok_or(ChainStateError::ValueOutOfRange)?;
        if value_pools.sprout < 0 || value_pools.sapling < 0 {
            return Err(ChainStateError::CorruptIndex(
                "negative shielded value pool",
            ));
        }
        batch.put(Column::Meta, VALUE_POOLS_KEY, value_pools.encode());

        if let Ok(mut cache) = self.header_cache.lock() {
            cache.insert(*hash, entry.clone());
        }

        Ok(batch)
    }

    pub fn set_best_header(&self, hash: &Hash256) -> Result<(), ChainStateError> {
        if self.index.get_header(hash)?.is_none() {
            return Err(ChainStateError::MissingHeader);
        }
        let mut batch = WriteBatch::new();
        self.index.set_best_header(&mut batch, hash);
        self.store.write_batch(&batch)?;
        Ok(())
    }

    pub fn commit_batch(&self, batch: WriteBatch) -> Result<(), ChainStateError> {
        let mut sprout_bytes = None;
        let mut sapling_bytes = None;
        for op in batch.iter() {
            if let WriteOp::Put { column, key, value } = op {
                if *column != Column::Meta {
                    continue;
                }
                if key.as_slice() == SPROUT_TREE_KEY {
                    sprout_bytes = Some(value.clone());
                } else if key.as_slice() == SAPLING_TREE_KEY {
                    sapling_bytes = Some(value.clone());
                }
            }
        }
        self.store.write_batch(&batch)?;
        if sprout_bytes.is_some() || sapling_bytes.is_some() {
            self.update_shielded_cache(sprout_bytes, sapling_bytes)?;
        }
        let ops = batch.into_ops();
        if let Ok(mut meta_cache) = self.file_meta.lock() {
            for op in &ops {
                match op {
                    WriteOp::Put { column, key, value } if *column == Column::Meta => {
                        if let Some(file_id) = parse_block_file_info_key(key.as_slice()) {
                            if let Some(info) = FlatFileInfo::decode(value) {
                                meta_cache.blocks = Some(TrackedFlatFile { file_id, info });
                            }
                        } else if let Some(file_id) = parse_undo_file_info_key(key.as_slice()) {
                            if let Some(info) = FlatFileInfo::decode(value) {
                                meta_cache.undo = Some(TrackedFlatFile { file_id, info });
                            }
                        }
                    }
                    WriteOp::Delete { column, key } if *column == Column::Meta => {
                        if let Some(file_id) = parse_block_file_info_key(key.as_slice()) {
                            if meta_cache.blocks.map(|tracked| tracked.file_id) == Some(file_id) {
                                meta_cache.blocks = None;
                            }
                        } else if let Some(file_id) = parse_undo_file_info_key(key.as_slice()) {
                            if meta_cache.undo.map(|tracked| tracked.file_id) == Some(file_id) {
                                meta_cache.undo = None;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Ok(mut cache) = self.utxo_cache.lock() {
            for op in ops {
                match op {
                    WriteOp::Put { column, key, value } => {
                        if column != Column::Utxo {
                            continue;
                        }
                        let Some(outpoint_key) = OutPointKey::from_slice(key.as_slice()) else {
                            continue;
                        };
                        cache.insert(outpoint_key, value);
                    }
                    WriteOp::Delete { column, key } => {
                        if column != Column::Utxo {
                            continue;
                        }
                        let Some(outpoint_key) = OutPointKey::from_slice(key.as_slice()) else {
                            continue;
                        };
                        cache.remove(&outpoint_key);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn read_block(&self, location: FileLocation) -> Result<Vec<u8>, ChainStateError> {
        Ok(self.blocks.read(location)?)
    }

    pub fn block_location(&self, hash: &[u8; 32]) -> Result<Option<FileLocation>, ChainStateError> {
        Ok(self.block_index_entry(hash)?.map(|entry| entry.block))
    }

    fn block_index_entry(
        &self,
        hash: &Hash256,
    ) -> Result<Option<BlockIndexEntry>, ChainStateError> {
        let bytes = match self.store.get(Column::BlockIndex, hash)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        BlockIndexEntry::decode(&bytes)
            .ok_or(ChainStateError::CorruptIndex("invalid block index entry"))
            .map(Some)
    }

    fn clear_block_index_undo(
        &self,
        hash: &Hash256,
        batch: &mut WriteBatch,
    ) -> Result<(), ChainStateError> {
        let Some(mut entry) = self.block_index_entry(hash)? else {
            return Ok(());
        };
        if entry.undo.is_none() && (entry.status & STATUS_HAVE_UNDO) == 0 {
            return Ok(());
        }
        entry.undo = None;
        entry.status &= !STATUS_HAVE_UNDO;
        batch.put(Column::BlockIndex, hash.to_vec(), entry.encode());
        Ok(())
    }

    fn flatfile_info_cached(
        &self,
        kind: FlatFileKind,
        file_id: u32,
    ) -> Result<FlatFileInfo, ChainStateError> {
        if let Ok(cache) = self.file_meta.lock() {
            let tracked = match kind {
                FlatFileKind::Blocks => cache.blocks,
                FlatFileKind::Undo => cache.undo,
            };
            if let Some(tracked) = tracked {
                if tracked.file_id == file_id {
                    return Ok(tracked.info);
                }
            }
        }

        let key = match kind {
            FlatFileKind::Blocks => block_file_info_key(file_id).to_vec(),
            FlatFileKind::Undo => undo_file_info_key(file_id).to_vec(),
        };
        let info = match self.store.get(Column::Meta, &key)? {
            Some(bytes) => FlatFileInfo::decode(&bytes)
                .ok_or(ChainStateError::CorruptIndex("invalid flatfile info entry"))?,
            None => FlatFileInfo::default(),
        };
        if let Ok(mut cache) = self.file_meta.lock() {
            let tracked = TrackedFlatFile { file_id, info };
            match kind {
                FlatFileKind::Blocks => cache.blocks = Some(tracked),
                FlatFileKind::Undo => cache.undo = Some(tracked),
            }
        }
        Ok(info)
    }

    fn update_flatfile_meta(
        &self,
        batch: &mut WriteBatch,
        kind: FlatFileKind,
        file_id: u32,
        file_len: u64,
        height: i32,
        time: u32,
    ) -> Result<(), ChainStateError> {
        let mut info = self.flatfile_info_cached(kind, file_id)?;
        if info.blocks == 0 {
            info.height_first = height;
            info.time_first = time;
        }
        info.blocks = info
            .blocks
            .checked_add(1)
            .ok_or(ChainStateError::ValueOutOfRange)?;
        info.size = file_len;
        info.height_last = height;
        info.time_last = time;

        match kind {
            FlatFileKind::Blocks => {
                let info_key = block_file_info_key(file_id);
                batch.put(Column::Meta, info_key, info.encode());
                batch.put(
                    Column::Meta,
                    META_BLOCK_FILES_LAST_FILE_KEY,
                    file_id.to_le_bytes().to_vec(),
                );
                batch.put(
                    Column::Meta,
                    META_BLOCK_FILES_LAST_LEN_KEY,
                    file_len.to_le_bytes().to_vec(),
                );
            }
            FlatFileKind::Undo => {
                let info_key = undo_file_info_key(file_id);
                batch.put(Column::Meta, info_key, info.encode());
                batch.put(
                    Column::Meta,
                    META_UNDO_FILES_LAST_FILE_KEY,
                    file_id.to_le_bytes().to_vec(),
                );
                batch.put(
                    Column::Meta,
                    META_UNDO_FILES_LAST_LEN_KEY,
                    file_len.to_le_bytes().to_vec(),
                );
            }
        }

        Ok(())
    }

    pub fn height_hash(&self, height: i32) -> Result<Option<Hash256>, ChainStateError> {
        Ok(self.index.height_hash(height)?)
    }

    pub fn scan_headers(&self) -> Result<Vec<(Hash256, HeaderEntry)>, ChainStateError> {
        Ok(self.index.scan_headers()?)
    }

    pub fn block_header_bytes(&self, hash: &[u8; 32]) -> Result<Option<Vec<u8>>, ChainStateError> {
        Ok(self.store.get(Column::BlockHeader, hash)?)
    }

    pub fn block_logical_time(&self, hash: &Hash256) -> Result<Option<u32>, ChainStateError> {
        let bytes = match self.store.get(Column::BlockTimestamp, hash)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        if bytes.len() != 4 {
            return Err(ChainStateError::CorruptIndex(
                "invalid block timestamp entry",
            ));
        }
        let mut buf = [0u8; 4];
        buf.copy_from_slice(&bytes);
        Ok(Some(u32::from_be_bytes(buf)))
    }

    fn block_undo(&self, hash: &Hash256) -> Result<Option<BlockUndo>, ChainStateError> {
        let bytes = match self.store.get(Column::BlockUndo, hash)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        if bytes.len() == 16 {
            let location = FileLocation::decode(&bytes)
                .ok_or(ChainStateError::CorruptIndex("invalid block undo entry"))?;
            let undo_bytes = self.undo.read(location)?;
            return BlockUndo::decode(&undo_bytes)
                .map(Some)
                .map_err(|_| ChainStateError::CorruptIndex("invalid block undo entry"));
        }
        BlockUndo::decode(&bytes)
            .map(Some)
            .map_err(|_| ChainStateError::CorruptIndex("invalid block undo entry"))
    }

    fn prune_block_undo(&self, height: i32, batch: &mut WriteBatch) -> Result<(), ChainStateError> {
        if height < 0 {
            return Ok(());
        }
        let max_depth = max_reorg_depth(height as i64) as i32;
        let prune_height = height.saturating_sub(max_depth.saturating_add(1));
        if prune_height < 0 {
            return Ok(());
        }
        if let Some(hash) = self.index.height_hash(prune_height)? {
            batch.delete(Column::BlockUndo, hash.to_vec());
            self.clear_block_index_undo(&hash, batch)?;
        }
        Ok(())
    }

    pub fn scan_timestamp_index(&self) -> Result<Vec<(u32, Hash256)>, ChainStateError> {
        let entries = self.store.scan_prefix(Column::TimestampIndex, &[])?;
        let mut out = Vec::with_capacity(entries.len());
        for (key, _) in entries {
            if key.len() != 36 {
                continue;
            }
            let mut ts_buf = [0u8; 4];
            ts_buf.copy_from_slice(&key[0..4]);
            let timestamp = u32::from_be_bytes(ts_buf);
            let mut hash = [0u8; 32];
            hash.copy_from_slice(&key[4..36]);
            out.push((timestamp, hash));
        }
        Ok(out)
    }

    pub fn tx_location(&self, txid: &[u8; 32]) -> Result<Option<TxLocation>, ChainStateError> {
        self.tx_index.get(txid).map_err(ChainStateError::from)
    }

    pub fn address_outpoints(
        &self,
        script_pubkey: &[u8],
    ) -> Result<Vec<OutPoint>, ChainStateError> {
        Ok(self.address_index.scan(script_pubkey)?)
    }

    pub fn utxo_exists(&self, outpoint: &OutPoint) -> Result<bool, ChainStateError> {
        let key = outpoint_key_bytes(outpoint);
        Ok(self.store.get(Column::Utxo, key.as_bytes())?.is_some())
    }

    pub fn utxo_entry(&self, outpoint: &OutPoint) -> Result<Option<UtxoEntry>, ChainStateError> {
        let key = outpoint_key_bytes(outpoint);
        self.utxo_entry_cached(key)
    }

    fn utxo_entry_cached(&self, key: OutPointKey) -> Result<Option<UtxoEntry>, ChainStateError> {
        if let Ok(mut cache) = self.utxo_cache.lock() {
            if let Some(bytes) = cache.get(&key) {
                let entry =
                    UtxoEntry::decode(bytes).map_err(|err| StoreError::Backend(err.to_string()))?;
                return Ok(Some(entry));
            }
        }

        let bytes = match self.store.get(Column::Utxo, key.as_bytes())? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        let entry =
            UtxoEntry::decode(&bytes).map_err(|err| StoreError::Backend(err.to_string()))?;
        if let Ok(mut cache) = self.utxo_cache.lock() {
            cache.insert(key, bytes);
        }
        Ok(Some(entry))
    }

    pub fn utxo_stats(&self) -> Result<Option<UtxoStats>, ChainStateError> {
        let bytes = match self.store.get(Column::Meta, UTXO_STATS_KEY)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        let stats = UtxoStats::decode(&bytes)
            .map_err(|_| ChainStateError::CorruptIndex("invalid utxo stats"))?;
        Ok(Some(stats))
    }

    pub fn ensure_utxo_stats(&self) -> Result<UtxoStats, ChainStateError> {
        if let Some(stats) = self.utxo_stats()? {
            return Ok(stats);
        }
        let stats = self.compute_utxo_stats()?;
        let mut batch = WriteBatch::new();
        batch.put(Column::Meta, UTXO_STATS_KEY, stats.encode());
        self.commit_batch(batch)?;
        Ok(stats)
    }

    pub fn utxo_stats_or_compute(&self) -> Result<UtxoStats, ChainStateError> {
        if let Some(stats) = self.utxo_stats()? {
            return Ok(stats);
        }
        self.compute_utxo_stats()
    }

    pub fn value_pools(&self) -> Result<Option<ValuePools>, ChainStateError> {
        let bytes = match self.store.get(Column::Meta, VALUE_POOLS_KEY)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        let pools = ValuePools::decode(&bytes)
            .map_err(|_| ChainStateError::CorruptIndex("invalid value pools"))?;
        Ok(Some(pools))
    }

    pub fn ensure_value_pools(&self) -> Result<ValuePools, ChainStateError> {
        if let Some(pools) = self.value_pools()? {
            return Ok(pools);
        }
        let pools = self.compute_value_pools()?;
        let mut batch = WriteBatch::new();
        batch.put(Column::Meta, VALUE_POOLS_KEY, pools.encode());
        self.commit_batch(batch)?;
        Ok(pools)
    }

    pub fn value_pools_or_compute(&self) -> Result<ValuePools, ChainStateError> {
        if let Some(pools) = self.value_pools()? {
            return Ok(pools);
        }
        self.compute_value_pools()
    }

    fn compute_utxo_stats(&self) -> Result<UtxoStats, ChainStateError> {
        let mut txouts = 0u64;
        let mut total_amount = 0i64;
        let mut visitor = |_: &[u8], value: &[u8]| -> Result<(), StoreError> {
            let entry =
                UtxoEntry::decode(value).map_err(|err| StoreError::Backend(err.to_string()))?;
            txouts = txouts
                .checked_add(1)
                .ok_or_else(|| StoreError::Backend("utxo txouts overflow".to_string()))?;
            total_amount = total_amount
                .checked_add(entry.value)
                .ok_or_else(|| StoreError::Backend("utxo total overflow".to_string()))?;
            Ok(())
        };
        self.store
            .for_each_prefix(Column::Utxo, &[], &mut visitor)?;
        Ok(UtxoStats {
            txouts,
            total_amount,
        })
    }

    fn compute_value_pools(&self) -> Result<ValuePools, ChainStateError> {
        let best = match self.best_block()? {
            Some(tip) => tip,
            None => return Ok(ValuePools::default()),
        };
        if best.height < 0 {
            return Ok(ValuePools::default());
        }

        let mut pools = ValuePools::default();
        let mut last_progress = Instant::now();
        for height in 0..=best.height {
            let hash = self
                .height_hash(height)?
                .ok_or(ChainStateError::CorruptIndex("missing height index entry"))?;
            let location = self
                .block_location(&hash)?
                .ok_or(ChainStateError::CorruptIndex("missing block index entry"))?;
            let bytes = self.read_block(location)?;
            let block = Block::consensus_decode(&bytes)
                .map_err(|_| ChainStateError::CorruptIndex("invalid block bytes"))?;
            let (sprout_delta, sapling_delta) = value_pool_deltas(&block)?;
            pools.sprout = pools
                .sprout
                .checked_add(sprout_delta)
                .ok_or(ChainStateError::ValueOutOfRange)?;
            pools.sapling = pools
                .sapling
                .checked_add(sapling_delta)
                .ok_or(ChainStateError::ValueOutOfRange)?;
            if pools.sprout < 0 || pools.sapling < 0 {
                return Err(ChainStateError::CorruptIndex(
                    "negative shielded value pool",
                ));
            }
            if height > 0 && height % 100_000 == 0 {
                println!(
                    "Rebuilt value pools to height {} (elapsed {:?})",
                    height,
                    last_progress.elapsed()
                );
                last_progress = Instant::now();
            }
        }
        Ok(pools)
    }

    pub fn fluxnode_records(&self) -> Result<Vec<FluxnodeRecord>, ChainStateError> {
        let entries = self.store.scan_prefix(Column::Fluxnode, &[])?;
        let mut records = Vec::with_capacity(entries.len());
        for (_, value) in entries {
            let record = FluxnodeRecord::decode(&value)
                .map_err(|_| ChainStateError::CorruptIndex("invalid fluxnode record"))?;
            records.push(record);
        }
        Ok(records)
    }

    pub fn fluxnode_key(&self, key: KeyId) -> Result<Option<Vec<u8>>, ChainStateError> {
        Ok(self.store.get(Column::FluxnodeKey, &key.0)?)
    }

    fn sprout_anchor_tree(
        &self,
        anchor: &fluxd_consensus::Hash256,
    ) -> Result<Option<SproutTree>, ChainStateError> {
        if *anchor == sprout_empty_root_hash() {
            return Ok(Some(empty_sprout_tree()));
        }
        let bytes = match self.anchors_sprout.get(anchor)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        let tree = sprout_tree_from_bytes(&bytes)
            .map_err(|_| ChainStateError::CorruptIndex("invalid sprout anchor tree"))?;
        Ok(Some(tree))
    }

    fn sapling_anchor_exists(
        &self,
        anchor: &fluxd_consensus::Hash256,
    ) -> Result<bool, ChainStateError> {
        if *anchor == sapling_empty_root_hash() {
            return Ok(true);
        }
        Ok(self.anchors_sapling.contains(anchor)?)
    }
}

const SPROUT_TREE_KEY: &[u8] = b"sprout_tree";
const SAPLING_TREE_KEY: &[u8] = b"sapling_tree";
const UTXO_STATS_KEY: &[u8] = b"utxo_stats_v1";
const VALUE_POOLS_KEY: &[u8] = b"value_pools_v1";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UtxoStats {
    pub txouts: u64,
    pub total_amount: i64,
}

impl UtxoStats {
    fn encode(self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.write_u64_le(self.txouts);
        encoder.write_i64_le(self.total_amount);
        encoder.into_inner()
    }

    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut decoder = Decoder::new(bytes);
        let txouts = decoder.read_u64_le()?;
        let total_amount = decoder.read_i64_le()?;
        if !decoder.is_empty() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(Self {
            txouts,
            total_amount,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ValuePools {
    pub sprout: i64,
    pub sapling: i64,
}

impl ValuePools {
    fn encode(self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.write_i64_le(self.sprout);
        encoder.write_i64_le(self.sapling);
        encoder.into_inner()
    }

    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut decoder = Decoder::new(bytes);
        let sprout = decoder.read_i64_le()?;
        let sapling = decoder.read_i64_le()?;
        if !decoder.is_empty() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(Self { sprout, sapling })
    }
}

#[derive(Clone, Debug)]
struct ShieldedTreesCache {
    sprout_tree: SproutTree,
    sprout_root: Hash256,
    sprout_bytes: Vec<u8>,
    sapling_tree: SaplingTree,
    sapling_root: Hash256,
    sapling_bytes: Vec<u8>,
}

impl ShieldedTreesCache {
    fn load<S: KeyValueStore>(store: &S) -> Result<Self, ChainStateError> {
        let (sprout_tree, sprout_bytes) = match store.get(Column::Meta, SPROUT_TREE_KEY)? {
            Some(bytes) => (
                sprout_tree_from_bytes(&bytes)
                    .map_err(|_| ChainStateError::CorruptIndex("invalid sprout tree"))?,
                bytes,
            ),
            None => {
                let tree = empty_sprout_tree();
                let bytes = sprout_tree_to_bytes(&tree)
                    .map_err(|_| ChainStateError::CorruptIndex("invalid sprout tree"))?;
                (tree, bytes)
            }
        };
        let (sapling_tree, sapling_bytes) = match store.get(Column::Meta, SAPLING_TREE_KEY)? {
            Some(bytes) => (
                sapling_tree_from_bytes(&bytes)
                    .map_err(|_| ChainStateError::CorruptIndex("invalid sapling tree"))?,
                bytes,
            ),
            None => {
                let tree = empty_sapling_tree();
                let bytes = sapling_tree_to_bytes(&tree)
                    .map_err(|_| ChainStateError::CorruptIndex("invalid sapling tree"))?;
                (tree, bytes)
            }
        };
        Ok(Self {
            sprout_root: sprout_root_hash(&sprout_tree),
            sprout_tree,
            sprout_bytes,
            sapling_root: sapling_root_hash(&sapling_tree),
            sapling_tree,
            sapling_bytes,
        })
    }
}

impl<S: KeyValueStore> ChainState<S> {
    fn shielded_cache_snapshot(
        &self,
    ) -> Result<(Hash256, Vec<u8>, Hash256, Vec<u8>), ChainStateError> {
        let mut cache = self
            .shielded_cache
            .lock()
            .map_err(|_| ChainStateError::CorruptIndex("shielded cache poisoned"))?;
        if cache.is_none() {
            *cache = Some(ShieldedTreesCache::load(&self.store)?);
        }
        let cache = cache
            .as_ref()
            .ok_or(ChainStateError::CorruptIndex("missing shielded cache"))?;
        Ok((
            cache.sprout_root,
            cache.sprout_bytes.clone(),
            cache.sapling_root,
            cache.sapling_bytes.clone(),
        ))
    }

    fn shielded_cache_sprout_tree(&self) -> Result<SproutTree, ChainStateError> {
        let mut cache = self
            .shielded_cache
            .lock()
            .map_err(|_| ChainStateError::CorruptIndex("shielded cache poisoned"))?;
        if cache.is_none() {
            *cache = Some(ShieldedTreesCache::load(&self.store)?);
        }
        Ok(cache
            .as_ref()
            .ok_or(ChainStateError::CorruptIndex("missing shielded cache"))?
            .sprout_tree
            .clone())
    }

    fn shielded_cache_sapling_tree(&self) -> Result<SaplingTree, ChainStateError> {
        let mut cache = self
            .shielded_cache
            .lock()
            .map_err(|_| ChainStateError::CorruptIndex("shielded cache poisoned"))?;
        if cache.is_none() {
            *cache = Some(ShieldedTreesCache::load(&self.store)?);
        }
        Ok(cache
            .as_ref()
            .ok_or(ChainStateError::CorruptIndex("missing shielded cache"))?
            .sapling_tree
            .clone())
    }

    fn update_shielded_cache(
        &self,
        sprout_bytes: Option<Vec<u8>>,
        sapling_bytes: Option<Vec<u8>>,
    ) -> Result<(), ChainStateError> {
        let mut cache = self
            .shielded_cache
            .lock()
            .map_err(|_| ChainStateError::CorruptIndex("shielded cache poisoned"))?;
        if cache.is_none() {
            *cache = Some(ShieldedTreesCache::load(&self.store)?);
        }
        let cache = cache
            .as_mut()
            .ok_or(ChainStateError::CorruptIndex("missing shielded cache"))?;

        if let Some(bytes) = sprout_bytes {
            let tree = sprout_tree_from_bytes(&bytes)
                .map_err(|_| ChainStateError::CorruptIndex("invalid sprout tree"))?;
            cache.sprout_root = sprout_root_hash(&tree);
            cache.sprout_tree = tree;
            cache.sprout_bytes = bytes;
        }
        if let Some(bytes) = sapling_bytes {
            let tree = sapling_tree_from_bytes(&bytes)
                .map_err(|_| ChainStateError::CorruptIndex("invalid sapling tree"))?;
            cache.sapling_root = sapling_root_hash(&tree);
            cache.sapling_tree = tree;
            cache.sapling_bytes = bytes;
        }
        Ok(())
    }
}

fn fluxnode_collateral(tx: &Transaction) -> Option<&OutPoint> {
    match tx.fluxnode.as_ref()? {
        FluxnodeTx::V5(FluxnodeTxV5::Start(start)) => Some(&start.collateral),
        FluxnodeTx::V5(FluxnodeTxV5::Confirm(confirm)) => Some(&confirm.collateral),
        FluxnodeTx::V6(FluxnodeTxV6::Start(start)) => match &start.variant {
            FluxnodeStartVariantV6::Normal { collateral, .. } => Some(collateral),
            FluxnodeStartVariantV6::P2sh { collateral, .. } => Some(collateral),
        },
        FluxnodeTx::V6(FluxnodeTxV6::Confirm(confirm)) => Some(&confirm.collateral),
    }
}

fn fluxnode_undo_entry<S: KeyValueStore>(
    store: &S,
    tx: &Transaction,
) -> Result<Option<FluxnodeUndo>, ChainStateError> {
    let Some(collateral) = fluxnode_collateral(tx) else {
        return Ok(None);
    };
    let key = outpoint_key_bytes(collateral);
    let prev = match store.get(Column::Fluxnode, key.as_bytes())? {
        Some(bytes) => Some(
            FluxnodeRecord::decode(&bytes)
                .map_err(|_| ChainStateError::CorruptIndex("invalid fluxnode record"))?,
        ),
        None => None,
    };
    Ok(Some(FluxnodeUndo {
        collateral: collateral.clone(),
        prev,
    }))
}

fn money_range(value: i64) -> bool {
    (0..=MAX_MONEY).contains(&value)
}

fn tx_value_out(tx: &Transaction) -> Result<i64, ChainStateError> {
    let mut total = 0i64;
    for output in &tx.vout {
        if output.value < 0 || output.value > MAX_MONEY {
            return Err(ChainStateError::ValueOutOfRange);
        }
        total = total
            .checked_add(output.value)
            .ok_or(ChainStateError::ValueOutOfRange)?;
        if !money_range(total) {
            return Err(ChainStateError::ValueOutOfRange);
        }
    }

    if tx.value_balance <= 0 {
        let balance = -tx.value_balance;
        total = total
            .checked_add(balance)
            .ok_or(ChainStateError::ValueOutOfRange)?;
        if !money_range(balance) || !money_range(total) {
            return Err(ChainStateError::ValueOutOfRange);
        }
    }

    for joinsplit in &tx.join_splits {
        total = total
            .checked_add(joinsplit.vpub_old)
            .ok_or(ChainStateError::ValueOutOfRange)?;
        if !money_range(joinsplit.vpub_old) || !money_range(total) {
            return Err(ChainStateError::ValueOutOfRange);
        }
    }

    Ok(total)
}

fn tx_shielded_value_in(tx: &Transaction) -> Result<i64, ChainStateError> {
    let mut total = 0i64;
    if tx.value_balance >= 0 {
        total = total
            .checked_add(tx.value_balance)
            .ok_or(ChainStateError::ValueOutOfRange)?;
        if !money_range(tx.value_balance) || !money_range(total) {
            return Err(ChainStateError::ValueOutOfRange);
        }
    }

    for joinsplit in &tx.join_splits {
        total = total
            .checked_add(joinsplit.vpub_new)
            .ok_or(ChainStateError::ValueOutOfRange)?;
        if !money_range(joinsplit.vpub_new) || !money_range(total) {
            return Err(ChainStateError::ValueOutOfRange);
        }
    }
    Ok(total)
}

fn value_pool_deltas(block: &Block) -> Result<(i64, i64), ChainStateError> {
    let mut sprout_delta = 0i64;
    let mut sapling_delta = 0i64;
    for tx in &block.transactions {
        sapling_delta = sapling_delta
            .checked_sub(tx.value_balance)
            .ok_or(ChainStateError::ValueOutOfRange)?;
        for joinsplit in &tx.join_splits {
            sprout_delta = sprout_delta
                .checked_add(joinsplit.vpub_old)
                .and_then(|value| value.checked_sub(joinsplit.vpub_new))
                .ok_or(ChainStateError::ValueOutOfRange)?;
        }
    }
    Ok((sprout_delta, sapling_delta))
}

fn block_bits_from_params(params: &ConsensusParams) -> u32 {
    fluxd_pow::difficulty::target_to_compact(&params.pow_limit)
}

fn current_time_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn collect_headers<S: KeyValueStore>(
    state: &ChainState<S>,
    tip_hash: &fluxd_consensus::Hash256,
    count: usize,
    pending: Option<&HashMap<Hash256, HeaderEntry>>,
) -> Result<Vec<HeaderInfo>, ChainStateError> {
    let mut headers = Vec::new();
    let mut current = *tip_hash;
    for _ in 0..count {
        let entry = header_entry_with_pending(state, pending, &current)?
            .ok_or(ChainStateError::MissingHeader)?;
        headers.push(HeaderInfo {
            height: entry.height as i64,
            time: entry.time as i64,
            bits: entry.bits,
        });
        if entry.height == 0 {
            break;
        }
        current = entry.prev_hash;
    }
    headers.reverse();
    Ok(headers)
}

fn median_time_past(headers: &[HeaderInfo]) -> i64 {
    let mut times: Vec<i64> = headers.iter().map(|header| header.time).collect();
    times.sort_unstable();
    times[times.len() / 2]
}

fn last_checkpoint_on_chain<S: KeyValueStore>(
    state: &ChainState<S>,
    params: &ConsensusParams,
    best_block_height: i32,
) -> Option<fluxd_consensus::params::Checkpoint> {
    params
        .checkpoints
        .iter()
        .rev()
        .find(|checkpoint| {
            checkpoint.height <= best_block_height
                && state
                    .header_entry(&checkpoint.hash)
                    .ok()
                    .flatten()
                    .is_some()
        })
        .copied()
}

fn expected_pon_bits<S: KeyValueStore>(
    state: &ChainState<S>,
    prev_hash: &fluxd_consensus::Hash256,
    height: i32,
    params: &ConsensusParams,
    pending: Option<&HashMap<Hash256, HeaderEntry>>,
) -> Result<u32, ChainStateError> {
    let prev_entry = header_entry_with_pending(state, pending, prev_hash)?
        .ok_or(ChainStateError::MissingHeader)?;

    let activation_height = params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height;
    let lookback_window = params.pon_difficulty_window as i32;
    if height < activation_height {
        return Ok(prev_entry.bits);
    }

    let pon_start_bits = fluxd_pow::difficulty::target_to_compact(&params.pon_start_limit);
    if height < activation_height + lookback_window {
        return Ok(pon_start_bits);
    }

    let window = params.pon_difficulty_window as usize;
    let chain = collect_headers(state, prev_hash, window, pending)?;
    if chain.len() < window {
        return Ok(pon_start_bits);
    }

    let first = chain.first().expect("checked length");
    let last = chain.last().expect("checked length");

    let mut actual_timespan = last.time - first.time;
    let target_timespan = (lookback_window as i64 - 1) * params.pon_target_spacing;
    if target_timespan <= 0 {
        return Ok(fluxd_pow::difficulty::target_to_compact(&params.pon_limit));
    }

    let min_timespan = target_timespan * 4 / 5;
    let max_timespan = target_timespan * 5 / 4;
    if actual_timespan < min_timespan {
        actual_timespan = min_timespan;
    }
    if actual_timespan > max_timespan {
        actual_timespan = max_timespan;
    }

    let prev_target = fluxd_pow::difficulty::compact_to_u256(prev_entry.bits)
        .map_err(|_| ChainStateError::InvalidHeader("invalid pon target"))?;
    if prev_target.is_zero() {
        return Ok(fluxd_pow::difficulty::target_to_compact(&params.pon_limit));
    }

    let mut next_target = prev_target / primitive_types::U256::from(target_timespan as u64);
    next_target *= primitive_types::U256::from(actual_timespan as u64);

    let max_target = primitive_types::U256::from_little_endian(&params.pon_limit);
    if next_target > max_target {
        next_target = max_target;
    }
    if next_target.is_zero() {
        next_target = max_target;
    }

    Ok(fluxd_pow::difficulty::u256_to_compact(next_target))
}

fn header_entry_with_pending<S: KeyValueStore>(
    state: &ChainState<S>,
    pending: Option<&HashMap<Hash256, HeaderEntry>>,
    hash: &Hash256,
) -> Result<Option<HeaderEntry>, ChainStateError> {
    if let Some(pending) = pending {
        if let Some(entry) = pending.get(hash) {
            return Ok(Some(entry.clone()));
        }
    }
    state.header_entry(hash)
}

fn hash256_to_hex(hash: &Hash256) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(64);
    for byte in hash.iter().rev() {
        let _ = write!(out, "{:02x}", byte);
    }
    out
}

fn outpoint_to_string(outpoint: &OutPoint) -> String {
    format!("{}:{}", hash256_to_hex(&outpoint.hash), outpoint.index)
}

fn check_coinbase_funding(
    tx: &Transaction,
    height: i32,
    params: &ChainParams,
) -> Result<(), ChainStateError> {
    let consensus = &params.consensus;
    let network = params.network;

    if network_upgrade_active(height, &consensus.upgrades, UpgradeIndex::Pon) {
        let min_dev = min_dev_fund_amount(height, consensus);
        if min_dev > 0 {
            let dev_script = address_to_script_pubkey(params.funding.dev_fund_address, network)
                .map_err(|_| {
                    ChainStateError::Validation(ValidationError::InvalidTransaction(
                        "invalid dev fund address",
                    ))
                })?;
            let found = tx
                .vout
                .iter()
                .any(|out| out.script_pubkey == dev_script && out.value >= min_dev);
            if !found {
                return Err(ChainStateError::Validation(
                    ValidationError::InvalidTransaction("coinbase missing dev fund payment"),
                ));
            }
        }
    }

    let exchange_amount = exchange_fund_amount(height, &params.funding);
    if exchange_amount > 0 {
        let exchange_script = address_to_script_pubkey(params.funding.exchange_address, network)
            .map_err(|_| {
                ChainStateError::Validation(ValidationError::InvalidTransaction(
                    "invalid exchange address",
                ))
            })?;
        let found = tx
            .vout
            .iter()
            .any(|out| out.script_pubkey == exchange_script && out.value == exchange_amount);
        if !found {
            return Err(ChainStateError::Validation(
                ValidationError::InvalidTransaction("coinbase missing exchange funding"),
            ));
        }
    }

    let foundation_amount = foundation_fund_amount(height, &params.funding);
    if foundation_amount > 0 {
        let foundation_script =
            address_to_script_pubkey(params.funding.foundation_address, network).map_err(|_| {
                ChainStateError::Validation(ValidationError::InvalidTransaction(
                    "invalid foundation address",
                ))
            })?;
        let found = tx
            .vout
            .iter()
            .any(|out| out.script_pubkey == foundation_script && out.value == foundation_amount);
        if !found {
            return Err(ChainStateError::Validation(
                ValidationError::InvalidTransaction("coinbase missing foundation funding"),
            ));
        }
    }

    if is_swap_pool_interval(height as i64, &params.swap_pool) {
        let swap_amount = swap_pool_amount(height as i64, &params.swap_pool);
        let swap_script =
            address_to_script_pubkey(params.swap_pool.address, network).map_err(|_| {
                ChainStateError::Validation(ValidationError::InvalidTransaction(
                    "invalid swap pool address",
                ))
            })?;
        let found = tx
            .vout
            .iter()
            .any(|out| out.script_pubkey == swap_script && out.value == swap_amount);
        if !found {
            return Err(ChainStateError::Validation(
                ValidationError::InvalidTransaction("coinbase missing swap pool funding"),
            ));
        }
    }

    Ok(())
}

fn ensure_fluxnode_collateral_mature(
    collateral_height: u32,
    height: i32,
) -> Result<(), ChainStateError> {
    if height < 0 {
        return Ok(());
    }
    let age = height
        .checked_sub(collateral_height as i32)
        .unwrap_or_default();
    if age < FLUXNODE_MIN_CONFIRMATION_DETERMINISTIC {
        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
            "fluxnode collateral too new",
        )));
    }
    Ok(())
}

fn fluxnode_confirm_expiration_count(height: i32, consensus: &ConsensusParams) -> u32 {
    let upgrades = &consensus.upgrades;
    let count = if network_upgrade_active(height, upgrades, UpgradeIndex::Pon) {
        FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V4
    } else if network_upgrade_active(height, upgrades, UpgradeIndex::Halving) {
        FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V3
    } else if network_upgrade_active(height, upgrades, UpgradeIndex::Flux) {
        FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V2
    } else {
        FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V1
    };
    u32::try_from(count).unwrap_or_default()
}

fn validate_fluxnode_collateral_script(
    script_pubkey: &[u8],
    _height: i32,
    collateral_pubkey: &[u8],
    sig_time: u32,
    params: &ChainParams,
    redeem_script: Option<&[u8]>,
) -> Result<(), ChainStateError> {
    if let Some(script_hash) = script_p2sh_hash(script_pubkey) {
        if let Some(redeem_script) = redeem_script {
            if hash160(redeem_script) != script_hash {
                return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                    "fluxnode p2sh redeem script hash mismatch",
                )));
            }
            return Ok(());
        }

        let key =
            select_timed_pubkey(params.fluxnode.p2sh_public_keys, sig_time).ok_or_else(|| {
                ChainStateError::Validation(ValidationError::Fluxnode(
                    "fluxnode p2sh signing key missing",
                ))
            })?;
        let expected = hex_to_bytes(key.key).ok_or_else(|| {
            ChainStateError::Validation(ValidationError::Fluxnode(
                "invalid fluxnode p2sh signing pubkey",
            ))
        })?;
        if collateral_pubkey != expected.as_slice() {
            return Err(ChainStateError::Validation(ValidationError::Fluxnode(
                "fluxnode p2sh collateral pubkey mismatch",
            )));
        }
        return Ok(());
    }

    let Some(pubkey_hash) = script_p2pkh_hash(script_pubkey) else {
        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
            "fluxnode collateral script unsupported",
        )));
    };
    if collateral_pubkey.is_empty() {
        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
            "fluxnode collateral pubkey missing",
        )));
    }
    if hash160(collateral_pubkey) != pubkey_hash {
        return Err(ChainStateError::Validation(ValidationError::Fluxnode(
            "fluxnode collateral pubkey does not match script",
        )));
    }
    Ok(())
}

fn script_p2pkh_hash(script: &[u8]) -> Option<[u8; 20]> {
    if script.len() != 25 {
        return None;
    }
    if script[0] != 0x76
        || script[1] != 0xa9
        || script[2] != 0x14
        || script[23] != 0x88
        || script[24] != 0xac
    {
        return None;
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&script[3..23]);
    Some(out)
}

fn script_p2sh_hash(script: &[u8]) -> Option<[u8; 20]> {
    if script.len() != 23 {
        return None;
    }
    if script[0] != 0xa9 || script[1] != 0x14 || script[22] != 0x87 {
        return None;
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&script[2..22]);
    Some(out)
}

fn select_timed_pubkey(
    keys: &[fluxd_consensus::TimedPublicKey],
    time: u32,
) -> Option<fluxd_consensus::TimedPublicKey> {
    let mut current = *keys.first()?;
    for key in keys {
        if key.valid_from <= time && key.valid_from >= current.valid_from {
            current = *key;
        }
    }
    Some(current)
}

fn hex_to_bytes(input: &str) -> Option<Vec<u8>> {
    let mut hex = input.trim();
    if let Some(stripped) = hex.strip_prefix("0x").or_else(|| hex.strip_prefix("0X")) {
        hex = stripped;
    }
    if hex.len() % 2 == 1 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut iter = hex.as_bytes().iter().copied();
    while let (Some(high), Some(low)) = (iter.next(), iter.next()) {
        let high = (high as char).to_digit(16)? as u8;
        let low = (low as char).to_digit(16)? as u8;
        bytes.push(high << 4 | low);
    }
    Some(bytes)
}

fn parse_multisig_redeem_script(script: &[u8]) -> Option<Vec<Vec<u8>>> {
    const OP_1: u8 = 0x51;
    const OP_16: u8 = 0x60;
    const OP_CHECKMULTISIG: u8 = 0xae;
    const OP_PUSHDATA1: u8 = 0x4c;
    const OP_PUSHDATA2: u8 = 0x4d;

    if script.len() < 1 + 1 + 1 {
        return None;
    }
    let mut cursor = 0usize;
    let opcode = *script.get(cursor)?;
    cursor += 1;
    if !(OP_1..=OP_16).contains(&opcode) {
        return None;
    }
    let required = opcode - OP_1 + 1;

    let mut pubkeys: Vec<Vec<u8>> = Vec::new();
    while cursor < script.len() {
        let op = *script.get(cursor)?;
        if (OP_1..=OP_16).contains(&op) {
            break;
        }
        cursor += 1;
        let len = if op <= 75 {
            op as usize
        } else if op == OP_PUSHDATA1 {
            let len = *script.get(cursor)? as usize;
            cursor += 1;
            len
        } else if op == OP_PUSHDATA2 {
            let lo = *script.get(cursor)? as u16;
            let hi = *script.get(cursor + 1)? as u16;
            cursor += 2;
            u16::from_le_bytes([lo as u8, hi as u8]) as usize
        } else {
            return None;
        };
        if cursor + len > script.len() {
            return None;
        }
        let data = &script[cursor..cursor + len];
        cursor += len;
        if matches!(data.len(), 33 | 65) {
            pubkeys.push(data.to_vec());
        }
    }

    let total_opcode = *script.get(cursor)?;
    cursor += 1;
    if !(OP_1..=OP_16).contains(&total_opcode) {
        return None;
    }
    let total = total_opcode - OP_1 + 1;

    if cursor >= script.len() || script[cursor] != OP_CHECKMULTISIG {
        return None;
    }
    cursor += 1;
    if cursor != script.len() {
        return None;
    }
    if total as usize != pubkeys.len() || required > total {
        return None;
    }
    Some(pubkeys)
}

fn fluxnode_start_operator_pubkey(tx: &Transaction) -> Option<(OutPoint, Vec<u8>)> {
    let fluxnode = tx.fluxnode.as_ref()?;
    match fluxnode {
        FluxnodeTx::V5(FluxnodeTxV5::Start(start)) => {
            Some((start.collateral.clone(), start.pubkey.clone()))
        }
        FluxnodeTx::V6(FluxnodeTxV6::Start(start)) => match &start.variant {
            FluxnodeStartVariantV6::Normal {
                collateral, pubkey, ..
            } => Some((collateral.clone(), pubkey.clone())),
            FluxnodeStartVariantV6::P2sh {
                collateral, pubkey, ..
            } => Some((collateral.clone(), pubkey.clone())),
        },
        _ => None,
    }
}

fn fluxnode_confirm_message(confirm: &FluxnodeConfirmTx) -> Vec<u8> {
    let hash_hex = hash256_to_hex(&confirm.collateral.hash);
    let prefix = &hash_hex[..10];
    let outpoint = format!("COutPoint({prefix}, {})", confirm.collateral.index);
    let mut msg = String::new();
    msg.push_str(&outpoint);
    msg.push_str(&confirm.collateral.index.to_string());
    msg.push_str(&confirm.update_type.to_string());
    msg.push_str(&confirm.sig_time.to_string());
    msg.into_bytes()
}

fn fluxnode_benchmark_message(confirm: &FluxnodeConfirmTx) -> Vec<u8> {
    let mut msg = Vec::with_capacity(confirm.sig.len() + 32 + confirm.ip.len());
    msg.extend_from_slice(&confirm.sig);
    msg.extend_from_slice(confirm.benchmark_tier.to_string().as_bytes());
    msg.extend_from_slice(confirm.benchmark_sig_time.to_string().as_bytes());
    msg.extend_from_slice(confirm.ip.as_bytes());
    msg
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxd_consensus::params::{chain_params, Network};
    use fluxd_consensus::rewards::min_dev_fund_amount;
    use fluxd_consensus::upgrades::UpgradeIndex;
    use fluxd_consensus::TimedPublicKey;
    use fluxd_primitives::block::{Block, BlockHeader, CURRENT_VERSION};
    use fluxd_primitives::outpoint::OutPoint;
    use fluxd_primitives::transaction::{
        FluxnodeConfirmTx, FluxnodeStartV5, FluxnodeTx, FluxnodeTxV5, Transaction, TxIn, TxOut,
        FLUXNODE_TX_VERSION, SAPLING_VERSION_GROUP_ID,
    };
    use fluxd_script::message::signed_message_hash;
    use fluxd_storage::memory::MemoryStore;
    use fluxd_storage::WriteBatch;
    use secp256k1::ecdsa::RecoverableSignature;
    use secp256k1::{Message, Secp256k1, SecretKey};
    use std::sync::Arc;

    fn make_tx(vin: Vec<TxIn>, vout: Vec<TxOut>) -> Transaction {
        Transaction {
            f_overwintered: false,
            version: 1,
            version_group_id: 0,
            vin,
            vout,
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: None,
        }
    }

    fn make_test_secret_key(last_byte: u8) -> SecretKey {
        let mut bytes = [0u8; 32];
        bytes[31] = last_byte;
        SecretKey::from_slice(&bytes).expect("secret key")
    }

    fn encode_compact(sig: &RecoverableSignature, compressed: bool) -> [u8; 65] {
        let (rec_id, bytes) = sig.serialize_compact();
        let mut out = [0u8; 65];
        let header = 27u8 + (rec_id.to_i32() as u8) + if compressed { 4 } else { 0 };
        out[0] = header;
        out[1..].copy_from_slice(&bytes);
        out
    }

    fn p2pkh_script_for_pubkey(pubkey: &[u8]) -> Vec<u8> {
        let hash = hash160(pubkey);
        let mut script = Vec::with_capacity(25);
        script.extend_from_slice(&[0x76, 0xa9, 0x14]);
        script.extend_from_slice(&hash);
        script.extend_from_slice(&[0x88, 0xac]);
        script
    }

    #[test]
    fn coinbase_funding_does_not_require_dev_fund_pre_pon() {
        let params = chain_params(Network::Mainnet);
        let height = params.consensus.upgrades[UpgradeIndex::Pon.as_usize()].activation_height - 1;
        let tx = make_tx(
            vec![],
            vec![TxOut {
                value: 0,
                script_pubkey: vec![0x51],
            }],
        );
        check_coinbase_funding(&tx, height, &params).expect("pre-pon coinbase funding ok");
    }

    #[test]
    fn coinbase_funding_requires_dev_fund_at_pon_activation() {
        let params = chain_params(Network::Mainnet);
        let height = params.consensus.upgrades[UpgradeIndex::Pon.as_usize()].activation_height;
        let required = min_dev_fund_amount(height, &params.consensus);
        assert!(required > 0);

        let tx = make_tx(
            vec![],
            vec![TxOut {
                value: 0,
                script_pubkey: vec![0x51],
            }],
        );
        let err = check_coinbase_funding(&tx, height, &params).expect_err("missing dev fund");
        match err {
            ChainStateError::Validation(ValidationError::InvalidTransaction(message)) => {
                assert_eq!(message, "coinbase missing dev fund payment");
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let dev_script = fluxd_primitives::address_to_script_pubkey(
            params.funding.dev_fund_address,
            params.network,
        )
        .expect("dev fund script");
        let tx = make_tx(
            vec![],
            vec![
                TxOut {
                    value: required,
                    script_pubkey: dev_script,
                },
                TxOut {
                    value: 0,
                    script_pubkey: vec![0x51],
                },
            ],
        );
        check_coinbase_funding(&tx, height, &params).expect("dev fund coinbase funding ok");
    }

    #[test]
    fn coinbase_funding_requires_exchange_payment_at_exchange_height() {
        let params = chain_params(Network::Mainnet);
        let height = params.funding.exchange_height as i32;

        let tx = make_tx(
            vec![],
            vec![TxOut {
                value: 0,
                script_pubkey: vec![0x51],
            }],
        );
        let err = check_coinbase_funding(&tx, height, &params).expect_err("missing exchange fund");
        match err {
            ChainStateError::Validation(ValidationError::InvalidTransaction(message)) => {
                assert_eq!(message, "coinbase missing exchange funding");
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let exchange_script = fluxd_primitives::address_to_script_pubkey(
            params.funding.exchange_address,
            params.network,
        )
        .expect("exchange script");
        let tx = make_tx(
            vec![],
            vec![
                TxOut {
                    value: params.funding.exchange_amount,
                    script_pubkey: exchange_script,
                },
                TxOut {
                    value: 0,
                    script_pubkey: vec![0x51],
                },
            ],
        );
        check_coinbase_funding(&tx, height, &params).expect("exchange funding ok");
    }

    #[test]
    fn coinbase_funding_requires_foundation_payment_at_foundation_height() {
        let params = chain_params(Network::Mainnet);
        let height = params.funding.foundation_height as i32;

        let tx = make_tx(
            vec![],
            vec![TxOut {
                value: 0,
                script_pubkey: vec![0x51],
            }],
        );
        let err =
            check_coinbase_funding(&tx, height, &params).expect_err("missing foundation fund");
        match err {
            ChainStateError::Validation(ValidationError::InvalidTransaction(message)) => {
                assert_eq!(message, "coinbase missing foundation funding");
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let foundation_script = fluxd_primitives::address_to_script_pubkey(
            params.funding.foundation_address,
            params.network,
        )
        .expect("foundation script");
        let tx = make_tx(
            vec![],
            vec![
                TxOut {
                    value: params.funding.foundation_amount,
                    script_pubkey: foundation_script,
                },
                TxOut {
                    value: 0,
                    script_pubkey: vec![0x51],
                },
            ],
        );
        check_coinbase_funding(&tx, height, &params).expect("foundation funding ok");
    }

    #[test]
    fn coinbase_funding_requires_swap_pool_payment_at_swap_pool_start_height() {
        let params = chain_params(Network::Mainnet);
        let height = params.swap_pool.start_height as i32;

        let tx = make_tx(
            vec![],
            vec![TxOut {
                value: 0,
                script_pubkey: vec![0x51],
            }],
        );
        let err = check_coinbase_funding(&tx, height, &params).expect_err("missing swap pool");
        match err {
            ChainStateError::Validation(ValidationError::InvalidTransaction(message)) => {
                assert_eq!(message, "coinbase missing swap pool funding");
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let swap_script =
            fluxd_primitives::address_to_script_pubkey(params.swap_pool.address, params.network)
                .expect("swap pool script");
        let tx = make_tx(
            vec![],
            vec![
                TxOut {
                    value: params.swap_pool.amount,
                    script_pubkey: swap_script,
                },
                TxOut {
                    value: 0,
                    script_pubkey: vec![0x51],
                },
            ],
        );
        check_coinbase_funding(&tx, height, &params).expect("swap pool funding ok");
    }

    #[test]
    fn coinbase_maturity_rejects_premature_spend() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let mut params = chain_params(Network::Regtest);
        let now = current_time_secs() as u32;

        let header0 = BlockHeader {
            version: CURRENT_VERSION,
            prev_block: [0u8; 32],
            merkle_root: [0u8; 32],
            final_sapling_root: [0u8; 32],
            time: now,
            bits: block_bits_from_params(&params.consensus),
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint::null(),
            block_sig: Vec::new(),
        };
        let hash0 = header0.hash();
        params.consensus.hash_genesis_block = hash0;
        params.consensus.checkpoints = vec![fluxd_consensus::params::Checkpoint {
            height: 0,
            hash: hash0,
        }];

        let header1 = BlockHeader {
            version: CURRENT_VERSION,
            prev_block: hash0,
            merkle_root: [0u8; 32],
            final_sapling_root: [0u8; 32],
            time: now + 1,
            bits: header0.bits,
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint::null(),
            block_sig: Vec::new(),
        };

        let mut header_batch = WriteBatch::new();
        chainstate
            .insert_headers_batch_with_pow(
                &[header0.clone(), header1.clone()],
                &params.consensus,
                &mut header_batch,
                false,
            )
            .expect("insert headers");
        chainstate
            .commit_batch(header_batch)
            .expect("commit headers");

        let coinbase0 = make_tx(
            vec![TxIn {
                prevout: OutPoint::null(),
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vec![TxOut {
                value: 50,
                script_pubkey: vec![0x51],
            }],
        );
        let block0 = Block {
            header: header0,
            transactions: vec![coinbase0.clone()],
        };
        let flags = ValidationFlags::default();
        let batch = chainstate
            .connect_block(&block0, 0, &params, &flags, true, None)
            .expect("connect block 0");
        chainstate.commit_batch(batch).expect("commit block 0");

        let coinbase0_txid = coinbase0.txid().expect("coinbase txid");
        let spend_tx = make_tx(
            vec![TxIn {
                prevout: OutPoint {
                    hash: coinbase0_txid,
                    index: 0,
                },
                script_sig: Vec::new(),
                sequence: 0,
            }],
            vec![TxOut {
                value: 50,
                script_pubkey: vec![0x52],
            }],
        );
        let coinbase1 = make_tx(
            vec![TxIn {
                prevout: OutPoint::null(),
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vec![TxOut {
                value: 0,
                script_pubkey: vec![0x51],
            }],
        );
        let block1 = Block {
            header: header1,
            transactions: vec![coinbase1, spend_tx],
        };

        let err = chainstate
            .connect_block(&block1, 1, &params, &flags, true, None)
            .expect_err("premature spend rejected");
        match err {
            ChainStateError::Validation(ValidationError::InvalidTransaction(message)) => {
                assert_eq!(message, "premature spend of coinbase");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    fn test_hash(height: u8) -> Hash256 {
        [height; 32]
    }

    fn make_header_entry(prev_hash: Hash256, height: i32, time: u32, bits: u32) -> HeaderEntry {
        HeaderEntry {
            prev_hash,
            height,
            time,
            bits,
            chainwork: [0u8; 32],
            status: status_with_header(0),
        }
    }

    #[test]
    fn pon_expected_bits_uses_start_limit_until_window_complete() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let mut params = chain_params(Network::Mainnet).consensus;
        let activation_height = 100;
        params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height = activation_height;

        let start_bits = fluxd_pow::difficulty::target_to_compact(&params.pon_start_limit);
        let prev_hash = test_hash((activation_height - 1) as u8);
        let prev_entry = make_header_entry([0u8; 32], activation_height - 1, 1_000_000, 0x1e7fffff);
        let pending = HashMap::from([(prev_hash, prev_entry)]);

        let bits = expected_pon_bits(
            &chainstate,
            &prev_hash,
            activation_height,
            &params,
            Some(&pending),
        )
        .expect("expected bits");
        assert_eq!(bits, start_bits);

        let bits = expected_pon_bits(
            &chainstate,
            &prev_hash,
            activation_height + params.pon_difficulty_window as i32 - 1,
            &params,
            Some(&pending),
        )
        .expect("expected bits");
        assert_eq!(bits, start_bits);
    }

    #[test]
    fn pon_expected_bits_adjusts_harder_for_fast_blocks() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let mut params = chain_params(Network::Mainnet).consensus;
        let activation_height = 100;
        params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height = activation_height;

        let window = params.pon_difficulty_window as i32;
        let start_bits = fluxd_pow::difficulty::target_to_compact(&params.pon_start_limit);
        let target_timespan = (window as i64 - 1) * params.pon_target_spacing;

        let mut pending: HashMap<Hash256, HeaderEntry> = HashMap::new();
        let base_time = 1_000_000u32;

        for height in (activation_height - 1)..=(activation_height + window - 1) {
            let hash = test_hash(height as u8);
            let prev_hash = if height == activation_height - 1 {
                [0u8; 32]
            } else {
                test_hash((height - 1) as u8)
            };
            let time = if height < activation_height {
                base_time
            } else {
                let offset = height - activation_height;
                base_time + (offset as u32) * (params.pon_target_spacing as u32 / 2)
            };
            let bits = if height < activation_height {
                0x1e7fffff
            } else {
                start_bits
            };
            pending.insert(hash, make_header_entry(prev_hash, height, time, bits));
        }

        let prev_hash = test_hash((activation_height + window - 1) as u8);
        let adjusted_bits = expected_pon_bits(
            &chainstate,
            &prev_hash,
            activation_height + window,
            &params,
            Some(&pending),
        )
        .expect("expected bits");

        let start_target = fluxd_pow::difficulty::compact_to_u256(start_bits).expect("start");
        let adjusted_target = fluxd_pow::difficulty::compact_to_u256(adjusted_bits).expect("adj");
        assert!(target_timespan > 0);
        assert!(adjusted_target < start_target);
    }

    #[test]
    fn pon_expected_bits_adjusts_easier_for_slow_blocks() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let mut params = chain_params(Network::Mainnet).consensus;
        let activation_height = 100;
        params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height = activation_height;

        let window = params.pon_difficulty_window as i32;
        let start_bits = fluxd_pow::difficulty::target_to_compact(&params.pon_start_limit);
        let max_target = primitive_types::U256::from_little_endian(&params.pon_limit);

        let mut pending: HashMap<Hash256, HeaderEntry> = HashMap::new();
        let base_time = 1_000_000u32;

        for height in (activation_height - 1)..=(activation_height + window - 1) {
            let hash = test_hash(height as u8);
            let prev_hash = if height == activation_height - 1 {
                [0u8; 32]
            } else {
                test_hash((height - 1) as u8)
            };
            let time = if height < activation_height {
                base_time
            } else {
                let offset = height - activation_height;
                base_time + (offset as u32) * (params.pon_target_spacing as u32 * 2)
            };
            let bits = if height < activation_height {
                0x1e7fffff
            } else {
                start_bits
            };
            pending.insert(hash, make_header_entry(prev_hash, height, time, bits));
        }

        let prev_hash = test_hash((activation_height + window - 1) as u8);
        let adjusted_bits = expected_pon_bits(
            &chainstate,
            &prev_hash,
            activation_height + window,
            &params,
            Some(&pending),
        )
        .expect("expected bits");

        let start_target = fluxd_pow::difficulty::compact_to_u256(start_bits).expect("start");
        let adjusted_target = fluxd_pow::difficulty::compact_to_u256(adjusted_bits).expect("adj");
        assert!(adjusted_target > start_target);
        assert!(adjusted_target <= max_target);
    }

    #[test]
    fn pon_expected_bits_stabilizes_under_perfect_timing() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let mut params = chain_params(Network::Mainnet).consensus;
        let activation_height = 100;
        params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height = activation_height;

        let last_height = activation_height + 120;
        let base_time = 1_000_000i64;
        let mut pending: HashMap<Hash256, HeaderEntry> = HashMap::new();

        for height in 0..activation_height {
            let hash = test_hash(height as u8);
            let prev_hash = if height == 0 {
                [0u8; 32]
            } else {
                test_hash((height - 1) as u8)
            };
            let time = base_time + (height as i64) * params.pow_target_spacing;
            pending.insert(
                hash,
                make_header_entry(prev_hash, height, time as u32, 0x1e7fffff),
            );
        }

        let pon_limit_bits = fluxd_pow::difficulty::target_to_compact(&params.pon_limit);
        for height in activation_height..=last_height {
            let hash = test_hash(height as u8);
            let prev_hash = test_hash((height - 1) as u8);
            let time = base_time
                + (activation_height as i64 - 1) * params.pow_target_spacing
                + (height as i64 - activation_height as i64 + 1) * params.pon_target_spacing;

            let bits = if height == activation_height {
                pon_limit_bits
            } else {
                expected_pon_bits(&chainstate, &prev_hash, height, &params, Some(&pending))
                    .expect("expected bits")
            };

            pending.insert(
                hash,
                make_header_entry(prev_hash, height, time as u32, bits),
            );
        }

        let stable_bits = pending
            .get(&test_hash(last_height as u8))
            .expect("stable entry")
            .bits;
        let previous_bits = pending
            .get(&test_hash((last_height - 10) as u8))
            .expect("previous entry")
            .bits;

        let stable_target = fluxd_pow::difficulty::compact_to_u256(stable_bits).expect("stable");
        let previous_target =
            fluxd_pow::difficulty::compact_to_u256(previous_bits).expect("previous");

        let diff = if stable_target > previous_target {
            stable_target - previous_target
        } else {
            previous_target - stable_target
        };
        let max_diff = previous_target / primitive_types::U256::from(100u64);
        assert!(diff <= max_diff);
    }

    #[test]
    fn pow_chainwork_accumulates_block_proof() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let mut consensus = chain_params(Network::Regtest).consensus;
        let pow_bits = fluxd_pow::difficulty::target_to_compact(&consensus.pow_limit);

        let header0 = BlockHeader {
            version: CURRENT_VERSION,
            prev_block: [0u8; 32],
            merkle_root: [0u8; 32],
            final_sapling_root: [0u8; 32],
            time: 1_000_000,
            bits: pow_bits,
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint::null(),
            block_sig: Vec::new(),
        };
        let hash0 = header0.hash();
        consensus.hash_genesis_block = hash0;
        consensus.checkpoints = vec![fluxd_consensus::params::Checkpoint {
            height: 0,
            hash: hash0,
        }];

        let header1 = BlockHeader {
            version: CURRENT_VERSION,
            prev_block: hash0,
            merkle_root: [0u8; 32],
            final_sapling_root: [0u8; 32],
            time: 1_000_120,
            bits: pow_bits,
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint::null(),
            block_sig: Vec::new(),
        };
        let hash1 = header1.hash();

        let mut batch = WriteBatch::new();
        chainstate
            .insert_headers_batch_with_pow(&[header0, header1], &consensus, &mut batch, false)
            .expect("insert headers");
        chainstate.commit_batch(batch).expect("commit headers");

        let entry0 = chainstate
            .header_entry(&hash0)
            .expect("entry0")
            .expect("entry0");
        let entry1 = chainstate
            .header_entry(&hash1)
            .expect("entry1")
            .expect("entry1");

        let work = fluxd_pow::difficulty::block_proof(pow_bits).expect("block proof");
        let expected_0 = work;
        let expected_1 = work + work;

        assert_eq!(entry0.chainwork_value(), expected_0);
        assert_eq!(entry1.chainwork_value(), expected_1);
    }

    #[test]
    fn pon_chainwork_uses_fixed_work() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let mut consensus = chain_params(Network::Regtest).consensus;
        consensus.upgrades[UpgradeIndex::Pon.as_usize()].activation_height = 1;

        let pow_bits = fluxd_pow::difficulty::target_to_compact(&consensus.pow_limit);
        let pon_bits = fluxd_pow::difficulty::target_to_compact(&consensus.pon_start_limit);

        let header0 = BlockHeader {
            version: CURRENT_VERSION,
            prev_block: [0u8; 32],
            merkle_root: [0u8; 32],
            final_sapling_root: [0u8; 32],
            time: 1_000_000,
            bits: pow_bits,
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint::null(),
            block_sig: Vec::new(),
        };
        let hash0 = header0.hash();
        consensus.hash_genesis_block = hash0;
        consensus.checkpoints = vec![fluxd_consensus::params::Checkpoint {
            height: 0,
            hash: hash0,
        }];

        let header1 = BlockHeader {
            version: fluxd_primitives::block::PON_VERSION,
            prev_block: hash0,
            merkle_root: [0u8; 32],
            final_sapling_root: [0u8; 32],
            time: 1_000_001,
            bits: pon_bits,
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint {
                hash: [0x11; 32],
                index: 1,
            },
            block_sig: vec![0x01],
        };
        let hash1 = header1.hash();

        let mut batch = WriteBatch::new();
        chainstate
            .insert_headers_batch_with_pow(&[header0, header1], &consensus, &mut batch, false)
            .expect("insert headers");
        chainstate.commit_batch(batch).expect("commit headers");

        let entry0 = chainstate
            .header_entry(&hash0)
            .expect("entry0")
            .expect("entry0");
        let entry1 = chainstate
            .header_entry(&hash1)
            .expect("entry1")
            .expect("entry1");

        let pow_work = fluxd_pow::difficulty::block_proof(pow_bits).expect("block proof");
        let pon_work = primitive_types::U256::from(1u64 << 40);

        assert_eq!(entry0.chainwork_value(), pow_work);
        assert_eq!(entry1.chainwork_value(), pow_work + pon_work);
    }

    #[test]
    fn disconnect_restores_utxo_set_for_intrablock_spends() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let seed_script = vec![0x51];
        let seed_outpoint = OutPoint {
            hash: [0x11; 32],
            index: 0,
        };
        let seed_entry = UtxoEntry {
            value: 50,
            script_pubkey: seed_script.clone(),
            height: 0,
            is_coinbase: false,
        };
        let mut seed_batch = WriteBatch::new();
        chainstate
            .utxos
            .put(&mut seed_batch, &seed_outpoint, &seed_entry);
        chainstate
            .address_index
            .insert(&mut seed_batch, &seed_entry.script_pubkey, &seed_outpoint);
        chainstate.commit_batch(seed_batch).expect("seed utxo");
        let seed_stats = chainstate.utxo_stats_or_compute().expect("seed stats");
        assert_eq!(
            seed_stats,
            UtxoStats {
                txouts: 1,
                total_amount: seed_entry.value
            }
        );

        let mut params = chain_params(Network::Regtest);
        let header = BlockHeader {
            version: CURRENT_VERSION,
            prev_block: [0u8; 32],
            merkle_root: [0u8; 32],
            final_sapling_root: [0u8; 32],
            time: current_time_secs() as u32,
            bits: block_bits_from_params(&params.consensus),
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint::null(),
            block_sig: Vec::new(),
        };
        let block_hash = header.hash();
        params.consensus.hash_genesis_block = block_hash;
        params.consensus.checkpoints = vec![fluxd_consensus::params::Checkpoint {
            height: 0,
            hash: block_hash,
        }];

        let mut header_batch = WriteBatch::new();
        chainstate
            .insert_headers_batch_with_pow(
                std::slice::from_ref(&header),
                &params.consensus,
                &mut header_batch,
                false,
            )
            .expect("insert header");
        chainstate
            .commit_batch(header_batch)
            .expect("commit header");

        let coinbase = make_tx(
            vec![TxIn {
                prevout: OutPoint::null(),
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vec![TxOut {
                value: 0,
                script_pubkey: vec![0x51],
            }],
        );
        let tx1 = make_tx(
            vec![TxIn {
                prevout: seed_outpoint.clone(),
                script_sig: Vec::new(),
                sequence: 0,
            }],
            vec![TxOut {
                value: seed_entry.value,
                script_pubkey: vec![0x52],
            }],
        );
        let tx1id = tx1.txid().expect("txid1");
        let outpoint1 = OutPoint {
            hash: tx1id,
            index: 0,
        };

        let tx2 = make_tx(
            vec![TxIn {
                prevout: outpoint1.clone(),
                script_sig: Vec::new(),
                sequence: 0,
            }],
            vec![TxOut {
                value: seed_entry.value,
                script_pubkey: vec![0x53],
            }],
        );
        let tx2id = tx2.txid().expect("txid2");
        let outpoint2 = OutPoint {
            hash: tx2id,
            index: 0,
        };

        let block = Block {
            header,
            transactions: vec![coinbase, tx1.clone(), tx2.clone()],
        };

        let flags = ValidationFlags::default();
        let batch = chainstate
            .connect_block(&block, 0, &params, &flags, true, None)
            .expect("connect block");
        chainstate.commit_batch(batch).expect("commit connect");
        let connected_stats = chainstate.utxo_stats().expect("utxo stats").expect("stats");
        assert_eq!(
            connected_stats,
            UtxoStats {
                txouts: 2,
                total_amount: seed_entry.value
            }
        );

        assert!(!chainstate
            .utxo_exists(&seed_outpoint)
            .expect("seed utxo exists"));
        assert!(!chainstate.utxo_exists(&outpoint1).expect("tx1 utxo exists"));
        assert!(chainstate.utxo_exists(&outpoint2).expect("tx2 utxo exists"));

        let batch = chainstate
            .disconnect_block(&block_hash)
            .expect("disconnect");
        chainstate.commit_batch(batch).expect("commit disconnect");
        let disconnected_stats = chainstate.utxo_stats().expect("utxo stats").expect("stats");
        assert_eq!(
            disconnected_stats,
            UtxoStats {
                txouts: 1,
                total_amount: seed_entry.value
            }
        );

        assert!(chainstate
            .utxo_exists(&seed_outpoint)
            .expect("seed utxo restored"));
        assert!(!chainstate
            .utxo_exists(&outpoint1)
            .expect("tx1 output removed"));
        assert!(!chainstate
            .utxo_exists(&outpoint2)
            .expect("tx2 output removed"));

        assert!(chainstate
            .tx_location(&tx1id)
            .expect("tx location query")
            .is_none());
    }

    #[test]
    fn insert_headers_persists_header_bytes() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let mut params = chain_params(Network::Regtest);
        let header = BlockHeader {
            version: CURRENT_VERSION,
            prev_block: [0u8; 32],
            merkle_root: [0u8; 32],
            final_sapling_root: [0u8; 32],
            time: current_time_secs() as u32,
            bits: block_bits_from_params(&params.consensus),
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint::null(),
            block_sig: Vec::new(),
        };
        let hash = header.hash();
        params.consensus.hash_genesis_block = hash;
        params.consensus.checkpoints =
            vec![fluxd_consensus::params::Checkpoint { height: 0, hash }];

        let mut batch = WriteBatch::new();
        chainstate
            .insert_headers_batch_with_pow(
                std::slice::from_ref(&header),
                &params.consensus,
                &mut batch,
                false,
            )
            .expect("insert header");
        chainstate.commit_batch(batch).expect("commit header");

        let stored = chainstate
            .block_header_bytes(&hash)
            .expect("load header bytes")
            .expect("header bytes missing");
        assert_eq!(stored, header.consensus_encode());
    }

    #[test]
    fn connect_updates_sapling_value_pool() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let seed_script = vec![0x51];
        let seed_outpoint = OutPoint {
            hash: [0x11; 32],
            index: 0,
        };
        let seed_entry = UtxoEntry {
            value: 50,
            script_pubkey: seed_script.clone(),
            height: 0,
            is_coinbase: false,
        };
        let mut seed_batch = WriteBatch::new();
        chainstate
            .utxos
            .put(&mut seed_batch, &seed_outpoint, &seed_entry);
        chainstate
            .address_index
            .insert(&mut seed_batch, &seed_entry.script_pubkey, &seed_outpoint);
        chainstate.commit_batch(seed_batch).expect("seed utxo");

        let mut params = chain_params(Network::Regtest);
        let header = BlockHeader {
            version: CURRENT_VERSION,
            prev_block: [0u8; 32],
            merkle_root: [0u8; 32],
            final_sapling_root: [0u8; 32],
            time: current_time_secs() as u32,
            bits: block_bits_from_params(&params.consensus),
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint::null(),
            block_sig: Vec::new(),
        };
        let block_hash = header.hash();
        params.consensus.hash_genesis_block = block_hash;
        params.consensus.checkpoints = vec![fluxd_consensus::params::Checkpoint {
            height: 0,
            hash: block_hash,
        }];

        let mut header_batch = WriteBatch::new();
        chainstate
            .insert_headers_batch_with_pow(
                std::slice::from_ref(&header),
                &params.consensus,
                &mut header_batch,
                false,
            )
            .expect("insert header");
        chainstate
            .commit_batch(header_batch)
            .expect("commit header");

        let coinbase = make_tx(
            vec![TxIn {
                prevout: OutPoint::null(),
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vec![TxOut {
                value: 0,
                script_pubkey: vec![0x51],
            }],
        );

        let sapling_tx = Transaction {
            f_overwintered: true,
            version: 4,
            version_group_id: SAPLING_VERSION_GROUP_ID,
            vin: vec![TxIn {
                prevout: seed_outpoint.clone(),
                script_sig: Vec::new(),
                sequence: 0,
            }],
            vout: vec![TxOut {
                value: 40,
                script_pubkey: vec![0x52],
            }],
            lock_time: 0,
            expiry_height: 0,
            value_balance: -10,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: None,
        };

        let block = Block {
            header,
            transactions: vec![coinbase, sapling_tx],
        };

        let flags = ValidationFlags::default();
        let batch = chainstate
            .connect_block(&block, 0, &params, &flags, true, None)
            .expect("connect block");
        chainstate.commit_batch(batch).expect("commit connect");

        let utxo_stats = chainstate.utxo_stats().expect("utxo stats").expect("stats");
        assert_eq!(
            utxo_stats,
            UtxoStats {
                txouts: 2,
                total_amount: 40
            }
        );
        let pools = chainstate
            .value_pools()
            .expect("value pools")
            .expect("pools");
        assert_eq!(
            pools,
            ValuePools {
                sprout: 0,
                sapling: 10
            }
        );

        let batch = chainstate
            .disconnect_block(&block_hash)
            .expect("disconnect");
        chainstate.commit_batch(batch).expect("commit disconnect");

        let utxo_stats = chainstate.utxo_stats().expect("utxo stats").expect("stats");
        assert_eq!(
            utxo_stats,
            UtxoStats {
                txouts: 1,
                total_amount: 50
            }
        );
        let pools = chainstate
            .value_pools()
            .expect("value pools")
            .expect("pools");
        assert_eq!(
            pools,
            ValuePools {
                sprout: 0,
                sapling: 0
            }
        );
    }

    #[test]
    fn fluxnode_start_validates_signature_and_collateral_script() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let collateral_secret = make_test_secret_key(1);
        let operator_secret = make_test_secret_key(2);
        let secp = Secp256k1::signing_only();
        let collateral_pubkey = secp256k1::PublicKey::from_secret_key(&secp, &collateral_secret);
        let operator_pubkey = secp256k1::PublicKey::from_secret_key(&secp, &operator_secret);

        let collateral_outpoint = OutPoint {
            hash: [0x11; 32],
            index: 0,
        };
        let entry = UtxoEntry {
            value: 1000,
            script_pubkey: p2pkh_script_for_pubkey(&collateral_pubkey.serialize()),
            height: 0,
            is_coinbase: false,
        };
        store
            .put(
                Column::Utxo,
                outpoint_key_bytes(&collateral_outpoint).as_bytes(),
                &entry.encode(),
            )
            .expect("store utxo");

        let mut tx = Transaction {
            f_overwintered: false,
            version: FLUXNODE_TX_VERSION,
            version_group_id: 0,
            vin: Vec::new(),
            vout: Vec::new(),
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: Some(FluxnodeTx::V5(FluxnodeTxV5::Start(FluxnodeStartV5 {
                collateral: collateral_outpoint.clone(),
                collateral_pubkey: collateral_pubkey.serialize().to_vec(),
                pubkey: operator_pubkey.serialize().to_vec(),
                sig_time: 0,
                sig: Vec::new(),
            }))),
        };

        let txid = tx.txid().expect("txid");
        let message = hash256_to_hex(&txid);
        let digest = signed_message_hash(message.as_bytes());
        let msg = Message::from_digest_slice(&digest).expect("msg");
        let sig = secp.sign_ecdsa_recoverable(&msg, &collateral_secret);
        let sig_bytes = encode_compact(&sig, true);
        if let Some(FluxnodeTx::V5(FluxnodeTxV5::Start(start))) = tx.fluxnode.as_mut() {
            start.sig = sig_bytes.to_vec();
        }

        let params = chain_params(Network::Regtest);
        chainstate
            .validate_fluxnode_tx(
                &tx,
                &txid,
                FLUXNODE_MIN_CONFIRMATION_DETERMINISTIC,
                &params,
                &HashMap::new(),
                &HashMap::new(),
            )
            .expect("start tx valid");
    }

    #[test]
    fn fluxnode_start_allows_restart_after_confirm_expiration() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let collateral_secret = make_test_secret_key(1);
        let operator_secret = make_test_secret_key(2);
        let secp = Secp256k1::signing_only();
        let collateral_pubkey = secp256k1::PublicKey::from_secret_key(&secp, &collateral_secret);
        let operator_pubkey = secp256k1::PublicKey::from_secret_key(&secp, &operator_secret);

        let collateral_outpoint = OutPoint {
            hash: [0x44; 32],
            index: 0,
        };
        let entry = UtxoEntry {
            value: 1000,
            script_pubkey: p2pkh_script_for_pubkey(&collateral_pubkey.serialize()),
            height: 0,
            is_coinbase: false,
        };
        store
            .put(
                Column::Utxo,
                outpoint_key_bytes(&collateral_outpoint).as_bytes(),
                &entry.encode(),
            )
            .expect("store utxo");

        let record = FluxnodeRecord {
            collateral: collateral_outpoint.clone(),
            tier: 0,
            start_height: 0,
            last_confirmed_height: 1,
            last_paid_height: 0,
            operator_pubkey: KeyId([0x11; 32]),
            collateral_pubkey: None,
            p2sh_script: None,
        };
        store
            .put(
                Column::Fluxnode,
                outpoint_key_bytes(&collateral_outpoint).as_bytes(),
                &record.encode(),
            )
            .expect("store record");

        let mut tx = Transaction {
            f_overwintered: false,
            version: FLUXNODE_TX_VERSION,
            version_group_id: 0,
            vin: Vec::new(),
            vout: Vec::new(),
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: Some(FluxnodeTx::V5(FluxnodeTxV5::Start(FluxnodeStartV5 {
                collateral: collateral_outpoint.clone(),
                collateral_pubkey: collateral_pubkey.serialize().to_vec(),
                pubkey: operator_pubkey.serialize().to_vec(),
                sig_time: 0,
                sig: Vec::new(),
            }))),
        };

        let txid = tx.txid().expect("txid");
        let message = hash256_to_hex(&txid);
        let digest = signed_message_hash(message.as_bytes());
        let msg = Message::from_digest_slice(&digest).expect("msg");
        let sig = secp.sign_ecdsa_recoverable(&msg, &collateral_secret);
        let sig_bytes = encode_compact(&sig, true);
        if let Some(FluxnodeTx::V5(FluxnodeTxV5::Start(start))) = tx.fluxnode.as_mut() {
            start.sig = sig_bytes.to_vec();
        }

        let params = chain_params(Network::Regtest);
        chainstate
            .validate_fluxnode_tx(
                &tx,
                &txid,
                FLUXNODE_MIN_CONFIRMATION_DETERMINISTIC,
                &params,
                &HashMap::new(),
                &HashMap::new(),
            )
            .expect("start tx valid after expiration");
    }

    #[test]
    fn fluxnode_start_rejects_restart_when_still_confirmed() {
        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let collateral_secret = make_test_secret_key(1);
        let operator_secret = make_test_secret_key(2);
        let secp = Secp256k1::signing_only();
        let collateral_pubkey = secp256k1::PublicKey::from_secret_key(&secp, &collateral_secret);
        let operator_pubkey = secp256k1::PublicKey::from_secret_key(&secp, &operator_secret);

        let collateral_outpoint = OutPoint {
            hash: [0x55; 32],
            index: 0,
        };
        let entry = UtxoEntry {
            value: 1000,
            script_pubkey: p2pkh_script_for_pubkey(&collateral_pubkey.serialize()),
            height: 0,
            is_coinbase: false,
        };
        store
            .put(
                Column::Utxo,
                outpoint_key_bytes(&collateral_outpoint).as_bytes(),
                &entry.encode(),
            )
            .expect("store utxo");

        let record = FluxnodeRecord {
            collateral: collateral_outpoint.clone(),
            tier: 0,
            start_height: 0,
            last_confirmed_height: 80,
            last_paid_height: 0,
            operator_pubkey: KeyId([0x11; 32]),
            collateral_pubkey: None,
            p2sh_script: None,
        };
        store
            .put(
                Column::Fluxnode,
                outpoint_key_bytes(&collateral_outpoint).as_bytes(),
                &record.encode(),
            )
            .expect("store record");

        let mut tx = Transaction {
            f_overwintered: false,
            version: FLUXNODE_TX_VERSION,
            version_group_id: 0,
            vin: Vec::new(),
            vout: Vec::new(),
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: Some(FluxnodeTx::V5(FluxnodeTxV5::Start(FluxnodeStartV5 {
                collateral: collateral_outpoint.clone(),
                collateral_pubkey: collateral_pubkey.serialize().to_vec(),
                pubkey: operator_pubkey.serialize().to_vec(),
                sig_time: 0,
                sig: Vec::new(),
            }))),
        };

        let txid = tx.txid().expect("txid");
        let message = hash256_to_hex(&txid);
        let digest = signed_message_hash(message.as_bytes());
        let msg = Message::from_digest_slice(&digest).expect("msg");
        let sig = secp.sign_ecdsa_recoverable(&msg, &collateral_secret);
        let sig_bytes = encode_compact(&sig, true);
        if let Some(FluxnodeTx::V5(FluxnodeTxV5::Start(start))) = tx.fluxnode.as_mut() {
            start.sig = sig_bytes.to_vec();
        }

        let params = chain_params(Network::Regtest);
        let err = chainstate
            .validate_fluxnode_tx(
                &tx,
                &txid,
                FLUXNODE_MIN_CONFIRMATION_DETERMINISTIC,
                &params,
                &HashMap::new(),
                &HashMap::new(),
            )
            .expect_err("start tx rejected while confirmed");
        match err {
            ChainStateError::Validation(ValidationError::Fluxnode(message)) => {
                assert_eq!(message, "fluxnode start collateral already registered");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn fluxnode_confirm_validates_operator_and_benchmark_signatures() {
        const BENCH_KEYS: [TimedPublicKey; 1] = [TimedPublicKey {
            key: "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
            valid_from: 0,
        }];

        let store = Arc::new(MemoryStore::new());
        let dir = tempfile::tempdir().expect("tempdir");
        let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let operator_secret = make_test_secret_key(2);
        let benchmark_secret = make_test_secret_key(1);
        let secp = Secp256k1::signing_only();
        let operator_pubkey = secp256k1::PublicKey::from_secret_key(&secp, &operator_secret);

        let collateral_outpoint = OutPoint {
            hash: [0x22; 32],
            index: 5,
        };

        let mut params = chain_params(Network::Regtest);
        params.fluxnode.benchmarking_public_keys = &BENCH_KEYS;

        let mut tx = Transaction {
            f_overwintered: false,
            version: FLUXNODE_TX_VERSION,
            version_group_id: 0,
            vin: Vec::new(),
            vout: Vec::new(),
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: Some(FluxnodeTx::V5(FluxnodeTxV5::Confirm(FluxnodeConfirmTx {
                collateral: collateral_outpoint.clone(),
                sig_time: 1,
                benchmark_tier: 1,
                benchmark_sig_time: 1,
                update_type: 0,
                ip: "127.0.0.1".to_string(),
                sig: Vec::new(),
                benchmark_sig: Vec::new(),
            }))),
        };

        let confirm = match tx.fluxnode.as_mut() {
            Some(FluxnodeTx::V5(FluxnodeTxV5::Confirm(confirm))) => confirm,
            _ => panic!("missing confirm"),
        };
        let operator_msg = fluxnode_confirm_message(confirm);
        let operator_digest = signed_message_hash(&operator_msg);
        let operator_msg = Message::from_digest_slice(&operator_digest).expect("msg");
        let sig = secp.sign_ecdsa_recoverable(&operator_msg, &operator_secret);
        confirm.sig = encode_compact(&sig, true).to_vec();

        let benchmark_msg = fluxnode_benchmark_message(confirm);
        let benchmark_digest = signed_message_hash(&benchmark_msg);
        let benchmark_msg = Message::from_digest_slice(&benchmark_digest).expect("msg");
        let sig = secp.sign_ecdsa_recoverable(&benchmark_msg, &benchmark_secret);
        confirm.benchmark_sig = encode_compact(&sig, true).to_vec();

        let txid = tx.txid().expect("txid");
        let mut operator_pubkeys = HashMap::new();
        operator_pubkeys.insert(collateral_outpoint, operator_pubkey.serialize().to_vec());
        chainstate
            .validate_fluxnode_tx(&tx, &txid, 100, &params, &HashMap::new(), &operator_pubkeys)
            .expect("confirm valid");
    }
}
