//! Address (script) to outpoint index backed by the storage trait.

use fluxd_consensus::Hash256;
use fluxd_primitives::hash::{hash160, sha256};
use fluxd_primitives::outpoint::OutPoint;
use fluxd_script::standard::{classify_script_pubkey, ScriptType};
use fluxd_storage::{Column, KeyValueStore, StoreError, WriteBatch};

use crate::utxo::outpoint_key_bytes;

const SCRIPT_HASH_LEN: usize = 32;
const OUTPOINT_KEY_LEN: usize = 36;

pub fn script_hash(script_pubkey: &[u8]) -> Option<Hash256> {
    match classify_script_pubkey(script_pubkey) {
        ScriptType::P2Pkh | ScriptType::P2Sh => Some(sha256(script_pubkey)),
        ScriptType::P2Pk => normalized_p2pk_hash(script_pubkey),
        ScriptType::P2Wpkh | ScriptType::P2Wsh | ScriptType::Unknown => None,
    }
}

pub fn address_outpoint_key(
    script_pubkey: &[u8],
    outpoint: &OutPoint,
) -> Option<[u8; SCRIPT_HASH_LEN + OUTPOINT_KEY_LEN]> {
    let mut key = [0u8; SCRIPT_HASH_LEN + OUTPOINT_KEY_LEN];
    let script = script_hash(script_pubkey)?;
    key[..SCRIPT_HASH_LEN].copy_from_slice(&script);
    let outpoint_key = outpoint_key_bytes(outpoint);
    key[SCRIPT_HASH_LEN..].copy_from_slice(outpoint_key.as_bytes());
    Some(key)
}

pub fn address_outpoint_key_with_script_hash(
    script_hash: &Hash256,
    outpoint: &OutPoint,
) -> [u8; SCRIPT_HASH_LEN + OUTPOINT_KEY_LEN] {
    let mut key = [0u8; SCRIPT_HASH_LEN + OUTPOINT_KEY_LEN];
    key[..SCRIPT_HASH_LEN].copy_from_slice(script_hash);
    let outpoint_key = outpoint_key_bytes(outpoint);
    key[SCRIPT_HASH_LEN..].copy_from_slice(outpoint_key.as_bytes());
    key
}

pub fn address_prefix(script_pubkey: &[u8]) -> Option<[u8; SCRIPT_HASH_LEN]> {
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
        let Some(key) = address_outpoint_key(script_pubkey, outpoint) else {
            return;
        };
        batch.put(Column::AddressOutpoint, key, []);
    }

    pub fn insert_with_script_hash(
        &self,
        batch: &mut WriteBatch,
        script_hash: &Hash256,
        outpoint: &OutPoint,
    ) {
        let key = address_outpoint_key_with_script_hash(script_hash, outpoint);
        batch.put(Column::AddressOutpoint, key, []);
    }

    pub fn delete(&self, batch: &mut WriteBatch, script_pubkey: &[u8], outpoint: &OutPoint) {
        let Some(key) = address_outpoint_key(script_pubkey, outpoint) else {
            return;
        };
        batch.delete(Column::AddressOutpoint, key);
    }

    pub fn delete_with_script_hash(
        &self,
        batch: &mut WriteBatch,
        script_hash: &Hash256,
        outpoint: &OutPoint,
    ) {
        let key = address_outpoint_key_with_script_hash(script_hash, outpoint);
        batch.delete(Column::AddressOutpoint, key);
    }

    pub fn scan(&self, script_pubkey: &[u8]) -> Result<Vec<OutPoint>, StoreError> {
        let Some(prefix) = address_prefix(script_pubkey) else {
            return Ok(Vec::new());
        };
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

fn normalized_p2pk_hash(script_pubkey: &[u8]) -> Option<Hash256> {
    let key_len = match script_pubkey.first().copied() {
        Some(len @ 33) => len as usize,
        Some(len @ 65) => len as usize,
        _ => return None,
    };
    let pubkey_end = key_len.checked_add(1)?;
    let pubkey = script_pubkey.get(1..pubkey_end)?;
    let hash = hash160(pubkey);
    let mut normalized = [0u8; 25];
    normalized[0] = 0x76;
    normalized[1] = 0xa9;
    normalized[2] = 0x14;
    normalized[3..23].copy_from_slice(&hash);
    normalized[23] = 0x88;
    normalized[24] = 0xac;
    Some(sha256(&normalized))
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
