use std::fmt;
use std::sync::Arc;

pub mod memory;

#[cfg(feature = "fjall")]
pub mod fjall;

#[derive(Debug)]
pub enum StoreError {
    Backend(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::Backend(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for StoreError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Column {
    BlockIndex,
    HeaderIndex,
    HeightIndex,
    BlockHeader,
    TxIndex,
    Utxo,
    AnchorSprout,
    AnchorSapling,
    NullifierSprout,
    NullifierSapling,
    Fluxnode,
    FluxnodeKey,
    AddressOutpoint,
    TimestampIndex,
    BlockTimestamp,
    Meta,
}

impl Column {
    pub fn as_str(self) -> &'static str {
        match self {
            Column::BlockIndex => "block_index",
            Column::HeaderIndex => "header_index",
            Column::HeightIndex => "height_index",
            Column::BlockHeader => "block_header",
            Column::TxIndex => "tx_index",
            Column::Utxo => "utxo",
            Column::AnchorSprout => "anchor_sprout",
            Column::AnchorSapling => "anchor_sapling",
            Column::NullifierSprout => "nullifier_sprout",
            Column::NullifierSapling => "nullifier_sapling",
            Column::Fluxnode => "fluxnode",
            Column::FluxnodeKey => "fluxnode_key",
            Column::AddressOutpoint => "address_outpoint",
            Column::TimestampIndex => "timestamp_index",
            Column::BlockTimestamp => "block_timestamp",
            Column::Meta => "meta",
        }
    }
}

#[derive(Clone, Debug)]
pub enum WriteOp {
    Put {
        column: Column,
        key: Vec<u8>,
        value: Vec<u8>,
    },
    Delete {
        column: Column,
        key: Vec<u8>,
    },
}

#[derive(Clone, Debug, Default)]
pub struct WriteBatch {
    ops: Vec<WriteOp>,
}

impl WriteBatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&mut self, column: Column, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) {
        self.ops.push(WriteOp::Put {
            column,
            key: key.into(),
            value: value.into(),
        });
    }

    pub fn delete(&mut self, column: Column, key: impl Into<Vec<u8>>) {
        self.ops.push(WriteOp::Delete {
            column,
            key: key.into(),
        });
    }

    pub fn iter(&self) -> impl Iterator<Item = &WriteOp> {
        self.ops.iter()
    }
}

pub trait KeyValueStore: Send + Sync {
    fn get(&self, column: Column, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>;
    fn put(&self, column: Column, key: &[u8], value: &[u8]) -> Result<(), StoreError>;
    fn delete(&self, column: Column, key: &[u8]) -> Result<(), StoreError>;
    fn scan_prefix(&self, column: Column, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError>;
    fn write_batch(&self, batch: WriteBatch) -> Result<(), StoreError>;
}

impl<T: KeyValueStore + ?Sized> KeyValueStore for Arc<T> {
    fn get(&self, column: Column, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        self.as_ref().get(column, key)
    }

    fn put(&self, column: Column, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        self.as_ref().put(column, key, value)
    }

    fn delete(&self, column: Column, key: &[u8]) -> Result<(), StoreError> {
        self.as_ref().delete(column, key)
    }

    fn scan_prefix(
        &self,
        column: Column,
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError> {
        self.as_ref().scan_prefix(column, prefix)
    }

    fn write_batch(&self, batch: WriteBatch) -> Result<(), StoreError> {
        self.as_ref().write_batch(batch)
    }
}
