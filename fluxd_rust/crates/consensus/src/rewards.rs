//! Subsidy and funding schedule helpers.

use crate::money::{Amount, COIN};
use crate::params::{ConsensusParams, FundingParams, SwapPoolParams};
use crate::upgrades::{network_upgrade_active, UpgradeIndex};

pub fn block_subsidy(height: i32, params: &ConsensusParams) -> Amount {
    if network_upgrade_active(height, &params.upgrades, UpgradeIndex::Pon) {
        let mut subsidy = params.pon_initial_subsidy as Amount * COIN;
        let activation_height = params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height;
        let blocks_since_pon = height.saturating_sub(activation_height);
        let years_elapsed = blocks_since_pon / params.pon_subsidy_reduction_interval;
        let reductions = years_elapsed.min(params.pon_max_reductions);
        for _ in 0..reductions {
            subsidy = subsidy * 9 / 10;
        }
        return subsidy;
    }

    let mut subsidy = 150 * COIN;
    if height == 1 {
        return 13_020_000 * COIN;
    }

    if height < params.subsidy_slow_start_interval / 2 {
        subsidy /= params.subsidy_slow_start_interval as Amount;
        subsidy *= height as Amount;
        return subsidy;
    }
    if height < params.subsidy_slow_start_interval {
        subsidy /= params.subsidy_slow_start_interval as Amount;
        subsidy *= (height + 1) as Amount;
        return subsidy;
    }

    let shift = params.subsidy_slow_start_shift();
    let halvings = (height - shift) / params.subsidy_halving_interval;
    if halvings >= 64 {
        return 0;
    }
    if halvings >= 2 {
        subsidy >>= 2;
        return subsidy;
    }

    subsidy >>= halvings;
    subsidy
}

pub fn fluxnode_subsidy(
    height: i32,
    block_value: Amount,
    tier: i32,
    params: &ConsensusParams,
) -> Amount {
    if network_upgrade_active(height, &params.upgrades, UpgradeIndex::Pon) {
        const PON_INITIAL_TOTAL: Amount = 14 * COIN;
        const PON_CUMULUS_BASE: Amount = COIN;
        const PON_NIMBUS_BASE: Amount = 35 * COIN / 10;
        const PON_STRATUS_BASE: Amount = 9 * COIN;

        let base = match tier {
            1 => PON_CUMULUS_BASE,
            2 => PON_NIMBUS_BASE,
            3 => PON_STRATUS_BASE,
            _ => return 0,
        };
        return block_value * base / PON_INITIAL_TOTAL;
    }

    let flux_rebrand_active = network_upgrade_active(height, &params.upgrades, UpgradeIndex::Flux);
    let multiple = if flux_rebrand_active { 2.0 } else { 1.0 };
    let percentage = match tier {
        1 => 0.0375,
        2 => 0.0625,
        3 => 0.15,
        _ => return 0,
    };
    ((block_value as f64) * (percentage * multiple)) as Amount
}

pub fn min_dev_fund_amount(height: i32, params: &ConsensusParams) -> Amount {
    if !network_upgrade_active(height, &params.upgrades, UpgradeIndex::Pon) {
        return 0;
    }
    let block_value = block_subsidy(height, params);
    let cumulus = fluxnode_subsidy(height, block_value, 1, params);
    let nimbus = fluxnode_subsidy(height, block_value, 2, params);
    let stratus = fluxnode_subsidy(height, block_value, 3, params);
    block_value - cumulus - nimbus - stratus
}

pub fn exchange_fund_amount(height: i32, funding: &FundingParams) -> Amount {
    if height as i64 == funding.exchange_height {
        funding.exchange_amount
    } else {
        0
    }
}

pub fn foundation_fund_amount(height: i32, funding: &FundingParams) -> Amount {
    if height as i64 == funding.foundation_height {
        funding.foundation_amount
    } else {
        0
    }
}

pub fn is_swap_pool_interval(height: i64, swap_pool: &SwapPoolParams) -> bool {
    if height < swap_pool.start_height {
        return false;
    }
    if height > swap_pool.start_height + (swap_pool.interval * swap_pool.max_times as i64) {
        return false;
    }
    for i in 0..swap_pool.max_times {
        if height == swap_pool.start_height + (swap_pool.interval * i as i64) {
            return true;
        }
    }
    false
}

pub fn swap_pool_amount(height: i64, swap_pool: &SwapPoolParams) -> Amount {
    if is_swap_pool_interval(height, swap_pool) {
        swap_pool.amount
    } else {
        0
    }
}
