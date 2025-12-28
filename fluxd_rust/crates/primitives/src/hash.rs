use fluxd_consensus::Hash256;
use ripemd::{Digest as RipemdDigest, Ripemd160};
use sha2::Sha256;

pub fn sha256(data: &[u8]) -> Hash256 {
    let digest = Sha256::digest(data);
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

pub fn sha256d(data: &[u8]) -> Hash256 {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    let mut out = [0u8; 32];
    out.copy_from_slice(&second);
    out
}

pub fn hash160(data: &[u8]) -> [u8; 20] {
    let sha = sha256(data);
    let digest = Ripemd160::digest(sha);
    let mut out = [0u8; 20];
    out.copy_from_slice(&digest);
    out
}
