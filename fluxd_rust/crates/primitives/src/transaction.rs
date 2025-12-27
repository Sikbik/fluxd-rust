//! Transaction types and serialization.

use fluxd_consensus::Hash256;

use crate::encoding::{Decodable, DecodeError, Decoder, Encodable, Encoder};
use crate::hash::sha256d;
use crate::outpoint::OutPoint;

pub const OVERWINTER_VERSION_GROUP_ID: u32 = 0x03C4_8270;
pub const SAPLING_VERSION_GROUP_ID: u32 = 0x892F_2085;

pub const FLUXNODE_TX_VERSION: i32 = 5;
pub const FLUXNODE_TX_UPGRADEABLE_VERSION: i32 = 6;

pub const FLUXNODE_INTERNAL_NORMAL_TX_VERSION: i32 = 1;
pub const FLUXNODE_INTERNAL_P2SH_TX_VERSION: i32 = 2;

pub const FLUXNODE_TX_TYPE_MASK: i32 = 0xFF;
pub const FLUXNODE_TX_TYPE_NORMAL_BIT: i32 = 0x01;
pub const FLUXNODE_TX_TYPE_P2SH_BIT: i32 = 0x02;

pub const FLUXNODE_TX_FEATURE_MASK: i32 = 0xFF00;
pub const FLUXNODE_TX_FEATURE_DELEGATES_BIT: i32 = 0x0100;

pub const FLUXNODE_START_TX_TYPE: u8 = 1 << 1;
pub const FLUXNODE_CONFIRM_TX_TYPE: u8 = 1 << 2;

pub const ZC_NUM_JS_INPUTS: usize = 2;
pub const ZC_NUM_JS_OUTPUTS: usize = 2;
pub const ZC_NOTE_CIPHERTEXT_SIZE: usize = 601;
pub const SAPLING_ENC_CIPHERTEXT_SIZE: usize = 580;
pub const SAPLING_OUT_CIPHERTEXT_SIZE: usize = 80;
pub const GROTH_PROOF_SIZE: usize = 192;
pub const PHGR_PROOF_SIZE: usize = 296;

pub fn has_conflicting_bits(version: i32) -> bool {
    (version & FLUXNODE_TX_TYPE_NORMAL_BIT) != 0 && (version & FLUXNODE_TX_TYPE_P2SH_BIT) != 0
}

pub fn is_flux_tx_normal_type(version: i32, include_bit_check: bool) -> bool {
    if include_bit_check {
        if has_conflicting_bits(version) {
            return false;
        }
        return (version & FLUXNODE_TX_TYPE_NORMAL_BIT) != 0
            || version == FLUXNODE_INTERNAL_NORMAL_TX_VERSION;
    }
    version == FLUXNODE_INTERNAL_NORMAL_TX_VERSION
}

pub fn is_flux_tx_p2sh_type(version: i32, include_bit_check: bool) -> bool {
    if include_bit_check {
        if has_conflicting_bits(version) {
            return false;
        }
        return (version & FLUXNODE_TX_TYPE_P2SH_BIT) != 0
            || version == FLUXNODE_INTERNAL_P2SH_TX_VERSION;
    }
    version == FLUXNODE_INTERNAL_P2SH_TX_VERSION
}

pub fn has_flux_tx_delegates_feature(version: i32) -> bool {
    (version & FLUXNODE_TX_FEATURE_DELEGATES_BIT) != 0
}

#[derive(Clone, Debug, PartialEq)]
pub struct TxIn {
    pub prevout: OutPoint,
    pub script_sig: Vec<u8>,
    pub sequence: u32,
}

impl Encodable for TxIn {
    fn consensus_encode(&self, encoder: &mut Encoder) {
        self.prevout.consensus_encode(encoder);
        encoder.write_var_bytes(&self.script_sig);
        encoder.write_u32_le(self.sequence);
    }
}

