use std::collections::HashMap;
use std::path::Path;

use fjall::{Config, Keyspace, PartitionCreateOptions, PartitionHandle};

use crate::{Column, KeyValueStore, PrefixVisitor, StoreError, WriteBatch, WriteOp};

const ALL_COLUMNS: [Column; 18] = [
    Column::BlockIndex,
    Column::HeaderIndex,
    Column::HeightIndex,
    Column::BlockHeader,
    Column::TxIndex,
    Column::SpentIndex,
    Column::Utxo,
    Column::AnchorSprout,
    Column::AnchorSapling,
    Column::NullifierSprout,
    Column::NullifierSapling,
    Column::Fluxnode,
    Column::FluxnodeKey,
    Column::AddressOutpoint,
    Column::TimestampIndex,
    Column::BlockTimestamp,
    Column::BlockUndo,
    Column::Meta,
];

pub struct FjallStore {
    keyspace: Keyspace,
    partitions: HashMap<Column, PartitionHandle>,
}

#[derive(Clone, Debug, Default)]
pub struct FjallOptions {
    pub cache_bytes: Option<u64>,
    pub write_buffer_bytes: Option<u64>,
    pub journal_bytes: Option<u64>,
    pub memtable_bytes: Option<u32>,
}

impl FjallOptions {
    fn apply_config(&self, mut config: Config) -> Config {
        if let Some(bytes) = self.cache_bytes {
            config = config.cache_size(bytes);
        }
        if let Some(bytes) = self.write_buffer_bytes {
            config = config.max_write_buffer_size(bytes);
        }
        if let Some(bytes) = self.journal_bytes {
            config = config.max_journaling_size(bytes);
        }
        config
    }

    fn partition_options(&self) -> PartitionCreateOptions {
        let mut options = PartitionCreateOptions::default();
        if let Some(bytes) = self.memtable_bytes {
            options = options.max_memtable_size(bytes);
        }
        options
    }
}

impl FjallStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with_config(Config::new(path))
    }

    pub fn open_with_config(config: Config) -> Result<Self, StoreError> {
        Self::open_with_config_and_options(config, PartitionCreateOptions::default())
    }

    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: FjallOptions,
    ) -> Result<Self, StoreError> {
        let config = options.apply_config(Config::new(path));
        let partition_options = options.partition_options();
        Self::open_with_config_and_options(config, partition_options)
    }

    pub fn open_with_config_and_options(
        config: Config,
        partition_options: PartitionCreateOptions,
    ) -> Result<Self, StoreError> {
        let keyspace = config.open().map_err(map_err)?;
        let mut partitions = HashMap::new();
        for column in ALL_COLUMNS {
            let handle = keyspace
                .open_partition(column.as_str(), partition_options.clone())
                .map_err(map_err)?;
            partitions.insert(column, handle);
        }
        Ok(Self {
            keyspace,
            partitions,
        })
    }

    fn partition(&self, column: Column) -> Result<&PartitionHandle, StoreError> {
        self.partitions
            .get(&column)
            .ok_or_else(|| StoreError::Backend(format!("missing partition {}", column.as_str())))
    }
}

impl KeyValueStore for FjallStore {
    fn get(&self, column: Column, key: &[u8]) -> Result<Option<Vec<u8>>, StoreError> {
        let partition = self.partition(column)?;
        let value = partition.get(key).map_err(map_err)?;
        Ok(value.map(|bytes| bytes.to_vec()))
    }

    fn put(&self, column: Column, key: &[u8], value: &[u8]) -> Result<(), StoreError> {
        let partition = self.partition(column)?;
        partition.insert(key, value).map_err(map_err)?;
        Ok(())
    }

    fn delete(&self, column: Column, key: &[u8]) -> Result<(), StoreError> {
        let partition = self.partition(column)?;
        partition.remove(key).map_err(map_err)?;
        Ok(())
    }

    fn scan_prefix(
        &self,
        column: Column,
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError> {
        let partition = self.partition(column)?;
        let mut results = Vec::new();
        for entry in partition.prefix(prefix) {
            let (key, value) = entry.map_err(map_err)?;
            results.push((key.to_vec(), value.to_vec()));
        }
        Ok(results)
    }

    fn for_each_prefix<'a>(
        &self,
        column: Column,
        prefix: &[u8],
        visitor: &mut PrefixVisitor<'a>,
    ) -> Result<(), StoreError> {
        let partition = self.partition(column)?;
        for entry in partition.prefix(prefix) {
            let (key, value) = entry.map_err(map_err)?;
            visitor(key.as_ref(), value.as_ref())?;
        }
        Ok(())
    }

    fn write_batch(&self, batch: &WriteBatch) -> Result<(), StoreError> {
        let mut fjall_batch = self.keyspace.batch();
        for op in batch.iter() {
            match op {
                WriteOp::Put { column, key, value } => {
                    let partition = self.partition(*column)?;
                    fjall_batch.insert(partition, key.as_slice(), value.as_slice());
                }
                WriteOp::Delete { column, key } => {
                    let partition = self.partition(*column)?;
                    fjall_batch.remove(partition, key.as_slice());
                }
            }
        }
        fjall_batch.commit().map_err(map_err)?;
        Ok(())
    }
}

fn map_err(err: fjall::Error) -> StoreError {
    StoreError::Backend(err.to_string())
}
