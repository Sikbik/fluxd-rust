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
    utxo_get_us: AtomicU64,
    utxo_get_ops: AtomicU64,
    utxo_cache_hits: AtomicU64,
    utxo_cache_misses: AtomicU64,
    utxo_put_us: AtomicU64,
    utxo_put_ops: AtomicU64,
    utxo_delete_us: AtomicU64,
    utxo_delete_ops: AtomicU64,
    spent_index_ops: AtomicU64,
    address_index_inserts: AtomicU64,
    address_index_deletes: AtomicU64,
    address_delta_inserts: AtomicU64,
    tx_index_ops: AtomicU64,
    header_index_ops: AtomicU64,
    timestamp_index_ops: AtomicU64,
    undo_encode_us: AtomicU64,
    undo_bytes: AtomicU64,
    undo_append_us: AtomicU64,
    fluxnode_tx_us: AtomicU64,
    fluxnode_tx_count: AtomicU64,
    fluxnode_sig_us: AtomicU64,
    fluxnode_sig_checks: AtomicU64,
    pon_sig_us: AtomicU64,
    pon_sig_blocks: AtomicU64,
    payout_us: AtomicU64,
    payout_blocks: AtomicU64,
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
    pub fluxnode_tx_us: u64,
    pub fluxnode_tx_count: u64,
    pub fluxnode_sig_us: u64,
    pub fluxnode_sig_checks: u64,
    pub pon_sig_us: u64,
    pub pon_sig_blocks: u64,
    pub payout_us: u64,
    pub payout_blocks: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ConnectMetricsDelta {
    pub utxo_us: u64,
    pub index_us: u64,
    pub anchor_us: u64,
    pub flatfile_us: u64,
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
    pub fluxnode_tx_us: u64,
    pub fluxnode_tx_count: u64,
    pub fluxnode_sig_us: u64,
    pub fluxnode_sig_checks: u64,
    pub pon_sig_us: u64,
    pub pon_sig_blocks: u64,
    pub payout_us: u64,
    pub payout_blocks: u64,
}

