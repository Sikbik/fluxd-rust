use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fjall::{AbstractTree, Config, Keyspace, PartitionCreateOptions, PartitionHandle};

use crate::{Column, KeyValueStore, PrefixVisitor, StoreError, WriteBatch, WriteOp};

const SLOW_COMMIT_THRESHOLD: Duration = Duration::from_millis(500);
const SLOW_COMMIT_LOG_INTERVAL_SECS: u64 = 30;
const WRITE_BUFFER_RELIEF_LOG_INTERVAL_SECS: u64 = 30;
const WRITE_BUFFER_RELIEF_COOLDOWN_SECS: u64 = 1;

const WRITE_BUFFER_HIGH_WATERMARK_PCT: u64 = 90;
const JOURNAL_HIGH_WATERMARK_PCT: u64 = 80;
const JOURNAL_RELIEF_LOG_INTERVAL_SECS: u64 = 30;
const JOURNAL_RELIEF_COOLDOWN_SECS: u64 = 2;

static LAST_SLOW_COMMIT_LOG_SECS: AtomicU64 = AtomicU64::new(0);
static LAST_WRITE_BUFFER_RELIEF_LOG_SECS: AtomicU64 = AtomicU64::new(0);
static LAST_JOURNAL_RELIEF_LOG_SECS: AtomicU64 = AtomicU64::new(0);

pub struct FjallStore {
    keyspace: Keyspace,
    partitions: HashMap<Column, PartitionHandle>,
    max_write_buffer_bytes: Option<u64>,
    max_journal_bytes: Option<u64>,
    last_pressure_relief_secs: AtomicU64,
}

