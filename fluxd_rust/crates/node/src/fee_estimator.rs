use std::collections::HashMap;
use std::fs;
use std::path::Path;

use fluxd_consensus::money::{COIN, MAX_MONEY};
use fluxd_consensus::Hash256;
use fluxd_primitives::encoding::{Decoder, Encoder};

const FEE_ESTIMATES_FILE_VERSION: u32 = 2;

const DEFAULT_DECAY: f64 = 0.998;
const MAX_BLOCK_CONFIRMS: u32 = 25;

const MIN_SUCCESS_PCT: f64 = 0.85;
const UNLIKELY_PCT: f64 = 0.50;

const SUFFICIENT_FEETXS: f64 = 1.0;
const SUFFICIENT_PRITXS: f64 = 0.2;

const MIN_FEERATE_PER_KB: i64 = 10;
const MAX_FEERATE_PER_KB: f64 = 1e7;
const INF_FEERATE_PER_KB: f64 = MAX_MONEY as f64;

const MIN_PRIORITY: f64 = 10.0;
const MAX_PRIORITY: f64 = 1e16;
const INF_PRIORITY: f64 = 1e9 * MAX_MONEY as f64;

const FEE_SPACING: f64 = 1.1;
const PRIORITY_SPACING: f64 = 2.0;

fn allow_free_threshold() -> f64 {
    (COIN * 144 / 250) as f64
}

#[derive(Clone, Copy, Debug)]
pub struct MempoolTxInfo {
    pub txid: Hash256,
    pub height: u32,
    pub fee: i64,
    pub size: usize,
    pub starting_priority: f64,
    pub was_clear_at_entry: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct BlockTxInfo {
    pub fee: i64,
    pub size: usize,
    pub height: u32,
    pub priority: f64,
    pub was_clear_at_entry: bool,
}

#[derive(Clone, Copy, Debug)]
enum TrackedStats {
    Fee,
    Priority,
}

#[derive(Clone, Copy, Debug)]
struct TxStatsInfo {
    stats: TrackedStats,
    block_height: u32,
    bucket_index: usize,
}

#[derive(Clone, Debug)]
struct TxConfirmStats {
    buckets: Vec<f64>,
    tx_ct_avg: Vec<f64>,
    cur_block_tx_ct: Vec<i32>,
    conf_avg: Vec<Vec<f64>>,
    cur_block_conf: Vec<Vec<i32>>,
    avg: Vec<f64>,
    cur_block_val: Vec<f64>,
    decay: f64,
    unconf_txs: Vec<Vec<i32>>,
    old_unconf_txs: Vec<i32>,
}

impl TxConfirmStats {
    fn initialize(default_buckets: &[f64], max_confirms: u32, decay: f64) -> Self {
        let mut buckets = Vec::with_capacity(default_buckets.len() + 1);
        buckets.extend_from_slice(default_buckets);
        buckets.push(f64::INFINITY);

        let bucket_count = buckets.len();
        let max_confirms = usize::try_from(max_confirms.max(1)).unwrap_or(1);

        let conf_avg = vec![vec![0.0; bucket_count]; max_confirms];
        let cur_block_conf = vec![vec![0; bucket_count]; max_confirms];
        let unconf_txs = vec![vec![0; bucket_count]; max_confirms];

        Self {
            buckets,
            tx_ct_avg: vec![0.0; bucket_count],
            cur_block_tx_ct: vec![0; bucket_count],
            conf_avg,
            cur_block_conf,
            avg: vec![0.0; bucket_count],
            cur_block_val: vec![0.0; bucket_count],
            decay,
            unconf_txs,
            old_unconf_txs: vec![0; bucket_count],
        }
    }

    fn max_confirms(&self) -> u32 {
        u32::try_from(self.conf_avg.len()).unwrap_or(u32::MAX)
    }

    fn find_bucket_index(&self, val: f64) -> usize {
        if !val.is_finite() {
            return self.buckets.len().saturating_sub(1);
        }
        match self.buckets.binary_search_by(|probe| probe.total_cmp(&val)) {
            Ok(idx) => idx,
            Err(idx) => idx.min(self.buckets.len().saturating_sub(1)),
        }
    }

