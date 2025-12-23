//! Chainstate connection metrics.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[derive(Debug, Default)]
pub struct ConnectMetrics {
    utxo_us: AtomicU64,
    utxo_blocks: AtomicU64,
    index_us: AtomicU64,
    index_blocks: AtomicU64,
    anchor_us: AtomicU64,
    anchor_blocks: AtomicU64,
    flatfile_us: AtomicU64,
    flatfile_blocks: AtomicU64,
}

#[derive(Clone, Debug, Default)]
pub struct ConnectMetricsSnapshot {
    pub utxo_us: u64,
    pub utxo_blocks: u64,
    pub index_us: u64,
    pub index_blocks: u64,
    pub anchor_us: u64,
    pub anchor_blocks: u64,
    pub flatfile_us: u64,
    pub flatfile_blocks: u64,
}

impl ConnectMetrics {
    pub fn record_utxo(&self, elapsed: Duration) {
        self.utxo_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.utxo_blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_index(&self, elapsed: Duration) {
        self.index_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.index_blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_anchor(&self, elapsed: Duration) {
        self.anchor_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.anchor_blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_flatfile(&self, elapsed: Duration) {
        self.flatfile_us
            .fetch_add(elapsed.as_micros() as u64, Ordering::Relaxed);
        self.flatfile_blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> ConnectMetricsSnapshot {
        ConnectMetricsSnapshot {
            utxo_us: self.utxo_us.load(Ordering::Relaxed),
            utxo_blocks: self.utxo_blocks.load(Ordering::Relaxed),
            index_us: self.index_us.load(Ordering::Relaxed),
            index_blocks: self.index_blocks.load(Ordering::Relaxed),
            anchor_us: self.anchor_us.load(Ordering::Relaxed),
            anchor_blocks: self.anchor_blocks.load(Ordering::Relaxed),
            flatfile_us: self.flatfile_us.load(Ordering::Relaxed),
            flatfile_blocks: self.flatfile_blocks.load(Ordering::Relaxed),
        }
    }
}
