use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use fluxd_consensus::params::Network;
use sha2::{Digest, Sha256};

use crate::{ShieldedError, ShieldedParams};

const PARAMS_BASE_URL: &str = "https://images.runonflux.io/fluxd/chain-params";

const SAPLING_SPEND_NAME: &str = "sapling-spend.params";
const SAPLING_OUTPUT_NAME: &str = "sapling-output.params";
const SPROUT_GROTH16_NAME: &str = "sprout-groth16.params";

const SAPLING_SPEND_TESTNET_NAME: &str = "sapling-spend-testnet.params";
const SAPLING_OUTPUT_TESTNET_NAME: &str = "sapling-output-testnet.params";
const SPROUT_GROTH16_TESTNET_NAME: &str = "sprout-groth16-testnet.params";

const SAPLING_SPEND_SHA256: &str =
    "8e48ffd23abb3a5fd9c5589204f32d9c31285a04b78096ba40a79b75677efc13";
const SAPLING_OUTPUT_SHA256: &str =
    "2f0ebbcbb9bb0bcffe95a397e7eba89c29eb4dde6191c339db88570e3f3fb0e4";
const SPROUT_GROTH16_SHA256: &str =
    "b685d700c60328498fbde589c8c7c484c722b788b265b72af448a5bf0ee55b50";

pub struct ParamPaths {
    pub spend: PathBuf,
    pub output: PathBuf,
    pub sprout: PathBuf,
}

pub fn default_params_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join("Library/Application Support/ZcashParams");
        }
    }

    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("ZcashParams");
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".zcash-params");
    }

    PathBuf::from(".zcash-params")
}

pub fn load_params(params_dir: &Path, network: Network) -> Result<ShieldedParams, ShieldedError> {
    let paths = resolve_param_paths(params_dir, network)?;
    let params = std::panic::catch_unwind(|| {
        zcash_proofs::load_parameters(&paths.spend, &paths.output, Some(&paths.sprout))
    })
    .map_err(|_| {
        ShieldedError::InvalidParams("failed to parse shielded parameter files".to_string())
    })?;
    let sprout_vk = params
        .sprout_vk
        .ok_or_else(|| ShieldedError::InvalidParams("missing Sprout verifying key".to_string()))?;

    Ok(ShieldedParams {
        spend_vk: params.spend_vk,
        output_vk: params.output_vk,
        sprout_vk,
    })
}

pub fn fetch_params(params_dir: &Path, network: Network) -> Result<(), ShieldedError> {
    fs::create_dir_all(params_dir)?;

    let (spend_name, output_name, sprout_name) = match network {
        Network::Testnet => (
            SAPLING_SPEND_TESTNET_NAME,
            SAPLING_OUTPUT_TESTNET_NAME,
            SPROUT_GROTH16_TESTNET_NAME,
        ),
        _ => (SAPLING_SPEND_NAME, SAPLING_OUTPUT_NAME, SPROUT_GROTH16_NAME),
    };

    download_param(params_dir, spend_name, SAPLING_SPEND_SHA256)?;
    download_param(params_dir, output_name, SAPLING_OUTPUT_SHA256)?;
    download_param(params_dir, sprout_name, SPROUT_GROTH16_SHA256)?;

    Ok(())
}

fn resolve_param_paths(params_dir: &Path, network: Network) -> Result<ParamPaths, ShieldedError> {
    let (spend_primary, output_primary, sprout_primary) = match network {
        Network::Testnet => (
            SAPLING_SPEND_TESTNET_NAME,
            SAPLING_OUTPUT_TESTNET_NAME,
            SPROUT_GROTH16_TESTNET_NAME,
        ),
        _ => (SAPLING_SPEND_NAME, SAPLING_OUTPUT_NAME, SPROUT_GROTH16_NAME),
    };

    let spend = pick_param(params_dir, spend_primary, Some(SAPLING_SPEND_NAME))?;
    let output = pick_param(params_dir, output_primary, Some(SAPLING_OUTPUT_NAME))?;
    let sprout = pick_param(params_dir, sprout_primary, Some(SPROUT_GROTH16_NAME))?;

    Ok(ParamPaths {
        spend,
        output,
        sprout,
    })
}

