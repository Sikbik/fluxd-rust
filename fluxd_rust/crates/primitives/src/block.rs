//! Block header and block types.

use fluxd_consensus::Hash256;

use crate::encoding::{Decodable, DecodeError, Decoder, Encodable, Encoder};
use crate::hash::sha256d;
use crate::outpoint::OutPoint;
use crate::transaction::{Transaction, TransactionDecodeError, TransactionEncodeError};

pub const CURRENT_VERSION: i32 = 4;
pub const PON_VERSION: i32 = 100;

#[derive(Clone, Debug, PartialEq)]
pub struct BlockHeader {
    pub version: i32,
    pub prev_block: Hash256,
    pub merkle_root: Hash256,
    pub final_sapling_root: Hash256,
    pub time: u32,
    pub bits: u32,
    pub nonce: Hash256,
    pub solution: Vec<u8>,
    pub nodes_collateral: OutPoint,
    pub block_sig: Vec<u8>,
}

impl BlockHeader {
    pub fn is_pon(&self) -> bool {
        self.version >= PON_VERSION
    }

    pub fn consensus_encode(&self) -> Vec<u8> {
        self.encode_with_mode(true)
    }

    pub fn consensus_encode_for_hash(&self) -> Vec<u8> {
        self.encode_with_mode(false)
    }

    pub fn hash(&self) -> Hash256 {
        sha256d(&self.consensus_encode_for_hash())
    }

    fn encode_with_mode(&self, include_signature: bool) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.write_i32_le(self.version);
        encoder.write_hash_le(&self.prev_block);
        encoder.write_hash_le(&self.merkle_root);
        encoder.write_hash_le(&self.final_sapling_root);
        encoder.write_u32_le(self.time);
        encoder.write_u32_le(self.bits);

        if self.is_pon() {
            self.nodes_collateral.consensus_encode(&mut encoder);
            if include_signature {
                encoder.write_var_bytes(&self.block_sig);
            }
        } else {
            encoder.write_hash_le(&self.nonce);
            encoder.write_var_bytes(&self.solution);
        }

        encoder.into_inner()
    }

    pub fn consensus_decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut decoder = Decoder::new(bytes);
        let header = Self::consensus_decode_from(&mut decoder, true)?;
        if !decoder.is_empty() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(header)
    }

    pub fn consensus_decode_for_hash(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut decoder = Decoder::new(bytes);
        let header = Self::consensus_decode_from(&mut decoder, false)?;
        if !decoder.is_empty() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(header)
    }

    pub fn consensus_decode_from(
        decoder: &mut Decoder,
        include_signature: bool,
    ) -> Result<Self, DecodeError> {
        let version = decoder.read_i32_le()?;
        let prev_block = decoder.read_hash_le()?;
        let merkle_root = decoder.read_hash_le()?;
        let final_sapling_root = decoder.read_hash_le()?;
        let time = decoder.read_u32_le()?;
        let bits = decoder.read_u32_le()?;

        if version >= PON_VERSION {
            let nodes_collateral = OutPoint::consensus_decode(decoder)?;
            let block_sig = if include_signature {
                decoder.read_var_bytes()?
            } else {
                Vec::new()
            };
            Ok(Self {
                version,
                prev_block,
                merkle_root,
                final_sapling_root,
                time,
                bits,
                nonce: [0u8; 32],
                solution: Vec::new(),
                nodes_collateral,
                block_sig,
            })
        } else {
            let nonce = decoder.read_hash_le()?;
            let solution = decoder.read_var_bytes()?;
            Ok(Self {
                version,
                prev_block,
                merkle_root,
                final_sapling_root,
                time,
                bits,
                nonce,
                solution,
                nodes_collateral: OutPoint::null(),
                block_sig: Vec::new(),
            })
        }
    }
}

#[derive(Clone, Debug)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
}

impl Block {
    pub fn consensus_encode(&self) -> Result<Vec<u8>, TransactionEncodeError> {
        let mut encoder = Encoder::new();
        encoder.write_bytes(&self.header.consensus_encode());
        encoder.write_varint(self.transactions.len() as u64);
        for tx in &self.transactions {
            encoder.write_bytes(&tx.consensus_encode()?);
        }
        Ok(encoder.into_inner())
    }

    pub fn consensus_decode(bytes: &[u8]) -> Result<Self, BlockDecodeError> {
        let mut decoder = Decoder::new(bytes);
        let header = BlockHeader::consensus_decode_from(&mut decoder, true)?;
        let count = decoder.read_varint()?;
        let count = usize::try_from(count).map_err(|_| DecodeError::SizeTooLarge)?;
        let mut transactions = Vec::with_capacity(count);
        for _ in 0..count {
            transactions.push(Transaction::decode_from(&mut decoder, true)?);
        }
        if !decoder.is_empty() {
            return Err(BlockDecodeError::Decode(DecodeError::TrailingBytes));
        }
        Ok(Self {
            header,
            transactions,
        })
    }
}

#[derive(Debug)]
pub enum BlockDecodeError {
    Decode(DecodeError),
    Transaction(TransactionDecodeError),
}

impl From<DecodeError> for BlockDecodeError {
    fn from(error: DecodeError) -> Self {
        BlockDecodeError::Decode(error)
    }
}

impl From<TransactionDecodeError> for BlockDecodeError {
    fn from(error: TransactionDecodeError) -> Self {
        BlockDecodeError::Transaction(error)
    }
}

impl std::fmt::Display for BlockDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockDecodeError::Decode(error) => write!(f, "{error}"),
            BlockDecodeError::Transaction(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for BlockDecodeError {}
