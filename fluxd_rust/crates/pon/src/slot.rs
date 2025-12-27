//! Slot calculation and PON hash.

use fluxd_consensus::params::ConsensusParams;
use fluxd_consensus::Hash256;
use fluxd_primitives::encoding::{Encodable, Encoder};
use fluxd_primitives::hash::sha256d;
use fluxd_primitives::outpoint::OutPoint;

pub fn get_slot_number(timestamp: i64, genesis_time: u32, params: &ConsensusParams) -> u32 {
    let time_since_genesis = timestamp - genesis_time as i64;
    if time_since_genesis <= 0 {
        return 0;
    }
    (time_since_genesis / params.pon_target_spacing) as u32
}

pub fn pon_hash(collateral: &OutPoint, prev_block_hash: &Hash256, slot: u32) -> Hash256 {
    let mut encoder = Encoder::new();
    collateral.consensus_encode(&mut encoder);
    encoder.write_hash_le(prev_block_hash);
    encoder.write_u32_le(slot);
    sha256d(&encoder.into_inner())
}
