use std::fmt;
use std::sync::Arc;

use smallvec::SmallVec;

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
    SpentIndex,
    Utxo,
    AnchorSprout,
    AnchorSapling,
    NullifierSprout,
    NullifierSapling,
    Fluxnode,
    FluxnodeKey,
    AddressOutpoint,
    AddressDelta,
    TimestampIndex,
    BlockTimestamp,
    BlockUndo,
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
            Column::SpentIndex => "spent_index",
            Column::Utxo => "utxo",
            Column::AnchorSprout => "anchor_sprout",
            Column::AnchorSapling => "anchor_sapling",
            Column::NullifierSprout => "nullifier_sprout",
            Column::NullifierSapling => "nullifier_sapling",
            Column::Fluxnode => "fluxnode",
            Column::FluxnodeKey => "fluxnode_key",
            Column::AddressOutpoint => "address_outpoint",
            Column::AddressDelta => "address_delta",
            Column::TimestampIndex => "timestamp_index",
            Column::BlockTimestamp => "block_timestamp",
            Column::BlockUndo => "block_undo",
            Column::Meta => "meta",
        }
    }
}

#[derive(Clone, Debug)]
pub struct WriteKey(SmallVec<[u8; 72]>);

impl WriteKey {
    pub fn as_slice(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl AsRef<[u8]> for WriteKey {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl From<Vec<u8>> for WriteKey {
    fn from(value: Vec<u8>) -> Self {
        Self(SmallVec::from_vec(value))
    }
}

impl From<&[u8]> for WriteKey {
    fn from(value: &[u8]) -> Self {
        Self(SmallVec::from_slice(value))
    }
}

impl<const N: usize> From<[u8; N]> for WriteKey {
    fn from(value: [u8; N]) -> Self {
        Self(SmallVec::from_slice(&value))
    }
}

impl<const N: usize> From<&[u8; N]> for WriteKey {
    fn from(value: &[u8; N]) -> Self {
        Self(SmallVec::from_slice(value))
    }
}

#[derive(Clone, Debug)]
pub enum WriteOp {
    Put {
        column: Column,
        key: WriteKey,
        value: Vec<u8>,
    },
    Delete {
        column: Column,
        key: WriteKey,
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

    pub fn put(&mut self, column: Column, key: impl Into<WriteKey>, value: impl Into<Vec<u8>>) {
        self.ops.push(WriteOp::Put {
            column,
            key: key.into(),
            value: value.into(),
        });
    }

    pub fn delete(&mut self, column: Column, key: impl Into<WriteKey>) {
        self.ops.push(WriteOp::Delete {
            column,
            key: key.into(),
        });
    }

    pub fn iter(&self) -> impl Iterator<Item = &WriteOp> {
        self.ops.iter()
    }

    pub fn into_ops(self) -> Vec<WriteOp> {
        self.ops
    }
}

pub type ScanResult = Vec<(Vec<u8>, Vec<u8>)>;
pub type PrefixVisitor<'a> = dyn FnMut(&[u8], &[u8]) -> Result<(), StoreError> + 'a;

pub trait KeyValueStore: Send + Sync {
    fn get(&self, column: Column, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError>;
    fn put(&self, column: Column, key: &[u8], value: &[u8]) -> Result<(), StoreError>;
    fn delete(&self, column: Column, key: &[u8]) -> Result<(), StoreError>;
    fn scan_prefix(&self, column: Column, prefix: &[u8]) -> Result<ScanResult, StoreError>;
    fn for_each_prefix<'a>(
        &self,
        column: Column,
        prefix: &[u8],
        visitor: &mut PrefixVisitor<'a>,
    ) -> Result<(), StoreError>;
    fn write_batch(&self, batch: &WriteBatch) -> Result<(), StoreError>;
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

    fn scan_prefix(&self, column: Column, prefix: &[u8]) -> Result<ScanResult, StoreError> {
        self.as_ref().scan_prefix(column, prefix)
    }

    fn for_each_prefix<'a>(
        &self,
        column: Column,
        prefix: &[u8],
        visitor: &mut PrefixVisitor<'a>,
    ) -> Result<(), StoreError> {
        self.as_ref().for_each_prefix(column, prefix, visitor)
    }

    fn write_batch(&self, batch: &WriteBatch) -> Result<(), StoreError> {
        self.as_ref().write_batch(batch)
    }
}