impl ConnectMetrics {
    pub fn record_block(&self, delta: &ConnectMetricsDelta) {
        self.utxo_us.fetch_add(delta.utxo_us, Ordering::Relaxed);
        self.utxo_blocks.fetch_add(1, Ordering::Relaxed);
        self.index_us.fetch_add(delta.index_us, Ordering::Relaxed);
        self.index_blocks.fetch_add(1, Ordering::Relaxed);
        self.anchor_us.fetch_add(delta.anchor_us, Ordering::Relaxed);
        self.anchor_blocks.fetch_add(1, Ordering::Relaxed);
        self.flatfile_us
            .fetch_add(delta.flatfile_us, Ordering::Relaxed);
        self.flatfile_blocks.fetch_add(1, Ordering::Relaxed);

        self.utxo_get_us
            .fetch_add(delta.utxo_get_us, Ordering::Relaxed);
        self.utxo_get_ops
            .fetch_add(delta.utxo_get_ops, Ordering::Relaxed);
        self.utxo_cache_hits
            .fetch_add(delta.utxo_cache_hits, Ordering::Relaxed);
        self.utxo_cache_misses
            .fetch_add(delta.utxo_cache_misses, Ordering::Relaxed);
        self.utxo_put_us
            .fetch_add(delta.utxo_put_us, Ordering::Relaxed);
        self.utxo_put_ops
            .fetch_add(delta.utxo_put_ops, Ordering::Relaxed);
        self.utxo_delete_us
            .fetch_add(delta.utxo_delete_us, Ordering::Relaxed);
        self.utxo_delete_ops
            .fetch_add(delta.utxo_delete_ops, Ordering::Relaxed);

        self.spent_index_ops
            .fetch_add(delta.spent_index_ops, Ordering::Relaxed);
        self.address_index_inserts
            .fetch_add(delta.address_index_inserts, Ordering::Relaxed);
        self.address_index_deletes
            .fetch_add(delta.address_index_deletes, Ordering::Relaxed);
        self.address_delta_inserts
            .fetch_add(delta.address_delta_inserts, Ordering::Relaxed);
        self.tx_index_ops
            .fetch_add(delta.tx_index_ops, Ordering::Relaxed);
        self.header_index_ops
            .fetch_add(delta.header_index_ops, Ordering::Relaxed);
        self.timestamp_index_ops
            .fetch_add(delta.timestamp_index_ops, Ordering::Relaxed);

        self.undo_encode_us
            .fetch_add(delta.undo_encode_us, Ordering::Relaxed);
        self.undo_bytes
            .fetch_add(delta.undo_bytes, Ordering::Relaxed);
        self.undo_append_us
            .fetch_add(delta.undo_append_us, Ordering::Relaxed);

        self.fluxnode_tx_us
            .fetch_add(delta.fluxnode_tx_us, Ordering::Relaxed);
        self.fluxnode_tx_count
            .fetch_add(delta.fluxnode_tx_count, Ordering::Relaxed);
        self.fluxnode_sig_us
            .fetch_add(delta.fluxnode_sig_us, Ordering::Relaxed);
        self.fluxnode_sig_checks
            .fetch_add(delta.fluxnode_sig_checks, Ordering::Relaxed);
        self.pon_sig_us
            .fetch_add(delta.pon_sig_us, Ordering::Relaxed);
        self.pon_sig_blocks
            .fetch_add(delta.pon_sig_blocks, Ordering::Relaxed);
        self.payout_us.fetch_add(delta.payout_us, Ordering::Relaxed);
        self.payout_blocks
            .fetch_add(delta.payout_blocks, Ordering::Relaxed);
    }

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
            utxo_get_us: self.utxo_get_us.load(Ordering::Relaxed),
            utxo_get_ops: self.utxo_get_ops.load(Ordering::Relaxed),
            utxo_cache_hits: self.utxo_cache_hits.load(Ordering::Relaxed),
            utxo_cache_misses: self.utxo_cache_misses.load(Ordering::Relaxed),
            utxo_put_us: self.utxo_put_us.load(Ordering::Relaxed),
            utxo_put_ops: self.utxo_put_ops.load(Ordering::Relaxed),
            utxo_delete_us: self.utxo_delete_us.load(Ordering::Relaxed),
            utxo_delete_ops: self.utxo_delete_ops.load(Ordering::Relaxed),
            spent_index_ops: self.spent_index_ops.load(Ordering::Relaxed),
            address_index_inserts: self.address_index_inserts.load(Ordering::Relaxed),
            address_index_deletes: self.address_index_deletes.load(Ordering::Relaxed),
            address_delta_inserts: self.address_delta_inserts.load(Ordering::Relaxed),
            tx_index_ops: self.tx_index_ops.load(Ordering::Relaxed),
            header_index_ops: self.header_index_ops.load(Ordering::Relaxed),
            timestamp_index_ops: self.timestamp_index_ops.load(Ordering::Relaxed),
            undo_encode_us: self.undo_encode_us.load(Ordering::Relaxed),
            undo_bytes: self.undo_bytes.load(Ordering::Relaxed),
            undo_append_us: self.undo_append_us.load(Ordering::Relaxed),
            fluxnode_tx_us: self.fluxnode_tx_us.load(Ordering::Relaxed),
            fluxnode_tx_count: self.fluxnode_tx_count.load(Ordering::Relaxed),
            fluxnode_sig_us: self.fluxnode_sig_us.load(Ordering::Relaxed),
            fluxnode_sig_checks: self.fluxnode_sig_checks.load(Ordering::Relaxed),
            pon_sig_us: self.pon_sig_us.load(Ordering::Relaxed),
            pon_sig_blocks: self.pon_sig_blocks.load(Ordering::Relaxed),
            payout_us: self.payout_us.load(Ordering::Relaxed),
            payout_blocks: self.payout_blocks.load(Ordering::Relaxed),
        }
    }
}
