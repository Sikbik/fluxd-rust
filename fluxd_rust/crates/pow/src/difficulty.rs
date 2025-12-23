//! Difficulty and compact target utilities.

use std::cmp::Ordering;

use fluxd_consensus::upgrades::UpgradeIndex;
use fluxd_consensus::{ConsensusParams, Hash256};
use primitive_types::U256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactError {
    Negative,
    Overflow,
}

impl std::fmt::Display for CompactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompactError::Negative => write!(f, "compact target has negative sign bit"),
            CompactError::Overflow => write!(f, "compact target overflows 256-bit range"),
        }
    }
}

impl std::error::Error for CompactError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DifficultyError {
    EmptyChain,
    NonContiguous,
    Compact(CompactError),
}

impl std::fmt::Display for DifficultyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DifficultyError::EmptyChain => write!(f, "no headers available"),
            DifficultyError::NonContiguous => write!(f, "header list must be contiguous by height"),
            DifficultyError::Compact(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for DifficultyError {}

impl From<CompactError> for DifficultyError {
    fn from(err: CompactError) -> Self {
        DifficultyError::Compact(err)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HeaderInfo {
    pub height: i64,
    pub time: i64,
    pub bits: u32,
}

pub fn compact_to_u256(bits: u32) -> Result<U256, CompactError> {
    let size = (bits >> 24) as u32;
    let mut word = bits & 0x007f_ffff;
    let negative = (bits & 0x0080_0000) != 0;

    if negative {
        return Err(CompactError::Negative);
    }

    let value = if size <= 3 {
        let shift = 8 * (3 - size);
        word >>= shift;
        U256::from(word)
    } else {
        let shift = 8 * (size - 3);
        U256::from(word) << shift
    };

    if word != 0 {
        let overflow = size > 34
            || (word > 0xff && size > 33)
            || (word > 0xffff && size > 32);
        if overflow {
            return Err(CompactError::Overflow);
        }
    }

    Ok(value)
}

pub fn u256_to_compact(value: U256) -> u32 {
    if value.is_zero() {
        return 0;
    }

    let mut size = ((value.bits() + 7) / 8) as u32;
    let mut compact: u32;

    if size <= 3 {
        compact = value.low_u32() << (8 * (3 - size));
    } else {
        let shift = 8 * (size - 3);
        compact = (value >> shift).low_u32();
    }

    if (compact & 0x0080_0000) != 0 {
        compact >>= 8;
        size += 1;
    }

    (size << 24) | (compact & 0x007f_ffff)
}

pub fn compact_to_target(bits: u32) -> Result<Hash256, CompactError> {
    let value = compact_to_u256(bits)?;
    Ok(u256_to_hash(value))
}

pub fn target_to_compact(target: &Hash256) -> u32 {
    let value = U256::from_little_endian(target);
    u256_to_compact(value)
}

pub fn hash_meets_target(hash: &Hash256, target: &Hash256) -> bool {
    let hash_value = U256::from_little_endian(hash);
    let target_value = U256::from_little_endian(target);
    hash_value <= target_value
}

pub fn block_proof(bits: u32) -> Result<U256, CompactError> {
    let target = compact_to_u256(bits)?;
    if target.is_zero() {
        return Ok(U256::zero());
    }
    let one = U256::from(1u64);
    Ok((!target / (target + one)) + one)
}

pub fn cmp_be(a: &Hash256, b: &Hash256) -> Ordering {
    let left = U256::from_little_endian(a);
    let right = U256::from_little_endian(b);
    left.cmp(&right)
}

pub fn get_next_work_required(
    chain: &[HeaderInfo],
    next_block_time: Option<i64>,
    params: &ConsensusParams,
) -> Result<u32, DifficultyError> {
    let pow_limit_bits = target_to_compact(&params.pow_limit);
    if chain.is_empty() {
        return Ok(pow_limit_bits);
    }

    ensure_contiguous(chain)?;

    let last = chain.last().expect("checked not empty");
    let last_height = last.height;

    let epoch_1_end = eh_epoch_end(params, UpgradeIndex::Equi144_5);
    if last_height > epoch_1_end - 1 && last_height < epoch_1_end + 61 {
        return Ok(pow_limit_bits);
    }

    let epoch_2_end = eh_epoch_end(params, UpgradeIndex::Kamiooka);
    if last_height > epoch_2_end - 1 && last_height < epoch_2_end + 64 {
        let lwma_bits = lwma3_next_work_required(chain, params)?;
        let mut full_target = compact_to_u256(lwma_bits)?;
        let mut target_step = full_target >> 5;
        let step = epoch_2_end + 61 - last_height;
        let (mul_step, _) = target_step.overflowing_mul(U256::from(step as u32));
        target_step = mul_step;
        let (sum_target, _) = full_target.overflowing_add(target_step);
        full_target = sum_target;
        let pow_limit = U256::from_little_endian(&params.pow_limit);
        if full_target > pow_limit {
            full_target = pow_limit;
        }
        return Ok(u256_to_compact(full_target));
    }

    if let Some(min_height) = params.pow_allow_min_difficulty_after_height {
        if last_height >= min_height as i64 {
            if let Some(next_time) = next_block_time {
                if next_time > last.time + params.pow_target_spacing * 2 {
                    return Ok(pow_limit_bits);
                }
            }
        }
    }

    let window = params.digishield_averaging_window as usize;
    if chain.len() <= window {
        return Ok(pow_limit_bits);
    }

    let start = chain.len() - window;
    let mut total = U256::zero();
    for header in &chain[start..] {
        total = total.saturating_add(compact_to_u256(header.bits)?);
    }

    let avg = total / U256::from(window as u64);
    let next_height = last_height + 1;
    let lwma_height = params.upgrades[UpgradeIndex::Lwma.as_usize()].activation_height as i64;
    let acadia_height = params.upgrades[UpgradeIndex::Acadia.as_usize()].activation_height as i64;

    if next_height < lwma_height {
        let last_mtp = median_time_past(chain, chain.len() - 1);
        let first_mtp = median_time_past(chain, start.saturating_sub(1));
        Ok(digishield_next_work_required(avg, last_mtp, first_mtp, params))
    } else if next_height < acadia_height {
        lwma_next_work_required(chain, params)
    } else {
        lwma3_next_work_required(chain, params)
    }
}

fn u256_to_hash(value: U256) -> Hash256 {
    value.to_little_endian()
}

fn ensure_contiguous(chain: &[HeaderInfo]) -> Result<(), DifficultyError> {
    let base = chain[0].height;
    for (idx, header) in chain.iter().enumerate() {
        if header.height != base + idx as i64 {
            return Err(DifficultyError::NonContiguous);
        }
    }
    Ok(())
}

fn eh_epoch_end(params: &ConsensusParams, upgrade: UpgradeIndex) -> i64 {
    params.upgrades[upgrade.as_usize()].activation_height as i64
        + params.eh_epoch_fade_length as i64
        - 1
}

fn median_time_past(chain: &[HeaderInfo], idx: usize) -> i64 {
    let start = idx.saturating_sub(10);
    let mut times: Vec<i64> = chain[start..=idx].iter().map(|header| header.time).collect();
    times.sort_unstable();
    times[times.len() / 2]
}

fn digishield_next_work_required(
    avg_target: U256,
    last_mtp: i64,
    first_mtp: i64,
    params: &ConsensusParams,
) -> u32 {
    let mut actual_timespan = last_mtp - first_mtp;
    let target_timespan = params.digishield_averaging_window_timespan();

    actual_timespan = target_timespan + (actual_timespan - target_timespan) / 4;

    if actual_timespan < params.digishield_min_actual_timespan() {
        actual_timespan = params.digishield_min_actual_timespan();
    }
    if actual_timespan > params.digishield_max_actual_timespan() {
        actual_timespan = params.digishield_max_actual_timespan();
    }

    let mut next = avg_target;
    next /= U256::from(target_timespan as u64);
    next *= U256::from(actual_timespan as u64);

    let pow_limit = U256::from_little_endian(&params.pow_limit);
    if next > pow_limit {
        next = pow_limit;
    }

    u256_to_compact(next)
}

fn lwma_next_work_required(chain: &[HeaderInfo], params: &ConsensusParams) -> Result<u32, DifficultyError> {
    const FTL: i64 = 360;
    let t = params.pow_target_spacing;
    let n = params.zawy_lwma_averaging_window;
    let k = n * (n + 1) * t / 2;

    let height = chain.last().expect("checked not empty").height;
    if height <= n {
        return Ok(target_to_compact(&params.pow_limit));
    }

    let mut sum_target = U256::zero();
    let mut weighted_time: i64 = 0;
    let mut j: i64 = 0;

    let start_height = height - n + 1;
    let base_height = chain[0].height;

    for i in start_height..=height {
        let idx = (i - base_height) as usize;
        let prev_idx = (i - 1 - base_height) as usize;
        let block = &chain[idx];
        let prev_block = &chain[prev_idx];
        let mut solvetime = block.time - prev_block.time;
        solvetime = solvetime.clamp(-FTL, 6 * t);

        j += 1;
        weighted_time += solvetime * j;

        let target = compact_to_u256(block.bits)?;
        let divisor = U256::from((k * n) as u64);
        sum_target = sum_target.saturating_add(target / divisor);
    }

    if weighted_time < k / 10 {
        weighted_time = k / 10;
    }

    let mut next = sum_target * U256::from(weighted_time as u64);
    let pow_limit = U256::from_little_endian(&params.pow_limit);
    if next > pow_limit {
        next = pow_limit;
    }

    Ok(u256_to_compact(next))
}

fn lwma3_next_work_required(chain: &[HeaderInfo], params: &ConsensusParams) -> Result<u32, DifficultyError> {
    let t = params.pow_target_spacing;
    let n = params.zawy_lwma_averaging_window;
    let k = n * (n + 1) * t / 2;

    let height = chain.last().expect("checked not empty").height;
    let pow_limit = U256::from_little_endian(&params.pow_limit);

    if height < n {
        return Ok(u256_to_compact(pow_limit));
    }

    let mut sum_target = U256::zero();
    let mut previous_diff = U256::zero();
    let mut weighted_time: i64 = 0;
    let mut j: i64 = 0;

    let base_height = chain[0].height;
    let mut previous_timestamp = chain[(height - n - base_height) as usize].time;

    for i in (height - n + 1)..=height {
        let idx = (i - base_height) as usize;
        let block = &chain[idx];
        let this_timestamp = if block.time > previous_timestamp {
            block.time
        } else {
            previous_timestamp + 1
        };

        let solvetime = std::cmp::min(6 * t, this_timestamp - previous_timestamp);
        previous_timestamp = this_timestamp;

        j += 1;
        weighted_time += solvetime * j;

        let target = compact_to_u256(block.bits)?;
        let divisor = U256::from((k * n) as u64);
        sum_target = sum_target.saturating_add(target / divisor);

        if i == height {
            previous_diff = target;
        }
    }

    let mut next = sum_target * U256::from(weighted_time as u64);
    let max_target = previous_diff * U256::from(150u64) / U256::from(100u64);
    let min_target = previous_diff * U256::from(67u64) / U256::from(100u64);

    if next > max_target {
        next = max_target;
    }
    if next < min_target {
        next = min_target;
    }

    if next > pow_limit {
        next = pow_limit;
    }

    Ok(u256_to_compact(next))
}
