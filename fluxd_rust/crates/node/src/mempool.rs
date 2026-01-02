use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::{validate_mempool_transaction, ValidationFlags};
use fluxd_consensus::constants::{COINBASE_MATURITY, MAX_BLOCK_SIGOPS, TX_EXPIRING_SOON_THRESHOLD};
use fluxd_consensus::money::{money_range, MAX_MONEY};
use fluxd_consensus::params::ChainParams;
use fluxd_consensus::upgrades::{current_epoch_branch_id, network_upgrade_active, UpgradeIndex};
use fluxd_consensus::Hash256;
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::transaction::Transaction;
use fluxd_script::interpreter::{
    verify_script, BLOCK_SCRIPT_VERIFY_FLAGS, STANDARD_SCRIPT_VERIFY_FLAGS,
};
use fluxd_script::standard::{classify_script_pubkey, ScriptType};
use fluxd_shielded::verify_transaction;

use crate::stats::hash256_to_hex;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MempoolErrorKind {
    AlreadyInMempool,
    ConflictingInput,
    InsufficientFee,
    MissingInput,
    MempoolFull,
    NonStandard,
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
    pub fee_delta: i64,
    pub priority_delta: f64,
    pub spent_outpoints: Vec<OutPoint>,
    pub parents: Vec<Hash256>,
}

impl MempoolEntry {
    pub fn size(&self) -> usize {
        self.raw.len()
    }

    pub fn modified_fee(&self) -> i64 {
        self.fee.saturating_add(self.fee_delta)
    }
}

#[derive(Clone, Debug)]
pub struct MempoolPrevout {
    pub value: i64,
    pub script_pubkey: Vec<u8>,
}

#[derive(Default)]
pub struct Mempool {
    entries: HashMap<Hash256, MempoolEntry>,
    spent: HashMap<OutPoint, Hash256>,
    sprout_nullifiers: HashMap<Hash256, Hash256>,
    sapling_nullifiers: HashMap<Hash256, Hash256>,
    children: HashMap<Hash256, Vec<Hash256>>,
    prioritisations: HashMap<Hash256, Prioritisation>,
    total_bytes: usize,
    max_bytes: usize,
    revision: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Prioritisation {
    pub priority_delta: f64,
    pub fee_delta: i64,
}

impl Mempool {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            spent: HashMap::new(),
            sprout_nullifiers: HashMap::new(),
            sapling_nullifiers: HashMap::new(),
            children: HashMap::new(),
            prioritisations: HashMap::new(),
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

    pub fn sprout_nullifier_spender(&self, nullifier: &Hash256) -> Option<Hash256> {
        self.sprout_nullifiers.get(nullifier).copied()
    }

