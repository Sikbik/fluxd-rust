use bellman::groth16::{PreparedVerifyingKey, Proof};
use bls12_381::Bls12;
use ed25519_dalek::{Signature, VerifyingKey};
use fluxd_consensus::Hash256;
use fluxd_primitives::transaction::{JoinSplit, SproutProof, Transaction};
use fluxd_script::sighash::{signature_hash, SighashType, SIGHASH_ALL};
use group::{ff::PrimeField, GroupEncoding};
use sapling_crypto::{
    note::ExtractedNoteCommitment, value::ValueCommitment, SaplingVerificationContext,
};

use crate::ShieldedError;

pub struct ShieldedParams {
    pub(crate) spend_vk: sapling_crypto::circuit::PreparedSpendVerifyingKey,
    pub(crate) output_vk: sapling_crypto::circuit::PreparedOutputVerifyingKey,
    pub(crate) sprout_vk: PreparedVerifyingKey<Bls12>,
}

impl std::fmt::Debug for ShieldedParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShieldedParams").finish_non_exhaustive()
    }
}

pub fn verify_transaction(
    tx: &Transaction,
    branch_id: u32,
    params: &ShieldedParams,
) -> Result<(), ShieldedError> {
    let has_joinsplit = !tx.join_splits.is_empty();
    let has_sapling = !(tx.shielded_spends.is_empty() && tx.shielded_outputs.is_empty());
    if !has_joinsplit && !has_sapling {
        return Ok(());
    }

    let sighash = signature_hash(tx, None, &[], 0, SighashType(SIGHASH_ALL), branch_id)
        .map_err(|err| ShieldedError::Sighash(err.to_string()))?;

    if has_joinsplit {
        verify_joinsplit_signature(&tx.join_split_pub_key, &tx.join_split_sig, &sighash)?;
        for joinsplit in &tx.join_splits {
            verify_joinsplit_proof(joinsplit, &tx.join_split_pub_key, &params.sprout_vk)?;
        }
    }

    if has_sapling {
        verify_sapling(tx, &sighash, params)?;
    }

    Ok(())
}

pub fn hash256_le_bytes(hash: &Hash256) -> [u8; 32] {
    *hash
}

fn verify_joinsplit_signature(
    pubkey: &[u8; 32],
    sig: &[u8; 64],
    sighash: &[u8; 32],
) -> Result<(), ShieldedError> {
    let key = VerifyingKey::from_bytes(pubkey)
        .map_err(|_| ShieldedError::InvalidTransaction("invalid joinsplit pubkey"))?;
    let signature = Signature::from_bytes(sig);
    key.verify_strict(sighash, &signature)
        .map_err(|_| ShieldedError::InvalidTransaction("invalid joinsplit signature"))?;
    Ok(())
}

fn verify_joinsplit_proof(
    joinsplit: &JoinSplit,
    join_split_pub_key: &[u8; 32],
    sprout_vk: &PreparedVerifyingKey<Bls12>,
) -> Result<(), ShieldedError> {
    let h_sig = joinsplit_hsig(
        &hash256_le_bytes(&joinsplit.random_seed),
        &[
            hash256_le_bytes(&joinsplit.nullifiers[0]),
            hash256_le_bytes(&joinsplit.nullifiers[1]),
        ],
        join_split_pub_key,
    );

    let vpub_old = u64::try_from(joinsplit.vpub_old)
        .map_err(|_| ShieldedError::InvalidTransaction("joinsplit vpub_old out of range"))?;
    let vpub_new = u64::try_from(joinsplit.vpub_new)
        .map_err(|_| ShieldedError::InvalidTransaction("joinsplit vpub_new out of range"))?;

    match &joinsplit.proof {
        SproutProof::Groth(proof) => {
            let ok = zcash_proofs::sprout::verify_proof(
                proof,
                &hash256_le_bytes(&joinsplit.anchor),
                &h_sig,
                &hash256_le_bytes(&joinsplit.macs[0]),
                &hash256_le_bytes(&joinsplit.macs[1]),
                &hash256_le_bytes(&joinsplit.nullifiers[0]),
                &hash256_le_bytes(&joinsplit.nullifiers[1]),
                &hash256_le_bytes(&joinsplit.commitments[0]),
                &hash256_le_bytes(&joinsplit.commitments[1]),
                vpub_old,
                vpub_new,
                sprout_vk,
            );
            if !ok {
                return Err(ShieldedError::InvalidTransaction("invalid joinsplit proof"));
            }
            Ok(())
        }
        SproutProof::Phgr(_) => {
            // fluxd skips PHGR verification post-Sapling; accept for parity.
            Ok(())
        }
    }
}

