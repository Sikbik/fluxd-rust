use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fluxd_chainstate::metrics::ConnectMetrics;
use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::ValidationMetrics;
use fluxd_consensus::params::Network;
use fluxd_consensus::Hash256;
use fluxd_storage::KeyValueStore;

use crate::mempool::Mempool;
use crate::Backend;
use crate::Store;

#[derive(Clone, Debug)]
pub struct StatsSnapshot {
    pub network: String,
    pub backend: String,
    pub best_header_height: i32,
    pub best_block_height: i32,
    pub best_header_hash: Option<String>,
    pub best_block_hash: Option<String>,
    pub header_count: u64,
    pub block_count: u64,
    pub header_gap: i64,
    pub uptime_secs: u64,
    pub unix_time_secs: u64,
    pub sync_state: String,
    pub mempool_size: u64,
    pub mempool_bytes: u64,
    pub mempool_max_bytes: u64,
    pub mempool_rpc_accept: u64,
    pub mempool_rpc_reject: u64,
    pub mempool_relay_accept: u64,
    pub mempool_relay_reject: u64,
    pub mempool_evicted: u64,
    pub mempool_evicted_bytes: u64,
    pub mempool_loaded: u64,
    pub mempool_load_reject: u64,
    pub mempool_persisted_writes: u64,
    pub mempool_persisted_bytes: u64,
    pub download_us: u64,
    pub download_blocks: u64,
    pub verify_us: u64,
    pub verify_blocks: u64,
    pub commit_us: u64,
    pub commit_blocks: u64,
    pub header_request_us: u64,
    pub header_request_batches: u64,
    pub header_validate_us: u64,
    pub header_validate_headers: u64,
    pub header_commit_us: u64,
    pub header_commit_headers: u64,
    pub header_pow_us: u64,
    pub header_pow_headers: u64,
    pub validate_us: u64,
    pub validate_blocks: u64,
    pub script_us: u64,
    pub script_blocks: u64,
    pub shielded_us: u64,
    pub shielded_txs: u64,
    pub utxo_us: u64,
    pub utxo_blocks: u64,
    pub index_us: u64,
    pub index_blocks: u64,
    pub anchor_us: u64,
    pub anchor_blocks: u64,
    pub flatfile_us: u64,
    pub flatfile_blocks: u64,
    pub utxo_get_us: u64,
    pub utxo_get_ops: u64,
    pub utxo_cache_hits: u64,
    pub utxo_cache_misses: u64,
    pub utxo_put_us: u64,
    pub utxo_put_ops: u64,
    pub utxo_delete_us: u64,
    pub utxo_delete_ops: u64,
    pub spent_index_ops: u64,
    pub address_index_inserts: u64,
    pub address_index_deletes: u64,
    pub address_delta_inserts: u64,
    pub tx_index_ops: u64,
    pub header_index_ops: u64,
    pub timestamp_index_ops: u64,
    pub undo_encode_us: u64,
    pub undo_bytes: u64,
    pub undo_append_us: u64,
    pub db_write_buffer_bytes: Option<u64>,
    pub db_journal_count: Option<u64>,
    pub db_flushes_completed: Option<u64>,
    pub db_active_compactions: Option<u64>,
    pub db_compactions_completed: Option<u64>,
    pub db_time_compacting_us: Option<u64>,
    pub db_utxo_segments: Option<u64>,
    pub db_utxo_flushes_completed: Option<u64>,
    pub db_tx_index_segments: Option<u64>,
    pub db_tx_index_flushes_completed: Option<u64>,
    pub db_spent_index_segments: Option<u64>,
    pub db_spent_index_flushes_completed: Option<u64>,
    pub db_address_outpoint_segments: Option<u64>,
    pub db_address_outpoint_flushes_completed: Option<u64>,
    pub db_address_delta_segments: Option<u64>,
    pub db_address_delta_flushes_completed: Option<u64>,
    pub db_header_index_segments: Option<u64>,
    pub db_header_index_flushes_completed: Option<u64>,
}

