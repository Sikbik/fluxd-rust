use fluxd_consensus::{chain_params, Network};
use fluxd_fluxnode::validation::{
    benchmarking_key_at, enforced_tiers, p2sh_keys_at, should_enforce_new_collateral,
    start_payments_height, FluxnodeTier,
};

#[test]
fn mainnet_schedule_helpers() {
    let params = chain_params(Network::Mainnet);
    assert_eq!(start_payments_height(&params), 560_000);

    assert!(enforced_tiers(1_086_611, &params).is_empty());
    assert_eq!(
        enforced_tiers(1_086_612, &params),
        vec![FluxnodeTier::Cumulus]
    );
    assert_eq!(
        enforced_tiers(1_092_372, &params),
        vec![FluxnodeTier::Cumulus, FluxnodeTier::Nimbus]
    );
    assert_eq!(
        enforced_tiers(1_097_412, &params),
        vec![
            FluxnodeTier::Cumulus,
            FluxnodeTier::Nimbus,
            FluxnodeTier::Stratus
        ]
    );

    assert!(!should_enforce_new_collateral(1_076_531, &params));
    assert!(should_enforce_new_collateral(1_076_532, &params));
    assert!(should_enforce_new_collateral(1_097_421, &params));
    assert!(!should_enforce_new_collateral(1_097_422, &params));
}

#[test]
fn benchmarking_key_rotation_mainnet() {
    let params = chain_params(Network::Mainnet);
    let first = benchmarking_key_at(0, &params);
    let second = benchmarking_key_at(1_618_113_600, &params);
    assert_ne!(first.key, second.key);
    let pre_switch = benchmarking_key_at(1_618_113_599, &params);
    assert_eq!(pre_switch.key, first.key);
    let latest = benchmarking_key_at(1_800_000_000, &params);
    assert_ne!(latest.key, first.key);
}

#[test]
fn p2sh_keys_filter_by_time() {
    let params = chain_params(Network::Mainnet);
    let keys = p2sh_keys_at(0, &params);
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].valid_from, 0);
}
