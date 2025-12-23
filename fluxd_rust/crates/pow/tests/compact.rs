use fluxd_pow::difficulty::{compact_to_target, hash_meets_target, target_to_compact};

#[test]
fn compact_to_target_roundtrip() {
    let bits = 0x1d00ffff;
    let target = compact_to_target(bits).expect("target");
    let back = target_to_compact(&target);
    assert_eq!(back, bits);
}

#[test]
fn compact_target_layout() {
    let bits = 0x207fffff;
    let target = compact_to_target(bits).expect("target");
    assert!(target[..29].iter().all(|b| *b == 0));
    assert_eq!(target[29], 0xff);
    assert_eq!(target[30], 0xff);
    assert_eq!(target[31], 0x7f);
}

#[test]
fn hash_meets_target_cmp() {
    let target = [0x10u8; 32];
    let smaller = [0x00u8; 32];
    let larger = [0xffu8; 32];
    assert!(hash_meets_target(&smaller, &target));
    assert!(!hash_meets_target(&larger, &target));
}