impl StatsSnapshot {
    pub fn to_json(&self) -> String {
        let best_header_hash = match &self.best_header_hash {
            Some(value) => json_string(value),
            None => "null".to_string(),
        };
        let best_block_hash = match &self.best_block_hash {
            Some(value) => json_string(value),
            None => "null".to_string(),
        };

        let mut json = String::with_capacity(1024);
        json.push('{');
        json.push_str("\"network\":");
        json.push_str(&json_string(&self.network));
        json.push_str(",\"backend\":");
        json.push_str(&json_string(&self.backend));
        json.push_str(",\"best_header_height\":");
        json.push_str(&self.best_header_height.to_string());
        json.push_str(",\"best_block_height\":");
        json.push_str(&self.best_block_height.to_string());
        json.push_str(",\"best_header_hash\":");
        json.push_str(&best_header_hash);
        json.push_str(",\"best_block_hash\":");
        json.push_str(&best_block_hash);
        json.push_str(",\"header_count\":");
        json.push_str(&self.header_count.to_string());
        json.push_str(",\"block_count\":");
        json.push_str(&self.block_count.to_string());
        json.push_str(",\"header_gap\":");
        json.push_str(&self.header_gap.to_string());
        json.push_str(",\"uptime_secs\":");
        json.push_str(&self.uptime_secs.to_string());
        json.push_str(",\"unix_time_secs\":");
        json.push_str(&self.unix_time_secs.to_string());
        json.push_str(",\"sync_state\":");
        json.push_str(&json_string(&self.sync_state));
        json.push_str(",\"mempool_size\":");
        json.push_str(&self.mempool_size.to_string());
        json.push_str(",\"mempool_bytes\":");
        json.push_str(&self.mempool_bytes.to_string());
        json.push_str(",\"mempool_max_bytes\":");
        json.push_str(&self.mempool_max_bytes.to_string());
        json.push_str(",\"mempool_rpc_accept\":");
        json.push_str(&self.mempool_rpc_accept.to_string());
        json.push_str(",\"mempool_rpc_reject\":");
        json.push_str(&self.mempool_rpc_reject.to_string());
        json.push_str(",\"mempool_relay_accept\":");
        json.push_str(&self.mempool_relay_accept.to_string());
        json.push_str(",\"mempool_relay_reject\":");
        json.push_str(&self.mempool_relay_reject.to_string());
        json.push_str(",\"mempool_evicted\":");
        json.push_str(&self.mempool_evicted.to_string());
        json.push_str(",\"mempool_evicted_bytes\":");
        json.push_str(&self.mempool_evicted_bytes.to_string());
        json.push_str(",\"mempool_loaded\":");
        json.push_str(&self.mempool_loaded.to_string());
        json.push_str(",\"mempool_load_reject\":");
        json.push_str(&self.mempool_load_reject.to_string());
        json.push_str(",\"mempool_persisted_writes\":");
        json.push_str(&self.mempool_persisted_writes.to_string());
        json.push_str(",\"mempool_persisted_bytes\":");
        json.push_str(&self.mempool_persisted_bytes.to_string());
        json.push_str(",\"download_us\":");
        json.push_str(&self.download_us.to_string());
        json.push_str(",\"download_blocks\":");
        json.push_str(&self.download_blocks.to_string());
        json.push_str(",\"verify_us\":");
        json.push_str(&self.verify_us.to_string());
        json.push_str(",\"verify_blocks\":");
        json.push_str(&self.verify_blocks.to_string());
        json.push_str(",\"commit_us\":");
        json.push_str(&self.commit_us.to_string());
        json.push_str(",\"commit_blocks\":");
        json.push_str(&self.commit_blocks.to_string());
        json.push_str(",\"header_request_us\":");
        json.push_str(&self.header_request_us.to_string());
        json.push_str(",\"header_request_batches\":");
        json.push_str(&self.header_request_batches.to_string());
        json.push_str(",\"header_validate_us\":");
        json.push_str(&self.header_validate_us.to_string());
        json.push_str(",\"header_validate_headers\":");
        json.push_str(&self.header_validate_headers.to_string());
        json.push_str(",\"header_commit_us\":");
        json.push_str(&self.header_commit_us.to_string());
        json.push_str(",\"header_commit_headers\":");
        json.push_str(&self.header_commit_headers.to_string());
        json.push_str(",\"header_pow_us\":");
        json.push_str(&self.header_pow_us.to_string());
        json.push_str(",\"header_pow_headers\":");
        json.push_str(&self.header_pow_headers.to_string());
        json.push_str(",\"validate_us\":");
        json.push_str(&self.validate_us.to_string());
        json.push_str(",\"validate_blocks\":");
        json.push_str(&self.validate_blocks.to_string());
        json.push_str(",\"script_us\":");
        json.push_str(&self.script_us.to_string());
        json.push_str(",\"script_blocks\":");
        json.push_str(&self.script_blocks.to_string());
        json.push_str(",\"shielded_us\":");
        json.push_str(&self.shielded_us.to_string());
        json.push_str(",\"shielded_txs\":");
        json.push_str(&self.shielded_txs.to_string());
        json.push_str(",\"utxo_us\":");
        json.push_str(&self.utxo_us.to_string());
        json.push_str(",\"utxo_blocks\":");
        json.push_str(&self.utxo_blocks.to_string());
        json.push_str(",\"index_us\":");
        json.push_str(&self.index_us.to_string());
        json.push_str(",\"index_blocks\":");
        json.push_str(&self.index_blocks.to_string());
        json.push_str(",\"anchor_us\":");
        json.push_str(&self.anchor_us.to_string());
        json.push_str(",\"anchor_blocks\":");
        json.push_str(&self.anchor_blocks.to_string());
        json.push_str(",\"flatfile_us\":");
        json.push_str(&self.flatfile_us.to_string());
        json.push_str(",\"flatfile_blocks\":");
        json.push_str(&self.flatfile_blocks.to_string());
        json.push_str(",\"utxo_get_us\":");
        json.push_str(&self.utxo_get_us.to_string());
        json.push_str(",\"utxo_get_ops\":");
        json.push_str(&self.utxo_get_ops.to_string());
        json.push_str(",\"utxo_cache_hits\":");
        json.push_str(&self.utxo_cache_hits.to_string());
        json.push_str(",\"utxo_cache_misses\":");
        json.push_str(&self.utxo_cache_misses.to_string());
        json.push_str(",\"utxo_put_us\":");
        json.push_str(&self.utxo_put_us.to_string());
        json.push_str(",\"utxo_put_ops\":");
        json.push_str(&self.utxo_put_ops.to_string());
        json.push_str(",\"utxo_delete_us\":");
        json.push_str(&self.utxo_delete_us.to_string());
        json.push_str(",\"utxo_delete_ops\":");
        json.push_str(&self.utxo_delete_ops.to_string());
        json.push_str(",\"spent_index_ops\":");
        json.push_str(&self.spent_index_ops.to_string());
        json.push_str(",\"address_index_inserts\":");
        json.push_str(&self.address_index_inserts.to_string());
        json.push_str(",\"address_index_deletes\":");
        json.push_str(&self.address_index_deletes.to_string());
        json.push_str(",\"address_delta_inserts\":");
        json.push_str(&self.address_delta_inserts.to_string());
        json.push_str(",\"tx_index_ops\":");
        json.push_str(&self.tx_index_ops.to_string());
        json.push_str(",\"header_index_ops\":");
        json.push_str(&self.header_index_ops.to_string());
        json.push_str(",\"timestamp_index_ops\":");
        json.push_str(&self.timestamp_index_ops.to_string());
        json.push_str(",\"undo_encode_us\":");
        json.push_str(&self.undo_encode_us.to_string());
        json.push_str(",\"undo_bytes\":");
        json.push_str(&self.undo_bytes.to_string());
        json.push_str(",\"undo_append_us\":");
        json.push_str(&self.undo_append_us.to_string());

        json.push_str(",\"db_write_buffer_bytes\":");
        push_json_u64_opt(&mut json, self.db_write_buffer_bytes);
        json.push_str(",\"db_journal_count\":");
        push_json_u64_opt(&mut json, self.db_journal_count);
        json.push_str(",\"db_flushes_completed\":");
        push_json_u64_opt(&mut json, self.db_flushes_completed);
        json.push_str(",\"db_active_compactions\":");
        push_json_u64_opt(&mut json, self.db_active_compactions);
        json.push_str(",\"db_compactions_completed\":");
        push_json_u64_opt(&mut json, self.db_compactions_completed);
        json.push_str(",\"db_time_compacting_us\":");
        push_json_u64_opt(&mut json, self.db_time_compacting_us);
        json.push_str(",\"db_utxo_segments\":");
        push_json_u64_opt(&mut json, self.db_utxo_segments);
        json.push_str(",\"db_utxo_flushes_completed\":");
        push_json_u64_opt(&mut json, self.db_utxo_flushes_completed);
        json.push_str(",\"db_tx_index_segments\":");
        push_json_u64_opt(&mut json, self.db_tx_index_segments);
        json.push_str(",\"db_tx_index_flushes_completed\":");
        push_json_u64_opt(&mut json, self.db_tx_index_flushes_completed);
        json.push_str(",\"db_spent_index_segments\":");
        push_json_u64_opt(&mut json, self.db_spent_index_segments);
        json.push_str(",\"db_spent_index_flushes_completed\":");
        push_json_u64_opt(&mut json, self.db_spent_index_flushes_completed);
        json.push_str(",\"db_address_outpoint_segments\":");
        push_json_u64_opt(&mut json, self.db_address_outpoint_segments);
        json.push_str(",\"db_address_outpoint_flushes_completed\":");
        push_json_u64_opt(&mut json, self.db_address_outpoint_flushes_completed);
        json.push_str(",\"db_address_delta_segments\":");
        push_json_u64_opt(&mut json, self.db_address_delta_segments);
        json.push_str(",\"db_address_delta_flushes_completed\":");
        push_json_u64_opt(&mut json, self.db_address_delta_flushes_completed);
        json.push_str(",\"db_header_index_segments\":");
        push_json_u64_opt(&mut json, self.db_header_index_segments);
        json.push_str(",\"db_header_index_flushes_completed\":");
        push_json_u64_opt(&mut json, self.db_header_index_flushes_completed);
        json.push('}');
        json
    }
}

