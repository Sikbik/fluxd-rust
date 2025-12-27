//! Transaction index helpers backed by the storage trait.

use fluxd_consensus::Hash256;
use fluxd_storage::{Column, KeyValueStore, StoreError, WriteBatch};

use crate::flatfiles::FileLocation;

const TX_LOCATION_LEN: usize = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxLocation {
    pub block: FileLocation,
    pub index: u32,
}

impl TxLocation {
    pub fn encode(&self) -> [u8; TX_LOCATION_LEN] {
        let mut out = [0u8; TX_LOCATION_LEN];
        out[0..16].copy_from_slice(&self.block.encode());
        out[16..20].copy_from_slice(&self.index.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != TX_LOCATION_LEN {
            return None;
        }
        let block = FileLocation::decode(&bytes[0..16])?;
        let index = u32::from_le_bytes(bytes[16..20].try_into().ok()?);
        Some(Self { block, index })
    }
}

pub struct TxIndex<S> {
    store: S,
}

impl<S> TxIndex<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S: KeyValueStore> TxIndex<S> {
    pub fn insert(&self, batch: &mut WriteBatch, txid: &Hash256, location: TxLocation) {
        batch.put(Column::TxIndex, txid.to_vec(), location.encode());
    }

    pub fn delete(&self, batch: &mut WriteBatch, txid: &Hash256) {
        batch.delete(Column::TxIndex, txid.to_vec());
    }

    pub fn get(&self, txid: &Hash256) -> Result<Option<TxLocation>, StoreError> {
        let bytes = match self.store.get(Column::TxIndex, txid)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        TxLocation::decode(&bytes)
            .ok_or_else(|| StoreError::Backend("invalid tx index entry".to_string()))
            .map(Some)
    }
}