impl Decodable for TxIn {
    fn consensus_decode(decoder: &mut Decoder) -> Result<Self, DecodeError> {
        let prevout = OutPoint::consensus_decode(decoder)?;
        let script_sig = decoder.read_var_bytes()?;
        let sequence = decoder.read_u32_le()?;
        Ok(Self {
            prevout,
            script_sig,
            sequence,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TxOut {
    pub value: i64,
    pub script_pubkey: Vec<u8>,
}

impl Encodable for TxOut {
    fn consensus_encode(&self, encoder: &mut Encoder) {
        encoder.write_i64_le(self.value);
        encoder.write_var_bytes(&self.script_pubkey);
    }
}

impl Decodable for TxOut {
    fn consensus_decode(decoder: &mut Decoder) -> Result<Self, DecodeError> {
        let value = decoder.read_i64_le()?;
        let script_pubkey = decoder.read_var_bytes()?;
        Ok(Self {
            value,
            script_pubkey,
        })
    }
}

pub type GrothProof = [u8; GROTH_PROOF_SIZE];
pub type PhgrProof = [u8; PHGR_PROOF_SIZE];
pub type JoinSplitCiphertext = [u8; ZC_NOTE_CIPHERTEXT_SIZE];
pub type SaplingEncCiphertext = [u8; SAPLING_ENC_CIPHERTEXT_SIZE];
pub type SaplingOutCiphertext = [u8; SAPLING_OUT_CIPHERTEXT_SIZE];

#[derive(Clone, Debug, PartialEq)]
pub enum SproutProof {
    Groth(GrothProof),
    Phgr(PhgrProof),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpendDescription {
    pub cv: Hash256,
    pub anchor: Hash256,
    pub nullifier: Hash256,
    pub rk: Hash256,
    pub zkproof: GrothProof,
    pub spend_auth_sig: [u8; 64],
}

impl Encodable for SpendDescription {
    fn consensus_encode(&self, encoder: &mut Encoder) {
        encoder.write_hash_le(&self.cv);
        encoder.write_hash_le(&self.anchor);
        encoder.write_hash_le(&self.nullifier);
        encoder.write_hash_le(&self.rk);
        encoder.write_bytes(&self.zkproof);
        encoder.write_bytes(&self.spend_auth_sig);
    }
}

impl Decodable for SpendDescription {
    fn consensus_decode(decoder: &mut Decoder) -> Result<Self, DecodeError> {
        Ok(Self {
            cv: decoder.read_hash_le()?,
            anchor: decoder.read_hash_le()?,
            nullifier: decoder.read_hash_le()?,
            rk: decoder.read_hash_le()?,
            zkproof: decoder.read_fixed::<GROTH_PROOF_SIZE>()?,
            spend_auth_sig: decoder.read_fixed::<64>()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OutputDescription {
    pub cv: Hash256,
    pub cm: Hash256,
    pub ephemeral_key: Hash256,
    pub enc_ciphertext: SaplingEncCiphertext,
    pub out_ciphertext: SaplingOutCiphertext,
    pub zkproof: GrothProof,
}

impl Encodable for OutputDescription {
    fn consensus_encode(&self, encoder: &mut Encoder) {
        encoder.write_hash_le(&self.cv);
        encoder.write_hash_le(&self.cm);
        encoder.write_hash_le(&self.ephemeral_key);
        encoder.write_bytes(&self.enc_ciphertext);
        encoder.write_bytes(&self.out_ciphertext);
        encoder.write_bytes(&self.zkproof);
    }
}

impl Decodable for OutputDescription {
    fn consensus_decode(decoder: &mut Decoder) -> Result<Self, DecodeError> {
        Ok(Self {
            cv: decoder.read_hash_le()?,
            cm: decoder.read_hash_le()?,
            ephemeral_key: decoder.read_hash_le()?,
            enc_ciphertext: decoder.read_fixed::<SAPLING_ENC_CIPHERTEXT_SIZE>()?,
            out_ciphertext: decoder.read_fixed::<SAPLING_OUT_CIPHERTEXT_SIZE>()?,
            zkproof: decoder.read_fixed::<GROTH_PROOF_SIZE>()?,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct JoinSplit {
    pub vpub_old: i64,
    pub vpub_new: i64,
    pub anchor: Hash256,
    pub nullifiers: [Hash256; ZC_NUM_JS_INPUTS],
    pub commitments: [Hash256; ZC_NUM_JS_OUTPUTS],
    pub ephemeral_key: Hash256,
    pub random_seed: Hash256,
    pub macs: [Hash256; ZC_NUM_JS_INPUTS],
    pub proof: SproutProof,
    pub ciphertexts: [JoinSplitCiphertext; ZC_NUM_JS_OUTPUTS],
}

impl JoinSplit {
    pub fn consensus_encode_with(
        &self,
        encoder: &mut Encoder,
        use_groth: bool,
    ) -> Result<(), TransactionEncodeError> {
        encoder.write_i64_le(self.vpub_old);
        encoder.write_i64_le(self.vpub_new);
        encoder.write_hash_le(&self.anchor);
        for nullifier in &self.nullifiers {
            encoder.write_hash_le(nullifier);
        }
        for commitment in &self.commitments {
            encoder.write_hash_le(commitment);
        }
        encoder.write_hash_le(&self.ephemeral_key);
        encoder.write_hash_le(&self.random_seed);
        for mac in &self.macs {
            encoder.write_hash_le(mac);
        }
        match (&self.proof, use_groth) {
            (SproutProof::Groth(proof), true) => encoder.write_bytes(proof),
            (SproutProof::Phgr(proof), false) => encoder.write_bytes(proof),
            _ => {
                return Err(TransactionEncodeError::InvalidTransactionFormat(
                    "joinsplit proof does not match transaction format",
                ))
            }
        }
        for ciphertext in &self.ciphertexts {
            encoder.write_bytes(ciphertext);
        }
        Ok(())
    }

    fn consensus_decode_with(decoder: &mut Decoder, use_groth: bool) -> Result<Self, DecodeError> {
        let vpub_old = decoder.read_i64_le()?;
        let vpub_new = decoder.read_i64_le()?;
        let anchor = decoder.read_hash_le()?;
        let mut nullifiers: [Hash256; ZC_NUM_JS_INPUTS] = [[0u8; 32]; ZC_NUM_JS_INPUTS];
        for slot in &mut nullifiers {
            *slot = decoder.read_hash_le()?;
        }
        let mut commitments: [Hash256; ZC_NUM_JS_OUTPUTS] = [[0u8; 32]; ZC_NUM_JS_OUTPUTS];
        for slot in &mut commitments {
            *slot = decoder.read_hash_le()?;
        }
        let ephemeral_key = decoder.read_hash_le()?;
        let random_seed = decoder.read_hash_le()?;
        let mut macs: [Hash256; ZC_NUM_JS_INPUTS] = [[0u8; 32]; ZC_NUM_JS_INPUTS];
        for slot in &mut macs {
            *slot = decoder.read_hash_le()?;
        }
        let proof = if use_groth {
            SproutProof::Groth(decoder.read_fixed::<GROTH_PROOF_SIZE>()?)
        } else {
            SproutProof::Phgr(decoder.read_fixed::<PHGR_PROOF_SIZE>()?)
        };
        let mut ciphertexts: [JoinSplitCiphertext; ZC_NUM_JS_OUTPUTS] =
            [[0u8; ZC_NOTE_CIPHERTEXT_SIZE]; ZC_NUM_JS_OUTPUTS];
        for slot in &mut ciphertexts {
            *slot = decoder.read_fixed::<ZC_NOTE_CIPHERTEXT_SIZE>()?;
        }
        Ok(Self {
            vpub_old,
            vpub_new,
            anchor,
            nullifiers,
            commitments,
            ephemeral_key,
            random_seed,
            macs,
            proof,
            ciphertexts,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FluxnodeDelegates {
    pub version: u8,
    pub kind: u8,
    pub delegate_starting_keys: Vec<Vec<u8>>,
}

impl FluxnodeDelegates {
    pub const INITIAL_VERSION: u8 = 1;
    pub const NONE: u8 = 0;
    pub const UPDATE: u8 = 1;
    pub const SIGNING: u8 = 2;
}

impl Encodable for FluxnodeDelegates {
    fn consensus_encode(&self, encoder: &mut Encoder) {
        encoder.write_u8(self.version);
        encoder.write_u8(self.kind);
        if self.version == Self::INITIAL_VERSION && self.kind == Self::UPDATE {
            encoder.write_varint(self.delegate_starting_keys.len() as u64);
            for key in &self.delegate_starting_keys {
                encoder.write_var_bytes(key);
            }
        }
    }
}

impl Decodable for FluxnodeDelegates {
    fn consensus_decode(decoder: &mut Decoder) -> Result<Self, DecodeError> {
        let version = decoder.read_u8()?;
        let kind = decoder.read_u8()?;
        let mut delegate_starting_keys = Vec::new();
        if version == Self::INITIAL_VERSION && kind == Self::UPDATE {
            let count = decoder.read_varint()?;
            let count = usize::try_from(count).map_err(|_| DecodeError::SizeTooLarge)?;
            delegate_starting_keys.reserve(count);
            for _ in 0..count {
                delegate_starting_keys.push(decoder.read_var_bytes()?);
            }
        }
        Ok(Self {
            version,
            kind,
            delegate_starting_keys,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FluxnodeConfirmTx {
    pub collateral: OutPoint,
    pub sig_time: u32,
    pub benchmark_tier: u8,
    pub benchmark_sig_time: u32,
    pub update_type: u8,
    pub ip: String,
    pub sig: Vec<u8>,
    pub benchmark_sig: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FluxnodeStartV5 {
    pub collateral: OutPoint,
    pub collateral_pubkey: Vec<u8>,
    pub pubkey: Vec<u8>,
    pub sig_time: u32,
    pub sig: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FluxnodeStartVariantV6 {
    Normal {
        collateral: OutPoint,
        collateral_pubkey: Vec<u8>,
        pubkey: Vec<u8>,
        sig_time: u32,
        sig: Vec<u8>,
    },
    P2sh {
        collateral: OutPoint,
        pubkey: Vec<u8>,
        redeem_script: Vec<u8>,
        sig_time: u32,
        sig: Vec<u8>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct FluxnodeStartV6 {
    pub flux_tx_version: i32,
    pub variant: FluxnodeStartVariantV6,
    pub using_delegates: bool,
    pub delegates: Option<FluxnodeDelegates>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FluxnodeTxV5 {
    Start(FluxnodeStartV5),
    Confirm(FluxnodeConfirmTx),
}

#[derive(Clone, Debug, PartialEq)]
pub enum FluxnodeTxV6 {
    Start(FluxnodeStartV6),
    Confirm(FluxnodeConfirmTx),
}

#[derive(Clone, Debug, PartialEq)]
pub enum FluxnodeTx {
    V5(FluxnodeTxV5),
    V6(FluxnodeTxV6),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Transaction {
    pub f_overwintered: bool,
    pub version: i32,
    pub version_group_id: u32,
    pub vin: Vec<TxIn>,
    pub vout: Vec<TxOut>,
    pub lock_time: u32,
    pub expiry_height: u32,
    pub value_balance: i64,
    pub shielded_spends: Vec<SpendDescription>,
    pub shielded_outputs: Vec<OutputDescription>,
    pub join_splits: Vec<JoinSplit>,
    pub join_split_pub_key: [u8; 32],
    pub join_split_sig: [u8; 64],
    pub binding_sig: [u8; 64],
    pub fluxnode: Option<FluxnodeTx>,
}

impl Transaction {
    pub fn header(&self) -> u32 {
        let mut header = self.version as u32;
        if self.f_overwintered {
            header |= 1 << 31;
        }
        header
    }

    pub fn consensus_encode(&self) -> Result<Vec<u8>, TransactionEncodeError> {
        self.encode_with_mode(true)
    }

    pub fn consensus_encode_for_hash(&self) -> Result<Vec<u8>, TransactionEncodeError> {
        self.encode_with_mode(false)
    }

    pub fn txid(&self) -> Result<Hash256, TransactionEncodeError> {
        Ok(sha256d(&self.consensus_encode_for_hash()?))
    }

    fn encode_with_mode(
        &self,
        include_signatures: bool,
    ) -> Result<Vec<u8>, TransactionEncodeError> {
        let mut encoder = Encoder::new();
        let mut header = self.version as u32;
        if self.f_overwintered {
            header |= 1 << 31;
        }
        encoder.write_u32_le(header);

        if self.f_overwintered {
            encoder.write_u32_le(self.version_group_id);
        }

        if self.version == FLUXNODE_TX_VERSION {
            if self.f_overwintered {
                return Err(TransactionEncodeError::InvalidTransactionFormat(
                    "fluxnode tx must not be overwintered",
                ));
            }
            let fluxnode =
                self.fluxnode
                    .as_ref()
                    .ok_or(TransactionEncodeError::InvalidTransactionFormat(
                        "missing fluxnode payload for v5",
                    ))?;
            encode_fluxnode_v5(fluxnode, &mut encoder, include_signatures)?;
            return Ok(encoder.into_inner());
        }

        if self.version == FLUXNODE_TX_UPGRADEABLE_VERSION {
            if self.f_overwintered {
                return Err(TransactionEncodeError::InvalidTransactionFormat(
                    "fluxnode tx must not be overwintered",
                ));
            }
            let fluxnode =
                self.fluxnode
                    .as_ref()
                    .ok_or(TransactionEncodeError::InvalidTransactionFormat(
                        "missing fluxnode payload for v6",
                    ))?;
            encode_fluxnode_v6(fluxnode, &mut encoder, include_signatures)?;
            return Ok(encoder.into_inner());
        }

        let is_overwinter_v3 = self.f_overwintered
            && self.version_group_id == OVERWINTER_VERSION_GROUP_ID
            && self.version == 3;
        let is_sapling_v4 = self.f_overwintered
            && self.version_group_id == SAPLING_VERSION_GROUP_ID
            && self.version == 4;

        if self.f_overwintered && !(is_overwinter_v3 || is_sapling_v4) {
            return Err(TransactionEncodeError::InvalidTransactionFormat(
                "unknown overwinter transaction format",
            ));
        }

        encoder.write_varint(self.vin.len() as u64);
        for input in &self.vin {
            input.consensus_encode(&mut encoder);
        }
        encoder.write_varint(self.vout.len() as u64);
        for output in &self.vout {
            output.consensus_encode(&mut encoder);
        }
        encoder.write_u32_le(self.lock_time);

        if is_overwinter_v3 || is_sapling_v4 {
            encoder.write_u32_le(self.expiry_height);
        }
        if is_sapling_v4 {
            encoder.write_i64_le(self.value_balance);
            write_vec(&mut encoder, &self.shielded_spends);
            write_vec(&mut encoder, &self.shielded_outputs);
        }

        if self.version >= 2 {
            encoder.write_varint(self.join_splits.len() as u64);
            let use_groth = self.f_overwintered && self.version >= 4;
            for join_split in &self.join_splits {
                join_split.consensus_encode_with(&mut encoder, use_groth)?;
            }
            if !self.join_splits.is_empty() {
                encoder.write_bytes(&self.join_split_pub_key);
                encoder.write_bytes(&self.join_split_sig);
            }
        }

        if is_sapling_v4 && !(self.shielded_spends.is_empty() && self.shielded_outputs.is_empty()) {
            encoder.write_bytes(&self.binding_sig);
        }

        Ok(encoder.into_inner())
    }

    pub fn consensus_decode(bytes: &[u8]) -> Result<Self, TransactionDecodeError> {
        Self::decode_with_mode(bytes, true)
    }

    pub fn consensus_decode_for_hash(bytes: &[u8]) -> Result<Self, TransactionDecodeError> {
        Self::decode_with_mode(bytes, false)
    }

    fn decode_with_mode(
        bytes: &[u8],
        include_signatures: bool,
    ) -> Result<Self, TransactionDecodeError> {
        let mut decoder = Decoder::new(bytes);
        let tx = Self::decode_from(&mut decoder, include_signatures)?;
        if !decoder.is_empty() {
            return Err(TransactionDecodeError::Decode(DecodeError::TrailingBytes));
        }
        Ok(tx)
    }

    pub(crate) fn decode_from(
        decoder: &mut Decoder,
        include_signatures: bool,
    ) -> Result<Self, TransactionDecodeError> {
        let header = decoder.read_u32_le()?;
        let f_overwintered = (header >> 31) != 0;
        let version = (header & 0x7fff_ffff) as i32;
        let version_group_id = if f_overwintered {
            decoder.read_u32_le()?
        } else {
            0
        };

        let is_overwinter_v3 =
            f_overwintered && version_group_id == OVERWINTER_VERSION_GROUP_ID && version == 3;
        let is_sapling_v4 =
            f_overwintered && version_group_id == SAPLING_VERSION_GROUP_ID && version == 4;

        if f_overwintered && !(is_overwinter_v3 || is_sapling_v4) {
            return Err(TransactionDecodeError::InvalidTransactionFormat(
                "unknown overwinter transaction format",
            ));
        }

        if version == FLUXNODE_TX_VERSION {
            if f_overwintered {
                return Err(TransactionDecodeError::InvalidTransactionFormat(
                    "fluxnode tx must not be overwintered",
                ));
            }
            let fluxnode = decode_fluxnode_v5(decoder, include_signatures)?;
            return Ok(Transaction {
                f_overwintered,
                version,
                version_group_id,
                vin: Vec::new(),
                vout: Vec::new(),
                lock_time: 0,
                expiry_height: 0,
                value_balance: 0,
                shielded_spends: Vec::new(),
                shielded_outputs: Vec::new(),
                join_splits: Vec::new(),
                join_split_pub_key: [0u8; 32],
                join_split_sig: [0u8; 64],
                binding_sig: [0u8; 64],
                fluxnode: Some(FluxnodeTx::V5(fluxnode)),
            });
        }

        if version == FLUXNODE_TX_UPGRADEABLE_VERSION {
            if f_overwintered {
                return Err(TransactionDecodeError::InvalidTransactionFormat(
                    "fluxnode tx must not be overwintered",
                ));
            }
            let fluxnode = decode_fluxnode_v6(decoder, include_signatures)?;
            return Ok(Transaction {
                f_overwintered,
                version,
                version_group_id,
                vin: Vec::new(),
                vout: Vec::new(),
                lock_time: 0,
                expiry_height: 0,
                value_balance: 0,
                shielded_spends: Vec::new(),
                shielded_outputs: Vec::new(),
                join_splits: Vec::new(),
                join_split_pub_key: [0u8; 32],
                join_split_sig: [0u8; 64],
                binding_sig: [0u8; 64],
                fluxnode: Some(FluxnodeTx::V6(fluxnode)),
            });
        }

        let vin = read_vec(decoder)?;
        let vout = read_vec(decoder)?;
        let lock_time = decoder.read_u32_le()?;
        let expiry_height = if is_overwinter_v3 || is_sapling_v4 {
            decoder.read_u32_le()?
        } else {
            0
        };

        let (value_balance, shielded_spends, shielded_outputs) = if is_sapling_v4 {
            let value_balance = decoder.read_i64_le()?;
            let shielded_spends = read_vec(decoder)?;
            let shielded_outputs = read_vec(decoder)?;
            (value_balance, shielded_spends, shielded_outputs)
        } else {
            (0, Vec::new(), Vec::new())
        };

        let (join_splits, join_split_pub_key, join_split_sig) = if version >= 2 {
            let count = decoder.read_varint()?;
            let count = usize::try_from(count).map_err(|_| DecodeError::SizeTooLarge)?;
            let use_groth = f_overwintered && version >= 4;
            let mut join_splits = Vec::with_capacity(count);
            for _ in 0..count {
                join_splits.push(JoinSplit::consensus_decode_with(decoder, use_groth)?);
            }
            let mut join_split_pub_key = [0u8; 32];
            let mut join_split_sig = [0u8; 64];
            if !join_splits.is_empty() {
                join_split_pub_key = decoder.read_fixed::<32>()?;
                join_split_sig = decoder.read_fixed::<64>()?;
            }
            (join_splits, join_split_pub_key, join_split_sig)
        } else {
            (Vec::new(), [0u8; 32], [0u8; 64])
        };

        let binding_sig =
            if is_sapling_v4 && !(shielded_spends.is_empty() && shielded_outputs.is_empty()) {
                decoder.read_fixed::<64>()?
            } else {
                [0u8; 64]
            };

        Ok(Transaction {
            f_overwintered,
            version,
            version_group_id,
            vin,
            vout,
            lock_time,
            expiry_height,
            value_balance,
            shielded_spends,
            shielded_outputs,
            join_splits,
            join_split_pub_key,
            join_split_sig,
            binding_sig,
            fluxnode: None,
        })
    }
}

fn write_vec<T: Encodable>(encoder: &mut Encoder, values: &[T]) {
    encoder.write_varint(values.len() as u64);
    for value in values {
        value.consensus_encode(encoder);
    }
}

fn read_vec<T: Decodable>(decoder: &mut Decoder) -> Result<Vec<T>, DecodeError> {
    let count = decoder.read_varint()?;
    let count = usize::try_from(count).map_err(|_| DecodeError::SizeTooLarge)?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(T::consensus_decode(decoder)?);
    }
    Ok(values)
}

fn encode_fluxnode_v5(
    fluxnode: &FluxnodeTx,
    encoder: &mut Encoder,
    include_signatures: bool,
) -> Result<(), TransactionEncodeError> {
    let body = match fluxnode {
        FluxnodeTx::V5(body) => body,
        _ => {
            return Err(TransactionEncodeError::InvalidTransactionFormat(
                "expected v5 fluxnode payload",
            ))
        }
    };

    match body {
        FluxnodeTxV5::Start(start) => {
            encoder.write_u8(FLUXNODE_START_TX_TYPE);
            start.collateral.consensus_encode(encoder);
            encoder.write_var_bytes(&start.collateral_pubkey);
            encoder.write_var_bytes(&start.pubkey);
            encoder.write_u32_le(start.sig_time);
            if include_signatures {
                encoder.write_var_bytes(&start.sig);
            }
        }
        FluxnodeTxV5::Confirm(confirm) => {
            encoder.write_u8(FLUXNODE_CONFIRM_TX_TYPE);
            confirm.collateral.consensus_encode(encoder);
            encoder.write_u32_le(confirm.sig_time);
            encoder.write_u8(confirm.benchmark_tier);
            encoder.write_u32_le(confirm.benchmark_sig_time);
            encoder.write_u8(confirm.update_type);
            encoder.write_var_str(&confirm.ip);
            if include_signatures {
                encoder.write_var_bytes(&confirm.sig);
                encoder.write_var_bytes(&confirm.benchmark_sig);
            }
        }
    }

    Ok(())
}

fn encode_fluxnode_v6(
    fluxnode: &FluxnodeTx,
    encoder: &mut Encoder,
    include_signatures: bool,
) -> Result<(), TransactionEncodeError> {
    let body = match fluxnode {
        FluxnodeTx::V6(body) => body,
        _ => {
            return Err(TransactionEncodeError::InvalidTransactionFormat(
                "expected v6 fluxnode payload",
            ))
        }
    };

    match body {
        FluxnodeTxV6::Start(start) => {
            encoder.write_u8(FLUXNODE_START_TX_TYPE);
            encoder.write_i32_le(start.flux_tx_version);

            let is_normal = is_flux_tx_normal_type(start.flux_tx_version, true)
                && (start.flux_tx_version & FLUXNODE_TX_TYPE_P2SH_BIT) == 0;
            let is_p2sh = is_flux_tx_p2sh_type(start.flux_tx_version, true);

            if is_normal {
                if let FluxnodeStartVariantV6::Normal {
                    collateral,
                    collateral_pubkey,
                    pubkey,
                    sig_time,
                    sig,
                } = &start.variant
                {
                    collateral.consensus_encode(encoder);
                    encoder.write_var_bytes(collateral_pubkey);
                    encoder.write_var_bytes(pubkey);
                    encoder.write_u32_le(*sig_time);
                    if include_signatures {
                        encoder.write_var_bytes(sig);
                    }
                } else {
                    return Err(TransactionEncodeError::InvalidTransactionFormat(
                        "fluxnode v6 normal start missing fields",
                    ));
                }
            } else if is_p2sh {
                if let FluxnodeStartVariantV6::P2sh {
                    collateral,
                    pubkey,
                    redeem_script,
                    sig_time,
                    sig,
                } = &start.variant
                {
                    collateral.consensus_encode(encoder);
                    encoder.write_var_bytes(pubkey);
                    encoder.write_var_bytes(redeem_script);
                    encoder.write_u32_le(*sig_time);
                    if include_signatures {
                        encoder.write_var_bytes(sig);
                    }
                } else {
                    return Err(TransactionEncodeError::InvalidTransactionFormat(
                        "fluxnode v6 p2sh start missing fields",
                    ));
                }
            } else {
                return Err(TransactionEncodeError::InvalidTransactionFormat(
                    "fluxnode v6 start has invalid version bits",
                ));
            }

            if has_flux_tx_delegates_feature(start.flux_tx_version) {
                encoder.write_u8(if start.using_delegates { 1 } else { 0 });
                if start.using_delegates {
                    let delegates = start.delegates.as_ref().ok_or(
                        TransactionEncodeError::InvalidTransactionFormat(
                            "missing delegates data for fluxnode v6 start",
                        ),
                    )?;
                    delegates.consensus_encode(encoder);
                }
            }
        }
        FluxnodeTxV6::Confirm(confirm) => {
            encoder.write_u8(FLUXNODE_CONFIRM_TX_TYPE);
            confirm.collateral.consensus_encode(encoder);
            encoder.write_u32_le(confirm.sig_time);
            encoder.write_u8(confirm.benchmark_tier);
            encoder.write_u32_le(confirm.benchmark_sig_time);
            encoder.write_u8(confirm.update_type);
            encoder.write_var_str(&confirm.ip);
            if include_signatures {
                encoder.write_var_bytes(&confirm.sig);
                encoder.write_var_bytes(&confirm.benchmark_sig);
            }
        }
    }

    Ok(())
}

fn decode_fluxnode_v5(
    decoder: &mut Decoder,
    include_signatures: bool,
) -> Result<FluxnodeTxV5, TransactionDecodeError> {
    let tx_type = decoder.read_u8()?;
    match tx_type {
        FLUXNODE_START_TX_TYPE => {
            let collateral = OutPoint::consensus_decode(decoder)?;
            let collateral_pubkey = decoder.read_var_bytes()?;
            let pubkey = decoder.read_var_bytes()?;
            let sig_time = decoder.read_u32_le()?;
            let sig = if include_signatures {
                decoder.read_var_bytes()?
            } else {
                Vec::new()
            };
            Ok(FluxnodeTxV5::Start(FluxnodeStartV5 {
                collateral,
                collateral_pubkey,
                pubkey,
                sig_time,
                sig,
            }))
        }
        FLUXNODE_CONFIRM_TX_TYPE => {
            let collateral = OutPoint::consensus_decode(decoder)?;
            let sig_time = decoder.read_u32_le()?;
            let benchmark_tier = decoder.read_u8()?;
            let benchmark_sig_time = decoder.read_u32_le()?;
            let update_type = decoder.read_u8()?;
            let ip = decoder.read_var_str()?;
            let (sig, benchmark_sig) = if include_signatures {
                (decoder.read_var_bytes()?, decoder.read_var_bytes()?)
            } else {
                (Vec::new(), Vec::new())
            };
            Ok(FluxnodeTxV5::Confirm(FluxnodeConfirmTx {
                collateral,
                sig_time,
                benchmark_tier,
                benchmark_sig_time,
                update_type,
                ip,
                sig,
                benchmark_sig,
            }))
        }
        _ => Err(TransactionDecodeError::InvalidTransactionFormat(
            "unknown fluxnode v5 transaction type",
        )),
    }
}

fn decode_fluxnode_v6(
    decoder: &mut Decoder,
    include_signatures: bool,
) -> Result<FluxnodeTxV6, TransactionDecodeError> {
    let tx_type = decoder.read_u8()?;
    match tx_type {
        FLUXNODE_START_TX_TYPE => {
            let flux_tx_version = decoder.read_i32_le()?;
            let is_normal = is_flux_tx_normal_type(flux_tx_version, true)
                && (flux_tx_version & FLUXNODE_TX_TYPE_P2SH_BIT) == 0;
            let is_p2sh = is_flux_tx_p2sh_type(flux_tx_version, true);

            let variant = if is_normal {
                let collateral = OutPoint::consensus_decode(decoder)?;
                let collateral_pubkey = decoder.read_var_bytes()?;
                let pubkey = decoder.read_var_bytes()?;
                let sig_time = decoder.read_u32_le()?;
                let sig = if include_signatures {
                    decoder.read_var_bytes()?
                } else {
                    Vec::new()
                };
                FluxnodeStartVariantV6::Normal {
                    collateral,
                    collateral_pubkey,
                    pubkey,
                    sig_time,
                    sig,
                }
            } else if is_p2sh {
                let collateral = OutPoint::consensus_decode(decoder)?;
                let pubkey = decoder.read_var_bytes()?;
                let redeem_script = decoder.read_var_bytes()?;
                let sig_time = decoder.read_u32_le()?;
                let sig = if include_signatures {
                    decoder.read_var_bytes()?
                } else {
                    Vec::new()
                };
                FluxnodeStartVariantV6::P2sh {
                    collateral,
                    pubkey,
                    redeem_script,
                    sig_time,
                    sig,
                }
            } else {
                return Err(TransactionDecodeError::InvalidTransactionFormat(
                    "fluxnode v6 start has invalid version bits",
                ));
            };

            let (using_delegates, delegates) = if has_flux_tx_delegates_feature(flux_tx_version) {
                let using_delegates = decoder.read_bool()?;
                let delegates = if using_delegates {
                    Some(FluxnodeDelegates::consensus_decode(decoder)?)
                } else {
                    None
                };
                (using_delegates, delegates)
            } else {
                (false, None)
            };

            Ok(FluxnodeTxV6::Start(FluxnodeStartV6 {
                flux_tx_version,
                variant,
                using_delegates,
                delegates,
            }))
        }
        FLUXNODE_CONFIRM_TX_TYPE => {
            let collateral = OutPoint::consensus_decode(decoder)?;
            let sig_time = decoder.read_u32_le()?;
            let benchmark_tier = decoder.read_u8()?;
            let benchmark_sig_time = decoder.read_u32_le()?;
            let update_type = decoder.read_u8()?;
            let ip = decoder.read_var_str()?;
            let (sig, benchmark_sig) = if include_signatures {
                (decoder.read_var_bytes()?, decoder.read_var_bytes()?)
            } else {
                (Vec::new(), Vec::new())
            };
            Ok(FluxnodeTxV6::Confirm(FluxnodeConfirmTx {
                collateral,
                sig_time,
                benchmark_tier,
                benchmark_sig_time,
                update_type,
                ip,
                sig,
                benchmark_sig,
            }))
        }
        _ => Err(TransactionDecodeError::InvalidTransactionFormat(
            "unknown fluxnode v6 transaction type",
        )),
    }
}

#[derive(Debug)]
pub enum TransactionDecodeError {
    Decode(DecodeError),
    InvalidTransactionFormat(&'static str),
}

impl From<DecodeError> for TransactionDecodeError {
    fn from(error: DecodeError) -> Self {
        TransactionDecodeError::Decode(error)
    }
}

impl std::fmt::Display for TransactionDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransactionDecodeError::Decode(error) => write!(f, "{error}"),
            TransactionDecodeError::InvalidTransactionFormat(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for TransactionDecodeError {}

#[derive(Debug)]
pub enum TransactionEncodeError {
    InvalidTransactionFormat(&'static str),
}

impl std::fmt::Display for TransactionEncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransactionEncodeError::InvalidTransactionFormat(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for TransactionEncodeError {}