    fn clear_current(&mut self, block_height: u32) {
        if self.unconf_txs.is_empty() {
            return;
        }
        let block_index = usize::try_from(block_height % self.max_confirms()).unwrap_or(0);

        for bucket in 0..self.buckets.len() {
            self.old_unconf_txs[bucket] =
                self.old_unconf_txs[bucket].saturating_add(self.unconf_txs[block_index][bucket]);
            self.unconf_txs[block_index][bucket] = 0;
            for conf in &mut self.cur_block_conf {
                conf[bucket] = 0;
            }
            self.cur_block_tx_ct[bucket] = 0;
            self.cur_block_val[bucket] = 0.0;
        }
    }

    fn record(&mut self, blocks_to_confirm: u32, val: f64) {
        if blocks_to_confirm < 1 {
            return;
        }
        let Some(max_confirms) = usize::try_from(self.max_confirms()).ok() else {
            return;
        };
        let bucket_index = self.find_bucket_index(val);
        let blocks_to_confirm = usize::try_from(blocks_to_confirm).unwrap_or(usize::MAX);
        for idx in blocks_to_confirm..=max_confirms {
            let confirm_index = idx.saturating_sub(1);
            let Some(row) = self.cur_block_conf.get_mut(confirm_index) else {
                continue;
            };
            if let Some(cell) = row.get_mut(bucket_index) {
                *cell = cell.saturating_add(1);
            }
        }
        if let Some(cell) = self.cur_block_tx_ct.get_mut(bucket_index) {
            *cell = cell.saturating_add(1);
        }
        if let Some(cell) = self.cur_block_val.get_mut(bucket_index) {
            *cell += val;
        }
    }

    fn new_tx(&mut self, block_height: u32, val: f64) -> usize {
        let bucket_index = self.find_bucket_index(val);
        if self.unconf_txs.is_empty() {
            return bucket_index;
        }
        let block_index = usize::try_from(block_height % self.max_confirms()).unwrap_or(0);
        if let Some(cell) = self
            .unconf_txs
            .get_mut(block_index)
            .and_then(|row| row.get_mut(bucket_index))
        {
            *cell = cell.saturating_add(1);
        }
        bucket_index
    }

    fn remove_tx(&mut self, entry_height: u32, best_seen_height: u32, bucket_index: usize) {
        let mut blocks_ago = best_seen_height as i64 - entry_height as i64;
        if best_seen_height == 0 {
            blocks_ago = 0;
        }
        if blocks_ago < 0 {
            return;
        }

        let bins = self.unconf_txs.len();
        if bins == 0 {
            return;
        }
        if blocks_ago as usize >= bins {
            if let Some(cell) = self.old_unconf_txs.get_mut(bucket_index) {
                *cell = cell.saturating_sub(1);
            }
            return;
        }

        let entry_index = usize::try_from(entry_height % self.max_confirms()).unwrap_or(0);
        if let Some(cell) = self
            .unconf_txs
            .get_mut(entry_index)
            .and_then(|row| row.get_mut(bucket_index))
        {
            *cell = cell.saturating_sub(1);
        }
    }

    fn update_moving_averages(&mut self) {
        for bucket in 0..self.buckets.len() {
            for conf in 0..self.conf_avg.len() {
                self.conf_avg[conf][bucket] = self.conf_avg[conf][bucket] * self.decay
                    + self.cur_block_conf[conf][bucket] as f64;
            }
            self.avg[bucket] = self.avg[bucket] * self.decay + self.cur_block_val[bucket];
            self.tx_ct_avg[bucket] =
                self.tx_ct_avg[bucket] * self.decay + self.cur_block_tx_ct[bucket] as f64;
        }
    }

