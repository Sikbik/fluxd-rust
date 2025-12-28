use fluxd_primitives::transaction::Transaction;
use fluxd_script::sighash::{signature_hash, SighashType};

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

fn hash256_to_hex(hash: &[u8; 32]) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(64);
    for byte in hash.iter().rev() {
        let _ = write!(out, "{:02x}", byte);
    }
    out
}

#[test]
fn sighash_vectors_match_cpp() {
    let vectors = include_str!("vectors/sighash.json");
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(vectors).expect("parse sighash vectors json");
    let mut exercised = 0usize;

    for row in rows {
        let Some(values) = row.as_array() else {
            continue;
        };
        if values.len() == 1 {
            continue;
        }
        assert_eq!(values.len(), 6, "unexpected sighash vector row");

        let tx_hex = values[0].as_str().expect("tx hex").trim();
        let script_hex = values[1].as_str().expect("script hex").trim();
        let input_index = values[2].as_u64().expect("input index") as usize;
        let hash_type =
            i32::try_from(values[3].as_i64().expect("hash type")).expect("hash type range") as u32;
        let branch_id = values[4].as_u64().expect("branch id") as u32;
        let expected_hex = values[5].as_str().expect("expected sighash").trim();

        let tx_bytes = hex_to_bytes(tx_hex).expect("decode tx hex");
        let tx = Transaction::consensus_decode(&tx_bytes).expect("decode tx");
        let encoded = tx.consensus_encode().expect("encode tx");
        assert_eq!(encoded, tx_bytes);

        let script_code = hex_to_bytes(script_hex).expect("decode script hex");
        let sighash = signature_hash(
            &tx,
            Some(input_index),
            &script_code,
            0,
            SighashType(hash_type),
            branch_id,
        )
        .expect("signature hash");
        assert_eq!(hash256_to_hex(&sighash), expected_hex.to_ascii_lowercase(),);
        exercised += 1;
    }

    assert!(exercised > 0, "no sighash vectors were exercised");
}
