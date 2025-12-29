//! Subsidy and funding schedule helpers.

use crate::money::{Amount, COIN};
use crate::params::{ConsensusParams, FluxnodeParams, FundingParams, SwapPoolParams};
use crate::upgrades::{network_upgrade_active, UpgradeIndex};

const V1_FLUXNODE_COLLAT_CUMULUS: Amount = 10_000 * COIN;
const V1_FLUXNODE_COLLAT_NIMBUS: Amount = 25_000 * COIN;
const V1_FLUXNODE_COLLAT_STRATUS: Amount = 100_000 * COIN;

const V2_FLUXNODE_COLLAT_CUMULUS: Amount = 1_000 * COIN;
const V2_FLUXNODE_COLLAT_NIMBUS: Amount = 12_500 * COIN;
const V2_FLUXNODE_COLLAT_STRATUS: Amount = 40_000 * COIN;

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

pub fn fluxnode_tier_from_collateral(
    height: i32,
    amount: Amount,
    params: &FluxnodeParams,
) -> Option<u8> {
    for tier in 1u8..=3u8 {
        if fluxnode_collateral_matches_tier(height, amount, tier, params) {
            return Some(tier);
        }
    }
    None
}

pub fn fluxnode_collateral_matches_tier(
    height: i32,
    amount: Amount,
    tier: u8,
    params: &FluxnodeParams,
) -> bool {
    match tier {
        1 => {
            if (height as i64) < params.cumulus_transition_start {
                amount == V1_FLUXNODE_COLLAT_CUMULUS
            } else if (height as i64) < params.cumulus_transition_end {
                amount == V1_FLUXNODE_COLLAT_CUMULUS || amount == V2_FLUXNODE_COLLAT_CUMULUS
            } else {
                amount == V2_FLUXNODE_COLLAT_CUMULUS
            }
        }
        2 => {
            if (height as i64) < params.nimbus_transition_start {
                amount == V1_FLUXNODE_COLLAT_NIMBUS
            } else if (height as i64) < params.nimbus_transition_end {
                amount == V1_FLUXNODE_COLLAT_NIMBUS || amount == V2_FLUXNODE_COLLAT_NIMBUS
            } else {
                amount == V2_FLUXNODE_COLLAT_NIMBUS
            }
        }
        3 => {
            if (height as i64) < params.stratus_transition_start {
                amount == V1_FLUXNODE_COLLAT_STRATUS
            } else if (height as i64) < params.stratus_transition_end {
                amount == V1_FLUXNODE_COLLAT_STRATUS || amount == V2_FLUXNODE_COLLAT_STRATUS
            } else {
                amount == V2_FLUXNODE_COLLAT_STRATUS
            }
        }
        _ => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::chain_params;
    use crate::params::{consensus_params, Network};
    use crate::upgrades::UpgradeIndex;

    #[test]
    fn mainnet_canceled_halving_at_1_968_550() {
        let params = consensus_params(Network::Mainnet);
        let height = 1_968_550;

        let before = block_subsidy(height - 1, &params);
        let at = block_subsidy(height, &params);
        let after = block_subsidy(height + 1, &params);

        assert_eq!(before, at);
        assert_eq!(after, at);
        assert_eq!(at, 150 * COIN / 4);
        assert_ne!(at, 150 * COIN / 8);
    }

    #[test]
    fn mainnet_pon_activation_switches_subsidy() {
        let params = consensus_params(Network::Mainnet);
        let activation_height = params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height;
        assert_eq!(activation_height, 2_020_000);

        let pow_before = block_subsidy(activation_height - 1, &params);
        assert_eq!(pow_before, 150 * COIN / 4);

        let pon_at = block_subsidy(activation_height, &params);
        assert_eq!(pon_at, params.pon_initial_subsidy as Amount * COIN);
        assert_eq!(pon_at, 14 * COIN);

        let pon_after = block_subsidy(activation_height + 1, &params);
        assert_eq!(pon_after, pon_at);
    }

    #[test]
    fn mainnet_min_dev_fund_is_required_at_pon_activation() {
        let params = consensus_params(Network::Mainnet);
        let activation_height = params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height;

        assert_eq!(min_dev_fund_amount(activation_height - 1, &params), 0);
        assert_eq!(min_dev_fund_amount(activation_height, &params), COIN / 2);
    }

    #[test]
    fn mainnet_pon_subsidy_reduces_on_schedule_and_caps() {
        let params = consensus_params(Network::Mainnet);
        let activation_height = params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height;

        assert_eq!(block_subsidy(activation_height, &params), 14 * COIN);
        assert_eq!(
            block_subsidy(
                activation_height + params.pon_subsidy_reduction_interval - 1,
                &params
            ),
            14 * COIN
        );

        assert_eq!(
            block_subsidy(
                activation_height + params.pon_subsidy_reduction_interval,
                &params
            ),
            12_600_000_000 / 10
        );
        assert_eq!(
            block_subsidy(
                activation_height + 2 * params.pon_subsidy_reduction_interval,
                &params
            ),
            11_340_000_000 / 10
        );
        assert_eq!(
            block_subsidy(
                activation_height + 5 * params.pon_subsidy_reduction_interval,
                &params
            ),
            8_266_860_000 / 10
        );

        let year_20 = block_subsidy(
            activation_height + 20 * params.pon_subsidy_reduction_interval,
            &params,
        );
        assert_eq!(year_20, 170_207_313);
        assert_eq!(
            block_subsidy(
                activation_height + 21 * params.pon_subsidy_reduction_interval,
                &params
            ),
            year_20
        );
        assert_eq!(
            block_subsidy(
                activation_height + 30 * params.pon_subsidy_reduction_interval,
                &params
            ),
            year_20
        );
    }

    #[test]
    fn mainnet_pon_reward_distribution_matches_cpp() {
        let params = consensus_params(Network::Mainnet);
        let activation_height = params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height;

        let total = block_subsidy(activation_height, &params);
        assert_eq!(total, 14 * COIN);

        let cumulus = fluxnode_subsidy(activation_height, total, 1, &params);
        let nimbus = fluxnode_subsidy(activation_height, total, 2, &params);
        let stratus = fluxnode_subsidy(activation_height, total, 3, &params);
        assert_eq!(cumulus, COIN);
        assert_eq!(nimbus, 35 * COIN / 10);
        assert_eq!(stratus, 9 * COIN);

        let dev_fund = min_dev_fund_amount(activation_height, &params);
        assert_eq!(dev_fund, total / 28);
        assert_eq!(cumulus + nimbus + stratus + dev_fund, total);

        let year_1 = block_subsidy(
            activation_height + params.pon_subsidy_reduction_interval,
            &params,
        );
        assert_eq!(year_1, 12_600_000_000 / 10);
        assert_eq!(
            min_dev_fund_amount(
                activation_height + params.pon_subsidy_reduction_interval,
                &params
            ),
            year_1 / 28
        );
    }

    #[test]
    fn fluxnode_tier_from_collateral_respects_transition_windows() {
        let params = chain_params(Network::Mainnet);
        let flux = &params.fluxnode;

        let before = flux.cumulus_transition_start as i32 - 1;
        assert_eq!(
            fluxnode_tier_from_collateral(before, V1_FLUXNODE_COLLAT_CUMULUS, flux),
            Some(1)
        );
        assert_eq!(
            fluxnode_tier_from_collateral(before, V2_FLUXNODE_COLLAT_CUMULUS, flux),
            None
        );

        let during = flux.cumulus_transition_start as i32;
        assert_eq!(
            fluxnode_tier_from_collateral(during, V1_FLUXNODE_COLLAT_CUMULUS, flux),
            Some(1)
        );
        assert_eq!(
            fluxnode_tier_from_collateral(during, V2_FLUXNODE_COLLAT_CUMULUS, flux),
            Some(1)
        );

        let after = flux.cumulus_transition_end as i32;
        assert_eq!(
            fluxnode_tier_from_collateral(after, V1_FLUXNODE_COLLAT_CUMULUS, flux),
            None
        );
        assert_eq!(
            fluxnode_tier_from_collateral(after, V2_FLUXNODE_COLLAT_CUMULUS, flux),
            Some(1)
        );
    }
}
