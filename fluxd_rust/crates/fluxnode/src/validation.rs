//! Fluxnode schedule helpers derived from chain parameters.

use fluxd_consensus::{ChainParams, FluxnodeParams, TimedPublicKey};
use fluxd_primitives::transaction::{
    has_flux_tx_delegates_feature, is_flux_tx_normal_type, is_flux_tx_p2sh_type, FluxnodeTx,
    FluxnodeTxV5, FluxnodeTxV6, FLUXNODE_TX_TYPE_P2SH_BIT,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FluxnodeTier {
    Cumulus,
    Nimbus,
    Stratus,
}

pub fn start_payments_height(params: &ChainParams) -> i64 {
    params.fluxnode.start_payments_height
}

pub fn enforced_tiers(height: i64, params: &ChainParams) -> Vec<FluxnodeTier> {
    let mut tiers = Vec::new();
    if height >= params.fluxnode.cumulus_transition_end {
        tiers.push(FluxnodeTier::Cumulus);
    }
    if height >= params.fluxnode.nimbus_transition_end {
        tiers.push(FluxnodeTier::Nimbus);
    }
    if height >= params.fluxnode.stratus_transition_end {
        tiers.push(FluxnodeTier::Stratus);
    }
    tiers
}

pub fn should_enforce_new_collateral(height: i64, params: &ChainParams) -> bool {
    height >= params.fluxnode.cumulus_transition_start
        && height < params.fluxnode.stratus_transition_end + 10
}

pub fn benchmarking_key_at(time: u32, params: &ChainParams) -> TimedPublicKey {
    select_active_key(
        time,
        params.fluxnode.benchmarking_public_keys,
        "benchmarking",
    )
}

pub fn p2sh_keys_at(time: u32, params: &ChainParams) -> Vec<TimedPublicKey> {
    params
        .fluxnode
        .p2sh_public_keys
        .iter()
        .copied()
        .filter(|key| key.valid_from <= time)
        .collect()
}

fn select_active_key(time: u32, keys: &[TimedPublicKey], label: &'static str) -> TimedPublicKey {
    let mut current = keys
        .first()
        .copied()
        .unwrap_or_else(|| panic!("missing {label} public keys"));
    for key in keys {
        if key.valid_from <= time && key.valid_from >= current.valid_from {
            current = *key;
        }
    }
    current
}

pub fn fluxnode_params(params: &ChainParams) -> &FluxnodeParams {
    &params.fluxnode
}

pub fn validate_fluxnode_tx(tx: &FluxnodeTx, version: i32) -> Result<(), &'static str> {
    match tx {
        FluxnodeTx::V5(payload) => validate_fluxnode_v5(payload, version),
        FluxnodeTx::V6(payload) => validate_fluxnode_v6(payload, version),
    }
}

fn validate_fluxnode_v5(tx: &FluxnodeTxV5, version: i32) -> Result<(), &'static str> {
    if version != 5 {
        return Err("fluxnode v5 payload used with non-v5 transaction");
    }
    match tx {
        FluxnodeTxV5::Start(start) => {
            if start.collateral_pubkey.is_empty() || start.pubkey.is_empty() {
                return Err("fluxnode v5 start missing pubkeys");
            }
        }
        FluxnodeTxV5::Confirm(_) => {}
    }
    Ok(())
}

fn validate_fluxnode_v6(tx: &FluxnodeTxV6, version: i32) -> Result<(), &'static str> {
    if version != 6 {
        return Err("fluxnode v6 payload used with non-v6 transaction");
    }
    match tx {
        FluxnodeTxV6::Start(start) => {
            let flux_version = start.flux_tx_version;
            let is_normal = is_flux_tx_normal_type(flux_version, true)
                && (flux_version & FLUXNODE_TX_TYPE_P2SH_BIT) == 0;
            let is_p2sh = is_flux_tx_p2sh_type(flux_version, true);
            if !(is_normal || is_p2sh) {
                return Err("fluxnode v6 start has invalid version bits");
            }
            if has_flux_tx_delegates_feature(flux_version) {
                if start.using_delegates && start.delegates.is_none() {
                    return Err("fluxnode v6 delegates flag set without payload");
                }
                if !start.using_delegates && start.delegates.is_some() {
                    return Err("fluxnode v6 delegates payload present without flag");
                }
            }
        }
        FluxnodeTxV6::Confirm(_) => {}
    }
    Ok(())
}