fn joinsplit_hsig(
    random_seed: &[u8; 32],
    nullifiers: &[[u8; 32]; 2],
    join_split_pub_key: &[u8; 32],
) -> [u8; 32] {
    let mut data = Vec::with_capacity(32 + 32 * 2 + 32);
    data.extend_from_slice(random_seed);
    for nf in nullifiers {
        data.extend_from_slice(nf);
    }
    data.extend_from_slice(join_split_pub_key);

    let hash = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(b"ZcashComputehSig")
        .hash(&data);
    let mut out = [0u8; 32];
    out.copy_from_slice(hash.as_bytes());
    out
}

fn verify_sapling(
    tx: &Transaction,
    sighash: &[u8; 32],
    params: &ShieldedParams,
) -> Result<(), ShieldedError> {
    let mut ctx = SaplingVerificationContext::new();

    for spend in &tx.shielded_spends {
        let cv = parse_value_commitment(&hash256_le_bytes(&spend.cv))?;
        let anchor = parse_anchor(&hash256_le_bytes(&spend.anchor))?;
        let rk = parse_rk(&hash256_le_bytes(&spend.rk))?;
        let spend_auth_sig = redjubjub::Signature::from(spend.spend_auth_sig);
        let proof = Proof::<Bls12>::read(&spend.zkproof[..])
            .map_err(|_| ShieldedError::InvalidTransaction("invalid sapling spend proof"))?;

        let ok = ctx.check_spend(
            &cv,
            anchor,
            &hash256_le_bytes(&spend.nullifier),
            rk,
            sighash,
            spend_auth_sig,
            proof,
            &params.spend_vk,
        );
        if !ok {
            return Err(ShieldedError::InvalidTransaction(
                "sapling spend description invalid",
            ));
        }
    }

    for output in &tx.shielded_outputs {
        let cv = parse_value_commitment(&hash256_le_bytes(&output.cv))?;
        let cmu = parse_cmu(&hash256_le_bytes(&output.cm))?;
        let epk = parse_epk(&hash256_le_bytes(&output.ephemeral_key))?;
        let proof = Proof::<Bls12>::read(&output.zkproof[..])
            .map_err(|_| ShieldedError::InvalidTransaction("invalid sapling output proof"))?;

        let ok = ctx.check_output(&cv, cmu, epk, proof, &params.output_vk);
        if !ok {
            return Err(ShieldedError::InvalidTransaction(
                "sapling output description invalid",
            ));
        }
    }

    if !(tx.shielded_spends.is_empty() && tx.shielded_outputs.is_empty()) {
        let binding_sig = redjubjub::Signature::from(tx.binding_sig);
        if !ctx.final_check(tx.value_balance, sighash, binding_sig) {
            return Err(ShieldedError::InvalidTransaction(
                "sapling binding signature invalid",
            ));
        }
    }

    Ok(())
}

fn parse_value_commitment(bytes: &[u8; 32]) -> Result<ValueCommitment, ShieldedError> {
    Option::from(ValueCommitment::from_bytes_not_small_order(bytes)).ok_or(
        ShieldedError::InvalidTransaction("sapling value commitment invalid"),
    )
}

fn parse_anchor(bytes: &[u8; 32]) -> Result<bls12_381::Scalar, ShieldedError> {
    Option::from(jubjub::Base::from_repr(*bytes))
        .ok_or(ShieldedError::InvalidTransaction("sapling anchor invalid"))
}

fn parse_rk(
    bytes: &[u8; 32],
) -> Result<redjubjub::VerificationKey<redjubjub::SpendAuth>, ShieldedError> {
    redjubjub::VerificationKey::try_from(*bytes)
        .map_err(|_| ShieldedError::InvalidTransaction("sapling spend auth key invalid"))
}

fn parse_cmu(bytes: &[u8; 32]) -> Result<ExtractedNoteCommitment, ShieldedError> {
    Option::from(ExtractedNoteCommitment::from_bytes(bytes)).ok_or(
        ShieldedError::InvalidTransaction("sapling note commitment invalid"),
    )
}

fn parse_epk(bytes: &[u8; 32]) -> Result<jubjub::ExtendedPoint, ShieldedError> {
    Option::from(jubjub::ExtendedPoint::from_bytes(bytes))
        .ok_or(ShieldedError::InvalidTransaction("sapling epk invalid"))
}