fn pick_param(
    params_dir: &Path,
    primary: &str,
    fallback: Option<&str>,
) -> Result<PathBuf, ShieldedError> {
    let primary_path = params_dir.join(primary);
    if primary_path.exists() {
        return Ok(primary_path);
    }
    if let Some(fallback) = fallback {
        let fallback_path = params_dir.join(fallback);
        if fallback_path.exists() {
            return Ok(fallback_path);
        }
    }

    Err(ShieldedError::MissingParams(format!(
        "missing shielded params: expected {primary} in {} (run fetch-params)",
        params_dir.display()
    )))
}

fn download_param(
    params_dir: &Path,
    name: &str,
    expected_sha256: &str,
) -> Result<(), ShieldedError> {
    let dest = params_dir.join(name);
    if dest.exists() && verify_sha256_cached(&dest, expected_sha256).is_ok() {
        return Ok(());
    }

    let tmp = params_dir.join(format!("{name}.dl"));
    download_parts(name, &tmp)?;
    verify_sha256(&tmp, expected_sha256)?;
    fs::rename(&tmp, &dest)?;
    let _ = write_sha256_marker(&dest, expected_sha256);
    Ok(())
}

fn verify_sha256_cached(path: &Path, expected: &str) -> Result<(), ShieldedError> {
    if is_sha256_marker_valid(path, expected).unwrap_or(false) {
        return Ok(());
    }
    verify_sha256(path, expected)?;
    let _ = write_sha256_marker(path, expected);
    Ok(())
}

fn sha256_marker_path(path: &Path) -> Option<PathBuf> {
    let file_name = path.file_name()?.to_string_lossy();
    Some(path.with_file_name(format!("{file_name}.sha256")))
}

fn sha256_marker_fingerprint(path: &Path) -> io::Result<(u64, u64)> {
    let meta = fs::metadata(path)?;
    let size = meta.len();
    let modified = meta
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok((size, modified))
}

fn is_sha256_marker_valid(path: &Path, expected: &str) -> io::Result<bool> {
    let Some(marker_path) = sha256_marker_path(path) else {
        return Ok(false);
    };
    let marker = match fs::read_to_string(&marker_path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    let mut lines = marker.lines();
    let marker_expected = lines.next().unwrap_or_default();
    if marker_expected != expected {
        return Ok(false);
    }
    let marker_size = match lines.next().and_then(|value| value.parse::<u64>().ok()) {
        Some(value) => value,
        None => return Ok(false),
    };
    let marker_modified = match lines.next().and_then(|value| value.parse::<u64>().ok()) {
        Some(value) => value,
        None => return Ok(false),
    };
    let (size, modified) = sha256_marker_fingerprint(path)?;
    Ok(size == marker_size && modified == marker_modified)
}

fn write_sha256_marker(path: &Path, expected: &str) -> io::Result<()> {
    let Some(marker_path) = sha256_marker_path(path) else {
        return Ok(());
    };
    let (size, modified) = sha256_marker_fingerprint(path)?;
    fs::write(&marker_path, format!("{expected}\n{size}\n{modified}\n"))
}

fn download_parts(name: &str, dest: &Path) -> Result<(), ShieldedError> {
    let part1_url = format!("{PARAMS_BASE_URL}/{name}.part.1");
    let part2_url = format!("{PARAMS_BASE_URL}/{name}.part.2");

    let mut file = File::create(dest)?;
    download_into(&part1_url, &mut file)?;
    download_into(&part2_url, &mut file)?;
    file.flush()?;
    Ok(())
}

fn download_into(url: &str, writer: &mut impl Write) -> Result<(), ShieldedError> {
    let mut response = minreq::get(url)
        .send_lazy()
        .map_err(|err| ShieldedError::Download(format!("download failed: {url} ({err})")))?;
    if response.status_code != 200 {
        return Err(ShieldedError::Download(format!(
            "download failed: {url} (HTTP {})",
            response.status_code
        )));
    }
    io::copy(&mut response, writer)?;
    Ok(())
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), ShieldedError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    let actual = hex_lower(&hasher.finalize());
    if actual != expected {
        return Err(ShieldedError::InvalidParams(format!(
            "sha256 mismatch for {} (expected {expected}, got {actual})",
            path.display()
        )));
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'a' + (value - 10)) as char,
    }
}