    fn estimate_median_val(
        &self,
        conf_target: u32,
        sufficient_tx_val: f64,
        success_break_point: f64,
        require_greater: bool,
        block_height: u32,
    ) -> f64 {
        let Some(conf_target) = usize::try_from(conf_target).ok() else {
            return -1.0;
        };
        if conf_target == 0 || conf_target > self.conf_avg.len() {
            return -1.0;
        }
        if self.buckets.is_empty() {
            return -1.0;
        }

        let mut n_conf = 0.0;
        let mut total_num = 0.0;
        let mut extra_num: i32 = 0;

        let max_bucket_index = self.buckets.len().saturating_sub(1);
        let start_bucket = if require_greater { max_bucket_index } else { 0 };
        let step: i32 = if require_greater { -1 } else { 1 };

        let mut cur_near_bucket = start_bucket;
        let mut best_near_bucket = start_bucket;
        let mut best_far_bucket = start_bucket;

        let mut found_answer = false;
        let bins = self.unconf_txs.len();

        let mut bucket = start_bucket as i32;
        while bucket >= 0 && (bucket as usize) <= max_bucket_index {
            let bucket_usize = bucket as usize;
            n_conf += self.conf_avg[conf_target - 1][bucket_usize];
            total_num += self.tx_ct_avg[bucket_usize];

            for confct in conf_target..self.conf_avg.len() {
                if bins == 0 {
                    break;
                }
                let index = (block_height.wrapping_sub(confct as u32) % bins as u32) as usize;
                extra_num = extra_num.saturating_add(self.unconf_txs[index][bucket_usize]);
            }
            extra_num = extra_num.saturating_add(self.old_unconf_txs[bucket_usize]);

            if total_num >= sufficient_tx_val / (1.0 - self.decay) {
                let cur_pct = n_conf / (total_num + extra_num as f64);

                if require_greater && cur_pct < success_break_point {
                    break;
                }
                if !require_greater && cur_pct > success_break_point {
                    break;
                }

                found_answer = true;
                n_conf = 0.0;
                total_num = 0.0;
                extra_num = 0;
                best_near_bucket = cur_near_bucket;
                best_far_bucket = bucket_usize;

                let next = bucket.saturating_add(step);
                if let Some(next_bucket) = usize::try_from(next).ok() {
                    cur_near_bucket = next_bucket;
                }
            }

            bucket = bucket.saturating_add(step);
        }

        let min_bucket = best_near_bucket.min(best_far_bucket);
        let max_bucket = best_near_bucket.max(best_far_bucket);

        let mut tx_sum = 0.0;
        for idx in min_bucket..=max_bucket {
            tx_sum += self.tx_ct_avg[idx];
        }

        if !found_answer || tx_sum == 0.0 {
            return -1.0;
        }

        tx_sum /= 2.0;
        for idx in min_bucket..=max_bucket {
            if self.tx_ct_avg[idx] < tx_sum {
                tx_sum -= self.tx_ct_avg[idx];
            } else {
                return self.avg[idx] / self.tx_ct_avg[idx];
            }
        }

        -1.0
    }

    fn encode(&self, encoder: &mut Encoder) {
        write_f64(encoder, self.decay);
        write_vec_f64(encoder, &self.buckets);
        write_vec_f64(encoder, &self.avg);
        write_vec_f64(encoder, &self.tx_ct_avg);
        write_vec_vec_f64(encoder, &self.conf_avg);
    }