#[derive(Clone, Debug, Default)]
pub struct FjallTelemetrySnapshot {
    pub write_buffer_bytes: u64,
    pub max_write_buffer_bytes: Option<u64>,
    pub journal_count: u64,
    pub journal_disk_space_bytes: u64,
    pub max_journal_bytes: Option<u64>,
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
        Self::open_with_config_and_options(config, PartitionCreateOptions::default(), None, None)
    }

    pub fn open_with_options(
        path: impl AsRef<Path>,
        options: FjallOptions,
    ) -> Result<Self, StoreError> {
        let config = options.apply_config(Config::new(path));
        let partition_options = options.partition_options();
        Self::open_with_config_and_options(
            config,
            partition_options,
            options.write_buffer_bytes,
            options.journal_bytes,
        )
    }

    pub fn open_with_config_and_options(
        config: Config,
        partition_options: PartitionCreateOptions,
        max_write_buffer_bytes: Option<u64>,
        max_journal_bytes: Option<u64>,
    ) -> Result<Self, StoreError> {
        let keyspace = config.open().map_err(map_err)?;
        let mut partitions = HashMap::new();
        for column in Column::ALL {
            let handle = keyspace
                .open_partition(column.as_str(), partition_options.clone())
                .map_err(map_err)?;
            partitions.insert(column, handle);
        }
        let store = Self {
            keyspace,
            partitions,
            max_write_buffer_bytes,
            max_journal_bytes,
            last_pressure_relief_secs: AtomicU64::new(0),
        };
        store.spawn_journal_pressure_watchdog();
        Ok(store)
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
            max_write_buffer_bytes: self.max_write_buffer_bytes,
            journal_count: self.keyspace.journal_count() as u64,
            journal_disk_space_bytes: self.keyspace.journal_disk_space(),
            max_journal_bytes: self.max_journal_bytes,
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

    fn maybe_relieve_write_buffer_pressure(&self, touched: &HashSet<Column>) {
        let Some(limit) = self.max_write_buffer_bytes else {
            return;
        };
        if limit == 0 {
            return;
        }
        let current = self.keyspace.write_buffer_size();
        if current == 0 {
            return;
        }

        let watermark = limit.saturating_mul(WRITE_BUFFER_HIGH_WATERMARK_PCT) / 100;
        if current < watermark {
            return;
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last = self.last_pressure_relief_secs.load(Ordering::Relaxed);
        if now.saturating_sub(last) < WRITE_BUFFER_RELIEF_COOLDOWN_SECS {
            return;
        }
        let _ = self.last_pressure_relief_secs.compare_exchange(
            last,
            now,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );

        let mut did_rotate = false;

        for column in touched.iter().copied() {
            let Ok(partition) = self.partition(column) else {
                continue;
            };
            match partition.rotate_memtable() {
                Ok(true) => {
                    did_rotate = true;
                    break;
                }
                Ok(false) => {}
                Err(_) => {}
            }
        }

        if !did_rotate {
            let mut candidates: Vec<(u32, &PartitionHandle)> = self
                .partitions
                .values()
                .map(|partition| (partition.tree.active_memtable_size(), partition))
                .collect();
            candidates.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
            for (_, partition) in candidates.into_iter().take(3) {
                match partition.rotate_memtable() {
                    Ok(true) => {
                        did_rotate = true;
                        break;
                    }
                    Ok(false) => {}
                    Err(_) => {}
                }
            }
        }

        if did_rotate {
            let last = LAST_WRITE_BUFFER_RELIEF_LOG_SECS.load(Ordering::Relaxed);
            if now.saturating_sub(last) >= WRITE_BUFFER_RELIEF_LOG_INTERVAL_SECS
                && LAST_WRITE_BUFFER_RELIEF_LOG_SECS
                    .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                fluxd_log::log_warn!(
                    "Warning: Fjall write buffer pressure {pressure:.1}% ({current}B/{limit}B); rotating memtables to trigger flushes",
                    pressure = current as f64 / limit as f64 * 100.0,
                );
            }
        }
    }

    fn spawn_journal_pressure_watchdog(&self) {
        let Some(limit) = self.max_journal_bytes else {
            return;
        };
        if limit == 0 {
            return;
        }
        let keyspace = self.keyspace.clone();
        let partitions: Vec<PartitionHandle> = self.partitions.values().cloned().collect();
        let _ = thread::Builder::new()
            .name("fjall-journal-watchdog".to_string())
            .spawn(move || {
                let mut last_relief = Instant::now()
                    .checked_sub(Duration::from_secs(JOURNAL_RELIEF_COOLDOWN_SECS))
                    .unwrap_or_else(Instant::now);
                let cooldown = Duration::from_secs(JOURNAL_RELIEF_COOLDOWN_SECS);
                let watermark = limit.saturating_mul(JOURNAL_HIGH_WATERMARK_PCT) / 100;
                loop {
                    thread::sleep(Duration::from_millis(500));
                    let current = keyspace.journal_disk_space();
                    if current < watermark {
                        continue;
                    }
                    if last_relief.elapsed() < cooldown {
                        continue;
                    }
                    last_relief = Instant::now();

                    let mut candidates: Vec<(u32, &PartitionHandle)> = partitions
                        .iter()
                        .map(|partition| (partition.tree.active_memtable_size(), partition))
                        .collect();
                    candidates.sort_by_key(|(size, _)| std::cmp::Reverse(*size));
                    let mut rotated = 0usize;
                    for (size, partition) in candidates {
                        if rotated >= 3 {
                            break;
                        }
                        if size == 0 {
                            continue;
                        }
                        if matches!(partition.rotate_memtable(), Ok(true)) {
                            rotated = rotated.saturating_add(1);
                        }
                    }

                    if rotated == 0 {
                        continue;
                    }

                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let last = LAST_JOURNAL_RELIEF_LOG_SECS.load(Ordering::Relaxed);
                    if now.saturating_sub(last) >= JOURNAL_RELIEF_LOG_INTERVAL_SECS
                        && LAST_JOURNAL_RELIEF_LOG_SECS
                            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                            .is_ok()
                    {
                        fluxd_log::log_warn!(
                            "Warning: Fjall journal pressure {pressure:.1}% ({current}B/{limit}B); rotated {rotated} memtable(s) to trigger flush + journal GC",
                            pressure = current as f64 / limit as f64 * 100.0,
                        );
                    }
                }
            });
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
        let mut touched: HashSet<Column> = HashSet::new();
        let mut fjall_batch = self.keyspace.batch();
        for op in batch.iter() {
            match op {
                WriteOp::Put { column, key, value } => {
                    touched.insert(*column);
                    let partition = self.partition(*column)?;
                    fjall_batch.insert(partition, key.as_slice(), value.as_slice());
                }
                WriteOp::Delete { column, key } => {
                    touched.insert(*column);
                    let partition = self.partition(*column)?;
                    fjall_batch.remove(partition, key.as_slice());
                }
            }
        }
        if !touched.is_empty() {
            self.maybe_relieve_write_buffer_pressure(&touched);
        }
        let commit_start = Instant::now();
        fjall_batch.commit().map_err(map_err)?;
        let elapsed = commit_start.elapsed();
        if elapsed >= SLOW_COMMIT_THRESHOLD {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let last = LAST_SLOW_COMMIT_LOG_SECS.load(Ordering::Relaxed);
            if now.saturating_sub(last) >= SLOW_COMMIT_LOG_INTERVAL_SECS
                && LAST_SLOW_COMMIT_LOG_SECS
                    .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                fluxd_log::log_warn!(
                    "Warning: Fjall write_batch commit took {}ms (ops {}, write_buffer {}B, journals {}, active_compactions {})",
                    elapsed.as_millis(),
                    batch.len(),
                    self.keyspace.write_buffer_size(),
                    self.keyspace.journal_count(),
                    self.keyspace.active_compactions(),
                );
            }
        }
        Ok(())
    }
}

fn map_err(err: fjall::Error) -> StoreError {
    StoreError::Backend(err.to_string())
}
