# Configuration reference

This document describes CLI flags for the fluxd-rust daemon (binary `fluxd`)
and how they affect behavior.

## Storage and data

- `--backend fjall|memory`
  - `fjall` is the default persistent store.
  - `memory` is non-persistent and only intended for testing.
- `--data-dir PATH`
  - Base data directory (default: `./data`).
  - Layout:
    - `db/` - Fjall keyspace.
    - `blocks/` - flatfile block store.
    - `peers.dat` - persisted peer address manager (success/fail stats, last-seen, last-height).
    - `banlist.dat` - persisted peer bans (best-effort).
    - `mempool.dat` - persisted mempool transactions (when enabled).
    - `fee_estimates.dat` - persisted fee estimator samples (when enabled).
    - `rpc.cookie` - RPC auth cookie when not using `--rpc-user`/`--rpc-pass`.

### Fjall tuning

These flags control Fjall memory usage.

Defaults (mainnet sync-focused; some values are auto-tuned to safe minima):
- `--db-cache-mb 256`
- `--db-write-buffer-mb 2048`
- `--db-journal-mb 2048`
- `--db-memtable-mb 64`
- `--db-flush-workers 2`
- `--db-compaction-workers 4`

- `--db-cache-mb N` - block cache size.
- `--db-write-buffer-mb N` - max write buffer size.
- `--db-journal-mb N` - max journaling size.
- `--db-memtable-mb N` - per-partition memtable size.
- `--db-flush-workers N` - flush worker threads.
- `--db-compaction-workers N` - compaction worker threads.
- `--db-fsync-ms N` - async fsync interval (0 disables).

If you see long pauses where blocks stop connecting while the process remains alive, this is often
Fjall write throttling due to L0 segment buildup. Practical mitigations:

- Ensure `--db-write-buffer-mb` is comfortably above `--db-memtable-mb × 19` (current partition count).
- Ensure `--db-journal-mb` is at least `2 × --db-memtable-mb × 19` (journal GC requires all partitions to flush).
- Increase `--db-compaction-workers` (and optionally `--db-flush-workers`) on hosts with spare CPU.

When `--db-write-buffer-mb` or `--db-journal-mb` is set below these minima, `fluxd` clamps the values
upward at startup (and prints a warning) to avoid long write halts.

## Chainstate caching

- `--utxo-cache-entries N`
  - In-memory cache for recently accessed UTXO entries (default: `200000`).
  - Set to `0` to disable.
  - This is a performance knob only; it does not affect consensus rules.

## Shielded parameters

- `--params-dir PATH` - directory for shielded params (default: `~/.zcash-params`).
- `--fetch-params` - download shielded params into `--params-dir` on startup.

## Network selection

- `--network mainnet|testnet|regtest` (default: mainnet).

RPC defaults:
- mainnet: `127.0.0.1:16124`
- testnet/regtest: `127.0.0.1:26124`

## Sync and peer behavior

- `--getdata-batch N` - max blocks per getdata request (default: 128).
- `--block-peers N` - parallel peers for block download (default: 3).
- `--header-peers N` - peers to probe for header sync (default: 4).
- `--header-peer IP:PORT` - pin a specific header peer (repeatable).
- `--header-lead N` - target header lead over blocks (default: 20000, 0 disables cap).
- `--tx-peers N` - relay peers for transaction inventory/tx relay (default: 2, 0 disables).
- `--inflight-per-peer N` - concurrent getdata requests per peer (default: 1).
- `--status-interval SECS` - status log interval (default: 15, 0 disables).

## Mempool

- `--mempool-max-mb N` (alias: `--maxmempool N`)
  - Maximum mempool size in MiB (default: `300`).
  - Set to `0` to disable the size cap.
  - When the cap is exceeded, the daemon evicts transactions by lowest fee-rate first (tie-break:
    oldest first).
- `--mempool-persist-interval SECS`
  - Persist mempool to `mempool.dat` every N seconds (default: `60`).
  - Set to `0` to disable mempool persistence (no load and no save).
- `--minrelaytxfee <rate>` (alias: `--min-relay-tx-fee`)
  - Minimum relay fee-rate for non-fluxnode transactions, expressed as zatoshis/kB.
  - Accepts either:
    - an integer zatoshi-per-kB value (example: `100`), or
    - a decimal FLUX-per-kB value (example: `0.00000100`).
  - Default: `100` (0.00000100 FLUX/kB).
- `--accept-non-standard`
  - Disable standardness checks (script template policy, dust, scriptSig push-only, etc.).
  - Default: standardness is required on mainnet/testnet and disabled on regtest.
- `--require-standard`
  - Force standardness checks even on regtest.
- `--fee-estimates-persist-interval SECS`
  - Persist fee estimator samples to `fee_estimates.dat` every N seconds (default: `300`).
  - Set to `0` to disable persistence.

Practical notes:
- Increase `--header-peers` and `--header-verify-workers` to boost header throughput.
- Increase `--getdata-batch` and `--inflight-per-peer` to raise block download parallelism.
- Set `--header-lead 0` to allow unlimited header lead (useful for fast bootstraps).

## Validation and workers

- `--skip-script` - disable script validation (testing only).
- `--header-verify-workers N` - PoW header verification threads (0 = auto).
- `--verify-workers N` - pre-validation worker threads (0 = auto).
- `--verify-queue N` - pre-validation queue depth (0 = auto).
- `--shielded-workers N` - shielded verification threads (0 = auto).

Auto worker defaults aim to keep shielded proof verification saturated while leaving CPU for
block connect/DB work.

## RPC

- `--rpc-addr IP:PORT` - bind address (defaults per network).
- `--rpc-user USER` and `--rpc-pass PASS` - explicit RPC credentials.

If you do not specify user/pass, the daemon writes `rpc.cookie` into `--data-dir`.

## Dashboard

- `--dashboard-addr IP:PORT` - enable HTTP dashboard server.

Endpoints:
- `/` - HTML dashboard.
- `/stats` - JSON stats.
- `/healthz` - simple liveness probe.

## Maintenance modes

- `--scan-flatfiles` - scan flatfiles for index mismatches, then exit.
- `--scan-supply` - scan blocks in the local DB and print coinbase totals, then exit.
- `--scan-fluxnodes` - scan fluxnode records in the local DB and print summary stats, then exit.
