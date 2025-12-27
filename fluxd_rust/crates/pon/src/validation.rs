//! PON header validation.

use std::collections::HashSet;

use fluxd_consensus::params::{hash256_from_hex, ConsensusParams, Network};
use fluxd_consensus::upgrades::UpgradeIndex;
use fluxd_primitives::block::BlockHeader;
use fluxd_primitives::encoding::Decoder;
use fluxd_primitives::outpoint::OutPoint;
use primitive_types::U256;
use secp256k1::{ecdsa::Signature, Message, PublicKey, Secp256k1};

use crate::slot::{get_slot_number, pon_hash};

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
    let secp = Secp256k1::verification_only();
    secp.verify_ecdsa(&msg, &sig, &pubkey)
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
    let secp = Secp256k1::verification_only();
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
