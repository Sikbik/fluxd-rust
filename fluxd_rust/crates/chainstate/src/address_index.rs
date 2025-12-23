//! Address (script) to outpoint index backed by the storage trait.

use fluxd_consensus::Hash256;
use fluxd_primitives::hash::sha256;
use fluxd_primitives::outpoint::OutPoint;
use fluxd_storage::{Column, KeyValueStore, StoreError, WriteBatch};

use crate::utxo::outpoint_key;

const SCRIPT_HASH_LEN: usize = 32;
const OUTPOINT_KEY_LEN: usize = 36;

pub fn script_hash(script_pubkey: &[u8]) -> Hash256 {
    sha256(script_pubkey)
}

pub fn address_outpoint_key(script_pubkey: &[u8], outpoint: &OutPoint) -> Vec<u8> {
    let mut key = Vec::with_capacity(SCRIPT_HASH_LEN + OUTPOINT_KEY_LEN);
    key.extend_from_slice(&script_hash(script_pubkey));
    key.extend_from_slice(&outpoint_key(outpoint));
    key
}

pub fn address_prefix(script_pubkey: &[u8]) -> [u8; SCRIPT_HASH_LEN] {
    script_hash(script_pubkey)
}

pub struct AddressIndex<S> {
    store: S,
}

impl<S> AddressIndex<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S: KeyValueStore> AddressIndex<S> {
    pub fn insert(&self, batch: &mut WriteBatch, script_pubkey: &[u8], outpoint: &OutPoint) {
        batch.put(
            Column::AddressOutpoint,
            address_outpoint_key(script_pubkey, outpoint),
            [],
        );
    }

    pub fn delete(&self, batch: &mut WriteBatch, script_pubkey: &[u8], outpoint: &OutPoint) {
        batch.delete(
            Column::AddressOutpoint,
            address_outpoint_key(script_pubkey, outpoint),
        );
    }

    pub fn scan(&self, script_pubkey: &[u8]) -> Result<Vec<OutPoint>, StoreError> {
        let prefix = address_prefix(script_pubkey);
        let entries = self.store.scan_prefix(Column::AddressOutpoint, &prefix)?;
        let mut outpoints = Vec::with_capacity(entries.len());
        for (key, _) in entries {
            if let Some(outpoint) = outpoint_from_key(&key) {
                outpoints.push(outpoint);
            }
        }
        Ok(outpoints)
    }
}

fn outpoint_from_key(key: &[u8]) -> Option<OutPoint> {
    if key.len() != SCRIPT_HASH_LEN + OUTPOINT_KEY_LEN {
        return None;
    }
    let hash_start = SCRIPT_HASH_LEN;
    let hash_end = hash_start + 32;
    let index_end = hash_end + 4;
    let hash: Hash256 = key[hash_start..hash_end].try_into().ok()?;
    let index = u32::from_le_bytes(key[hash_end..index_end].try_into().ok()?);
    Some(OutPoint { hash, index })
}
