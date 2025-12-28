//! UTXO set logic backed by the storage trait.

use fluxd_primitives::encoding::{DecodeError, Decoder, Encoder};
use fluxd_primitives::outpoint::OutPoint;
use fluxd_storage::{Column, KeyValueStore, StoreError, WriteBatch};

pub const OUTPOINT_KEY_LEN: usize = 36;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct OutPointKey([u8; OUTPOINT_KEY_LEN]);

impl OutPointKey {
    pub fn new(outpoint: &OutPoint) -> Self {
        let mut bytes = [0u8; OUTPOINT_KEY_LEN];
        bytes[..32].copy_from_slice(&outpoint.hash);
        bytes[32..].copy_from_slice(&outpoint.index.to_le_bytes());
        Self(bytes)
    }

    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != OUTPOINT_KEY_LEN {
            return None;
        }
        let mut out = [0u8; OUTPOINT_KEY_LEN];
        out.copy_from_slice(bytes);
        Some(Self(out))
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }
}

pub fn outpoint_key(outpoint: &OutPoint) -> Vec<u8> {
    OutPointKey::new(outpoint).0.to_vec()
}

pub fn outpoint_key_bytes(outpoint: &OutPoint) -> OutPointKey {
    OutPointKey::new(outpoint)
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
        let key = outpoint_key_bytes(outpoint);
        match self.store.get(Column::Utxo, key.as_bytes())? {
            Some(bytes) => Ok(Some(
                UtxoEntry::decode(&bytes).map_err(|err| StoreError::Backend(err.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    pub fn put(&self, batch: &mut WriteBatch, outpoint: &OutPoint, entry: &UtxoEntry) {
        let key = outpoint_key_bytes(outpoint);
        batch.put(Column::Utxo, key.as_bytes(), entry.encode());
    }

    pub fn delete(&self, batch: &mut WriteBatch, outpoint: &OutPoint) {
        let key = outpoint_key_bytes(outpoint);
        batch.delete(Column::Utxo, key.as_bytes());
    }
}
