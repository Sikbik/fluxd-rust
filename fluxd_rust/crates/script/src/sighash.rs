//! Signature hashing for transparent inputs.

use blake2b_simd::Params as Blake2bParams;
use fluxd_consensus::Hash256;
use fluxd_primitives::encoding::{Encodable, Encoder};
use fluxd_primitives::hash::sha256d;
use fluxd_primitives::transaction::{
    OutputDescription, SpendDescription, Transaction, TransactionEncodeError, TxOut,
    OVERWINTER_VERSION_GROUP_ID, SAPLING_VERSION_GROUP_ID,
};

pub const SIGHASH_ALL: u32 = 0x01;
pub const SIGHASH_NONE: u32 = 0x02;
pub const SIGHASH_SINGLE: u32 = 0x03;
pub const SIGHASH_ANYONECANPAY: u32 = 0x80;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SighashType(pub u32);

impl SighashType {
    pub fn base_type(self) -> u32 {
        self.0 & 0x1f
    }

    pub fn has_anyone_can_pay(self) -> bool {
        (self.0 & SIGHASH_ANYONECANPAY) != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SigVersion {
    Sprout,
    Overwinter,
    Sapling,
}

#[derive(Debug)]
pub enum SighashError {
    InputIndexOutOfRange,
    MissingOutput,
    UnsupportedTransactionFormat(&'static str),
    Encoding(TransactionEncodeError),
}

impl std::fmt::Display for SighashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SighashError::InputIndexOutOfRange => write!(f, "input index out of range"),
            SighashError::MissingOutput => write!(f, "no matching output for SIGHASH_SINGLE"),
            SighashError::UnsupportedTransactionFormat(message) => write!(f, "{message}"),
            SighashError::Encoding(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for SighashError {}

impl From<TransactionEncodeError> for SighashError {
    fn from(err: TransactionEncodeError) -> Self {
        SighashError::Encoding(err)
    }
}

const ZCASH_PREVOUTS_HASH_PERSONALIZATION: [u8; 16] = *b"ZcashPrevoutHash";
const ZCASH_SEQUENCE_HASH_PERSONALIZATION: [u8; 16] = *b"ZcashSequencHash";
const ZCASH_OUTPUTS_HASH_PERSONALIZATION: [u8; 16] = *b"ZcashOutputsHash";
const ZCASH_JOINSPLITS_HASH_PERSONALIZATION: [u8; 16] = *b"ZcashJSplitsHash";
const ZCASH_SHIELDED_SPENDS_HASH_PERSONALIZATION: [u8; 16] = *b"ZcashSSpendsHash";
const ZCASH_SHIELDED_OUTPUTS_HASH_PERSONALIZATION: [u8; 16] = *b"ZcashSOutputHash";

pub fn signature_hash(
    tx: &Transaction,
    input_index: Option<usize>,
    script_code: &[u8],
    amount: i64,
    sighash_type: SighashType,
    consensus_branch_id: u32,
) -> Result<Hash256, SighashError> {
    let sigversion = signature_hash_version(tx)?;

    match sigversion {
        SigVersion::Sprout => signature_hash_sprout(tx, input_index, script_code, sighash_type),
        SigVersion::Overwinter => signature_hash_overwinter(
            tx,
            input_index,
            script_code,
            amount,
            sighash_type,
            consensus_branch_id,
            false,
        ),
        SigVersion::Sapling => signature_hash_overwinter(
            tx,
            input_index,
            script_code,
            amount,
            sighash_type,
            consensus_branch_id,
            true,
        ),
    }
}

fn signature_hash_version(tx: &Transaction) -> Result<SigVersion, SighashError> {
    if tx.f_overwintered {
        if tx.version_group_id == SAPLING_VERSION_GROUP_ID {
            Ok(SigVersion::Sapling)
        } else if tx.version_group_id == OVERWINTER_VERSION_GROUP_ID {
            Ok(SigVersion::Overwinter)
        } else {
            Err(SighashError::UnsupportedTransactionFormat(
                "unknown overwintered version group id",
            ))
        }
    } else {
        Ok(SigVersion::Sprout)
    }
}

fn signature_hash_sprout(
    tx: &Transaction,
    input_index: Option<usize>,
    script_code: &[u8],
    sighash_type: SighashType,
) -> Result<Hash256, SighashError> {
    if let Some(index) = input_index {
        if index >= tx.vin.len() {
            return Err(SighashError::InputIndexOutOfRange);
        }
        if sighash_type.base_type() == SIGHASH_SINGLE && index >= tx.vout.len() {
            return Err(SighashError::MissingOutput);
        }
    } else if sighash_type.base_type() == SIGHASH_SINGLE {
        return Err(SighashError::MissingOutput);
    }

    if tx.f_overwintered {
        return Err(SighashError::UnsupportedTransactionFormat(
            "sprout sighash requires non-overwintered transaction",
        ));
    }

    let anyone_can_pay = sighash_type.has_anyone_can_pay();
    if anyone_can_pay && input_index.is_none() {
        return Err(SighashError::InputIndexOutOfRange);
    }
    let hash_single = sighash_type.base_type() == SIGHASH_SINGLE;
    let hash_none = sighash_type.base_type() == SIGHASH_NONE;

    let mut encoder = Encoder::new();
    encoder.write_i32_le(tx.version);

    let input_count = if anyone_can_pay { 1 } else { tx.vin.len() };
    encoder.write_varint(input_count as u64);
    for idx in 0..input_count {
        let actual_index = if anyone_can_pay {
            input_index.expect("checked by anyone_can_pay guard")
        } else {
            idx
        };
        let input = &tx.vin[actual_index];
        input.prevout.consensus_encode(&mut encoder);
        let is_signing = input_index.map_or(false, |signing| actual_index == signing);
        if is_signing {
            encoder.write_var_bytes(script_code);
        } else {
            encoder.write_varint(0);
        }

        if !is_signing && (hash_single || hash_none) {
            encoder.write_u32_le(0);
        } else {
            encoder.write_u32_le(input.sequence);
        }
    }

    let output_count = if hash_none {
        0
    } else if hash_single {
        input_index.ok_or(SighashError::MissingOutput)? + 1
    } else {
        tx.vout.len()
    };
    encoder.write_varint(output_count as u64);
    for idx in 0..output_count {
        if hash_single && Some(idx) != input_index {
            encoder.write_i64_le(-1);
            encoder.write_varint(0);
        } else {
            tx.vout[idx].consensus_encode(&mut encoder);
        }
    }

    encoder.write_u32_le(tx.lock_time);

    if tx.version >= 2 {
        encoder.write_varint(tx.join_splits.len() as u64);
        for join_split in &tx.join_splits {
            join_split.consensus_encode_with(&mut encoder, false)?;
        }
        if !tx.join_splits.is_empty() {
            encoder.write_bytes(&tx.join_split_pub_key);
            encoder.write_bytes(&[0u8; 64]);
        }
    }

    let mut payload = encoder.into_inner();
    let mut final_encoder = Encoder::new();
    final_encoder.write_bytes(&payload);
    final_encoder.write_u32_le(sighash_type.0);
    payload = final_encoder.into_inner();

    Ok(sha256d(&payload))
}

fn signature_hash_overwinter(
    tx: &Transaction,
    input_index: Option<usize>,
    script_code: &[u8],
    amount: i64,
    sighash_type: SighashType,
    consensus_branch_id: u32,
    sapling: bool,
) -> Result<Hash256, SighashError> {
    if let Some(index) = input_index {
        if index >= tx.vin.len() {
            return Err(SighashError::InputIndexOutOfRange);
        }
    }

    let anyone_can_pay = sighash_type.has_anyone_can_pay();
    let base = sighash_type.base_type();

    let hash_prevouts = if !anyone_can_pay {
        hash_prevouts(tx)
    } else {
        [0u8; 32]
    };

    let hash_sequence = if !anyone_can_pay && base != SIGHASH_SINGLE && base != SIGHASH_NONE {
        hash_sequence(tx)
    } else {
        [0u8; 32]
    };

    let hash_outputs = if base != SIGHASH_SINGLE && base != SIGHASH_NONE {
        hash_outputs_all(tx)
    } else if base == SIGHASH_SINGLE {
        if let Some(index) = input_index {
            if index < tx.vout.len() {
                hash_outputs_single(&tx.vout[index])
            } else {
                [0u8; 32]
            }
        } else {
            [0u8; 32]
        }
    } else {
        [0u8; 32]
    };

    let hash_join_splits = if !tx.join_splits.is_empty() {
        hash_join_splits(tx)?
    } else {
        [0u8; 32]
    };

    let hash_shielded_spends = if sapling && !tx.shielded_spends.is_empty() {
        hash_shielded_spends(&tx.shielded_spends)
    } else {
        [0u8; 32]
    };

    let hash_shielded_outputs = if sapling && !tx.shielded_outputs.is_empty() {
        hash_shielded_outputs(&tx.shielded_outputs)
    } else {
        [0u8; 32]
    };

    let mut personalization = [0u8; 16];
    personalization[..12].copy_from_slice(b"ZcashSigHash");
    personalization[12..].copy_from_slice(&consensus_branch_id.to_le_bytes());

    let mut encoder = Encoder::new();
    encoder.write_u32_le(tx.header());
    encoder.write_u32_le(tx.version_group_id);
    encoder.write_bytes(&hash_prevouts);
    encoder.write_bytes(&hash_sequence);
    encoder.write_bytes(&hash_outputs);
    encoder.write_bytes(&hash_join_splits);

    if sapling {
        encoder.write_bytes(&hash_shielded_spends);
        encoder.write_bytes(&hash_shielded_outputs);
    }

    encoder.write_u32_le(tx.lock_time);
    encoder.write_u32_le(tx.expiry_height);

    if sapling {
        encoder.write_i64_le(tx.value_balance);
    }

    encoder.write_u32_le(sighash_type.0);

    if let Some(index) = input_index {
        let input = &tx.vin[index];
        input.prevout.consensus_encode(&mut encoder);
        encoder.write_var_bytes(script_code);
        encoder.write_i64_le(amount);
        encoder.write_u32_le(input.sequence);
    }

    Ok(blake2b_hash(personalization, &encoder.into_inner()))
}

fn blake2b_hash(personalization: [u8; 16], data: &[u8]) -> Hash256 {
    let mut state = Blake2bParams::new()
        .hash_length(32)
        .personal(&personalization)
        .to_state();
    state.update(data);
    let hash = state.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(hash.as_bytes());
    out
}

fn hash_prevouts(tx: &Transaction) -> Hash256 {
    let mut encoder = Encoder::new();
    for input in &tx.vin {
        input.prevout.consensus_encode(&mut encoder);
    }
    blake2b_hash(ZCASH_PREVOUTS_HASH_PERSONALIZATION, &encoder.into_inner())
}

fn hash_sequence(tx: &Transaction) -> Hash256 {
    let mut encoder = Encoder::new();
    for input in &tx.vin {
        encoder.write_u32_le(input.sequence);
    }
    blake2b_hash(ZCASH_SEQUENCE_HASH_PERSONALIZATION, &encoder.into_inner())
}

fn hash_outputs_all(tx: &Transaction) -> Hash256 {
    let mut encoder = Encoder::new();
    for output in &tx.vout {
        output.consensus_encode(&mut encoder);
    }
    blake2b_hash(ZCASH_OUTPUTS_HASH_PERSONALIZATION, &encoder.into_inner())
}

fn hash_outputs_single(output: &TxOut) -> Hash256 {
    let mut encoder = Encoder::new();
    output.consensus_encode(&mut encoder);
    blake2b_hash(ZCASH_OUTPUTS_HASH_PERSONALIZATION, &encoder.into_inner())
}

fn hash_join_splits(tx: &Transaction) -> Result<Hash256, SighashError> {
    let mut encoder = Encoder::new();
    let use_groth = tx.f_overwintered && tx.version >= 4;
    for join_split in &tx.join_splits {
        join_split.consensus_encode_with(&mut encoder, use_groth)?;
    }
    encoder.write_bytes(&tx.join_split_pub_key);
    Ok(blake2b_hash(
        ZCASH_JOINSPLITS_HASH_PERSONALIZATION,
        &encoder.into_inner(),
    ))
}

fn hash_shielded_spends(spends: &[SpendDescription]) -> Hash256 {
    let mut encoder = Encoder::new();
    for spend in spends {
        encoder.write_hash_le(&spend.cv);
        encoder.write_hash_le(&spend.anchor);
        encoder.write_hash_le(&spend.nullifier);
        encoder.write_hash_le(&spend.rk);
        encoder.write_bytes(&spend.zkproof);
    }
    blake2b_hash(
        ZCASH_SHIELDED_SPENDS_HASH_PERSONALIZATION,
        &encoder.into_inner(),
    )
}

fn hash_shielded_outputs(outputs: &[OutputDescription]) -> Hash256 {
    let mut encoder = Encoder::new();
    for output in outputs {
        output.consensus_encode(&mut encoder);
    }
    blake2b_hash(
        ZCASH_SHIELDED_OUTPUTS_HASH_PERSONALIZATION,
        &encoder.into_inner(),
    )
}
