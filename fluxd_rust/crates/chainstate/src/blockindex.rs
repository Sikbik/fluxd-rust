//! Block index metadata stored in the database.

use crate::flatfiles::FileLocation;

const BLOCK_INDEX_ENTRY_LEN_V1: usize = 16;
const BLOCK_INDEX_ENTRY_LEN_V2: usize = 40;

pub const STATUS_HAVE_DATA: u32 = 1 << 0;
pub const STATUS_HAVE_UNDO: u32 = 1 << 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockIndexEntry {
    pub block: FileLocation,
    pub undo: Option<FileLocation>,
    pub tx_count: u32,
    pub status: u32,
}

impl BlockIndexEntry {
    pub fn encode(&self) -> [u8; BLOCK_INDEX_ENTRY_LEN_V2] {
        let mut out = [0u8; BLOCK_INDEX_ENTRY_LEN_V2];
        out[0..16].copy_from_slice(&self.block.encode());
        if let Some(location) = self.undo {
            out[16..32].copy_from_slice(&location.encode());
        }
        out[32..36].copy_from_slice(&self.tx_count.to_le_bytes());
        out[36..40].copy_from_slice(&self.status.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() == BLOCK_INDEX_ENTRY_LEN_V1 {
            let block = FileLocation::decode(bytes)?;
            return Some(Self {
                block,
                undo: None,
                tx_count: 0,
                status: STATUS_HAVE_DATA,
            });
        }
        if bytes.len() != BLOCK_INDEX_ENTRY_LEN_V2 {
            return None;
        }
        let block = FileLocation::decode(&bytes[0..16])?;
        let undo = FileLocation::decode(&bytes[16..32]).filter(|location| location.len != 0);
        let tx_count = u32::from_le_bytes(bytes[32..36].try_into().ok()?);
        let status = u32::from_le_bytes(bytes[36..40].try_into().ok()?);
        Some(Self {
            block,
            undo,
            tx_count,
            status,
        })
    }
}
