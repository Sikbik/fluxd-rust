use std::fs;
use std::path::PathBuf;

use fluxd_primitives::transaction::Transaction;

fn is_hex_string(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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

fn extract_first_strings(contents: &str) -> Vec<String> {
    let bytes = contents.as_bytes();
    let mut values = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'[' {
            let mut cursor = index + 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'"' {
                let start = cursor + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end] != b'"' {
                    end += 1;
                }
                if end < bytes.len() {
                    values.push(contents[start..end].to_string());
                    index = end + 1;
                    continue;
                }
            }
        }
        index += 1;
    }
    values
}

fn load_sighash_file() -> String {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("../../../fluxd/src/test/data/sighash.json");
    fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "failed to read sighash vectors at {}: {err}",
            path.display()
        )
    })
}

#[test]
fn sighash_vectors_roundtrip_sapling_and_joinsplit() {
    let contents = load_sighash_file();
    let mut joinsplit_vector: Option<Vec<u8>> = None;
    let mut sapling_vector: Option<Vec<u8>> = None;

    for entry in extract_first_strings(&contents) {
        if !is_hex_string(&entry) {
            continue;
        }
        let entry = entry.to_ascii_lowercase();
        if !(entry.starts_with("03000080") || entry.starts_with("04000080")) {
            continue;
        }
        let Some(bytes) = hex_to_bytes(&entry) else {
            continue;
        };
        let tx = match Transaction::consensus_decode(&bytes) {
            Ok(tx) => tx,
            Err(_) => continue,
        };
        if joinsplit_vector.is_none() && !tx.join_splits.is_empty() {
            joinsplit_vector = Some(bytes.clone());
        }
        if sapling_vector.is_none()
            && (!tx.shielded_spends.is_empty() || !tx.shielded_outputs.is_empty())
        {
            sapling_vector = Some(bytes.clone());
        }
        if joinsplit_vector.is_some() && sapling_vector.is_some() {
            break;
        }
    }

    let joinsplit_vector = joinsplit_vector.expect("missing joinsplit vector in sighash.json");
    let sapling_vector = sapling_vector.expect("missing sapling vector in sighash.json");

    for bytes in [joinsplit_vector, sapling_vector] {
        let tx = Transaction::consensus_decode(&bytes).expect("decode sighash vector");
        let encoded = tx.consensus_encode().expect("encode sighash vector");
        assert_eq!(encoded, bytes);
    }
}
