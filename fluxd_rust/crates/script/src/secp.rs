use std::sync::OnceLock;

use secp256k1::{Secp256k1, VerifyOnly};

static SECP256K1_VERIFY: OnceLock<Secp256k1<VerifyOnly>> = OnceLock::new();

pub(crate) fn secp256k1_verify() -> &'static Secp256k1<VerifyOnly> {
    SECP256K1_VERIFY.get_or_init(Secp256k1::verification_only)
}
