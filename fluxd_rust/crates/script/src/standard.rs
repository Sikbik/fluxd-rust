//! Standard script classification utilities.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptType {
    P2Pk,
    P2Pkh,
    P2Sh,
    P2Wpkh,
    P2Wsh,
    Unknown,
}

const OP_0: u8 = 0x00;
const OP_DUP: u8 = 0x76;
const OP_HASH160: u8 = 0xa9;
const OP_EQUAL: u8 = 0x87;
const OP_EQUALVERIFY: u8 = 0x88;
const OP_CHECKSIG: u8 = 0xac;

pub fn classify_script_pubkey(script: &[u8]) -> ScriptType {
    if is_p2pkh(script) {
        ScriptType::P2Pkh
    } else if is_p2sh(script) {
        ScriptType::P2Sh
    } else if is_p2wpkh(script) {
        ScriptType::P2Wpkh
    } else if is_p2wsh(script) {
        ScriptType::P2Wsh
    } else if is_p2pk(script) {
        ScriptType::P2Pk
    } else {
        ScriptType::Unknown
    }
}

fn is_p2pkh(script: &[u8]) -> bool {
    script.len() == 25
        && script[0] == OP_DUP
        && script[1] == OP_HASH160
        && script[2] == 0x14
        && script[23] == OP_EQUALVERIFY
        && script[24] == OP_CHECKSIG
}

fn is_p2sh(script: &[u8]) -> bool {
    script.len() == 23 && script[0] == OP_HASH160 && script[1] == 0x14 && script[22] == OP_EQUAL
}

fn is_p2wpkh(script: &[u8]) -> bool {
    script.len() == 22 && script[0] == OP_0 && script[1] == 0x14
}

fn is_p2wsh(script: &[u8]) -> bool {
    script.len() == 34 && script[0] == OP_0 && script[1] == 0x20
}

fn is_p2pk(script: &[u8]) -> bool {
    let key_len = match script.first().copied() {
        Some(len @ 33) => len,
        Some(len @ 65) => len,
        _ => return false,
    };

    let expected_len = key_len as usize + 2;
    script.len() == expected_len && script[script.len() - 1] == OP_CHECKSIG
}
