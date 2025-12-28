use std::collections::BTreeMap;
use std::sync::RwLock;

use crate::{Column, KeyValueStore, PrefixVisitor, StoreError, WriteBatch, WriteOp};

type MemoryStoreMap = BTreeMap<(Column, Vec<u8>), Vec<u8>>;

#[derive(Default)]
pub struct MemoryStore {
    inner: RwLock<MemoryStoreMap>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyValueStore for MemoryStore {
    fn get(&self, column: Column, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let guard = self.inner.read().expect("memory store lock");
        Ok(guard.get(&(column, key.to_vec())).cloned())
    }

    fn put(&self, column: Column, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let mut guard = self.inner.write().expect("memory store lock");
        guard.insert((column, key.to_vec()), value.to_vec());
        Ok(())
    }

    fn delete(&self, column: Column, key: &[u8]) -> Result<(), StoreError> {
        let mut guard = self.inner.write().expect("memory store lock");
        guard.remove(&(column, key.to_vec()));
        Ok(())
    }

    fn scan_prefix(
        &self,
        column: Column,
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError> {
        let guard = self.inner.read().expect("memory store lock");
        let mut results = Vec::new();
        for ((entry_column, key), value) in guard.iter() {
            if *entry_column == column && key.starts_with(prefix) {
                results.push((key.clone(), value.clone()));
            }
        }
        Ok(results)
    }

    fn for_each_prefix<'a>(
        &self,
        column: Column,
        prefix: &[u8],
        visitor: &mut PrefixVisitor<'a>,
    ) -> Result<(), StoreError> {
        let guard = self.inner.read().expect("memory store lock");
        for ((entry_column, key), value) in guard.iter() {
            if *entry_column == column && key.starts_with(prefix) {
                visitor(key.as_slice(), value.as_slice())?;
            }
        }
        Ok(())
    }

    fn write_batch(&self, batch: WriteBatch) -> Result<(), StoreError> {
        let mut guard = self.inner.write().expect("memory store lock");
        for op in batch.iter() {
            match op {
                WriteOp::Put { column, key, value } => {
                    guard.insert((*column, key.clone()), value.clone());
                }
                WriteOp::Delete { column, key } => {
                    guard.remove(&(*column, key.clone()));
                }
            }
        }
        Ok(())
    }
}
