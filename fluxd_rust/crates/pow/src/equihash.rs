//! Equihash verification.

use crate::equihash_verify::is_valid_solution;
use fluxd_consensus::params::EquihashParams;
use fluxd_consensus::upgrades::{network_upgrade_active, UpgradeIndex};
use fluxd_consensus::ConsensusParams;
use fluxd_primitives::block::BlockHeader;
use fluxd_primitives::encoding::Encoder;
use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub enum EquihashError {
    MissingSolution,
    UnsupportedSolutionSize,
    DisallowedParameters,
    InvalidSolution,
}

impl std::fmt::Display for EquihashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EquihashError::MissingSolution => write!(f, "missing equihash solution"),
            EquihashError::UnsupportedSolutionSize => write!(f, "unsupported equihash solution size"),
            EquihashError::DisallowedParameters => write!(f, "equihash parameters not allowed for height"),
            EquihashError::InvalidSolution => write!(f, "invalid equihash solution"),
        }
    }
}

impl std::error::Error for EquihashError {}

pub fn validate_equihash_solution(
    header: &BlockHeader,
    height: i32,
    params: &ConsensusParams,
) -> Result<(), EquihashError> {
    if header.solution.is_empty() {
        return Err(EquihashError::MissingSolution);
    }

    let solution_params = params_from_solution_size(header.solution.len())
        .ok_or(EquihashError::UnsupportedSolutionSize)?;
    let allowed = allowed_params_for_height(height, params);
    if !allowed.iter().any(|candidate| *candidate == solution_params) {
        return Err(EquihashError::DisallowedParameters);
    }

    let input = equihash_input_bytes(header);
    let nonce = hash_to_le_bytes(&header.nonce);

    match is_valid_solution(
        solution_params.n as u32,
        solution_params.k as u32,
        &input,
        &nonce,
        &header.solution,
    ) {
        Ok(()) => Ok(()),
        Err(err) => {
            maybe_dump_equihash_failure(
                header,
                height,
                &solution_params,
                &input,
                &nonce,
                &header.solution,
                &err,
            );
            Err(EquihashError::InvalidSolution)
        }
    }
}

fn params_from_solution_size(size: usize) -> Option<EquihashParams> {
    match size {
        1344 => Some(EquihashParams {
            n: 200,
            k: 9,
            solution_size: 1344,
        }),
        100 => Some(EquihashParams {
            n: 144,
            k: 5,
            solution_size: 100,
        }),
        68 => Some(EquihashParams {
            n: 96,
            k: 5,
            solution_size: 68,
        }),
        52 => Some(EquihashParams {
            n: 125,
            k: 4,
            solution_size: 52,
        }),
        36 => Some(EquihashParams {
            n: 48,
            k: 5,
            solution_size: 36,
        }),
        _ => None,
    }
}

fn allowed_params_for_height(height: i32, params: &ConsensusParams) -> Vec<EquihashParams> {
    let current_height = height.max(0);
    let mut modified_height = current_height - params.eh_epoch_fade_length as i32;
    if modified_height < 0 {
        modified_height = 0;
    }

    if network_upgrade_active(modified_height, &params.upgrades, UpgradeIndex::Kamiooka) {
        return vec![params.eh_epoch_3];
    }
    if network_upgrade_active(current_height, &params.upgrades, UpgradeIndex::Kamiooka) {
        return vec![params.eh_epoch_3, params.eh_epoch_2];
    }
    if network_upgrade_active(modified_height, &params.upgrades, UpgradeIndex::Equi144_5) {
        return vec![params.eh_epoch_2];
    }
    if network_upgrade_active(current_height, &params.upgrades, UpgradeIndex::Equi144_5) {
        return vec![params.eh_epoch_2, params.eh_epoch_1];
    }

    vec![params.eh_epoch_1]
}

fn equihash_input_bytes(header: &BlockHeader) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.write_i32_le(header.version);
    encoder.write_hash_le(&header.prev_block);
    encoder.write_hash_le(&header.merkle_root);
    encoder.write_hash_le(&header.final_sapling_root);
    encoder.write_u32_le(header.time);
    encoder.write_u32_le(header.bits);
    encoder.into_inner()
}

fn hash_to_le_bytes(hash: &[u8; 32]) -> [u8; 32] {
    *hash
}

static DUMPED_EQUIHASH: AtomicBool = AtomicBool::new(false);

fn maybe_dump_equihash_failure(
    header: &BlockHeader,
    height: i32,
    params: &EquihashParams,
    input: &[u8],
    nonce: &[u8; 32],
    solution: &[u8],
    error: &dyn std::error::Error,
) {
    let path = match env::var("FLUX_EQUIHASH_DUMP") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => return,
    };

    let dump_once = match env::var("FLUX_EQUIHASH_DUMP_ONCE") {
        Ok(value) => value != "0",
        Err(_) => true,
    };
    if dump_once && DUMPED_EQUIHASH.swap(true, Ordering::SeqCst) {
        return;
    }

    let mut file = match OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => file,
        Err(_) => return,
    };

    let header_hash = header.hash();
    let _ = writeln!(file, "equihash_failure:");
    let _ = writeln!(file, "  height: {}", height);
    let _ = writeln!(file, "  header_hash: {}", bytes_to_hex(&header_hash));
    let _ = writeln!(file, "  version: {}", header.version);
    let _ = writeln!(file, "  prev_block: {}", bytes_to_hex(&header.prev_block));
    let _ = writeln!(file, "  merkle_root: {}", bytes_to_hex(&header.merkle_root));
    let _ = writeln!(
        file,
        "  final_sapling_root: {}",
        bytes_to_hex(&header.final_sapling_root)
    );
    let _ = writeln!(file, "  time: {}", header.time);
    let _ = writeln!(file, "  bits: {}", header.bits);
    let _ = writeln!(file, "  nonce: {}", bytes_to_hex(nonce));
    let _ = writeln!(
        file,
        "  params: n={} k={} solution_size={}",
        params.n,
        params.k,
        params.solution_size
    );
    let _ = writeln!(file, "  input: {}", bytes_to_hex(input));
    let _ = writeln!(file, "  solution_len: {}", solution.len());
    let _ = writeln!(file, "  solution: {}", bytes_to_hex(solution));
    let _ = writeln!(file, "  error: {}", error);
    let _ = writeln!(file);
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{:02x}", byte);
    }
    out
}
