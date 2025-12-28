# Telemetry and performance diagnostics

This document describes the runtime counters exposed by the dashboard `/stats` endpoint and how to
use them to debug sync throughput issues (headers and block indexing).

## `/stats` basics

`/stats` returns **cumulative counters** since process start. To get per-second rates or per-block
costs, take two snapshots and compute deltas.

Useful pairs:

- `download_us` / `download_blocks` - time spent downloading blocks over P2P.
- `verify_us` / `verify_blocks` - time spent building/validating/connect-preparing blocks (CPU work).
- `commit_us` / `commit_blocks` - time spent committing the write batch to the DB.

Example (per-block):

- `verify_ms_per_block = (Δverify_us / 1000) / Δverify_blocks`
- `commit_ms_per_block = (Δcommit_us / 1000) / Δcommit_blocks`

## Connect-stage breakdown

Block connect is where we update UTXOs and indexes and generate undo data. `/stats` exposes
additional counters to explain why `verify_ms_per_block` changes over time:

- UTXO operations:
  - `utxo_get_us`, `utxo_get_ops` - time/ops for UTXO reads (cache + DB reads).
  - `utxo_put_us`, `utxo_put_ops` - time/ops for UTXO inserts (new outputs).
  - `utxo_delete_us`, `utxo_delete_ops` - time/ops for UTXO deletes (spent outputs).
  - `utxo_cache_hits`, `utxo_cache_misses` - read-cache effectiveness during sync.
- Index operation counts:
  - `spent_index_ops` - spent index inserts (one per transparent input).
  - `address_index_inserts`, `address_index_deletes` - address outpoint index updates.
  - `address_delta_inserts` - address delta rows written (spends + creates).
  - `tx_index_ops` - txindex rows written (one per tx).
  - `header_index_ops` - header/height tip updates written during block connect.
  - `timestamp_index_ops` - timestamp-related rows written during connect.
- Undo:
  - `undo_encode_us` - time spent encoding undo bytes (CPU).
  - `undo_bytes` - total undo bytes produced.
  - `undo_append_us` - time spent appending undo bytes to the undo flatfiles.

Tuning hint:

- If `utxo_cache_hits / (utxo_cache_hits + utxo_cache_misses)` is low and `utxo_get_us` dominates,
  increasing `--utxo-cache-entries` can help (memory permitting).

## Fjall (DB) health

When using the `fjall` backend, `/stats` includes `db_*` fields:

- Keyspace-level:
  - `db_write_buffer_bytes` - current global write buffer usage (active + sealed memtables).
  - `db_journal_count` - journals currently on disk.
  - `db_flushes_completed` - completed memtable flushes.
  - `db_active_compactions` - compactions currently running.
  - `db_compactions_completed` - completed compactions.
  - `db_time_compacting_us` - total time spent compacting.
- Partition-level (selected hot partitions):
  - `db_tx_index_segments`, `db_utxo_segments`, `db_spent_index_segments`,
    `db_address_outpoint_segments`, `db_address_delta_segments`, `db_header_index_segments`
  - `db_*_flushes_completed` for the same partitions

Interpreting stalls:

- If block connect “pauses” while the process is alive, and `db_*_segments` grows quickly while
  `db_compactions_completed` grows slowly, compaction is likely the limiter.
- If `db_write_buffer_bytes` stays high, flush/compaction may be falling behind.

See `docs/CONFIGURATION.md` for the primary tuning knobs:

- `--db-write-buffer-mb`, `--db-memtable-mb`
- `--db-compaction-workers`, `--db-flush-workers`
