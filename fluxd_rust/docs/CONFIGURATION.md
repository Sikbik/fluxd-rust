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
    - `rpc.cookie` - RPC auth cookie when not using `--rpc-user`/`--rpc-pass`.

### Fjall tuning (optional)

These flags control Fjall memory usage. Use them when running on VPS or constrained hosts.

- `--db-cache-mb N` - block cache size.
- `--db-write-buffer-mb N` - max write buffer size.
- `--db-journal-mb N` - max journaling size.
- `--db-memtable-mb N` - per-partition memtable size.

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
- `--block-peers N` - parallel peers for block download (default: 4).
- `--header-peers N` - peers to probe for header sync (default: 4).
- `--header-peer IP:PORT` - pin a specific header peer (repeatable).
- `--header-lead N` - target header lead over blocks (default: 20000, 0 disables cap).
- `--inflight-per-peer N` - concurrent getdata requests per peer (default: 2).
- `--status-interval SECS` - status log interval (default: 15, 0 disables).

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
- `--shielded-queue N` - shielded verification queue depth (0 = auto).

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
