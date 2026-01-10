use fluxd_consensus::Hash256;
use fluxd_primitives::encoding::{Decoder, Encoder};
use fluxd_storage::{Column, KeyValueStore, StoreError, WriteBatch};
use primitive_types::U256;
use std::sync::Arc;

const META_BEST_HEADER_KEY: &[u8] = b"best_header";
const META_BEST_BLOCK_KEY: &[u8] = b"best_block";

const STATUS_HAS_HEADER: u8 = 1 << 0;
const STATUS_HAS_BLOCK: u8 = 1 << 1;
const STATUS_FAILED_VALIDATION: u8 = 1 << 2;
const STATUS_FAILED_MASK: u8 = STATUS_FAILED_VALIDATION;

#[derive(Clone, Debug)]
pub struct HeaderEntry {
    pub prev_hash: Hash256,
    pub skip_hash: Hash256,
    pub height: i32,
    pub time: u32,
    pub bits: u32,
    pub chainwork: [u8; 32],
    pub status: u8,
}

impl HeaderEntry {
    pub fn has_block(&self) -> bool {
        (self.status & STATUS_HAS_BLOCK) != 0
    }

    pub fn has_header(&self) -> bool {
        (self.status & STATUS_HAS_HEADER) != 0
    }

    pub fn is_failed(&self) -> bool {
        (self.status & STATUS_FAILED_MASK) != 0
    }

    pub fn chainwork_value(&self) -> U256 {
        U256::from_big_endian(&self.chainwork)
    }
}

#[derive(Clone, Debug)]
pub struct ChainTip {
    pub hash: Hash256,
    pub height: i32,
    pub chainwork: [u8; 32],
}

pub struct ChainIndex<S> {
    store: Arc<S>,
}

impl<S: KeyValueStore> ChainIndex<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }

    pub fn get_header(&self, hash: &Hash256) -> Result<Option<HeaderEntry>, StoreError> {
        let bytes = match self.store.get(Column::HeaderIndex, hash)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        decode_header_entry(&bytes)
            .map(Some)
            .map_err(StoreError::Backend)
    }

    pub fn put_header(&self, batch: &mut WriteBatch, hash: &Hash256, entry: &HeaderEntry) {
        batch.put(Column::HeaderIndex, hash, encode_header_entry(entry));
    }

    pub fn set_best_header(&self, batch: &mut WriteBatch, hash: &Hash256) {
        batch.put(Column::Meta, META_BEST_HEADER_KEY, *hash);
    }

    pub fn set_best_block(&self, batch: &mut WriteBatch, hash: &Hash256) {
        batch.put(Column::Meta, META_BEST_BLOCK_KEY, *hash);
    }

    pub fn best_header(&self) -> Result<Option<ChainTip>, StoreError> {
        let hash = match self.store.get(Column::Meta, META_BEST_HEADER_KEY)? {
            Some(bytes) => decode_hash(&bytes).map_err(StoreError::Backend)?,
            None => return Ok(None),
        };
        let entry = match self.get_header(&hash)? {
            Some(entry) => entry,
            None => return Ok(None),
        };
        Ok(Some(ChainTip {
            hash,
            height: entry.height,
            chainwork: entry.chainwork,
        }))
    }

    pub fn best_block(&self) -> Result<Option<ChainTip>, StoreError> {
        let hash = match self.store.get(Column::Meta, META_BEST_BLOCK_KEY)? {
            Some(bytes) => decode_hash(&bytes).map_err(StoreError::Backend)?,
            None => return Ok(None),
        };
        let entry = match self.get_header(&hash)? {
            Some(entry) => entry,
            None => return Ok(None),
        };
        Ok(Some(ChainTip {
            hash,
            height: entry.height,
            chainwork: entry.chainwork,
        }))
    }

    pub fn height_hash(&self, height: i32) -> Result<Option<Hash256>, StoreError> {
        let key = height_key(height);
        let bytes = match self.store.get(Column::HeightIndex, &key)? {
            Some(bytes) => bytes,
            None => return Ok(None),
        };
        decode_hash(&bytes).map(Some).map_err(StoreError::Backend)
    }

    pub fn scan_headers(&self) -> Result<Vec<(Hash256, HeaderEntry)>, StoreError> {
        let entries = self.store.scan_prefix(Column::HeaderIndex, &[])?;
        let mut out = Vec::with_capacity(entries.len());
        for (key, value) in entries {
            let hash = decode_hash(&key).map_err(StoreError::Backend)?;
            let entry = decode_header_entry(&value).map_err(StoreError::Backend)?;
            out.push((hash, entry));
        }
        Ok(out)
    }

    pub fn set_height_hash(&self, batch: &mut WriteBatch, height: i32, hash: &Hash256) {
        let key = height_key(height);
        batch.put(Column::HeightIndex, key, *hash);
    }

    pub fn clear_height_hash(&self, batch: &mut WriteBatch, height: i32) {
        let key = height_key(height);
        batch.delete(Column::HeightIndex, key);
    }
}

pub fn height_key(height: i32) -> [u8; 4] {
    height.to_le_bytes()
}

fn encode_header_entry(entry: &HeaderEntry) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.write_hash_le(&entry.prev_hash);
    encoder.write_i32_le(entry.height);
    encoder.write_u32_le(entry.time);
    encoder.write_u32_le(entry.bits);
    encoder.write_bytes(&entry.chainwork);
    encoder.write_u8(entry.status);
    encoder.write_hash_le(&entry.skip_hash);
    encoder.into_inner()
}

pub(crate) fn decode_header_entry(bytes: &[u8]) -> Result<HeaderEntry, String> {
    let mut decoder = Decoder::new(bytes);
    let prev_hash = decoder.read_hash_le().map_err(|err| err.to_string())?;
    let height = decoder.read_i32_le().map_err(|err| err.to_string())?;
    let time = decoder.read_u32_le().map_err(|err| err.to_string())?;
    let bits = decoder.read_u32_le().map_err(|err| err.to_string())?;
    let chainwork = decoder.read_fixed::<32>().map_err(|err| err.to_string())?;
    let status = decoder.read_u8().map_err(|err| err.to_string())?;
    let skip_hash = if decoder.is_empty() {
        [0u8; 32]
    } else {
        let hash = decoder.read_hash_le().map_err(|err| err.to_string())?;
        if !decoder.is_empty() {
            return Err("trailing bytes in header entry".to_string());
        }
        hash
    };
    Ok(HeaderEntry {
        prev_hash,
        skip_hash,
        height,
        time,
        bits,
        chainwork,
        status,
    })
}

fn decode_hash(bytes: &[u8]) -> Result<Hash256, String> {
    if bytes.len() != 32 {
        return Err("invalid hash length".to_string());
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(bytes);
    Ok(hash)
}

pub fn status_with_header(status: u8) -> u8 {
    status | STATUS_HAS_HEADER
}

pub fn status_with_block(status: u8) -> u8 {
    status | STATUS_HAS_BLOCK
}

pub fn status_without_block(status: u8) -> u8 {
    status & !STATUS_HAS_BLOCK
}

pub fn status_with_failed(status: u8) -> u8 {
    status | STATUS_FAILED_VALIDATION
}

pub fn has_header(status: u8) -> bool {
    (status & STATUS_HAS_HEADER) != 0
}

pub fn has_block(status: u8) -> bool {
    (status & STATUS_HAS_BLOCK) != 0
}

pub fn is_failed(status: u8) -> bool {
    (status & STATUS_FAILED_MASK) != 0
}
