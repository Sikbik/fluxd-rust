//! Address delta index (Insight-style) backed by the storage trait.

use fluxd_consensus::Hash256;
use fluxd_storage::{Column, KeyValueStore, StoreError, WriteBatch};

use crate::address_index;

const SCRIPT_HASH_LEN: usize = 32;
const HEIGHT_LEN: usize = 4;
const TX_INDEX_LEN: usize = 4;
const TXID_LEN: usize = 32;
const INDEX_LEN: usize = 4;
const SPENDING_LEN: usize = 1;

const KEY_LEN: usize =
    SCRIPT_HASH_LEN + HEIGHT_LEN + TX_INDEX_LEN + TXID_LEN + INDEX_LEN + SPENDING_LEN;
const VALUE_LEN: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AddressDeltaEntry {
    pub height: u32,
    pub tx_index: u32,
    pub txid: Hash256,
    pub index: u32,
    pub spending: bool,
    pub satoshis: i64,
}

pub struct AddressDeltaIndex<S> {
    store: S,
}

impl<S> AddressDeltaIndex<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S: KeyValueStore> AddressDeltaIndex<S> {
    pub fn insert(
        &self,
        batch: &mut WriteBatch,
        script_pubkey: &[u8],
        height: u32,
        tx_index: u32,
        txid: &Hash256,
        index: u32,
        spending: bool,
        satoshis: i64,
    ) {
        let Some(prefix) = address_index::address_prefix(script_pubkey) else {
            return;
        };
        self.insert_with_prefix(
            batch, &prefix, height, tx_index, txid, index, spending, satoshis,
        );
    }

    pub fn insert_with_prefix(
        &self,
        batch: &mut WriteBatch,
        script_hash: &Hash256,
        height: u32,
        tx_index: u32,
        txid: &Hash256,
        index: u32,
        spending: bool,
        satoshis: i64,
    ) {
        let key = address_delta_key(script_hash, height, tx_index, txid, index, spending);
        batch.put(Column::AddressDelta, key, satoshis.to_le_bytes());
    }

    pub fn delete(
        &self,
        batch: &mut WriteBatch,
        script_pubkey: &[u8],
        height: u32,
        tx_index: u32,
        txid: &Hash256,
        index: u32,
        spending: bool,
    ) {
        let Some(prefix) = address_index::address_prefix(script_pubkey) else {
            return;
        };
        self.delete_with_prefix(batch, &prefix, height, tx_index, txid, index, spending);
    }

    pub fn delete_with_prefix(
        &self,
        batch: &mut WriteBatch,
        script_hash: &Hash256,
        height: u32,
        tx_index: u32,
        txid: &Hash256,
        index: u32,
        spending: bool,
    ) {
        let key = address_delta_key(script_hash, height, tx_index, txid, index, spending);
        batch.delete(Column::AddressDelta, key);
    }

    pub fn scan(&self, script_pubkey: &[u8]) -> Result<Vec<AddressDeltaEntry>, StoreError> {
        let Some(prefix) = address_index::address_prefix(script_pubkey) else {
            return Ok(Vec::new());
        };
        let entries = self.store.scan_prefix(Column::AddressDelta, &prefix)?;
        let mut out = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            let Some(entry) = decode_entry(&key, &value) else {
                continue;
            };
            out.push(entry);
        }
        Ok(out)
    }

    pub fn for_each<'a>(
        &self,
        script_pubkey: &[u8],
        visitor: &mut dyn FnMut(AddressDeltaEntry) -> Result<(), StoreError>,
    ) -> Result<(), StoreError> {
        let Some(prefix) = address_index::address_prefix(script_pubkey) else {
            return Ok(());
        };
        let mut adapter = |key: &[u8], value: &[u8]| {
            let Some(entry) = decode_entry(key, value) else {
                return Ok(());
            };
            visitor(entry)
        };
        self.store
            .for_each_prefix(Column::AddressDelta, &prefix, &mut adapter)
    }
}

fn address_delta_key(
    script_hash: &Hash256,
    height: u32,
    tx_index: u32,
    txid: &Hash256,
    index: u32,
    spending: bool,
) -> [u8; KEY_LEN] {
    let mut out = [0u8; KEY_LEN];
    let mut offset = 0;
    out[offset..offset + SCRIPT_HASH_LEN].copy_from_slice(script_hash);
    offset += SCRIPT_HASH_LEN;
    out[offset..offset + HEIGHT_LEN].copy_from_slice(&height.to_be_bytes());
    offset += HEIGHT_LEN;
    out[offset..offset + TX_INDEX_LEN].copy_from_slice(&tx_index.to_be_bytes());
    offset += TX_INDEX_LEN;
    out[offset..offset + TXID_LEN].copy_from_slice(txid);
    offset += TXID_LEN;
    out[offset..offset + INDEX_LEN].copy_from_slice(&index.to_le_bytes());
    offset += INDEX_LEN;
    out[offset] = if spending { 1 } else { 0 };
    out
}

fn decode_entry(key: &[u8], value: &[u8]) -> Option<AddressDeltaEntry> {
    if key.len() != KEY_LEN || value.len() != VALUE_LEN {
        return None;
    }
    let height = u32::from_be_bytes(key[32..36].try_into().ok()?);
    let tx_index = u32::from_be_bytes(key[36..40].try_into().ok()?);
    let txid: Hash256 = key[40..72].try_into().ok()?;
    let index = u32::from_le_bytes(key[72..76].try_into().ok()?);
    let spending = key[76] != 0;
    let satoshis = i64::from_le_bytes(value.try_into().ok()?);
    Some(AddressDeltaEntry {
        height,
        tx_index,
        txid,
        index,
        spending,
        satoshis,
    })
}
