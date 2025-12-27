//! UTXO set logic backed by the storage trait.

use fluxd_primitives::encoding::{DecodeError, Decoder, Encoder};
use fluxd_primitives::outpoint::OutPoint;
use fluxd_storage::{Column, KeyValueStore, StoreError, WriteBatch};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UtxoEntry {
    pub value: i64,
    pub script_pubkey: Vec<u8>,
    pub height: u32,
    pub is_coinbase: bool,
}

impl UtxoEntry {
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.write_i64_le(self.value);
        encoder.write_var_bytes(&self.script_pubkey);
        encoder.write_u32_le(self.height);
        encoder.write_u8(if self.is_coinbase { 1 } else { 0 });
        encoder.into_inner()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut decoder = Decoder::new(bytes);
        let value = decoder.read_i64_le()?;
        let script_pubkey = decoder.read_var_bytes()?;
        let height = decoder.read_u32_le()?;
        let is_coinbase = decoder.read_u8()? != 0;
        if !decoder.is_empty() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(Self {
            value,
            script_pubkey,
            height,
            is_coinbase,
        })
    }
}

pub fn outpoint_key(outpoint: &OutPoint) -> Vec<u8> {
    let mut key = Vec::with_capacity(36);
    key.extend_from_slice(&outpoint.hash);
    key.extend_from_slice(&outpoint.index.to_le_bytes());
    key
}

pub struct UtxoSet<S> {
    store: S,
}

impl<S> UtxoSet<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
}

impl<S: KeyValueStore> UtxoSet<S> {
    pub fn get(&self, outpoint: &OutPoint) -> Result<Option<UtxoEntry>, StoreError> {
        let key = outpoint_key(outpoint);
        match self.store.get(Column::Utxo, &key)? {
            Some(bytes) => Ok(Some(
                UtxoEntry::decode(&bytes).map_err(|err| StoreError::Backend(err.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    pub fn put(&self, batch: &mut WriteBatch, outpoint: &OutPoint, entry: &UtxoEntry) {
        batch.put(Column::Utxo, outpoint_key(outpoint), entry.encode());
    }

    pub fn delete(&self, batch: &mut WriteBatch, outpoint: &OutPoint) {
        batch.delete(Column::Utxo, outpoint_key(outpoint));
    }
}