fn push_json_u64_opt(json: &mut String, value: Option<u64>) {
    match value {
        Some(value) => json.push_str(&value.to_string()),
        None => json.push_str("null"),
    }
}

#[derive(Debug, Default)]
pub struct SyncMetrics {
    download_us: AtomicU64,
    download_blocks: AtomicU64,
    verify_us: AtomicU64,
    verify_blocks: AtomicU64,
    commit_us: AtomicU64,
    commit_blocks: AtomicU64,
}

impl SyncMetrics {
    pub fn record_download(&self, blocks: u64, elapsed: Duration) {
        self.download_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.download_blocks.fetch_add(blocks, Ordering::Relaxed);
    }

    pub fn record_verify(&self, blocks: u64, elapsed: Duration) {
        self.verify_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.verify_blocks.fetch_add(blocks, Ordering::Relaxed);
    }

    pub fn record_commit(&self, blocks: u64, elapsed: Duration) {
        self.commit_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.commit_blocks.fetch_add(blocks, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            download_us: self.download_us.load(Ordering::Relaxed),
            download_blocks: self.download_blocks.load(Ordering::Relaxed),
            verify_us: self.verify_us.load(Ordering::Relaxed),
            verify_blocks: self.verify_blocks.load(Ordering::Relaxed),
            commit_us: self.commit_us.load(Ordering::Relaxed),
            commit_blocks: self.commit_blocks.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MetricsSnapshot {
    pub download_us: u64,
    pub download_blocks: u64,
    pub verify_us: u64,
    pub verify_blocks: u64,
    pub commit_us: u64,
    pub commit_blocks: u64,
}

#[derive(Debug, Default)]
pub struct HeaderMetrics {
    request_us: AtomicU64,
    request_batches: AtomicU64,
    validate_us: AtomicU64,
    validate_headers: AtomicU64,
    commit_us: AtomicU64,
    commit_headers: AtomicU64,
    pow_us: AtomicU64,
    pow_headers: AtomicU64,
}

impl HeaderMetrics {
    pub fn record_request(&self, batches: u64, elapsed: Duration) {
        self.request_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.request_batches.fetch_add(batches, Ordering::Relaxed);
    }

    pub fn record_validate(&self, headers: u64, elapsed: Duration) {
        self.validate_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.validate_headers.fetch_add(headers, Ordering::Relaxed);
    }

    pub fn record_commit(&self, headers: u64, elapsed: Duration) {
        self.commit_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.commit_headers.fetch_add(headers, Ordering::Relaxed);
    }

    pub fn record_pow(&self, headers: u64, elapsed: Duration) {
        self.pow_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.pow_headers.fetch_add(headers, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> HeaderMetricsSnapshot {
        HeaderMetricsSnapshot {
            request_us: self.request_us.load(Ordering::Relaxed),
            request_batches: self.request_batches.load(Ordering::Relaxed),
            validate_us: self.validate_us.load(Ordering::Relaxed),
            validate_headers: self.validate_headers.load(Ordering::Relaxed),
            commit_us: self.commit_us.load(Ordering::Relaxed),
            commit_headers: self.commit_headers.load(Ordering::Relaxed),
            pow_us: self.pow_us.load(Ordering::Relaxed),
            pow_headers: self.pow_headers.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct HeaderMetricsSnapshot {
    pub request_us: u64,
    pub request_batches: u64,
    pub validate_us: u64,
    pub validate_headers: u64,
    pub commit_us: u64,
    pub commit_headers: u64,
    pub pow_us: u64,
    pub pow_headers: u64,
}

#[derive(Debug, Default)]
pub struct MempoolMetrics {
    rpc_accept: AtomicU64,
    rpc_reject: AtomicU64,
    relay_accept: AtomicU64,
    relay_reject: AtomicU64,
    evicted: AtomicU64,
    evicted_bytes: AtomicU64,
    loaded: AtomicU64,
    load_reject: AtomicU64,
    persisted_writes: AtomicU64,
    persisted_bytes: AtomicU64,
}

impl MempoolMetrics {
    pub fn note_rpc_accept(&self) {
        self.rpc_accept.fetch_add(1, Ordering::Relaxed);
    }

    pub fn note_rpc_reject(&self) {
        self.rpc_reject.fetch_add(1, Ordering::Relaxed);
    }

    pub fn note_relay_accept(&self) {
        self.relay_accept.fetch_add(1, Ordering::Relaxed);
    }

    pub fn note_relay_reject(&self) {
        self.relay_reject.fetch_add(1, Ordering::Relaxed);
    }

    pub fn note_evicted(&self, count: u64, bytes: u64) {
        self.evicted.fetch_add(count, Ordering::Relaxed);
        self.evicted_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn note_loaded(&self, count: u64) {
        self.loaded.fetch_add(count, Ordering::Relaxed);
    }

    pub fn note_load_reject(&self, count: u64) {
        self.load_reject.fetch_add(count, Ordering::Relaxed);
    }

    pub fn note_persisted(&self, bytes: u64) {
        self.persisted_writes.fetch_add(1, Ordering::Relaxed);
        self.persisted_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MempoolMetricsSnapshot {
        MempoolMetricsSnapshot {
            rpc_accept: self.rpc_accept.load(Ordering::Relaxed),
            rpc_reject: self.rpc_reject.load(Ordering::Relaxed),
            relay_accept: self.relay_accept.load(Ordering::Relaxed),
            relay_reject: self.relay_reject.load(Ordering::Relaxed),
            evicted: self.evicted.load(Ordering::Relaxed),
            evicted_bytes: self.evicted_bytes.load(Ordering::Relaxed),
            loaded: self.loaded.load(Ordering::Relaxed),
            load_reject: self.load_reject.load(Ordering::Relaxed),
            persisted_writes: self.persisted_writes.load(Ordering::Relaxed),
            persisted_bytes: self.persisted_bytes.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MempoolMetricsSnapshot {
    pub rpc_accept: u64,
    pub rpc_reject: u64,
    pub relay_accept: u64,
    pub relay_reject: u64,
    pub evicted: u64,
    pub evicted_bytes: u64,
    pub loaded: u64,
    pub load_reject: u64,
    pub persisted_writes: u64,
    pub persisted_bytes: u64,
}

#[allow(clippy::too_many_arguments)]
pub fn snapshot_stats<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    store: Option<&Store>,
    network: Network,
    backend: Backend,
    start_time: Instant,
    sync_metrics: Option<&SyncMetrics>,
    header_metrics: Option<&HeaderMetrics>,
    validation_metrics: Option<&ValidationMetrics>,
    connect_metrics: Option<&ConnectMetrics>,
    mempool: Option<&Mutex<Mempool>>,
    mempool_metrics: Option<&MempoolMetrics>,
) -> Result<StatsSnapshot, String> {
    let best_header = chainstate.best_header().map_err(|err| err.to_string())?;
    let best_block = chainstate.best_block().map_err(|err| err.to_string())?;

    let best_header_height = best_header.as_ref().map(|tip| tip.height).unwrap_or(-1);
    let best_block_height = best_block.as_ref().map(|tip| tip.height).unwrap_or(-1);

    let best_header_hash = best_header.map(|tip| hash256_to_hex(&tip.hash));
    let best_block_hash = best_block.map(|tip| hash256_to_hex(&tip.hash));

    let header_count = if best_header_height >= 0 {
        best_header_height as u64 + 1
    } else {
        0
    };
    let block_count = if best_block_height >= 0 {
        best_block_height as u64 + 1
    } else {
        0
    };

    let header_gap = best_header_height as i64 - best_block_height as i64;
    let sync_state = if header_gap <= 0 { "synced" } else { "syncing" };

    let uptime_secs = start_time.elapsed().as_secs();
    let unix_time_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let metrics = sync_metrics.map(SyncMetrics::snapshot).unwrap_or_default();
    let header_metrics = header_metrics
        .map(HeaderMetrics::snapshot)
        .unwrap_or_default();
    let validation = validation_metrics
        .map(ValidationMetrics::snapshot)
        .unwrap_or_default();
    let connect = connect_metrics
        .map(ConnectMetrics::snapshot)
        .unwrap_or_default();
    let db = store.and_then(|store| store.fjall_telemetry_snapshot());
    let (mempool_size, mempool_bytes, mempool_max_bytes) = match mempool {
        Some(mempool) => match mempool.lock() {
            Ok(guard) => (
                guard.size() as u64,
                guard.bytes() as u64,
                guard.max_bytes() as u64,
            ),
            Err(_) => (0, 0, 0),
        },
        None => (0, 0, 0),
    };
    let mempool_metrics_snapshot = mempool_metrics
        .map(MempoolMetrics::snapshot)
        .unwrap_or_default();

    Ok(StatsSnapshot {
        network: format!("{network:?}"),
        backend: format!("{backend:?}"),
        best_header_height,
        best_block_height,
        best_header_hash,
        best_block_hash,
        header_count,
        block_count,
        header_gap,
        uptime_secs,
        unix_time_secs,
        sync_state: sync_state.to_string(),
        mempool_size,
        mempool_bytes,
        mempool_max_bytes,
        mempool_rpc_accept: mempool_metrics_snapshot.rpc_accept,
        mempool_rpc_reject: mempool_metrics_snapshot.rpc_reject,
        mempool_relay_accept: mempool_metrics_snapshot.relay_accept,
        mempool_relay_reject: mempool_metrics_snapshot.relay_reject,
        mempool_evicted: mempool_metrics_snapshot.evicted,
        mempool_evicted_bytes: mempool_metrics_snapshot.evicted_bytes,
        mempool_loaded: mempool_metrics_snapshot.loaded,
        mempool_load_reject: mempool_metrics_snapshot.load_reject,
        mempool_persisted_writes: mempool_metrics_snapshot.persisted_writes,
        mempool_persisted_bytes: mempool_metrics_snapshot.persisted_bytes,
        download_us: metrics.download_us,
        download_blocks: metrics.download_blocks,
        verify_us: metrics.verify_us,
        verify_blocks: metrics.verify_blocks,
        commit_us: metrics.commit_us,
        commit_blocks: metrics.commit_blocks,
        header_request_us: header_metrics.request_us,
        header_request_batches: header_metrics.request_batches,
        header_validate_us: header_metrics.validate_us,
        header_validate_headers: header_metrics.validate_headers,
        header_commit_us: header_metrics.commit_us,
        header_commit_headers: header_metrics.commit_headers,
        header_pow_us: header_metrics.pow_us,
        header_pow_headers: header_metrics.pow_headers,
        validate_us: validation.validate_us,
        validate_blocks: validation.validate_blocks,
        script_us: validation.script_us,
        script_blocks: validation.script_blocks,
        shielded_us: validation.shielded_us,
        shielded_txs: validation.shielded_txs,
        utxo_us: connect.utxo_us,
        utxo_blocks: connect.utxo_blocks,
        index_us: connect.index_us,
        index_blocks: connect.index_blocks,
        anchor_us: connect.anchor_us,
        anchor_blocks: connect.anchor_blocks,
        flatfile_us: connect.flatfile_us,
        flatfile_blocks: connect.flatfile_blocks,
        utxo_get_us: connect.utxo_get_us,
        utxo_get_ops: connect.utxo_get_ops,
        utxo_cache_hits: connect.utxo_cache_hits,
        utxo_cache_misses: connect.utxo_cache_misses,
        utxo_put_us: connect.utxo_put_us,
        utxo_put_ops: connect.utxo_put_ops,
        utxo_delete_us: connect.utxo_delete_us,
        utxo_delete_ops: connect.utxo_delete_ops,
        spent_index_ops: connect.spent_index_ops,
        address_index_inserts: connect.address_index_inserts,
        address_index_deletes: connect.address_index_deletes,
        address_delta_inserts: connect.address_delta_inserts,
        tx_index_ops: connect.tx_index_ops,
        header_index_ops: connect.header_index_ops,
        timestamp_index_ops: connect.timestamp_index_ops,
        undo_encode_us: connect.undo_encode_us,
        undo_bytes: connect.undo_bytes,
        undo_append_us: connect.undo_append_us,
        db_write_buffer_bytes: db.as_ref().map(|db| db.write_buffer_bytes),
        db_journal_count: db.as_ref().map(|db| db.journal_count),
        db_flushes_completed: db.as_ref().map(|db| db.flushes_completed),
        db_active_compactions: db.as_ref().map(|db| db.active_compactions),
        db_compactions_completed: db.as_ref().map(|db| db.compactions_completed),
        db_time_compacting_us: db.as_ref().map(|db| db.time_compacting_us),
        db_utxo_segments: db.as_ref().map(|db| db.utxo_segments),
        db_utxo_flushes_completed: db.as_ref().map(|db| db.utxo_flushes_completed),
        db_tx_index_segments: db.as_ref().map(|db| db.tx_index_segments),
        db_tx_index_flushes_completed: db.as_ref().map(|db| db.tx_index_flushes_completed),
        db_spent_index_segments: db.as_ref().map(|db| db.spent_index_segments),
        db_spent_index_flushes_completed: db.as_ref().map(|db| db.spent_index_flushes_completed),
        db_address_outpoint_segments: db.as_ref().map(|db| db.address_outpoint_segments),
        db_address_outpoint_flushes_completed: db
            .as_ref()
            .map(|db| db.address_outpoint_flushes_completed),
        db_address_delta_segments: db.as_ref().map(|db| db.address_delta_segments),
        db_address_delta_flushes_completed: db
            .as_ref()
            .map(|db| db.address_delta_flushes_completed),
        db_header_index_segments: db.as_ref().map(|db| db.header_index_segments),
        db_header_index_flushes_completed: db.as_ref().map(|db| db.header_index_flushes_completed),
    })
}

pub fn hash256_to_hex(hash: &Hash256) -> String {
    let mut out = String::with_capacity(64);
    for byte in hash.iter().rev() {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'a' + (value - 10)) as char,
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}