    pub fn sapling_nullifier_spender(&self, nullifier: &Hash256) -> Option<Hash256> {
        self.sapling_nullifiers.get(nullifier).copied()
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

    pub fn prevout(&self, outpoint: &OutPoint) -> Option<MempoolPrevout> {
        let entry = self.entries.get(&outpoint.hash)?;
        let index = usize::try_from(outpoint.index).ok()?;
        let txout = entry.tx.vout.get(index)?;
        Some(MempoolPrevout {
            value: txout.value,
            script_pubkey: txout.script_pubkey.clone(),
        })
    }

    pub fn prevouts_for_tx(&self, tx: &Transaction) -> HashMap<OutPoint, MempoolPrevout> {
        let mut out = HashMap::new();
        for input in &tx.vin {
            if out.contains_key(&input.prevout) {
                continue;
            }
            if let Some(prevout) = self.prevout(&input.prevout) {
                out.insert(input.prevout.clone(), prevout);
            }
        }
        out
    }

    pub fn entries(&self) -> impl Iterator<Item = &MempoolEntry> {
        self.entries.values()
    }

    pub fn prioritise_transaction(&mut self, txid: Hash256, priority_delta: f64, fee_delta: i64) {
        let entry = self.prioritisations.entry(txid).or_default();
        entry.priority_delta += priority_delta;
        entry.fee_delta = entry.fee_delta.saturating_add(fee_delta);

        if let Some(tx) = self.entries.get_mut(&txid) {
            tx.priority_delta += priority_delta;
            tx.fee_delta = tx.fee_delta.saturating_add(fee_delta);
        }
        self.revision = self.revision.saturating_add(1);
    }

    pub fn insert(&mut self, entry: MempoolEntry) -> Result<MempoolInsertOutcome, MempoolError> {
        let mut entry = entry;
        if let Some(priority) = self.prioritisations.get(&entry.txid) {
            entry.priority_delta += priority.priority_delta;
            entry.fee_delta = entry.fee_delta.saturating_add(priority.fee_delta);
        }

        let inserted_txid = entry.txid;
        let parents = entry.parents.clone();
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
        for joinsplit in &entry.tx.join_splits {
            for nullifier in &joinsplit.nullifiers {
                if let Some(conflict) = self.sprout_nullifiers.get(nullifier) {
                    return Err(MempoolError::new(
                        MempoolErrorKind::ConflictingInput,
                        format!(
                            "sprout nullifier {} already spent by {}",
                            hash256_to_hex(nullifier),
                            hash256_to_hex(conflict)
                        ),
                    ));
                }
            }
        }
        for spend in &entry.tx.shielded_spends {
            if let Some(conflict) = self.sapling_nullifiers.get(&spend.nullifier) {
                return Err(MempoolError::new(
                    MempoolErrorKind::ConflictingInput,
                    format!(
                        "sapling nullifier {} already spent by {}",
                        hash256_to_hex(&spend.nullifier),
                        hash256_to_hex(conflict)
                    ),
                ));
            }
        }
        for outpoint in &entry.spent_outpoints {
            self.spent.insert(outpoint.clone(), entry.txid);
        }
        for joinsplit in &entry.tx.join_splits {
            for nullifier in &joinsplit.nullifiers {
                self.sprout_nullifiers.insert(*nullifier, entry.txid);
            }
        }
        for spend in &entry.tx.shielded_spends {
            self.sapling_nullifiers.insert(spend.nullifier, entry.txid);
        }
        self.total_bytes = self.total_bytes.saturating_add(entry.raw.len());
        self.entries.insert(entry.txid, entry);
        for parent in parents {
            let children = self.children.entry(parent).or_default();
            if !children.contains(&inserted_txid) {
                children.push(inserted_txid);
            }
        }
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
        for joinsplit in &entry.tx.join_splits {
            for nullifier in &joinsplit.nullifiers {
                if self.sprout_nullifiers.get(nullifier) == Some(txid) {
                    self.sprout_nullifiers.remove(nullifier);
                }
            }
        }
        for spend in &entry.tx.shielded_spends {
            if self.sapling_nullifiers.get(&spend.nullifier) == Some(txid) {
                self.sapling_nullifiers.remove(&spend.nullifier);
            }
        }
        for parent in &entry.parents {
            let should_remove_parent = match self.children.get_mut(parent) {
                Some(children) => {
                    children.retain(|child| child != txid);
                    children.is_empty()
                }
                None => false,
            };
            if should_remove_parent {
                self.children.remove(parent);
            }
        }
        if let Some(children) = self.children.remove(txid) {
            for child in children {
                if let Some(child_entry) = self.entries.get_mut(&child) {
                    child_entry.parents.retain(|parent| parent != txid);
                }
            }
        }
        self.revision = self.revision.saturating_add(1);
        Some(entry)
    }

    pub fn remove_with_descendants(&mut self, txid: &Hash256) -> Vec<MempoolEntry> {
        let mut visited: HashSet<Hash256> = HashSet::new();
        let mut order: Vec<Hash256> = Vec::new();

        fn visit(
            mempool: &Mempool,
            txid: Hash256,
            visited: &mut HashSet<Hash256>,
            order: &mut Vec<Hash256>,
        ) {
            if !visited.insert(txid) {
                return;
            }
            if let Some(children) = mempool.children.get(&txid) {
                for child in children {
                    visit(mempool, *child, visited, order);
                }
            }
            order.push(txid);
        }

        visit(self, *txid, &mut visited, &mut order);

        let mut removed = Vec::new();
        for txid in order {
            if let Some(entry) = self.remove(&txid) {
                removed.push(entry);
            }
        }
        removed
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
                fee: entry.modified_fee(),
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
            let removed = self.remove_with_descendants(&candidate.txid);
            if removed.is_empty() {
                continue;
            }
            evicted = evicted.saturating_add(removed.len() as u64);
            evicted_bytes = evicted_bytes.saturating_add(
                removed
                    .iter()
                    .map(|entry| entry.raw.len() as u64)
                    .sum::<u64>(),
            );
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

#[derive(Clone, Debug)]
pub struct MempoolPolicy {
    pub require_standard: bool,
    /// Fee rate in zatoshis/KB.
    pub min_relay_fee_per_kb: i64,
    pub max_scriptsig_size: usize,
    pub max_op_return_bytes: usize,
    pub max_p2sh_sigops: u32,
    pub max_standard_tx_sigops: u32,
}

impl MempoolPolicy {
    pub fn standard(min_relay_fee_per_kb: i64, require_standard: bool) -> Self {
        let min_relay_fee_per_kb = min_relay_fee_per_kb.max(0);
        Self {
            require_standard,
            min_relay_fee_per_kb,
            max_scriptsig_size: 1650,
            max_op_return_bytes: 80,
            max_p2sh_sigops: 15,
            max_standard_tx_sigops: MAX_BLOCK_SIGOPS / 5,
        }
    }

    pub fn min_relay_fee_for_size(&self, size: usize) -> i64 {
        min_relay_fee_for_size(self.min_relay_fee_per_kb, size)
    }
}

pub fn build_mempool_entry<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool_prevouts: &HashMap<OutPoint, MempoolPrevout>,
    chain_params: &ChainParams,
    flags: &ValidationFlags,
    policy: &MempoolPolicy,
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

    if tx.expiry_height > 0
        && network_upgrade_active(
            next_height,
            &chain_params.consensus.upgrades,
            UpgradeIndex::Acadia,
        )
    {
        let next_height_u32 = u32::try_from(next_height).unwrap_or(0);
        let min_expiry = next_height_u32.saturating_add(TX_EXPIRING_SOON_THRESHOLD);
        if min_expiry > tx.expiry_height {
            return Err(MempoolError::new(
                MempoolErrorKind::InvalidTransaction,
                format!(
                    "tx-expiring-soon: expiryheight is {} but should be at least {} to avoid transaction expiring soon",
                    tx.expiry_height, min_expiry
                ),
            ));
        }
    }
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
    let require_standard = policy.require_standard;
    let mut prev_scripts = Vec::with_capacity(tx.vin.len());
    let mut spent_outpoints = Vec::with_capacity(tx.vin.len());
    let mut parents: HashSet<Hash256> = HashSet::new();
    let mut transparent_in = 0i64;
    for (input_index, input) in tx.vin.iter().enumerate() {
        if require_standard {
            if input.script_sig.len() > policy.max_scriptsig_size {
                return Err(MempoolError::new(
                    MempoolErrorKind::NonStandard,
                    "scriptsig-size",
                ));
            }
            if !is_push_only(&input.script_sig) {
                return Err(MempoolError::new(
                    MempoolErrorKind::NonStandard,
                    "scriptsig-not-pushonly",
                ));
            }
        }

        let (prev_value, prev_script_pubkey, prev_is_coinbase, prev_height) = match chainstate
            .utxo_entry(&input.prevout)
            .map_err(|err| MempoolError::new(MempoolErrorKind::Internal, err.to_string()))?
        {
            Some(entry) => (
                entry.value,
                entry.script_pubkey,
                entry.is_coinbase,
                entry.height,
            ),
            None => {
                let prevout = mempool_prevouts.get(&input.prevout).ok_or_else(|| {
                    MempoolError::new(MempoolErrorKind::MissingInput, "missing inputs")
                })?;
                parents.insert(input.prevout.hash);
                (prevout.value, prevout.script_pubkey.clone(), false, 0)
            }
        };

        if prev_is_coinbase {
            let spend_height = next_height as i64 - prev_height as i64;
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
        transparent_in = transparent_in.checked_add(prev_value).ok_or_else(|| {
            MempoolError::new(MempoolErrorKind::InvalidTransaction, "value out of range")
        })?;
        if flags.check_script {
            let script_flags = if require_standard {
                STANDARD_SCRIPT_VERIFY_FLAGS
            } else {
                BLOCK_SCRIPT_VERIFY_FLAGS
            };
            verify_script(
                &input.script_sig,
                &prev_script_pubkey,
                &tx,
                input_index,
                prev_value,
                script_flags,
                branch_id,
            )
            .map_err(|err| MempoolError::new(MempoolErrorKind::InvalidScript, err.to_string()))?;

            if require_standard {
                verify_script(
                    &input.script_sig,
                    &prev_script_pubkey,
                    &tx,
                    input_index,
                    prev_value,
                    BLOCK_SCRIPT_VERIFY_FLAGS,
                    branch_id,
                )
                .map_err(|err| {
                    MempoolError::new(
                        MempoolErrorKind::InvalidScript,
                        format!(
                            "BUG: failed against mandatory script flags but passed standard flags: {err}"
                        ),
                    )
                })?;
            }
        }
        prev_scripts.push(prev_script_pubkey.clone());
        spent_outpoints.push(input.prevout.clone());
    }

    if require_standard {
        enforce_standard_inputs(&tx, &prev_scripts, policy)?;
    }

    if require_standard {
        enforce_standard_outputs(&tx, policy)?;
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

    if policy.min_relay_fee_per_kb > 0 && tx.fluxnode.is_none() {
        let required_fee = policy.min_relay_fee_for_size(raw.len());
        if fee < required_fee {
            return Err(MempoolError::new(
                MempoolErrorKind::InsufficientFee,
                "insufficient fee",
            ));
        }
    }

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

    let mut parents: Vec<Hash256> = parents.into_iter().collect();
    parents.sort();

    Ok(MempoolEntry {
        txid,
        tx,
        raw,
        time: now,
        height: best_height,
        fee,
        fee_delta: 0,
        priority_delta: 0.0,
        spent_outpoints,
        parents,
    })
}

fn enforce_standard_outputs(tx: &Transaction, policy: &MempoolPolicy) -> Result<(), MempoolError> {
    let mut op_return_count = 0usize;
    for output in &tx.vout {
        if is_standard_op_return(&output.script_pubkey, policy.max_op_return_bytes) {
            op_return_count += 1;
            continue;
        }

        match classify_script_pubkey(&output.script_pubkey) {
            ScriptType::P2Pk | ScriptType::P2Pkh | ScriptType::P2Sh => {}
            ScriptType::P2Wpkh | ScriptType::P2Wsh => {
                return Err(MempoolError::new(
                    MempoolErrorKind::NonStandard,
                    "witness-program",
                ));
            }
            ScriptType::Unknown => {
                return Err(MempoolError::new(
                    MempoolErrorKind::NonStandard,
                    "scriptpubkey",
                ));
            }
        }

        if is_dust(
            output.value,
            &output.script_pubkey,
            policy.min_relay_fee_per_kb,
        ) {
            return Err(MempoolError::new(MempoolErrorKind::NonStandard, "dust"));
        }
    }

    if op_return_count > 1 {
        return Err(MempoolError::new(
            MempoolErrorKind::NonStandard,
            "multi-op-return",
        ));
    }

    Ok(())
}

fn enforce_standard_inputs(
    tx: &Transaction,
    prev_scripts: &[Vec<u8>],
    policy: &MempoolPolicy,
) -> Result<(), MempoolError> {
    let mut sigops: u32 = 0;

    for (input, prev_script) in tx.vin.iter().zip(prev_scripts.iter()) {
        let stack = parse_push_only_stack(&input.script_sig)
            .ok_or_else(|| MempoolError::new(MempoolErrorKind::NonStandard, "scriptsig"))?;

        let prev_type = classify_script_pubkey(prev_script);
        match prev_type {
            ScriptType::P2Pkh => {
                if stack.len() != 2 {
                    return Err(MempoolError::new(
                        MempoolErrorKind::NonStandard,
                        "scriptsig-args",
                    ));
                }
            }
            ScriptType::P2Pk => {
                if stack.len() != 1 {
                    return Err(MempoolError::new(
                        MempoolErrorKind::NonStandard,
                        "scriptsig-args",
                    ));
                }
            }
            ScriptType::P2Sh => {
                let redeem = stack
                    .last()
                    .filter(|item| !item.is_empty())
                    .ok_or_else(|| {
                        MempoolError::new(MempoolErrorKind::NonStandard, "p2sh-redeem")
                    })?;
                if redeem.len() > 520 {
                    return Err(MempoolError::new(
                        MempoolErrorKind::NonStandard,
                        "p2sh-redeem-size",
                    ));
                }
                let redeem_sigops = count_sigops(redeem, true).ok_or_else(|| {
                    MempoolError::new(MempoolErrorKind::NonStandard, "p2sh-redeem")
                })?;
                if redeem_sigops > policy.max_p2sh_sigops {
                    return Err(MempoolError::new(
                        MempoolErrorKind::NonStandard,
                        "p2sh-sigops",
                    ));
                }
                sigops = sigops.saturating_add(redeem_sigops);
            }
            ScriptType::P2Wpkh | ScriptType::P2Wsh | ScriptType::Unknown => {
                return Err(MempoolError::new(
                    MempoolErrorKind::NonStandard,
                    "nonstandard-input",
                ));
            }
        }
    }

    for input in &tx.vin {
        if let Some(value) = count_sigops(&input.script_sig, false) {
            sigops = sigops.saturating_add(value);
        } else {
            return Err(MempoolError::new(
                MempoolErrorKind::NonStandard,
                "scriptsig-sigops",
            ));
        }
    }
    for output in &tx.vout {
        if let Some(value) = count_sigops(&output.script_pubkey, false) {
            sigops = sigops.saturating_add(value);
        } else {
            return Err(MempoolError::new(
                MempoolErrorKind::NonStandard,
                "scriptpubkey-sigops",
            ));
        }
    }

    if sigops > policy.max_standard_tx_sigops {
        return Err(MempoolError::new(
            MempoolErrorKind::NonStandard,
            "bad-txns-too-many-sigops",
        ));
    }

    Ok(())
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

fn min_relay_fee_for_size(min_fee_per_kb: i64, size: usize) -> i64 {
    if min_fee_per_kb <= 0 {
        return 0;
    }

    let size = i64::try_from(size).unwrap_or(i64::MAX);
    let mut fee = min_fee_per_kb.saturating_mul(size).saturating_div(1000);
    if fee == 0 {
        fee = min_fee_per_kb;
    }
    fee
}

fn is_dust(value: i64, script_pubkey: &[u8], min_fee_per_kb: i64) -> bool {
    if min_fee_per_kb <= 0 {
        return false;
    }
    if is_unspendable(script_pubkey) {
        return false;
    }
    if value < 0 {
        return true;
    }
    let out_size = 8usize
        .saturating_add(compact_size_len(script_pubkey.len()))
        .saturating_add(script_pubkey.len());
    let spend_size = out_size.saturating_add(148);
    let fee = min_relay_fee_for_size(min_fee_per_kb, spend_size);
    let dust_threshold = fee.saturating_mul(3);
    value < dust_threshold
}

fn compact_size_len(value: usize) -> usize {
    if value < 0xfd {
        1
    } else if value <= 0xffff {
        3
    } else if value <= 0xffff_ffff {
        5
    } else {
        9
    }
}

fn is_unspendable(script_pubkey: &[u8]) -> bool {
    script_pubkey.first().copied() == Some(OP_RETURN)
}

const OP_0: u8 = 0x00;
const OP_1NEGATE: u8 = 0x4f;
const OP_PUSHDATA1: u8 = 0x4c;
const OP_PUSHDATA2: u8 = 0x4d;
const OP_PUSHDATA4: u8 = 0x4e;
const OP_1: u8 = 0x51;
const OP_16: u8 = 0x60;
const OP_CHECKSIG: u8 = 0xac;
const OP_CHECKSIGVERIFY: u8 = 0xad;
const OP_CHECKMULTISIG: u8 = 0xae;
const OP_CHECKMULTISIGVERIFY: u8 = 0xaf;
const OP_RETURN: u8 = 0x6a;

fn is_push_only(script: &[u8]) -> bool {
    parse_push_only_stack(script).is_some()
}

fn parse_push_only_stack(script: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut cursor = 0usize;
    let mut stack = Vec::new();
    while cursor < script.len() {
        let opcode = *script.get(cursor)?;
        cursor = cursor.saturating_add(1);
        let (len, is_data) = match opcode {
            0x01..=0x4b => (opcode as usize, true),
            OP_PUSHDATA1 => (*script.get(cursor)? as usize, {
                cursor = cursor.saturating_add(1);
                true
            }),
            OP_PUSHDATA2 => {
                let lo = *script.get(cursor)? as usize;
                let hi = *script.get(cursor + 1)? as usize;
                cursor = cursor.saturating_add(2);
                ((hi << 8) | lo, true)
            }
            OP_PUSHDATA4 => {
                let b0 = *script.get(cursor)? as usize;
                let b1 = *script.get(cursor + 1)? as usize;
                let b2 = *script.get(cursor + 2)? as usize;
                let b3 = *script.get(cursor + 3)? as usize;
                cursor = cursor.saturating_add(4);
                ((b3 << 24) | (b2 << 16) | (b1 << 8) | b0, true)
            }
            OP_0 => {
                stack.push(Vec::new());
                (0, false)
            }
            OP_1NEGATE => {
                stack.push(vec![0x81]);
                (0, false)
            }
            OP_1..=OP_16 => {
                stack.push(vec![opcode - OP_1 + 1]);
                (0, false)
            }
            _ => return None,
        };

        if is_data {
            if cursor.saturating_add(len) > script.len() {
                return None;
            }
            let data = script[cursor..cursor + len].to_vec();
            stack.push(data);
            cursor = cursor.saturating_add(len);
        }
    }
    Some(stack)
}

fn count_sigops(script: &[u8], accurate: bool) -> Option<u32> {
    let mut cursor = 0usize;
    let mut last_opcode = 0u8;
    let mut count = 0u32;
    while cursor < script.len() {
        let opcode = *script.get(cursor)?;
        cursor = cursor.saturating_add(1);
        match opcode {
            0x01..=0x4b => {
                let len = opcode as usize;
                cursor = cursor.saturating_add(len);
            }
            OP_PUSHDATA1 => {
                let len = *script.get(cursor)? as usize;
                cursor = cursor.saturating_add(1 + len);
            }
            OP_PUSHDATA2 => {
                let lo = *script.get(cursor)? as usize;
                let hi = *script.get(cursor + 1)? as usize;
                let len = (hi << 8) | lo;
                cursor = cursor.saturating_add(2 + len);
            }
            OP_PUSHDATA4 => {
                let b0 = *script.get(cursor)? as usize;
                let b1 = *script.get(cursor + 1)? as usize;
                let b2 = *script.get(cursor + 2)? as usize;
                let b3 = *script.get(cursor + 3)? as usize;
                let len = (b3 << 24) | (b2 << 16) | (b1 << 8) | b0;
                cursor = cursor.saturating_add(4 + len);
            }
            OP_CHECKSIG | OP_CHECKSIGVERIFY => {
                count = count.saturating_add(1);
            }
            OP_CHECKMULTISIG | OP_CHECKMULTISIGVERIFY => {
                let add = if accurate {
                    decode_op_n(last_opcode).unwrap_or(20) as u32
                } else {
                    20
                };
                count = count.saturating_add(add);
            }
            _ => {}
        }
        if cursor > script.len() {
            return None;
        }
        last_opcode = opcode;
    }
    Some(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxd_primitives::transaction::{TxIn, TxOut};

    fn dummy_tx(vin: Vec<TxIn>, vout: Vec<TxOut>) -> Transaction {
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

    #[test]
    fn remove_mined_parent_detaches_children() {
        let parent_txid: Hash256 = [1u8; 32];
        let child_txid: Hash256 = [2u8; 32];
        let parent_outpoint = OutPoint {
            hash: parent_txid,
            index: 0,
        };

        let parent_entry = MempoolEntry {
            txid: parent_txid,
            tx: dummy_tx(
                Vec::new(),
                vec![TxOut {
                    value: 50,
                    script_pubkey: vec![0x51],
                }],
            ),
            raw: vec![0u8; 10],
            time: 0,
            height: 0,
            fee: 0,
            fee_delta: 0,
            priority_delta: 0.0,
            spent_outpoints: Vec::new(),
            parents: Vec::new(),
        };
        let child_entry = MempoolEntry {
            txid: child_txid,
            tx: dummy_tx(
                vec![TxIn {
                    prevout: parent_outpoint.clone(),
                    script_sig: Vec::new(),
                    sequence: 0,
                }],
                vec![TxOut {
                    value: 25,
                    script_pubkey: vec![0x51],
                }],
            ),
            raw: vec![0u8; 10],
            time: 0,
            height: 0,
            fee: 0,
            fee_delta: 0,
            priority_delta: 0.0,
            spent_outpoints: vec![parent_outpoint],
            parents: vec![parent_txid],
        };

        let mut mempool = Mempool::new(0);
        mempool.insert(parent_entry).expect("insert parent");
        mempool.insert(child_entry).expect("insert child");

        assert!(mempool
            .children
            .get(&parent_txid)
            .is_some_and(|children| children.contains(&child_txid)));

        let removed = mempool.remove(&parent_txid).expect("remove parent");
        assert_eq!(removed.txid, parent_txid);

        let child = mempool.entries.get(&child_txid).expect("child remains");
        assert!(child.parents.is_empty());
        assert!(mempool.children.get(&parent_txid).is_none());
    }

    #[test]
    fn remove_with_descendants_removes_entire_subtree() {
        let parent_txid: Hash256 = [1u8; 32];
        let child_txid: Hash256 = [2u8; 32];
        let parent_outpoint = OutPoint {
            hash: parent_txid,
            index: 0,
        };

        let parent_entry = MempoolEntry {
            txid: parent_txid,
            tx: dummy_tx(
                Vec::new(),
                vec![TxOut {
                    value: 50,
                    script_pubkey: vec![0x51],
                }],
            ),
            raw: vec![0u8; 10],
            time: 0,
            height: 0,
            fee: 0,
            fee_delta: 0,
            priority_delta: 0.0,
            spent_outpoints: Vec::new(),
            parents: Vec::new(),
        };
        let child_entry = MempoolEntry {
            txid: child_txid,
            tx: dummy_tx(
                vec![TxIn {
                    prevout: parent_outpoint.clone(),
                    script_sig: Vec::new(),
                    sequence: 0,
                }],
                vec![TxOut {
                    value: 25,
                    script_pubkey: vec![0x51],
                }],
            ),
            raw: vec![0u8; 10],
            time: 0,
            height: 0,
            fee: 0,
            fee_delta: 0,
            priority_delta: 0.0,
            spent_outpoints: vec![parent_outpoint],
            parents: vec![parent_txid],
        };

        let mut mempool = Mempool::new(0);
        mempool.insert(parent_entry).expect("insert parent");
        mempool.insert(child_entry).expect("insert child");

        let removed = mempool.remove_with_descendants(&parent_txid);
        let removed_ids: HashSet<Hash256> = removed.into_iter().map(|entry| entry.txid).collect();
        assert!(removed_ids.contains(&parent_txid));
        assert!(removed_ids.contains(&child_txid));
        assert!(mempool.entries.is_empty());
        assert!(mempool.children.is_empty());
    }
}

fn decode_op_n(opcode: u8) -> Option<u8> {
    match opcode {
        OP_0 => Some(0),
        OP_1..=OP_16 => Some(opcode - OP_1 + 1),
        _ => None,
    }
}

fn is_standard_op_return(script_pubkey: &[u8], max_bytes: usize) -> bool {
    if script_pubkey.first().copied() != Some(OP_RETURN) {
        return false;
    }
    if script_pubkey.len() == 1 {
        return true;
    }

    let mut cursor = 1usize;
    let opcode = match script_pubkey.get(cursor) {
        Some(opcode) => *opcode,
        None => return false,
    };
    cursor = cursor.saturating_add(1);

    let len = match opcode {
        0x01..=0x4b => opcode as usize,
        OP_PUSHDATA1 => {
            let len = match script_pubkey.get(cursor) {
                Some(byte) => *byte as usize,
                None => return false,
            };
            cursor = cursor.saturating_add(1);
            len
        }
        OP_PUSHDATA2 => {
            let lo = match script_pubkey.get(cursor) {
                Some(byte) => *byte as usize,
                None => return false,
            };
            let hi = match script_pubkey.get(cursor + 1) {
                Some(byte) => *byte as usize,
                None => return false,
            };
            cursor = cursor.saturating_add(2);
            (hi << 8) | lo
        }
        OP_PUSHDATA4 => {
            let b0 = match script_pubkey.get(cursor) {
                Some(byte) => *byte as usize,
                None => return false,
            };
            let b1 = match script_pubkey.get(cursor + 1) {
                Some(byte) => *byte as usize,
                None => return false,
            };
            let b2 = match script_pubkey.get(cursor + 2) {
                Some(byte) => *byte as usize,
                None => return false,
            };
            let b3 = match script_pubkey.get(cursor + 3) {
                Some(byte) => *byte as usize,
                None => return false,
            };
            cursor = cursor.saturating_add(4);
            (b3 << 24) | (b2 << 16) | (b1 << 8) | b0
        }
        OP_0 | OP_1NEGATE | OP_1..=OP_16 => 0,
        _ => return false,
    };

    if len > max_bytes {
        return false;
    }
    cursor.saturating_add(len) == script_pubkey.len()
}