    fn decode(decoder: &mut Decoder) -> Result<Self, String> {
        let decay = read_f64(decoder)?;
        if !(0.0..1.0).contains(&decay) {
            return Err("corrupt fee estimates file: decay must be between 0 and 1".to_string());
        }

        let buckets = read_vec_f64(decoder)?;
        let num_buckets = buckets.len();
        if num_buckets <= 1 || num_buckets > 1000 {
            return Err("corrupt fee estimates file: invalid bucket count".to_string());
        }
        let avg = read_vec_f64(decoder)?;
        if avg.len() != num_buckets {
            return Err("corrupt fee estimates file: mismatch in average bucket count".to_string());
        }
        let tx_ct_avg = read_vec_f64(decoder)?;
        if tx_ct_avg.len() != num_buckets {
            return Err(
                "corrupt fee estimates file: mismatch in tx count bucket count".to_string(),
            );
        }
        let conf_avg = read_vec_vec_f64(decoder)?;
        let max_confirms = conf_avg.len();
        if max_confirms == 0 || max_confirms > 6 * 24 * 7 {
            return Err("corrupt fee estimates file: invalid max confirms".to_string());
        }
        if max_confirms != usize::try_from(MAX_BLOCK_CONFIRMS).unwrap_or(25) {
            return Err("corrupt fee estimates file: unsupported max confirms".to_string());
        }
        for row in &conf_avg {
            if row.len() != num_buckets {
                return Err(
                    "corrupt fee estimates file: mismatch in conf average bucket count".to_string(),
                );
            }
        }

        let cur_block_conf = vec![vec![0; num_buckets]; max_confirms];
        let unconf_txs = vec![vec![0; num_buckets]; max_confirms];

        Ok(Self {
            buckets,
            tx_ct_avg,
            cur_block_tx_ct: vec![0; num_buckets],
            conf_avg,
            cur_block_conf,
            avg,
            cur_block_val: vec![0.0; num_buckets],
            decay,
            unconf_txs,
            old_unconf_txs: vec![0; num_buckets],
        })
    }
}

fn fee_rate_per_kb(fee: i64, size: usize) -> i64 {
    if fee <= 0 {
        return 0;
    }
    let size = i64::try_from(size.max(1)).unwrap_or(i64::MAX);
    fee.saturating_mul(1000).saturating_div(size)
}

pub struct FeeEstimator {
    min_tracked_fee_per_kb: i64,
    min_tracked_priority: f64,
    best_seen_height: u32,
    fee_stats: TxConfirmStats,
    pri_stats: TxConfirmStats,
    mempool_txs: HashMap<Hash256, TxStatsInfo>,
    fee_unlikely: f64,
    fee_likely: f64,
    pri_unlikely: f64,
    pri_likely: f64,
    revision: u64,
}

impl FeeEstimator {
    pub fn new(min_relay_fee_per_kb: i64) -> Self {
        let min_tracked_fee_per_kb = min_relay_fee_per_kb.max(MIN_FEERATE_PER_KB);
        let mut fee_buckets = Vec::new();
        let mut fee_boundary = min_tracked_fee_per_kb as f64;
        while fee_boundary <= MAX_FEERATE_PER_KB {
            fee_buckets.push(fee_boundary);
            fee_boundary *= FEE_SPACING;
        }

        let min_tracked_priority = allow_free_threshold().max(MIN_PRIORITY);
        let mut priority_buckets = Vec::new();
        let mut priority_boundary = min_tracked_priority;
        while priority_boundary <= MAX_PRIORITY {
            priority_buckets.push(priority_boundary);
            priority_boundary *= PRIORITY_SPACING;
        }

        Self {
            min_tracked_fee_per_kb,
            min_tracked_priority,
            best_seen_height: 0,
            fee_stats: TxConfirmStats::initialize(&fee_buckets, MAX_BLOCK_CONFIRMS, DEFAULT_DECAY),
            pri_stats: TxConfirmStats::initialize(
                &priority_buckets,
                MAX_BLOCK_CONFIRMS,
                DEFAULT_DECAY,
            ),
            mempool_txs: HashMap::new(),
            fee_unlikely: 0.0,
            fee_likely: INF_FEERATE_PER_KB,
            pri_unlikely: 0.0,
            pri_likely: INF_PRIORITY,
            revision: 0,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn load(path: &Path, min_relay_fee_per_kb: i64) -> Result<Self, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::new(min_relay_fee_per_kb));
            }
            Err(err) => return Err(err.to_string()),
        };

        let mut decoder = Decoder::new(&bytes);
        let version = decoder
            .read_u32_le()
            .map_err(|err| format!("invalid fee estimates file: {err}"))?;
        if version != FEE_ESTIMATES_FILE_VERSION {
            return Err(format!(
                "unsupported fee estimates file version {version} (expected {FEE_ESTIMATES_FILE_VERSION})"
            ));
        }

        let best_seen_height = decoder
            .read_u32_le()
            .map_err(|err| format!("invalid fee estimates file: {err}"))?;
        let fee_stats =
            TxConfirmStats::decode(&mut decoder).map_err(|err| format!("fee stats: {err}"))?;
        let pri_stats =
            TxConfirmStats::decode(&mut decoder).map_err(|err| format!("priority stats: {err}"))?;

        if !decoder.is_empty() {
            return Err("invalid fee estimates file: trailing bytes".to_string());
        }

