use std::collections::HashMap;
use std::path::Path;

use fjall::{Config, Keyspace, PartitionCreateOptions, PartitionHandle};

use crate::{Column, KeyValueStore, PrefixVisitor, StoreError, WriteBatch, WriteOp};

pub struct FjallStore {
    keyspace: Keyspace,
    partitions: HashMap<Column, PartitionHandle>,
}

#[derive(Clone, Debug, Default)]
pub struct FjallTelemetrySnapshot {
    pub write_buffer_bytes: u64,
    pub journal_count: u64,
    pub flushes_completed: u64,
    pub active_compactions: u64,
    pub compactions_completed: u64,
    pub time_compacting_us: u64,
    pub utxo_segments: u64,
    pub utxo_flushes_completed: u64,
    pub tx_index_segments: u64,
    pub tx_index_flushes_completed: u64,
    pub spent_index_segments: u64,
    pub spent_index_flushes_completed: u64,
    pub address_outpoint_segments: u64,
    pub address_outpoint_flushes_completed: u64,
    pub address_delta_segments: u64,
    pub address_delta_flushes_completed: u64,
    pub header_index_segments: u64,
    pub header_index_flushes_completed: u64,
}

#[derive(Clone, Debug, Default)]
pub struct FjallOptions {
    pub cache_bytes: Option<u64>,
    pub write_buffer_bytes: Option<u64>,
    pub journal_bytes: Option<u64>,
    pub memtable_bytes: Option<u32>,
    pub flush_workers: Option<usize>,
    pub compaction_workers: Option<usize>,
    pub fsync_ms: Option<u16>,
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
        if let Some(workers) = self.flush_workers {
            config = config.flush_workers(workers);
        }
        if let Some(workers) = self.compaction_workers {
            config = config.compaction_workers(workers);
        }
        if let Some(ms) = self.fsync_ms {
            config = config.fsync_ms(Some(ms));
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
        for column in Column::ALL {
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

    pub fn telemetry_snapshot(&self) -> FjallTelemetrySnapshot {
        let (utxo_segments, utxo_flushes_completed) = self.partition_telemetry(Column::Utxo);
        let (tx_index_segments, tx_index_flushes_completed) =
            self.partition_telemetry(Column::TxIndex);
        let (spent_index_segments, spent_index_flushes_completed) =
            self.partition_telemetry(Column::SpentIndex);
        let (address_outpoint_segments, address_outpoint_flushes_completed) =
            self.partition_telemetry(Column::AddressOutpoint);
        let (address_delta_segments, address_delta_flushes_completed) =
            self.partition_telemetry(Column::AddressDelta);
        let (header_index_segments, header_index_flushes_completed) =
            self.partition_telemetry(Column::HeaderIndex);

        FjallTelemetrySnapshot {
            write_buffer_bytes: self.keyspace.write_buffer_size(),
            journal_count: self.keyspace.journal_count() as u64,
            flushes_completed: self.keyspace.flushes_completed() as u64,
            active_compactions: self.keyspace.active_compactions() as u64,
            compactions_completed: self.keyspace.compactions_completed() as u64,
            time_compacting_us: self.keyspace.time_compacting().as_micros() as u64,
            utxo_segments,
            utxo_flushes_completed,
            tx_index_segments,
            tx_index_flushes_completed,
            spent_index_segments,
            spent_index_flushes_completed,
            address_outpoint_segments,
            address_outpoint_flushes_completed,
            address_delta_segments,
            address_delta_flushes_completed,
            header_index_segments,
            header_index_flushes_completed,
        }
    }

    fn partition_telemetry(&self, column: Column) -> (u64, u64) {
        match self.partition(column) {
            Ok(partition) => (
                partition.segment_count() as u64,
                partition.flushes_completed() as u64,
            ),
            Err(_) => (0, 0),
        }
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
