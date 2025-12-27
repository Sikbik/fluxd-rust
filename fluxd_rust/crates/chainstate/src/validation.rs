//! Block/transaction validation pipeline.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fluxd_consensus::constants::{
    MAX_BLOCK_SIGOPS, MAX_BLOCK_SIZE, MAX_TX_SIZE_AFTER_SAPLING, MAX_TX_SIZE_BEFORE_SAPLING,
    MIN_BLOCK_VERSION, MIN_PON_BLOCK_VERSION, OVERWINTER_MIN_TX_VERSION, SAPLING_MAX_TX_VERSION,
    SAPLING_MIN_TX_VERSION, SPROUT_MIN_TX_VERSION, TX_EXPIRY_HEIGHT_THRESHOLD,
};
use fluxd_consensus::money::MAX_MONEY;
use fluxd_consensus::params::ConsensusParams;
use fluxd_consensus::upgrades::{current_epoch_branch_id, network_upgrade_active, UpgradeIndex};
use fluxd_consensus::Hash256;
use fluxd_fluxnode::validation::{self as fluxnode_validation};
use fluxd_pon::validation as pon_validation;
use fluxd_pow::validation as pow_validation;
use fluxd_primitives::block::Block;
use fluxd_primitives::hash::sha256d;
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::transaction::{
    has_flux_tx_delegates_feature, FluxnodeStartVariantV6, FluxnodeTx, FluxnodeTxV5, FluxnodeTxV6,
    Transaction, TransactionEncodeError, FLUXNODE_INTERNAL_NORMAL_TX_VERSION,
    FLUXNODE_INTERNAL_P2SH_TX_VERSION, FLUXNODE_TX_UPGRADEABLE_VERSION, FLUXNODE_TX_VERSION,
};
use fluxd_shielded::{verify_transaction, ShieldedError, ShieldedParams};
use rayon::prelude::*;

#[derive(Clone, Debug, Default)]
pub struct ValidationFlags {
    pub check_pow: bool,
    pub check_pon: bool,
    pub check_script: bool,
    pub check_shielded: bool,
    pub shielded_params: Option<Arc<ShieldedParams>>,
    pub metrics: Option<Arc<ValidationMetrics>>,
}

#[derive(Debug, Default)]
pub struct ValidationMetrics {
    validate_us: AtomicU64,
    validate_blocks: AtomicU64,
    script_us: AtomicU64,
    script_blocks: AtomicU64,
    shielded_us: AtomicU64,
    shielded_txs: AtomicU64,
}

#[derive(Clone, Debug, Default)]
pub struct ValidationMetricsSnapshot {
    pub validate_us: u64,
    pub validate_blocks: u64,
    pub script_us: u64,
    pub script_blocks: u64,
    pub shielded_us: u64,
    pub shielded_txs: u64,
}