        let mut estimator = Self::new(min_relay_fee_per_kb);
        estimator.best_seen_height = best_seen_height;
        estimator.fee_stats = fee_stats;
        estimator.pri_stats = pri_stats;
        Ok(estimator)
    }

    pub fn save(&self, path: &Path) -> Result<usize, String> {
        let mut encoder = Encoder::new();
        encoder.write_u32_le(FEE_ESTIMATES_FILE_VERSION);
        encoder.write_u32_le(self.best_seen_height);
        self.fee_stats.encode(&mut encoder);
        self.pri_stats.encode(&mut encoder);
        let bytes = encoder.into_inner();
        let len = bytes.len();
        crate::write_file_atomic(path, &bytes)?;
        Ok(len)
    }

    pub fn process_transaction(&mut self, tx: MempoolTxInfo, current_estimate: bool) {
        if self.mempool_txs.contains_key(&tx.txid) {
            return;
        }
        if tx.height < self.best_seen_height {
            return;
        }
        if !current_estimate {
            return;
        }
        if !tx.was_clear_at_entry {
            return;
        }

        let fee_rate = fee_rate_per_kb(tx.fee, tx.size);
        let priority = tx.starting_priority;

        if tx.fee == 0 || self.is_priority_data_point(fee_rate, priority) {
            let bucket_index = self.pri_stats.new_tx(tx.height, priority);
            self.mempool_txs.insert(
                tx.txid,
                TxStatsInfo {
                    stats: TrackedStats::Priority,
                    block_height: tx.height,
                    bucket_index,
                },
            );
        } else if self.is_fee_data_point(fee_rate, priority) {
            let bucket_index = self.fee_stats.new_tx(tx.height, fee_rate as f64);
            self.mempool_txs.insert(
                tx.txid,
                TxStatsInfo {
                    stats: TrackedStats::Fee,
                    block_height: tx.height,
                    bucket_index,
                },
            );
        }
    }

    pub fn remove_transaction(&mut self, txid: &Hash256) {
        let Some(info) = self.mempool_txs.remove(txid) else {
            return;
        };
        match info.stats {
            TrackedStats::Fee => self.fee_stats.remove_tx(
                info.block_height,
                self.best_seen_height,
                info.bucket_index,
            ),
            TrackedStats::Priority => self.pri_stats.remove_tx(
                info.block_height,
                self.best_seen_height,
                info.bucket_index,
            ),
        }
    }

    pub fn process_block(
        &mut self,
        block_height: u32,
        entries: &[BlockTxInfo],
        current_estimate: bool,
    ) {
        if block_height <= self.best_seen_height {
            return;
        }
        self.best_seen_height = block_height;

        if !current_estimate {
            return;
        }

        self.pri_likely = self
            .pri_stats
            .estimate_median_val(2, SUFFICIENT_PRITXS, MIN_SUCCESS_PCT, true, block_height)
            .max(0.0);
        if self.pri_likely == 0.0 {
            self.pri_likely = INF_PRIORITY;
        }

        let fee_likely = self.fee_stats.estimate_median_val(
            2,
            SUFFICIENT_FEETXS,
            MIN_SUCCESS_PCT,
            true,
            block_height,
        );
        self.fee_likely = if fee_likely < 0.0 {
            INF_FEERATE_PER_KB
        } else {
            fee_likely
        };

        self.pri_unlikely = self
            .pri_stats
            .estimate_median_val(10, SUFFICIENT_PRITXS, UNLIKELY_PCT, false, block_height)
            .max(0.0);

        let fee_unlikely = self.fee_stats.estimate_median_val(
            10,
            SUFFICIENT_FEETXS,
            UNLIKELY_PCT,
            false,
            block_height,
        );
        self.fee_unlikely = if fee_unlikely < 0.0 {
            0.0
        } else {
            fee_unlikely
        };

        self.fee_stats.clear_current(block_height);
        self.pri_stats.clear_current(block_height);

        for entry in entries {
            self.process_block_tx(block_height, *entry);
        }

        self.fee_stats.update_moving_averages();
        self.pri_stats.update_moving_averages();

        self.revision = self.revision.saturating_add(1);
    }

    pub fn estimate_fee_per_kb(&self, target_blocks: u32) -> Option<i64> {
        if target_blocks < 1 || target_blocks > self.fee_stats.max_confirms() {
            return None;
        }
        let median = self.fee_stats.estimate_median_val(
            target_blocks,
            SUFFICIENT_FEETXS,
            MIN_SUCCESS_PCT,
            true,
            self.best_seen_height,
        );
        if median <= 0.0 || !median.is_finite() {
            return None;
        }
        let median = median.min(i64::MAX as f64);
        Some(median as i64)
    }

    pub fn estimate_priority(&self, target_blocks: u32) -> Option<f64> {
        if target_blocks < 1 || target_blocks > self.pri_stats.max_confirms() {
            return None;
        }
        let estimate = self.pri_stats.estimate_median_val(
            target_blocks,
            SUFFICIENT_PRITXS,
            MIN_SUCCESS_PCT,
            true,
            self.best_seen_height,
        );
        if estimate < 0.0 || !estimate.is_finite() {
            return None;
        }
        Some(estimate)
    }

    fn is_fee_data_point(&self, fee_rate_per_kb: i64, priority: f64) -> bool {
        let fee_rate = fee_rate_per_kb as f64;
        (priority < self.min_tracked_priority && fee_rate >= self.min_tracked_fee_per_kb as f64)
            || (priority < self.pri_unlikely && fee_rate > self.fee_likely)
    }

    fn is_priority_data_point(&self, fee_rate_per_kb: i64, priority: f64) -> bool {
        let fee_rate = fee_rate_per_kb as f64;
        (fee_rate < self.min_tracked_fee_per_kb as f64 && priority >= self.min_tracked_priority)
            || (fee_rate < self.fee_unlikely && priority > self.pri_likely)
    }

    fn process_block_tx(&mut self, block_height: u32, entry: BlockTxInfo) {
        if !entry.was_clear_at_entry {
            return;
        }
        let blocks_to_confirm = match block_height.checked_sub(entry.height) {
            Some(delta) => delta,
            None => return,
        };
        if blocks_to_confirm == 0 {
            return;
        }

        let fee_rate = fee_rate_per_kb(entry.fee, entry.size);
        if entry.fee == 0 || self.is_priority_data_point(fee_rate, entry.priority) {
            self.pri_stats.record(blocks_to_confirm, entry.priority);
        } else if self.is_fee_data_point(fee_rate, entry.priority) {
            self.fee_stats.record(blocks_to_confirm, fee_rate as f64);
        }
    }
}

