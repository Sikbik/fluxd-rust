use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::{validate_mempool_transaction, ValidationFlags};
use fluxd_consensus::constants::COINBASE_MATURITY;
use fluxd_consensus::money::{money_range, MAX_MONEY};
use fluxd_consensus::params::ChainParams;
use fluxd_consensus::upgrades::{current_epoch_branch_id, network_upgrade_active, UpgradeIndex};
use fluxd_consensus::Hash256;
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::transaction::Transaction;
use fluxd_script::interpreter::{verify_script, BLOCK_SCRIPT_VERIFY_FLAGS};
use fluxd_shielded::verify_transaction;

use crate::stats::hash256_to_hex;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MempoolErrorKind {
    AlreadyInMempool,
    ConflictingInput,
    MissingInput,
    MempoolFull,
    InvalidTransaction,
    InvalidScript,
    InvalidShielded,
    Internal,
}

#[derive(Clone, Debug)]
pub struct MempoolError {
    pub kind: MempoolErrorKind,
    pub message: String,
}

impl MempoolError {
    pub fn new(kind: MempoolErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for MempoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for MempoolError {}

pub struct MempoolEntry {
    pub txid: Hash256,
    pub tx: Transaction,
    pub raw: Vec<u8>,
    pub time: u64,
    pub height: i32,
    pub fee: i64,
    pub spent_outpoints: Vec<OutPoint>,
}

impl MempoolEntry {
    pub fn size(&self) -> usize {
        self.raw.len()
    }
}

#[derive(Default)]
pub struct Mempool {
    entries: HashMap<Hash256, MempoolEntry>,
    spent: HashMap<OutPoint, Hash256>,
    total_bytes: usize,
    max_bytes: usize,
    revision: u64,
}

impl Mempool {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            spent: HashMap::new(),
            total_bytes: 0,
            max_bytes,
            revision: 0,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn contains(&self, txid: &Hash256) -> bool {
        self.entries.contains_key(txid)
    }

    pub fn is_spent(&self, outpoint: &OutPoint) -> bool {
        self.spent.contains_key(outpoint)
    }

    pub fn spender(&self, outpoint: &OutPoint) -> Option<Hash256> {
        self.spent.get(outpoint).copied()
    }

    pub fn size(&self) -> usize {
        self.entries.len()
    }

    pub fn bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn usage(&self) -> usize {
        self.total_bytes
    }

    pub fn txids(&self) -> Vec<Hash256> {
        let mut out: Vec<_> = self.entries.keys().copied().collect();
        out.sort();
        out
    }

    pub fn get(&self, txid: &Hash256) -> Option<&MempoolEntry> {
        self.entries.get(txid)
    }

    pub fn entries(&self) -> impl Iterator<Item = &MempoolEntry> {
        self.entries.values()
    }

    pub fn insert(&mut self, entry: MempoolEntry) -> Result<MempoolInsertOutcome, MempoolError> {
        let inserted_txid = entry.txid;
        if self.max_bytes > 0 && entry.size() > self.max_bytes {
            return Err(MempoolError::new(
                MempoolErrorKind::MempoolFull,
                "transaction too large for mempool",
            ));
        }
        if self.entries.contains_key(&entry.txid) {
            return Err(MempoolError::new(
                MempoolErrorKind::AlreadyInMempool,
                "transaction already in mempool",
            ));
        }
        for outpoint in &entry.spent_outpoints {
            if let Some(conflict) = self.spent.get(outpoint) {
                return Err(MempoolError::new(
                    MempoolErrorKind::ConflictingInput,
                    format!(
                        "input {}:{} already spent by {}",
                        hash256_to_hex(&outpoint.hash),
                        outpoint.index,
                        hash256_to_hex(conflict)
                    ),
                ));
            }
        }
        for outpoint in &entry.spent_outpoints {
            self.spent.insert(outpoint.clone(), entry.txid);
        }
        self.total_bytes = self.total_bytes.saturating_add(entry.raw.len());
        self.entries.insert(entry.txid, entry);
        self.revision = self.revision.saturating_add(1);

        let mut outcome = MempoolInsertOutcome::default();
        if self.max_bytes > 0 && self.total_bytes > self.max_bytes {
            outcome = self.evict_to_fit();
        }

        if self.max_bytes > 0 && !self.entries.contains_key(&inserted_txid) {
            return Err(MempoolError::new(
                MempoolErrorKind::MempoolFull,
                "mempool full",
            ));
        }

        Ok(outcome)
    }

    #[allow(dead_code)]
    pub fn remove(&mut self, txid: &Hash256) -> Option<MempoolEntry> {
        let entry = self.entries.remove(txid)?;
        self.total_bytes = self.total_bytes.saturating_sub(entry.raw.len());
        for outpoint in &entry.spent_outpoints {
            if self.spent.get(outpoint) == Some(txid) {
                self.spent.remove(outpoint);
            }
        }
        self.revision = self.revision.saturating_add(1);
        Some(entry)
    }

    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    fn evict_to_fit(&mut self) -> MempoolInsertOutcome {
        let max_bytes = self.max_bytes;

        let mut candidates: Vec<EvictCandidate> = self
            .entries
            .values()
            .map(|entry| EvictCandidate {
                txid: entry.txid,
                fee: entry.fee,
                size: entry.size().max(1),
                time: entry.time,
            })
            .collect();

        candidates.sort_by(|a, b| {
            let fee_a = i128::from(a.fee);
            let fee_b = i128::from(b.fee);
            let size_a = a.size as i128;
            let size_b = b.size as i128;
            let left = fee_a.saturating_mul(size_b);
            let right = fee_b.saturating_mul(size_a);
            match left.cmp(&right) {
                std::cmp::Ordering::Equal => match a.time.cmp(&b.time) {
                    std::cmp::Ordering::Equal => a.txid.cmp(&b.txid),
                    other => other,
                },
                other => other,
            }
        });

        let mut evicted = 0u64;
        let mut evicted_bytes = 0u64;
        for candidate in candidates {
            if self.total_bytes <= max_bytes {
                break;
            }
            if let Some(removed) = self.remove(&candidate.txid) {
                evicted += 1;
                evicted_bytes += removed.raw.len() as u64;
            }
        }

        MempoolInsertOutcome {
            evicted,
            evicted_bytes,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MempoolInsertOutcome {
    pub evicted: u64,
    pub evicted_bytes: u64,
}

#[derive(Clone, Debug)]
struct EvictCandidate {
    txid: Hash256,
    fee: i64,
    size: usize,
    time: u64,
}

pub fn build_mempool_entry<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    chain_params: &ChainParams,
    flags: &ValidationFlags,
    tx: Transaction,
    raw: Vec<u8>,
) -> Result<MempoolEntry, MempoolError> {
    let txid = tx
        .txid()
        .map_err(|err| MempoolError::new(MempoolErrorKind::InvalidTransaction, err.to_string()))?;
    let best_height = chainstate
        .best_block()
        .map_err(|err| MempoolError::new(MempoolErrorKind::Internal, err.to_string()))?
        .map(|tip| tip.height)
        .unwrap_or(0);
    let next_height = best_height.saturating_add(1);
    let now = now_secs();
    let now_i64 = i64::try_from(now).unwrap_or(i64::MAX);
    let branch_id = current_epoch_branch_id(next_height, &chain_params.consensus.upgrades);

    let mut tx_flags = flags.clone();
    tx_flags.check_shielded = false;
    validate_mempool_transaction(
        &tx,
        next_height,
        now_i64,
        &chain_params.consensus,
        &tx_flags,
    )
    .map_err(|err| MempoolError::new(MempoolErrorKind::InvalidTransaction, err.to_string()))?;

    chainstate
        .validate_fluxnode_tx_for_mempool(&tx, &txid, next_height, chain_params)
        .map_err(|err| MempoolError::new(MempoolErrorKind::InvalidTransaction, err.to_string()))?;

    validate_shielded_state(chainstate, &tx)?;

    let flux_rebrand_active = network_upgrade_active(
        next_height,
        &chain_params.consensus.upgrades,
        UpgradeIndex::Flux,
    );
    let mut spent_outpoints = Vec::with_capacity(tx.vin.len());
    let mut transparent_in = 0i64;
    for (input_index, input) in tx.vin.iter().enumerate() {
        let entry = chainstate
            .utxo_entry(&input.prevout)
            .map_err(|err| MempoolError::new(MempoolErrorKind::Internal, err.to_string()))?
            .ok_or_else(|| MempoolError::new(MempoolErrorKind::MissingInput, "missing inputs"))?;
        if entry.is_coinbase {
            let spend_height = next_height as i64 - entry.height as i64;
            if spend_height < COINBASE_MATURITY as i64 {
                return Err(MempoolError::new(
                    MempoolErrorKind::InvalidTransaction,
                    "premature spend of coinbase",
                ));
            }
            if chain_params.consensus.coinbase_must_be_protected
                && !flux_rebrand_active
                && !tx.vout.is_empty()
            {
                return Err(MempoolError::new(
                    MempoolErrorKind::InvalidTransaction,
                    "coinbase spend has transparent outputs",
                ));
            }
        }
        transparent_in = transparent_in.checked_add(entry.value).ok_or_else(|| {
            MempoolError::new(MempoolErrorKind::InvalidTransaction, "value out of range")
        })?;
        if flags.check_script {
            verify_script(
                &input.script_sig,
                &entry.script_pubkey,
                &tx,
                input_index,
                entry.value,
                BLOCK_SCRIPT_VERIFY_FLAGS,
                branch_id,
            )
            .map_err(|err| MempoolError::new(MempoolErrorKind::InvalidScript, err.to_string()))?;
        }
        spent_outpoints.push(input.prevout.clone());
    }

    let value_out = tx_value_out(&tx)?;
    let shielded_value_in = tx_shielded_value_in(&tx)?;
    let value_in = shielded_value_in
        .checked_add(transparent_in)
        .ok_or_else(|| {
            MempoolError::new(MempoolErrorKind::InvalidTransaction, "value out of range")
        })?;
    if value_in < value_out {
        return Err(MempoolError::new(
            MempoolErrorKind::InvalidTransaction,
            "value out of range",
        ));
    }
    let fee = value_in - value_out;

    if flags.check_shielded && tx_needs_shielded(&tx) {
        let params = flags.shielded_params.as_ref().ok_or_else(|| {
            MempoolError::new(
                MempoolErrorKind::InvalidShielded,
                "shielded parameters not loaded",
            )
        })?;
        verify_transaction(&tx, branch_id, params)
            .map_err(|err| MempoolError::new(MempoolErrorKind::InvalidShielded, err.to_string()))?;
    }

    Ok(MempoolEntry {
        txid,
        tx,
        raw,
        time: now,
        height: best_height,
        fee,
        spent_outpoints,
    })
}

fn validate_shielded_state<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    tx: &Transaction,
) -> Result<(), MempoolError> {
    for joinsplit in &tx.join_splits {
        if !chainstate
            .sprout_anchor_exists(&joinsplit.anchor)
            .map_err(|err| MempoolError::new(MempoolErrorKind::Internal, err.to_string()))?
        {
            return Err(MempoolError::new(
                MempoolErrorKind::InvalidTransaction,
                "sprout anchor not found",
            ));
        }
        for nullifier in &joinsplit.nullifiers {
            if chainstate
                .sprout_nullifier_spent(nullifier)
                .map_err(|err| MempoolError::new(MempoolErrorKind::Internal, err.to_string()))?
            {
                return Err(MempoolError::new(
                    MempoolErrorKind::InvalidTransaction,
                    "sprout nullifier already spent",
                ));
            }
        }
    }

    for spend in &tx.shielded_spends {
        if !chainstate
            .sapling_anchor_exists(&spend.anchor)
            .map_err(|err| MempoolError::new(MempoolErrorKind::Internal, err.to_string()))?
        {
            return Err(MempoolError::new(
                MempoolErrorKind::InvalidTransaction,
                "sapling anchor not found",
            ));
        }
        if chainstate
            .sapling_nullifier_spent(&spend.nullifier)
            .map_err(|err| MempoolError::new(MempoolErrorKind::Internal, err.to_string()))?
        {
            return Err(MempoolError::new(
                MempoolErrorKind::InvalidTransaction,
                "sapling nullifier already spent",
            ));
        }
    }
    Ok(())
}

fn tx_needs_shielded(tx: &Transaction) -> bool {
    !(tx.join_splits.is_empty() && tx.shielded_spends.is_empty() && tx.shielded_outputs.is_empty())
}

fn tx_value_out(tx: &Transaction) -> Result<i64, MempoolError> {
    let mut total = 0i64;
    for output in &tx.vout {
        total = total.checked_add(output.value).ok_or_else(|| {
            MempoolError::new(MempoolErrorKind::InvalidTransaction, "value out of range")
        })?;
        if !money_range(total) || output.value < 0 || output.value > MAX_MONEY {
            return Err(MempoolError::new(
                MempoolErrorKind::InvalidTransaction,
                "value out of range",
            ));
        }
    }

    if tx.value_balance <= 0 {
        let balance = -tx.value_balance;
        total = total.checked_add(balance).ok_or_else(|| {
            MempoolError::new(MempoolErrorKind::InvalidTransaction, "value out of range")
        })?;
        if !money_range(balance) || !money_range(total) {
            return Err(MempoolError::new(
                MempoolErrorKind::InvalidTransaction,
                "value out of range",
            ));
        }
    }

    for joinsplit in &tx.join_splits {
        total = total.checked_add(joinsplit.vpub_old).ok_or_else(|| {
            MempoolError::new(MempoolErrorKind::InvalidTransaction, "value out of range")
        })?;
        if !money_range(joinsplit.vpub_old) || !money_range(total) {
            return Err(MempoolError::new(
                MempoolErrorKind::InvalidTransaction,
                "value out of range",
            ));
        }
    }

    Ok(total)
}

fn tx_shielded_value_in(tx: &Transaction) -> Result<i64, MempoolError> {
    let mut total = 0i64;
    if tx.value_balance >= 0 {
        total = total.checked_add(tx.value_balance).ok_or_else(|| {
            MempoolError::new(MempoolErrorKind::InvalidTransaction, "value out of range")
        })?;
        if !money_range(tx.value_balance) || !money_range(total) {
            return Err(MempoolError::new(
                MempoolErrorKind::InvalidTransaction,
                "value out of range",
            ));
        }
    }

    for joinsplit in &tx.join_splits {
        total = total.checked_add(joinsplit.vpub_new).ok_or_else(|| {
            MempoolError::new(MempoolErrorKind::InvalidTransaction, "value out of range")
        })?;
        if !money_range(joinsplit.vpub_new) || !money_range(total) {
            return Err(MempoolError::new(
                MempoolErrorKind::InvalidTransaction,
                "value out of range",
            ));
        }
    }
    Ok(total)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
