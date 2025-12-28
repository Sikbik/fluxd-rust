//! Sprout/Sapling anchors and nullifiers backed by the storage trait.

use fluxd_consensus::Hash256;
use fluxd_storage::{Column, KeyValueStore, StoreError, WriteBatch};

pub struct AnchorSet<S> {
    store: S,
    column: Column,
}

impl<S> AnchorSet<S> {
    pub fn new(store: S, column: Column) -> Self {
        Self { store, column }
    }
}

impl<S: KeyValueStore> AnchorSet<S> {
    pub fn get(&self, anchor: &Hash256) -> Result<Option<Vec<u8>>, StoreError> {
        self.store.get(self.column, anchor)
    }

    pub fn contains(&self, anchor: &Hash256) -> Result<bool, StoreError> {
        Ok(self.store.get(self.column, anchor)?.is_some())
    }

    pub fn insert(&self, batch: &mut WriteBatch, anchor: &Hash256, tree: Vec<u8>) {
        batch.put(self.column, anchor, tree);
    }

    pub fn remove(&self, batch: &mut WriteBatch, anchor: &Hash256) {
        batch.delete(self.column, anchor);
    }
}

pub struct NullifierSet<S> {
    store: S,
    column: Column,
}

impl<S> NullifierSet<S> {
    pub fn new(store: S, column: Column) -> Self {
        Self { store, column }
    }
}

impl<S: KeyValueStore> NullifierSet<S> {
    pub fn contains(&self, nullifier: &Hash256) -> Result<bool, StoreError> {
        Ok(self.store.get(self.column, nullifier)?.is_some())
    }

    pub fn insert(&self, batch: &mut WriteBatch, nullifier: &Hash256) {
        batch.put(self.column, nullifier, []);
    }

    pub fn remove(&self, batch: &mut WriteBatch, nullifier: &Hash256) {
        batch.delete(self.column, nullifier);
    }
}
