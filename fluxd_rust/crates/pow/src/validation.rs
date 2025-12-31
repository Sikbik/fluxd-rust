use fluxd_consensus::ConsensusParams;
use fluxd_primitives::block::BlockHeader;
use primitive_types::U256;

use crate::difficulty::{compact_to_u256, CompactError};
use crate::equihash::{self, EquihashError};

#[derive(Debug)]
pub enum PowError {
    InvalidHeader(&'static str),
    InvalidBits(&'static str),
    HashMismatch,
    Equihash(EquihashError),
    Compact(CompactError),
}

impl std::fmt::Display for PowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PowError::InvalidHeader(message) => write!(f, "{message}"),
            PowError::InvalidBits(message) => write!(f, "{message}"),
            PowError::HashMismatch => write!(f, "pow hash does not meet target"),
            PowError::Equihash(err) => write!(f, "{err}"),
            PowError::Compact(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for PowError {}

impl From<EquihashError> for PowError {
    fn from(err: EquihashError) -> Self {
        PowError::Equihash(err)
    }
}

impl From<CompactError> for PowError {
    fn from(err: CompactError) -> Self {
        PowError::Compact(err)
    }
}

pub fn validate_pow_header(
    header: &BlockHeader,
    height: i32,
    params: &ConsensusParams,
) -> Result<(), PowError> {
    if header.is_pon() {
        return Err(PowError::InvalidHeader("pow validation on pon header"));
    }

    let target = compact_to_u256(header.bits)?;
    if target.is_zero() {
        return Err(PowError::InvalidBits("pow target is zero"));
    }

    let pow_limit = U256::from_little_endian(&params.pow_limit);
    if target > pow_limit {
        return Err(PowError::InvalidBits("pow target above limit"));
    }

    let hash_bytes = header.hash();
    let hash_value = U256::from_little_endian(&hash_bytes);
    if hash_value > target {
        return Err(PowError::HashMismatch);
    }

    if height == 0 && header.prev_block == [0u8; 32] && hash_bytes == params.hash_genesis_block {
        return Ok(());
    }

    equihash::validate_equihash_solution(header, height, params)?;

    Ok(())
}
