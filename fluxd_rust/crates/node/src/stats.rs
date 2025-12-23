use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fluxd_chainstate::metrics::ConnectMetrics;
use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::ValidationMetrics;
use fluxd_consensus::params::Network;
use fluxd_consensus::Hash256;
use fluxd_storage::KeyValueStore;

use crate::Backend;

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

        let mut json = String::with_capacity(512);
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
        json.push('}');
        json
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

pub fn snapshot_stats<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    network: Network,
    backend: Backend,
    start_time: Instant,
    sync_metrics: Option<&SyncMetrics>,
    header_metrics: Option<&HeaderMetrics>,
    validation_metrics: Option<&ValidationMetrics>,
    connect_metrics: Option<&ConnectMetrics>,
) -> Result<StatsSnapshot, String> {
    let best_header = chainstate
        .best_header()
        .map_err(|err| err.to_string())?;
    let best_block = chainstate
        .best_block()
        .map_err(|err| err.to_string())?;

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
    let sync_state = if header_gap <= 0 {
        "synced"
    } else {
        "syncing"
    };

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
