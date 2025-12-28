//! Bitcoin-style signed message helpers (used by fluxnode transactions and RPC parity).

use fluxd_consensus::constants::SIGNED_MESSAGE_MAGIC;
use fluxd_consensus::Hash256;
use fluxd_primitives::encoding::Encoder;
use fluxd_primitives::hash::sha256d;
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use secp256k1::Message;

use crate::secp::secp256k1_verify;

#[derive(Debug)]
pub enum SignedMessageError {
    InvalidPubkey,
    InvalidSignature,
    InvalidRecoveryId,
    InvalidMessage,
    RecoverFailed,
    PubkeyMismatch,
}

impl std::fmt::Display for SignedMessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignedMessageError::InvalidPubkey => write!(f, "invalid pubkey"),
            SignedMessageError::InvalidSignature => write!(f, "invalid signature"),
            SignedMessageError::InvalidRecoveryId => write!(f, "invalid recovery id"),
            SignedMessageError::InvalidMessage => write!(f, "invalid message"),
            SignedMessageError::RecoverFailed => write!(f, "failed to recover pubkey"),
            SignedMessageError::PubkeyMismatch => write!(f, "pubkey mismatch"),
        }
    }
}

impl std::error::Error for SignedMessageError {}

pub fn signed_message_hash(message: &[u8]) -> Hash256 {
    let mut encoder = Encoder::new();
    encoder.write_var_str(SIGNED_MESSAGE_MAGIC);
    encoder.write_var_bytes(message);
    sha256d(&encoder.into_inner())
}

pub fn verify_signed_message(
    expected_pubkey: &[u8],
    signature: &[u8],
    message: &[u8],
) -> Result<(), SignedMessageError> {
    if expected_pubkey.is_empty() {
        return Err(SignedMessageError::InvalidPubkey);
    }
    let (recoverable, compressed) = decode_compact_signature(signature)?;
    let expected_compressed = expected_pubkey.len() == 33;
    if compressed != expected_compressed {
        return Err(SignedMessageError::PubkeyMismatch);
    }
    let digest = signed_message_hash(message);
    let msg =
        Message::from_digest_slice(&digest).map_err(|_| SignedMessageError::InvalidMessage)?;
    let pubkey = secp256k1::PublicKey::from_slice(expected_pubkey)
        .map_err(|_| SignedMessageError::InvalidPubkey)?;
    let sig = recoverable.to_standard();
    secp256k1_verify()
        .verify_ecdsa(&msg, &sig, &pubkey)
        .map_err(|_| SignedMessageError::InvalidSignature)?;
    Ok(())
}

fn decode_compact_signature(
    signature: &[u8],
) -> Result<(RecoverableSignature, bool), SignedMessageError> {
    if signature.len() != 65 {
        return Err(SignedMessageError::InvalidSignature);
    }
    let header = signature[0];
    if !(27..=34).contains(&header) {
        return Err(SignedMessageError::InvalidSignature);
    }
    let compressed = header >= 31;
    let recovery = if compressed { header - 31 } else { header - 27 };
    let rec_id =
        RecoveryId::from_i32(recovery as i32).map_err(|_| SignedMessageError::InvalidRecoveryId)?;
    let sig = RecoverableSignature::from_compact(&signature[1..65], rec_id)
        .map_err(|_| SignedMessageError::InvalidSignature)?;
    Ok((sig, compressed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::ecdsa::RecoverableSignature;
    use secp256k1::Secp256k1;
    use secp256k1::SecretKey;

    fn encode_compact(sig: &RecoverableSignature, compressed: bool) -> [u8; 65] {
        let (rec_id, bytes) = sig.serialize_compact();
        let mut out = [0u8; 65];
        let header = 27u8 + (rec_id.to_i32() as u8) + if compressed { 4 } else { 0 };
        out[0] = header;
        out[1..].copy_from_slice(&bytes);
        out
    }

    #[test]
    fn verify_signed_message_accepts_valid_compact_signature() {
        let secp = Secp256k1::signing_only();
        let secret = SecretKey::from_slice(&[1u8; 32]).expect("secret");
        let pubkey = secp256k1::PublicKey::from_secret_key(&secp, &secret);

        let message = b"hello";
        let digest = signed_message_hash(message);
        let msg = Message::from_digest_slice(&digest).expect("msg");
        let sig = secp.sign_ecdsa_recoverable(&msg, &secret);
        let sig_bytes = encode_compact(&sig, true);

        verify_signed_message(&pubkey.serialize(), &sig_bytes, message).expect("verify ok");
        let err = verify_signed_message(&pubkey.serialize_uncompressed(), &sig_bytes, message)
            .unwrap_err();
        assert!(matches!(err, SignedMessageError::PubkeyMismatch));
    }
}