impl ValidationMetrics {
    pub fn record_validate(&self, elapsed: Duration) {
        self.validate_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.validate_blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_script(&self, elapsed: Duration) {
        self.script_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.script_blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_shielded(&self, elapsed: Duration) {
        self.shielded_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.shielded_txs.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> ValidationMetricsSnapshot {
        ValidationMetricsSnapshot {
            validate_us: self.validate_us.load(Ordering::Relaxed),
            validate_blocks: self.validate_blocks.load(Ordering::Relaxed),
            script_us: self.script_us.load(Ordering::Relaxed),
            script_blocks: self.script_blocks.load(Ordering::Relaxed),
            shielded_us: self.shielded_us.load(Ordering::Relaxed),
            shielded_txs: self.shielded_txs.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug)]
pub enum ValidationError {
    InvalidBlock(&'static str),
    InvalidHeader(&'static str),
    InvalidTransaction(&'static str),
    ValueOutOfRange,
    DuplicateInput,
    DuplicateTransaction,
    MerkleMismatch,
    Shielded(ShieldedError),
    Pow(pow_validation::PowError),
    Pon(pon_validation::PonError),
    Fluxnode(&'static str),
    Encoding(TransactionEncodeError),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::InvalidBlock(message) => write!(f, "{message}"),
            ValidationError::InvalidHeader(message) => write!(f, "{message}"),
            ValidationError::InvalidTransaction(message) => write!(f, "{message}"),
            ValidationError::ValueOutOfRange => write!(f, "value out of range"),
            ValidationError::DuplicateInput => write!(f, "duplicate input"),
            ValidationError::DuplicateTransaction => write!(f, "duplicate transaction"),
            ValidationError::MerkleMismatch => write!(f, "merkle root mismatch"),
            ValidationError::Shielded(err) => write!(f, "{err}"),
            ValidationError::Pow(err) => write!(f, "{err}"),
            ValidationError::Pon(err) => write!(f, "{err}"),
            ValidationError::Fluxnode(message) => write!(f, "{message}"),
            ValidationError::Encoding(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for ValidationError {}

impl From<pow_validation::PowError> for ValidationError {
    fn from(err: pow_validation::PowError) -> Self {
        ValidationError::Pow(err)
    }
}

impl From<pon_validation::PonError> for ValidationError {
    fn from(err: pon_validation::PonError) -> Self {
        ValidationError::Pon(err)
    }
}

impl From<TransactionEncodeError> for ValidationError {
    fn from(err: TransactionEncodeError) -> Self {
        ValidationError::Encoding(err)
    }
}

impl From<ShieldedError> for ValidationError {
    fn from(err: ShieldedError) -> Self {
        ValidationError::Shielded(err)
    }
}

pub fn validate_block(
    block: &Block,
    height: i32,
    params: &ConsensusParams,
    flags: &ValidationFlags,
) -> Result<(), ValidationError> {
    if block.transactions.is_empty() {
        return Err(ValidationError::InvalidBlock(
            "block must contain at least one transaction",
        ));
    }
    if block.transactions.len() as u32 > MAX_BLOCK_SIZE {
        return Err(ValidationError::InvalidBlock(
            "block transaction count too large",
        ));
    }
    let block_size = block.consensus_encode()?.len() as u32;
    if block_size > MAX_BLOCK_SIZE {
        return Err(ValidationError::InvalidBlock("block size too large"));
    }

    let validate_start = Instant::now();
    validate_header(block, height, params, flags)?;
    validate_merkle_root(block)?;
    if height > 20 && !coinbase_height_matches(&block.transactions[0], height) {
        return Err(ValidationError::InvalidBlock("coinbase height mismatch"));
    }

    let branch_id = current_epoch_branch_id(height, &params.upgrades);
    let mut txids = HashSet::new();
    let mut fluxnode_outpoints = HashSet::new();
    let mut shielded_txs: Vec<&Transaction> = Vec::new();
    let mut tx_flags = flags.clone();
    if flags.check_shielded {
        tx_flags.check_shielded = false;
    }
    for (index, tx) in block.transactions.iter().enumerate() {
        let block_time = block.header.time as i64;
        if !is_final_tx(tx, height, block_time) {
            return Err(ValidationError::InvalidTransaction(
                "transaction is not final",
            ));
        }
        validate_transaction(tx, index == 0, height, params, branch_id, &tx_flags)?;
        let txid = tx.txid()?;
        if !txids.insert(txid) {
            return Err(ValidationError::DuplicateTransaction);
        }
        if index > 0 {
            if let Some(outpoint) = fluxnode_collateral_outpoint(tx) {
                if !fluxnode_outpoints.insert(outpoint) {
                    return Err(ValidationError::InvalidBlock(
                        "duplicate fluxnode collateral outpoint",
                    ));
                }
            }
        }
        if flags.check_shielded
            && (!tx.join_splits.is_empty()
                || !tx.shielded_spends.is_empty()
                || !tx.shielded_outputs.is_empty())
        {
            shielded_txs.push(tx);
        }
    }
    let sigops = block_sigops(block);
    if sigops > MAX_BLOCK_SIGOPS {
        return Err(ValidationError::InvalidBlock("block sigops limit exceeded"));
    }

    if flags.check_shielded && !shielded_txs.is_empty() {
        let params = flags.shielded_params.as_ref().ok_or_else(|| {
            ValidationError::Shielded(ShieldedError::MissingParams(
                "shielded parameters not loaded".to_string(),
            ))
        })?;
        let shielded_result =
            shielded_txs
                .par_iter()
                .try_for_each(|tx| -> Result<(), ValidationError> {
                    let shielded_start = Instant::now();
                    verify_transaction(tx, branch_id, params).map_err(ValidationError::from)?;
                    if let Some(metrics) = flags.metrics.as_ref() {
                        metrics.record_shielded(shielded_start.elapsed());
                    }
                    Ok(())
                });
        shielded_result?;
    }

    if let Some(metrics) = flags.metrics.as_ref() {
        metrics.record_validate(validate_start.elapsed());
    }
    Ok(())
}

fn validate_header(
    block: &Block,
    height: i32,
    params: &ConsensusParams,
    flags: &ValidationFlags,
) -> Result<(), ValidationError> {
    let pon_active = network_upgrade_active(height, &params.upgrades, UpgradeIndex::Pon);
    if block.header.version < MIN_BLOCK_VERSION {
        return Err(ValidationError::InvalidHeader("block version too low"));
    }
    if pon_active && block.header.version < MIN_PON_BLOCK_VERSION {
        return Err(ValidationError::InvalidHeader("pon block version too low"));
    }
    if pon_active && !block.header.is_pon() {
        return Err(ValidationError::InvalidHeader(
            "pon upgrade active but header is not pon",
        ));
    }
    if !pon_active && block.header.is_pon() {
        return Err(ValidationError::InvalidHeader(
            "pon upgrade inactive but header is pon",
        ));
    }

    for upgrade in &params.upgrades {
        if height == upgrade.activation_height {
            if let Some(expected_hash) = upgrade.hash_activation_block {
                if block.header.hash() != expected_hash {
                    return Err(ValidationError::InvalidHeader(
                        "activation block hash mismatch",
                    ));
                }
            }
        }
    }

    if block.header.is_pon() {
        if flags.check_pon {
            pon_validation::validate_pon_header(&block.header, height, params)?;
        }
    } else if flags.check_pow {
        pow_validation::validate_pow_header(&block.header, height, params)?;
    }

    Ok(())
}

fn validate_merkle_root(block: &Block) -> Result<(), ValidationError> {
    let txids: Result<Vec<Hash256>, _> = block.transactions.iter().map(|tx| tx.txid()).collect();
    let (root, mutated) = merkle_root(&txids?);
    if mutated {
        return Err(ValidationError::DuplicateTransaction);
    }
    if root != block.header.merkle_root {
        return Err(ValidationError::MerkleMismatch);
    }
    Ok(())
}

fn validate_transaction(
    tx: &Transaction,
    is_coinbase: bool,
    height: i32,
    params: &ConsensusParams,
    branch_id: u32,
    flags: &ValidationFlags,
) -> Result<(), ValidationError> {
    let is_fluxnode = is_fluxnode_tx(tx);
    let has_joinsplit = !tx.join_splits.is_empty();
    let has_spends = !tx.shielded_spends.is_empty();
    let has_outputs = !tx.shielded_outputs.is_empty();

    if !is_fluxnode {
        if !tx.f_overwintered {
            if tx.version < SPROUT_MIN_TX_VERSION {
                return Err(ValidationError::InvalidTransaction(
                    "transaction version too low",
                ));
            }
        } else {
            if tx.version < OVERWINTER_MIN_TX_VERSION {
                return Err(ValidationError::InvalidTransaction(
                    "overwinter version too low",
                ));
            }
            if tx.version_group_id != fluxd_primitives::transaction::OVERWINTER_VERSION_GROUP_ID
                && tx.version_group_id != fluxd_primitives::transaction::SAPLING_VERSION_GROUP_ID
            {
                return Err(ValidationError::InvalidTransaction(
                    "unknown transaction version group id",
                ));
            }
            if tx.expiry_height >= TX_EXPIRY_HEIGHT_THRESHOLD {
                return Err(ValidationError::InvalidTransaction(
                    "expiry height is too high",
                ));
            }
        }

        if tx.vin.is_empty() && !has_joinsplit && !has_spends {
            return Err(ValidationError::InvalidTransaction(
                "transaction must have inputs or shielded spends",
            ));
        }
        if tx.vout.is_empty() && !has_joinsplit && !has_outputs {
            return Err(ValidationError::InvalidTransaction(
                "transaction must have outputs or shielded outputs",
            ));
        }
    }

    if is_fluxnode
        && (!tx.vout.is_empty() || has_joinsplit || has_outputs || !tx.vin.is_empty() || has_spends)
    {
        return Err(ValidationError::InvalidTransaction(
            "fluxnode transaction must not contain regular inputs or outputs",
        ));
    }

    let tx_size = tx.consensus_encode()?.len() as u32;
    let sapling_active = network_upgrade_active(height, &params.upgrades, UpgradeIndex::Acadia);
    if !sapling_active && tx_size > MAX_TX_SIZE_BEFORE_SAPLING {
        return Err(ValidationError::InvalidTransaction("transaction too large"));
    }
    if tx_size > MAX_TX_SIZE_AFTER_SAPLING {
        return Err(ValidationError::InvalidTransaction("transaction too large"));
    }

    if !is_fluxnode {
        if sapling_active {
            if tx.version >= SAPLING_MIN_TX_VERSION && !tx.f_overwintered {
                return Err(ValidationError::InvalidTransaction(
                    "overwintered flag must be set",
                ));
            }
            if tx.f_overwintered
                && tx.version_group_id != fluxd_primitives::transaction::SAPLING_VERSION_GROUP_ID
            {
                return Err(ValidationError::InvalidTransaction(
                    "invalid sapling version group id",
                ));
            }
            if tx.f_overwintered && tx.version < SAPLING_MIN_TX_VERSION {
                return Err(ValidationError::InvalidTransaction(
                    "sapling version too low",
                ));
            }
            if tx.f_overwintered && tx.version > SAPLING_MAX_TX_VERSION {
                return Err(ValidationError::InvalidTransaction(
                    "sapling version too high",
                ));
            }
            if !tx.f_overwintered {
                return Err(ValidationError::InvalidTransaction(
                    "sapling active but transaction not overwintered",
                ));
            }
            if is_expired_tx(tx, height) {
                return Err(ValidationError::InvalidTransaction("transaction expired"));
            }
        } else if tx.f_overwintered {
            return Err(ValidationError::InvalidTransaction(
                "overwinter is not active yet",
            ));
        }

        if network_upgrade_active(height, &params.upgrades, UpgradeIndex::Flux)
            && !tx.vin.is_empty()
            && (has_joinsplit || has_outputs)
        {
            return Err(ValidationError::InvalidTransaction(
                "shielded outputs disabled after flux rebrand",
            ));
        }
    }

    if is_coinbase {
        if tx.vin.len() != 1 || tx.vin[0].prevout != OutPoint::null() {
            return Err(ValidationError::InvalidTransaction(
                "coinbase must have exactly one null input",
            ));
        }
        if has_joinsplit {
            return Err(ValidationError::InvalidTransaction(
                "coinbase must not contain joinsplits",
            ));
        }
        if has_spends {
            return Err(ValidationError::InvalidTransaction(
                "coinbase must not contain shielded spends",
            ));
        }
        if has_outputs {
            return Err(ValidationError::InvalidTransaction(
                "coinbase must not contain shielded outputs",
            ));
        }
        let script_len = tx.vin[0].script_sig.len();
        if !(2..=100).contains(&script_len) {
            return Err(ValidationError::InvalidTransaction(
                "coinbase scriptSig length out of range",
            ));
        }
    } else if tx.vin.iter().any(|input| input.prevout == OutPoint::null()) {
        return Err(ValidationError::InvalidTransaction(
            "non-coinbase cannot contain null prevout",
        ));
    }

    let mut value_out = 0i64;
    for output in &tx.vout {
        if output.value < 0 || output.value > MAX_MONEY {
            return Err(ValidationError::ValueOutOfRange);
        }
        value_out = value_out
            .checked_add(output.value)
            .ok_or(ValidationError::ValueOutOfRange)?;
        if value_out > MAX_MONEY {
            return Err(ValidationError::ValueOutOfRange);
        }
    }

    if !has_spends && !has_outputs && tx.value_balance != 0 && !is_fluxnode {
        return Err(ValidationError::InvalidTransaction(
            "value balance with no shielded components",
        ));
    }
    if tx.value_balance > MAX_MONEY || tx.value_balance < -MAX_MONEY {
        return Err(ValidationError::ValueOutOfRange);
    }

    if tx.value_balance <= 0 {
        let balance = -tx.value_balance;
        value_out = value_out
            .checked_add(balance)
            .ok_or(ValidationError::ValueOutOfRange)?;
        if value_out > MAX_MONEY {
            return Err(ValidationError::ValueOutOfRange);
        }
    }

    for joinsplit in &tx.join_splits {
        if joinsplit.vpub_old < 0 || joinsplit.vpub_old > MAX_MONEY {
            return Err(ValidationError::ValueOutOfRange);
        }
        if joinsplit.vpub_new < 0 || joinsplit.vpub_new > MAX_MONEY {
            return Err(ValidationError::ValueOutOfRange);
        }
        if joinsplit.vpub_new != 0 && joinsplit.vpub_old != 0 {
            return Err(ValidationError::InvalidTransaction(
                "joinsplit vpub_old and vpub_new both non-zero",
            ));
        }
        value_out = value_out
            .checked_add(joinsplit.vpub_old)
            .ok_or(ValidationError::ValueOutOfRange)?;
        if value_out > MAX_MONEY {
            return Err(ValidationError::ValueOutOfRange);
        }
    }

    let mut value_in = 0i64;
    for joinsplit in &tx.join_splits {
        value_in = value_in
            .checked_add(joinsplit.vpub_new)
            .ok_or(ValidationError::ValueOutOfRange)?;
        if value_in > MAX_MONEY {
            return Err(ValidationError::ValueOutOfRange);
        }
    }
    if tx.value_balance >= 0 {
        value_in = value_in
            .checked_add(tx.value_balance)
            .ok_or(ValidationError::ValueOutOfRange)?;
        if value_in > MAX_MONEY {
            return Err(ValidationError::ValueOutOfRange);
        }
    }

    let mut seen_inputs = HashSet::new();
    for input in &tx.vin {
        if !seen_inputs.insert((input.prevout.hash, input.prevout.index)) {
            return Err(ValidationError::DuplicateInput);
        }
    }

    let mut sprout_nullifiers = HashSet::new();
    for joinsplit in &tx.join_splits {
        for nullifier in &joinsplit.nullifiers {
            if !sprout_nullifiers.insert(*nullifier) {
                return Err(ValidationError::InvalidTransaction(
                    "duplicate joinsplit nullifier",
                ));
            }
        }
    }

    let mut sapling_nullifiers = HashSet::new();
    for spend in &tx.shielded_spends {
        if !sapling_nullifiers.insert(spend.nullifier) {
            return Err(ValidationError::InvalidTransaction(
                "duplicate sapling nullifier",
            ));
        }
    }

    if let Some(fluxnode) = &tx.fluxnode {
        fluxnode_validation::validate_fluxnode_tx(fluxnode, tx.version)
            .map_err(ValidationError::Fluxnode)?;
    }
    if is_fluxnode {
        let kamata_active = network_upgrade_active(height, &params.upgrades, UpgradeIndex::Kamata);
        if !kamata_active {
            return Err(ValidationError::InvalidTransaction(
                "fluxnode transaction before kamata activation",
            ));
        }
        if tx.version == FLUXNODE_TX_UPGRADEABLE_VERSION {
            let p2sh_active =
                network_upgrade_active(height, &params.upgrades, UpgradeIndex::P2ShNodes);
            if !p2sh_active {
                return Err(ValidationError::InvalidTransaction(
                    "fluxnode upgrade transaction before p2sh nodes activation",
                ));
            }
            if let Some(flux_version) = fluxnode_start_version(tx) {
                let pon_active =
                    network_upgrade_active(height, &params.upgrades, UpgradeIndex::Pon);
                if !pon_active {
                    if flux_version != FLUXNODE_INTERNAL_NORMAL_TX_VERSION
                        && flux_version != FLUXNODE_INTERNAL_P2SH_TX_VERSION
                    {
                        return Err(ValidationError::InvalidTransaction(
                            "fluxnode tx version bits not active yet",
                        ));
                    }
                    if has_flux_tx_delegates_feature(flux_version) {
                        return Err(ValidationError::InvalidTransaction(
                            "fluxnode delegates feature not active yet",
                        ));
                    }
                }
            }
        }
    }

    if flags.check_shielded && (has_joinsplit || has_spends || has_outputs) {
        let params = flags.shielded_params.as_ref().ok_or_else(|| {
            ValidationError::Shielded(ShieldedError::MissingParams(
                "shielded parameters not loaded".to_string(),
            ))
        })?;
        let shielded_start = Instant::now();
        verify_transaction(tx, branch_id, params)?;
        if let Some(metrics) = flags.metrics.as_ref() {
            metrics.record_shielded(shielded_start.elapsed());
        }
    }

    Ok(())
}

fn is_fluxnode_tx(tx: &Transaction) -> bool {
    tx.version == FLUXNODE_TX_VERSION || tx.version == FLUXNODE_TX_UPGRADEABLE_VERSION
}

fn is_coinbase_tx(tx: &Transaction) -> bool {
    tx.vin.len() == 1 && tx.vin[0].prevout == OutPoint::null()
}

fn is_expired_tx(tx: &Transaction, height: i32) -> bool {
    if tx.expiry_height == 0 || is_coinbase_tx(tx) || height < 0 {
        return false;
    }
    (height as u32) > tx.expiry_height
}

fn merkle_root(txids: &[Hash256]) -> (Hash256, bool) {
    if txids.is_empty() {
        return ([0u8; 32], false);
    }
    let mut layer = txids.to_vec();
    let mut mutated = false;
    while layer.len() > 1 {
        let size = layer.len();
        let mut next = Vec::with_capacity(size.div_ceil(2));
        let mut i = 0usize;
        while i < size {
            let i2 = if i + 1 < size { i + 1 } else { i };
            if i2 == i + 1 && i2 + 1 == size && layer[i] == layer[i2] {
                mutated = true;
            }
            let mut data = Vec::with_capacity(64);
            data.extend_from_slice(&layer[i]);
            data.extend_from_slice(&layer[i2]);
            next.push(sha256d(&data));
            i += 2;
        }
        layer = next;
    }
    (layer[0], mutated)
}

pub fn index_transactions(block: &Block) -> Result<HashMap<Hash256, usize>, ValidationError> {
    let mut map = HashMap::new();
    for (index, tx) in block.transactions.iter().enumerate() {
        let txid = tx.txid()?;
        map.insert(txid, index);
    }
    Ok(map)
}

fn is_final_tx(tx: &Transaction, height: i32, block_time: i64) -> bool {
    const LOCKTIME_THRESHOLD: i64 = 500_000_000;
    if tx.lock_time == 0 {
        return true;
    }
    let lock_time = tx.lock_time as i64;
    let compare = if lock_time < LOCKTIME_THRESHOLD {
        height as i64
    } else {
        block_time
    };
    if lock_time < compare {
        return true;
    }
    tx.vin.iter().all(|input| input.sequence == u32::MAX)
}

fn coinbase_height_matches(tx: &Transaction, height: i32) -> bool {
    if tx.vin.is_empty() {
        return false;
    }
    let expected = script_push_int(height as i64);
    tx.vin[0].script_sig.starts_with(&expected)
}

fn script_push_int(value: i64) -> Vec<u8> {
    const OP_0: u8 = 0x00;
    const OP_1NEGATE: u8 = 0x4f;
    const OP_1: u8 = 0x51;
    if value == 0 {
        return vec![OP_0];
    }
    if value == -1 {
        return vec![OP_1NEGATE];
    }
    if (1..=16).contains(&value) {
        return vec![OP_1 + (value as u8 - 1)];
    }
    let data = script_num_to_vec(value);
    let mut script = Vec::new();
    push_data(&mut script, &data);
    script
}

fn script_num_to_vec(value: i64) -> Vec<u8> {
    if value == 0 {
        return Vec::new();
    }
    let mut abs = value.unsigned_abs();
    let mut result = Vec::new();
    while abs > 0 {
        result.push((abs & 0xff) as u8);
        abs >>= 8;
    }
    let sign_bit = 0x80u8;
    if let Some(last) = result.last_mut() {
        if (*last & sign_bit) != 0 {
            result.push(if value < 0 { sign_bit } else { 0 });
        } else if value < 0 {
            *last |= sign_bit;
        }
    }
    result
}

fn push_data(script: &mut Vec<u8>, data: &[u8]) {
    const OP_PUSHDATA1: u8 = 0x4c;
    const OP_PUSHDATA2: u8 = 0x4d;
    const OP_PUSHDATA4: u8 = 0x4e;

    if data.len() < OP_PUSHDATA1 as usize {
        script.push(data.len() as u8);
    } else if data.len() <= u8::MAX as usize {
        script.push(OP_PUSHDATA1);
        script.push(data.len() as u8);
    } else if data.len() <= u16::MAX as usize {
        script.push(OP_PUSHDATA2);
        script.extend_from_slice(&(data.len() as u16).to_le_bytes());
    } else {
        script.push(OP_PUSHDATA4);
        script.extend_from_slice(&(data.len() as u32).to_le_bytes());
    }
    script.extend_from_slice(data);
}

fn block_sigops(block: &Block) -> u32 {
    block
        .transactions
        .iter()
        .map(|tx| {
            let input_ops: u32 = tx
                .vin
                .iter()
                .map(|input| legacy_sigops(&input.script_sig))
                .sum();
            let output_ops: u32 = tx
                .vout
                .iter()
                .map(|output| legacy_sigops(&output.script_pubkey))
                .sum();
            input_ops + output_ops
        })
        .sum()
}

fn legacy_sigops(script: &[u8]) -> u32 {
    const OP_CHECKSIG: u8 = 0xac;
    const OP_CHECKSIGVERIFY: u8 = 0xad;
    const OP_CHECKMULTISIG: u8 = 0xae;
    const OP_CHECKMULTISIGVERIFY: u8 = 0xaf;
    const OP_PUSHDATA1: u8 = 0x4c;
    const OP_PUSHDATA2: u8 = 0x4d;
    const OP_PUSHDATA4: u8 = 0x4e;

    let mut count = 0u32;
    let mut cursor = 0usize;
    while cursor < script.len() {
        let opcode = script[cursor];
        cursor += 1;
        match opcode {
            OP_CHECKSIG | OP_CHECKSIGVERIFY => count += 1,
            OP_CHECKMULTISIG | OP_CHECKMULTISIGVERIFY => count += 20,
            0x01..=0x4b => {
                let len = opcode as usize;
                if cursor + len > script.len() {
                    break;
                }
                cursor += len;
            }
            OP_PUSHDATA1 => {
                if cursor >= script.len() {
                    break;
                }
                let len = script[cursor] as usize;
                cursor += 1;
                if cursor + len > script.len() {
                    break;
                }
                cursor += len;
            }
            OP_PUSHDATA2 => {
                if cursor + 2 > script.len() {
                    break;
                }
                let len = u16::from_le_bytes([script[cursor], script[cursor + 1]]) as usize;
                cursor += 2;
                if cursor + len > script.len() {
                    break;
                }
                cursor += len;
            }
            OP_PUSHDATA4 => {
                if cursor + 4 > script.len() {
                    break;
                }
                let len = u32::from_le_bytes([
                    script[cursor],
                    script[cursor + 1],
                    script[cursor + 2],
                    script[cursor + 3],
                ]) as usize;
                cursor += 4;
                if cursor + len > script.len() {
                    break;
                }
                cursor += len;
            }
            _ => {}
        }
    }
    count
}

fn fluxnode_start_version(tx: &Transaction) -> Option<i32> {
    match tx.fluxnode.as_ref()? {
        FluxnodeTx::V6(FluxnodeTxV6::Start(start)) => Some(start.flux_tx_version),
        _ => None,
    }
}

fn fluxnode_collateral_outpoint(tx: &Transaction) -> Option<(Hash256, u32)> {
    let fluxnode = tx.fluxnode.as_ref()?;
    let outpoint = match fluxnode {
        FluxnodeTx::V5(FluxnodeTxV5::Start(start)) => start.collateral.clone(),
        FluxnodeTx::V5(FluxnodeTxV5::Confirm(confirm)) => confirm.collateral.clone(),
        FluxnodeTx::V6(FluxnodeTxV6::Start(start)) => match &start.variant {
            FluxnodeStartVariantV6::Normal { collateral, .. } => collateral.clone(),
            FluxnodeStartVariantV6::P2sh { collateral, .. } => collateral.clone(),
        },
        FluxnodeTx::V6(FluxnodeTxV6::Confirm(confirm)) => confirm.collateral.clone(),
    };
    Some((outpoint.hash, outpoint.index))
}

#[cfg(test)]
mod tests {
    use super::merkle_root;

    fn hash(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn merkle_mutation_only_on_last_pair() {
        let txids = vec![hash(1), hash(1), hash(2), hash(3)];
        let (_, mutated) = merkle_root(&txids);
        assert!(!mutated, "non-terminal duplicate should not mark mutation");
    }

    #[test]
    fn merkle_mutation_detects_terminal_pair() {
        let txids = vec![hash(1), hash(2), hash(3), hash(3)];
        let (_, mutated) = merkle_root(&txids);
        assert!(mutated, "terminal duplicate should mark mutation");
    }

    #[test]
    fn merkle_mutation_ignores_odd_duplication() {
        let txids = vec![hash(1), hash(2), hash(3)];
        let (_, mutated) = merkle_root(&txids);
        assert!(!mutated, "odd-length duplication should not mark mutation");
    }
}
