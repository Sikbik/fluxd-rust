use fluxd_script::sighash::{
    SighashType, SIGHASH_ALL, SIGHASH_ANYONECANPAY, SIGHASH_NONE, SIGHASH_SINGLE,
};

#[test]
fn sighash_type_flags() {
    let combined = SighashType(SIGHASH_ALL | SIGHASH_ANYONECANPAY);
    assert_eq!(combined.base_type(), SIGHASH_ALL);
    assert!(combined.has_anyone_can_pay());

    let none = SighashType(SIGHASH_NONE);
    assert_eq!(none.base_type(), SIGHASH_NONE);
    assert!(!none.has_anyone_can_pay());

    let single = SighashType(SIGHASH_SINGLE | SIGHASH_ANYONECANPAY);
    assert_eq!(single.base_type(), SIGHASH_SINGLE);
    assert!(single.has_anyone_can_pay());
}
