//! PON header validation.

use std::collections::HashSet;
use std::sync::OnceLock;

use fluxd_consensus::params::{hash256_from_hex, ConsensusParams, Network};
use fluxd_consensus::upgrades::UpgradeIndex;
use fluxd_primitives::block::BlockHeader;
use fluxd_primitives::encoding::Decoder;
use fluxd_primitives::outpoint::OutPoint;
use primitive_types::U256;
use secp256k1::{ecdsa::Signature, Message, PublicKey, Secp256k1, VerifyOnly};

use crate::slot::{get_slot_number, pon_hash};

static SECP256K1_VERIFY: OnceLock<Secp256k1<VerifyOnly>> = OnceLock::new();

fn secp256k1_verify() -> &'static Secp256k1<VerifyOnly> {
    SECP256K1_VERIFY.get_or_init(Secp256k1::verification_only)
}

#[derive(Debug)]
pub enum PonError {
    InvalidHeader(&'static str),
}

impl std::fmt::Display for PonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PonError::InvalidHeader(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for PonError {}

pub fn validate_pon_header(
    header: &BlockHeader,
    height: i32,
    params: &ConsensusParams,
) -> Result<(), PonError> {
    if header.nodes_collateral == OutPoint::null() {
        return Err(PonError::InvalidHeader(
            "pon header missing collateral outpoint",
        ));
    }
    if header.block_sig.is_empty() {
        return Err(PonError::InvalidHeader("pon header missing signature"));
    }

    if is_emergency_block(header, params) {
        if !is_emergency_allowed(height, params) {
            return Err(PonError::InvalidHeader("emergency block not allowed"));
        }
        return validate_emergency_signatures(header, params);
    }

    let slot = get_slot_number(header.time as i64, params.genesis_time, params);
    let hash = pon_hash(&header.nodes_collateral, &header.prev_block, slot);
    check_proof_of_node(&hash, header.bits, height, params)
}

pub fn validate_pon_signature(
    header: &BlockHeader,
    params: &ConsensusParams,
    pubkey_bytes: &[u8],
) -> Result<(), PonError> {
    if is_emergency_block(header, params) || is_testnet_bypass(header, params) {
        return Ok(());
    }
    if header.block_sig.is_empty() {
        return Err(PonError::InvalidHeader("pon header missing signature"));
    }

    let pubkey = PublicKey::from_slice(pubkey_bytes)
        .map_err(|_| PonError::InvalidHeader("invalid pubkey"))?;
    let sig = Signature::from_der(&header.block_sig)
        .map_err(|_| PonError::InvalidHeader("invalid pon signature"))?;
    let msg = Message::from_digest_slice(&header.hash())
        .map_err(|_| PonError::InvalidHeader("invalid pon hash"))?;
    secp256k1_verify()
        .verify_ecdsa(&msg, &sig, &pubkey)
        .map_err(|_| PonError::InvalidHeader("pon signature verification failed"))
}

fn check_proof_of_node(
    hash: &fluxd_consensus::Hash256,
    bits: u32,
    height: i32,
    params: &ConsensusParams,
) -> Result<(), PonError> {
    if params.network == Network::Regtest {
        return Ok(());
    }

    let target = fluxd_pow::difficulty::compact_to_u256(bits)
        .map_err(|_| PonError::InvalidHeader("invalid pon target"))?;

    if target.is_zero() {
        return Err(PonError::InvalidHeader("pon target is zero"));
    }

    let activation_height = params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height;
    let max_target = if height > 0 {
        if height >= activation_height + params.pon_difficulty_window as i32 {
            U256::from_little_endian(&params.pon_limit)
        } else {
            U256::from_little_endian(&params.pon_start_limit)
        }
    } else {
        let limit = U256::from_little_endian(&params.pon_limit);
        let start = U256::from_little_endian(&params.pon_start_limit);
        if limit > start {
            limit
        } else {
            start
        }
    };

    if target > max_target {
        return Err(PonError::InvalidHeader("pon target above limit"));
    }

    let hash_value = U256::from_little_endian(hash);
    if hash_value > target {
        return Err(PonError::InvalidHeader("pon hash does not meet target"));
    }

    Ok(())
}

fn is_emergency_block(header: &BlockHeader, params: &ConsensusParams) -> bool {
    header.is_pon()
        && header.nodes_collateral.hash == params.emergency.collateral_hash
        && header.nodes_collateral.index == 0
}

fn is_testnet_bypass(header: &BlockHeader, params: &ConsensusParams) -> bool {
    if params.network != Network::Testnet && params.network != Network::Regtest {
        return false;
    }
    let Ok(bypass_hash) =
        hash256_from_hex("0x544553544e4f4400000000000000000000000000000000000000000000000000")
    else {
        return false;
    };
    header.nodes_collateral.hash == bypass_hash && header.nodes_collateral.index == 0
}

fn is_emergency_allowed(height: i32, params: &ConsensusParams) -> bool {
    height >= params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height
}

fn validate_emergency_signatures(
    header: &BlockHeader,
    params: &ConsensusParams,
) -> Result<(), PonError> {
    let signatures = decode_multisig(&header.block_sig)?;
    let min_required = params.emergency.min_signatures.max(0) as usize;
    if signatures.len() < min_required {
        return Err(PonError::InvalidHeader(
            "emergency block has insufficient signatures",
        ));
    }

    if params.network == Network::Regtest {
        return Ok(());
    }

    let pubkeys = parse_emergency_pubkeys(params)?;
    if pubkeys.is_empty() {
        return Err(PonError::InvalidHeader(
            "emergency block missing valid pubkeys",
        ));
    }

    let msg = Message::from_digest_slice(&header.hash())
        .map_err(|_| PonError::InvalidHeader("invalid emergency block hash"))?;
    let secp = secp256k1_verify();
    let mut used_keys: HashSet<usize> = HashSet::new();
    let mut valid = 0usize;

    for sig in signatures {
        let sig = match Signature::from_der(&sig) {
            Ok(sig) => sig,
            Err(_) => continue,
        };
        for (index, pubkey) in pubkeys.iter().enumerate() {
            if used_keys.contains(&index) {
                continue;
            }
            if secp.verify_ecdsa(&msg, &sig, pubkey).is_ok() {
                used_keys.insert(index);
                valid += 1;
                break;
            }
        }
        if valid >= min_required {
            return Ok(());
        }
    }

    Err(PonError::InvalidHeader(
        "emergency block signature verification failed",
    ))
}

fn parse_emergency_pubkeys(params: &ConsensusParams) -> Result<Vec<PublicKey>, PonError> {
    let mut keys = Vec::new();
    for key in params.emergency.public_keys {
        let bytes = hex_to_bytes(key)?;
        if let Ok(pubkey) = PublicKey::from_slice(&bytes) {
            keys.push(pubkey);
        }
    }
    Ok(keys)
}

fn decode_multisig(bytes: &[u8]) -> Result<Vec<Vec<u8>>, PonError> {
    let mut decoder = Decoder::new(bytes);
    let count = decoder
        .read_varint()
        .map_err(|_| PonError::InvalidHeader("invalid emergency signature count"))?;
    let count = usize::try_from(count)
        .map_err(|_| PonError::InvalidHeader("emergency signature count too large"))?;
    let mut sigs = Vec::with_capacity(count);
    for _ in 0..count {
        let sig = decoder
            .read_var_bytes()
            .map_err(|_| PonError::InvalidHeader("invalid emergency signature"))?;
        sigs.push(sig);
    }
    if !decoder.is_empty() {
        return Err(PonError::InvalidHeader(
            "trailing bytes in emergency signatures",
        ));
    }
    Ok(sigs)
}

fn hex_to_bytes(input: &str) -> Result<Vec<u8>, PonError> {
    let mut hex = input.trim();
    if let Some(stripped) = hex.strip_prefix("0x").or_else(|| hex.strip_prefix("0X")) {
        hex = stripped;
    }
    if hex.len() % 2 == 1 {
        return Err(PonError::InvalidHeader("invalid hex pubkey"));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        let byte = u8::from_str_radix(&hex[i..i + 2], 16)
            .map_err(|_| PonError::InvalidHeader("invalid hex pubkey"))?;
        bytes.push(byte);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxd_consensus::params::{consensus_params, ConsensusParams, EmergencyParams, Network};
    use fluxd_primitives::block::{BlockHeader, PON_VERSION};
    use fluxd_primitives::encoding::Encoder;
    use secp256k1::SecretKey;

    const TEST_EMERGENCY_PUBKEYS: [&str; 2] = [
        "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
        "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
    ];

    fn make_test_secret_key(last_byte: u8) -> SecretKey {
        let mut bytes = [0u8; 32];
        bytes[31] = last_byte;
        SecretKey::from_slice(&bytes).expect("secret key")
    }

    fn make_emergency_header(params: &ConsensusParams) -> BlockHeader {
        BlockHeader {
            version: PON_VERSION,
            prev_block: [0x22; 32],
            merkle_root: [0u8; 32],
            final_sapling_root: [0u8; 32],
            time: 1_700_000_000,
            bits: 0x1d00ffff,
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint {
                hash: params.emergency.collateral_hash,
                index: 0,
            },
            block_sig: Vec::new(),
        }
    }

    fn encode_multisig(signatures: &[Vec<u8>]) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.write_varint(signatures.len() as u64);
        for sig in signatures {
            encoder.write_var_bytes(sig);
        }
        encoder.into_inner()
    }

    #[test]
    fn check_proof_of_node_accepts_small_hash_under_limit() {
        let params = consensus_params(Network::Mainnet);
        let activation = params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height;
        let height = activation + params.pon_difficulty_window as i32 + 1;
        let bits = fluxd_pow::difficulty::target_to_compact(&params.pon_limit);

        let easy_hash = [0u8; 32];
        check_proof_of_node(&easy_hash, bits, height, &params).expect("proof ok");

        let hard_hash = [0xff; 32];
        let err = check_proof_of_node(&hard_hash, bits, height, &params).expect_err("proof fails");
        match err {
            PonError::InvalidHeader(message) => {
                assert_eq!(message, "pon hash does not meet target")
            }
        }
    }

    #[test]
    fn check_proof_of_node_rejects_target_above_start_limit_during_initial_window() {
        let params = consensus_params(Network::Mainnet);
        let activation = params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height;
        let height = activation;
        let bits = fluxd_pow::difficulty::target_to_compact(&params.pon_limit);

        let hash = [0u8; 32];
        let err = check_proof_of_node(&hash, bits, height, &params).expect_err("too easy");
        match err {
            PonError::InvalidHeader(message) => assert_eq!(message, "pon target above limit"),
        }
    }

    #[test]
    fn validate_pon_signature_accepts_valid_signature() {
        let params = consensus_params(Network::Mainnet);

        let secret = make_test_secret_key(3);
        let secp = Secp256k1::signing_only();
        let pubkey = PublicKey::from_secret_key(&secp, &secret);

        let mut header = BlockHeader {
            version: PON_VERSION,
            prev_block: [0x11; 32],
            merkle_root: [0u8; 32],
            final_sapling_root: [0u8; 32],
            time: 1_700_000_001,
            bits: 0x1d00ffff,
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint {
                hash: [0x33; 32],
                index: 5,
            },
            block_sig: Vec::new(),
        };

        let msg = Message::from_digest_slice(&header.hash()).expect("msg");
        let sig = secp.sign_ecdsa(&msg, &secret).serialize_der().to_vec();
        header.block_sig = sig;

        validate_pon_signature(&header, &params, &pubkey.serialize()).expect("signature ok");

        let other_secret = make_test_secret_key(4);
        let other_pubkey = PublicKey::from_secret_key(&secp, &other_secret);
        let err = validate_pon_signature(&header, &params, &other_pubkey.serialize())
            .expect_err("wrong pubkey");
        match err {
            PonError::InvalidHeader(message) => {
                assert_eq!(message, "pon signature verification failed")
            }
        }
    }

    #[test]
    fn emergency_block_requires_activation_height_and_valid_multisig() {
        let mut params = consensus_params(Network::Regtest);
        params.network = Network::Mainnet;
        params.emergency = EmergencyParams {
            public_keys: &TEST_EMERGENCY_PUBKEYS,
            collateral_hash: [0x11; 32],
            min_signatures: 2,
        };
        params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height = 100;

        let mut header = make_emergency_header(&params);
        let msg = Message::from_digest_slice(&header.hash()).expect("msg");

        let sk1 = make_test_secret_key(1);
        let sk2 = make_test_secret_key(2);
        let secp = Secp256k1::signing_only();

        let sig1 = secp.sign_ecdsa(&msg, &sk1).serialize_der().to_vec();
        let sig2 = secp.sign_ecdsa(&msg, &sk2).serialize_der().to_vec();

        header.block_sig = encode_multisig(&[sig1.clone(), sig2.clone()]);
        validate_pon_header(
            &header,
            params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height,
            &params,
        )
        .expect("emergency ok");

        let err = validate_pon_header(
            &header,
            params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height - 1,
            &params,
        )
        .expect_err("emergency too early");
        match err {
            PonError::InvalidHeader(message) => assert_eq!(message, "emergency block not allowed"),
        }

        header.block_sig = encode_multisig(&[sig1.clone()]);
        let err = validate_pon_header(
            &header,
            params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height,
            &params,
        )
        .expect_err("insufficient signatures");
        match err {
            PonError::InvalidHeader(message) => {
                assert_eq!(message, "emergency block has insufficient signatures")
            }
        }

        header.block_sig = encode_multisig(&[sig1.clone(), sig1]);
        let err = validate_pon_header(
            &header,
            params.upgrades[UpgradeIndex::Pon.as_usize()].activation_height,
            &params,
        )
        .expect_err("duplicate key");
        match err {
            PonError::InvalidHeader(message) => {
                assert_eq!(message, "emergency block signature verification failed")
            }
        }
    }
}