fn write_f64(encoder: &mut Encoder, value: f64) {
    encoder.write_u64_le(value.to_bits());
}

fn read_f64(decoder: &mut Decoder<'_>) -> Result<f64, String> {
    let raw = decoder
        .read_u64_le()
        .map_err(|err| format!("invalid fee estimates file: {err}"))?;
    Ok(f64::from_bits(raw))
}

fn write_vec_f64(encoder: &mut Encoder, values: &[f64]) {
    encoder.write_varint(values.len() as u64);
    for value in values {
        write_f64(encoder, *value);
    }
}

fn read_vec_f64(decoder: &mut Decoder<'_>) -> Result<Vec<f64>, String> {
    let count = decoder
        .read_varint()
        .map_err(|err| format!("invalid fee estimates file: {err}"))?;
    let count =
        usize::try_from(count).map_err(|_| "fee estimates file count too large".to_string())?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(read_f64(decoder)?);
    }
    Ok(out)
}

fn write_vec_vec_f64(encoder: &mut Encoder, values: &[Vec<f64>]) {
    encoder.write_varint(values.len() as u64);
    for row in values {
        write_vec_f64(encoder, row);
    }
}

fn read_vec_vec_f64(decoder: &mut Decoder<'_>) -> Result<Vec<Vec<f64>>, String> {
    let count = decoder
        .read_varint()
        .map_err(|err| format!("invalid fee estimates file: {err}"))?;
    let count =
        usize::try_from(count).map_err(|_| "fee estimates file count too large".to_string())?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(read_vec_f64(decoder)?);
    }
    Ok(out)
}
