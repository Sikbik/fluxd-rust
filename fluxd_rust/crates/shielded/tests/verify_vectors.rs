use std::path::PathBuf;

use fluxd_consensus::params::Network;
use fluxd_primitives::transaction::Transaction;
use fluxd_shielded::{default_params_dir, load_params, verify_transaction};

#[derive(Debug, serde::Deserialize)]
struct VerifyVector {
    #[allow(dead_code)]
    name: Option<String>,
    branch_id: u32,
    tx_hex: String,
}

fn hex_to_bytes(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut iter = hex.as_bytes().iter().copied();
    while let (Some(high), Some(low)) = (iter.next(), iter.next()) {
        let high = (high as char).to_digit(16)? as u8;
        let low = (low as char).to_digit(16)? as u8;
        bytes.push(high << 4 | low);
    }
    Some(bytes)
}

#[test]
fn verify_vectors_parse() {
    let json = include_str!("vectors/verify_transaction.json");
    let rows: Vec<VerifyVector> = serde_json::from_str(json).expect("parse verify_transaction");
    assert!(!rows.is_empty(), "no verify_transaction vectors present");
    for row in rows {
        assert!(!row.tx_hex.trim().is_empty(), "empty tx hex in vector");
        assert_ne!(row.branch_id, 0, "unexpected branch_id=0 in vector");
        assert!(
            hex_to_bytes(row.tx_hex.trim()).is_some(),
            "invalid hex in vector"
        );
    }
}

#[test]
#[ignore = "requires shielded params and is CPU heavy; run via scripts/run_shielded_tests.sh"]
fn verify_vectors_pass_shielded_checks() {
    let json = include_str!("vectors/verify_transaction.json");
    let rows: Vec<VerifyVector> = serde_json::from_str(json).expect("parse verify_transaction");

    let params_dir = std::env::var_os("FLUXD_PARAMS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_params_dir);
    let params = load_params(&params_dir, Network::Mainnet).expect("load shielded params");

    for row in rows {
        let tx_bytes = hex_to_bytes(row.tx_hex.trim()).expect("decode tx hex");
        let tx = Transaction::consensus_decode(&tx_bytes).expect("decode tx");
        verify_transaction(&tx, row.branch_id, &params).expect("verify transaction");
    }
}
