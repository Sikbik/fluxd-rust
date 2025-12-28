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

#[cfg(test)]
mod tests {
    use super::*;
    use fluxd_consensus::params::{consensus_params, Network};

    #[test]
    fn slot_number_matches_cpp_examples() {
        let params = consensus_params(Network::Mainnet);
        let genesis = params.genesis_time as i64;

        assert_eq!(get_slot_number(genesis, params.genesis_time, &params), 0);
        assert_eq!(
            get_slot_number(
                genesis + params.pon_target_spacing,
                params.genesis_time,
                &params
            ),
            1
        );
        assert_eq!(
            get_slot_number(
                genesis + 10 * params.pon_target_spacing,
                params.genesis_time,
                &params
            ),
            10
        );
    }

    #[test]
    fn pon_hash_is_deterministic_and_sensitive_to_inputs() {
        let collateral = OutPoint {
            hash: [0x11; 32],
            index: 1,
        };
        let prev = [0x22; 32];

        let hash1 = pon_hash(&collateral, &prev, 12345);
        let hash2 = pon_hash(&collateral, &prev, 12345);
        assert_eq!(hash1, hash2);

        let hash3 = pon_hash(&collateral, &prev, 12346);
        assert_ne!(hash1, hash3);

        let collateral2 = OutPoint {
            hash: [0x33; 32],
            index: 2,
        };
        let hash4 = pon_hash(&collateral2, &prev, 12345);
        assert_ne!(hash1, hash4);

        let prev2 = [0x44; 32];
        let hash5 = pon_hash(&collateral, &prev2, 12345);
        assert_ne!(hash1, hash5);
    }
}
