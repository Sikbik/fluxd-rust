//! Transaction outpoint type.

use fluxd_consensus::Hash256;

use crate::encoding::{Decodable, DecodeError, Decoder, Encodable, Encoder};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutPoint {
    pub hash: Hash256,
    pub index: u32,
}

impl OutPoint {
    pub fn null() -> Self {
        Self {
            hash: [0u8; 32],
            index: u32::MAX,
        }
    }
}

impl Encodable for OutPoint {
    fn consensus_encode(&self, encoder: &mut Encoder) {
        encoder.write_hash_le(&self.hash);
        encoder.write_u32_le(self.index);
    }
}

impl Decodable for OutPoint {
    fn consensus_decode(decoder: &mut Decoder) -> Result<Self, DecodeError> {
        let hash = decoder.read_hash_le()?;
        let index = decoder.read_u32_le()?;
        Ok(Self { hash, index })
    }
}
