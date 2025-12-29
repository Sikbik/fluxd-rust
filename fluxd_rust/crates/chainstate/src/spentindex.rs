//! Spent output index backed by the storage trait.
//!
//! This maps an output outpoint (txid + vout) to the spending transaction.

use fluxd_consensus::Hash256;
use fluxd_primitives::outpoint::OutPoint;
use fluxd_storage::{Column, KeyValueStore, StoreError, WriteBatch};

use crate::utxo::outpoint_key_bytes;

const SPENT_INDEX_VALUE_LEN_V1: usize = 40;
const SPENT_INDEX_VALUE_LEN_V2: usize = 72;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpentIndexDetails {
    pub satoshis: i64,
    pub address_type: u32,
    pub address_hash: [u8; 20],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpentIndexValue {
    pub txid: Hash256,
    pub input_index: u32,
    pub block_height: u32,
    pub details: Option<SpentIndexDetails>,
}

impl SpentIndexValue {
    pub fn encode(&self) -> [u8; SPENT_INDEX_VALUE_LEN_V2] {
        let mut out = [0u8; SPENT_INDEX_VALUE_LEN_V2];
        out[0..32].copy_from_slice(&self.txid);
        out[32..36].copy_from_slice(&self.input_index.to_le_bytes());
        out[36..40].copy_from_slice(&self.block_height.to_le_bytes());

        if let Some(details) = self.details {
            out[40..48].copy_from_slice(&details.satoshis.to_le_bytes());
            out[48..52].copy_from_slice(&details.address_type.to_le_bytes());
            out[52..72].copy_from_slice(&details.address_hash);
        }

        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != SPENT_INDEX_VALUE_LEN_V1 && bytes.len() != SPENT_INDEX_VALUE_LEN_V2 {
            return None;
        }
        let mut txid = [0u8; 32];
        txid.copy_from_slice(&bytes[0..32]);
        let input_index = u32::from_le_bytes(bytes[32..36].try_into().ok()?);
        let block_height = u32::from_le_bytes(bytes[36..40].try_into().ok()?);
        let details = if bytes.len() == SPENT_INDEX_VALUE_LEN_V2 {
            let satoshis = i64::from_le_bytes(bytes[40..48].try_into().ok()?);
            let address_type = u32::from_le_bytes(bytes[48..52].try_into().ok()?);
            let mut address_hash = [0u8; 20];
            address_hash.copy_from_slice(&bytes[52..72]);
            Some(SpentIndexDetails {
                satoshis,
                address_type,
                address_hash,
            })
        } else {
            None
        };
        Some(Self {
            txid,
            input_index,
            block_height,
            details,
        })
    }
}

pub struct SpentIndex<S> {
    store: S,
}

impl<S> SpentIndex<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S: KeyValueStore> SpentIndex<S> {
    pub fn insert(&self, batch: &mut WriteBatch, outpoint: &OutPoint, value: SpentIndexValue) {
        let key = outpoint_key_bytes(outpoint);
        batch.put(Column::SpentIndex, key.as_bytes(), value.encode());
    }

    pub fn delete(&self, batch: &mut WriteBatch, outpoint: &OutPoint) {
        let key = outpoint_key_bytes(outpoint);
        batch.delete(Column::SpentIndex, key.as_bytes());
    }

    pub fn get(&self, outpoint: &OutPoint) -> Result<Option<SpentIndexValue>, StoreError> {
        let key = outpoint_key_bytes(outpoint);
        let bytes = match self.store.get(Column::SpentIndex, key.as_bytes())? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        SpentIndexValue::decode(&bytes)
            .ok_or_else(|| StoreError::Backend("invalid spent index entry".to_string()))
            .map(Some)
    }
}
