use fluxd_script::standard::{classify_script_pubkey, ScriptType};

#[test]
fn classify_p2pkh() {
    let mut script = vec![0x76, 0xa9, 0x14];
    script.extend_from_slice(&[0x11; 20]);
    script.extend_from_slice(&[0x88, 0xac]);
    assert_eq!(classify_script_pubkey(&script), ScriptType::P2Pkh);
}

#[test]
fn classify_p2sh() {
    let mut script = vec![0xa9, 0x14];
    script.extend_from_slice(&[0x22; 20]);
    script.push(0x87);
    assert_eq!(classify_script_pubkey(&script), ScriptType::P2Sh);
}

#[test]
fn classify_p2wpkh() {
    let mut script = vec![0x00, 0x14];
    script.extend_from_slice(&[0x33; 20]);
    assert_eq!(classify_script_pubkey(&script), ScriptType::P2Wpkh);
}

#[test]
fn classify_p2wsh() {
    let mut script = vec![0x00, 0x20];
    script.extend_from_slice(&[0x44; 32]);
    assert_eq!(classify_script_pubkey(&script), ScriptType::P2Wsh);
}

#[test]
fn classify_p2pk() {
    let mut script = vec![33];
    script.extend_from_slice(&[0x02; 33]);
    script.push(0xac);
    assert_eq!(classify_script_pubkey(&script), ScriptType::P2Pk);
}

#[test]
fn classify_unknown() {
    let script = vec![0x6a, 0x01, 0x01];
    assert_eq!(classify_script_pubkey(&script), ScriptType::Unknown);
}
