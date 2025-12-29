use fluxd_fluxnode::storage::FluxnodeRecord;
use fluxd_primitives::encoding::{Decodable, DecodeError, Decoder, Encodable, Encoder};
use fluxd_primitives::outpoint::OutPoint;

use crate::utxo::UtxoEntry;

const BLOCK_UNDO_VERSION: u8 = 2;

#[derive(Clone, Debug)]
pub struct SpentOutput {
    pub outpoint: OutPoint,
    pub entry: UtxoEntry,
}

#[derive(Clone, Debug)]
pub struct FluxnodeUndo {
    pub collateral: OutPoint,
    pub prev: Option<FluxnodeRecord>,
}

#[derive(Clone, Debug)]
pub struct BlockUndo {
    pub prev_sprout_tree: Vec<u8>,
    pub prev_sapling_tree: Vec<u8>,
    pub spent: Vec<SpentOutput>,
    pub fluxnode: Vec<FluxnodeUndo>,
    pub fluxnode_extra: Vec<FluxnodeUndo>,
}

impl BlockUndo {
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.write_u8(BLOCK_UNDO_VERSION);
        encoder.write_var_bytes(&self.prev_sprout_tree);
        encoder.write_var_bytes(&self.prev_sapling_tree);
        encoder.write_u32_le(self.spent.len() as u32);
        for spent in &self.spent {
            spent.outpoint.consensus_encode(&mut encoder);
            encoder.write_var_bytes(&spent.entry.encode());
        }
        encoder.write_u32_le(self.fluxnode.len() as u32);
        for entry in &self.fluxnode {
            entry.collateral.consensus_encode(&mut encoder);
            encoder.write_u8(if entry.prev.is_some() { 1 } else { 0 });
            if let Some(record) = &entry.prev {
                encoder.write_var_bytes(&record.encode());
            }
        }
        encoder.write_u32_le(self.fluxnode_extra.len() as u32);
        for entry in &self.fluxnode_extra {
            entry.collateral.consensus_encode(&mut encoder);
            encoder.write_u8(if entry.prev.is_some() { 1 } else { 0 });
            if let Some(record) = &entry.prev {
                encoder.write_var_bytes(&record.encode());
            }
        }
        encoder.into_inner()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut decoder = Decoder::new(bytes);
        let version = decoder.read_u8()?;
        if version != 1 && version != BLOCK_UNDO_VERSION {
            return Err(DecodeError::InvalidData("unsupported block undo version"));
        }
        let prev_sprout_tree = decoder.read_var_bytes()?;
        let prev_sapling_tree = decoder.read_var_bytes()?;
        let spent_len = decoder.read_u32_le()? as usize;
        let mut spent = Vec::with_capacity(spent_len);
        for _ in 0..spent_len {
            let outpoint = OutPoint::consensus_decode(&mut decoder)?;
            let entry_bytes = decoder.read_var_bytes()?;
            let entry = UtxoEntry::decode(&entry_bytes)
                .map_err(|_| DecodeError::InvalidData("invalid utxo entry in undo"))?;
            spent.push(SpentOutput { outpoint, entry });
        }
        let flux_len = decoder.read_u32_le()? as usize;
        let mut fluxnode = Vec::with_capacity(flux_len);
        for _ in 0..flux_len {
            let collateral = OutPoint::consensus_decode(&mut decoder)?;
            let has_prev = decoder.read_u8()? != 0;
            let prev = if has_prev {
                let record_bytes = decoder.read_var_bytes()?;
                Some(
                    FluxnodeRecord::decode(&record_bytes)
                        .map_err(|_| DecodeError::InvalidData("invalid fluxnode record in undo"))?,
                )
            } else {
                None
            };
            fluxnode.push(FluxnodeUndo { collateral, prev });
        }
        let fluxnode_extra = if version == 1 {
            Vec::new()
        } else {
            let extra_len = decoder.read_u32_le()? as usize;
            let mut extra = Vec::with_capacity(extra_len);
            for _ in 0..extra_len {
                let collateral = OutPoint::consensus_decode(&mut decoder)?;
                let has_prev = decoder.read_u8()? != 0;
                let prev =
                    if has_prev {
                        let record_bytes = decoder.read_var_bytes()?;
                        Some(FluxnodeRecord::decode(&record_bytes).map_err(|_| {
                            DecodeError::InvalidData("invalid fluxnode record in undo")
                        })?)
                    } else {
                        None
                    };
                extra.push(FluxnodeUndo { collateral, prev });
            }
            extra
        };
        if !decoder.is_empty() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(Self {
            prev_sprout_tree,
            prev_sapling_tree,
            spent,
            fluxnode,
            fluxnode_extra,
        })
    }
}
