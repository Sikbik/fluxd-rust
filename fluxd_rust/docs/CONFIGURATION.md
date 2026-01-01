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
- `--conf PATH`
  - Config file path (default: `<data-dir>/flux.conf`).

## Logging

- `--log-level error|warn|info|debug|trace` (default: `info`)
- `--log-format text|json` (default: `text`)
- `--log-timestamps` / `--no-log-timestamps` (text logs only; default: timestamps enabled)

Logs are written to stderr by default. CLI commands that return machine-readable output (like
`--db-info`) print to stdout.

When `--log-format json` is enabled, each line is a JSON object with keys like `ts_ms`, `level`,
`target`, `file`, `line`, and `msg`.

## flux.conf

The daemon optionally reads a `flux.conf` config file.

- Default path: `<data-dir>/flux.conf`
- Override path: `--conf PATH`
- Format: `key=value` (repeatable keys are allowed; `#` and `;` start comments)
- Precedence: CLI flags override config file values

Currently supported keys:
- `dbcache` (MiB; maps to `--db-cache-mb`)
- `maxconnections` (max peer connections; maps to `--maxconnections`)
- `maxmempool` (MiB; maps to `--mempool-max-mb`)
- `minrelaytxfee` (FLUX/kB; maps to `--minrelaytxfee`)
- `listen` (`1|0`; enables/disables inbound P2P listener)
- `bind` (IP or IP:PORT; binds inbound P2P listener; defaults to network P2P port)
- `rpcuser`, `rpcpassword`
- `rpcbind`, `rpcport`
- `rpcallowip` (repeatable; IP or CIDR, e.g. `127.0.0.1`, `10.0.0.0/8`)
- `loglevel` (`error|warn|info|debug|trace`)
- `logformat` (`text|json`)
- `logtimestamps` (`1|0`)
- `addnode` (repeatable)
- `mineraddress` (default coinbase/miner address for `getblocktemplate`)
- `testnet=1` / `regtest=1` (network selection; CLI `--network ...` overrides)

RPC allowlist notes:
- By default, RPC only allows connections from `127.0.0.1` and `::1` (localhost).
- Use `rpcallowip=...` (or CLI `--rpc-allow-ip ...`) to permit non-local RPC clients.

Unsupported keys are ignored; `fluxd` prints a warning listing the ignored keys to help catch
misconfigurations.

## Run profiles

`--profile low|default|high` applies a preset for performance-related tuning knobs (sync, Fjall,
and worker configuration).

Precedence (highest to lowest):
1. Explicit CLI flags (e.g. `--db-cache-mb 512`)
2. `flux.conf` values
3. `--profile ...`
4. Built-in defaults

The daemon prints `Using profile <name>` at startup when a non-default profile is selected.

Current presets:

- `low` (constrained hosts)
  - Sync: `--getdata-batch 64`, `--block-peers 1`, `--header-peers 2`, `--tx-peers 0`,
    `--inflight-per-peer 1`
  - Mempool: `--mempool-max-mb 100`, `--mempool-persist-interval 0`,
    `--fee-estimates-persist-interval 0`
  - DB: `--db-cache-mb 128`, `--db-write-buffer-mb 512`, `--db-journal-mb 1024`,
    `--db-memtable-mb 16`, `--db-flush-workers 1`, `--db-compaction-workers 2`
  - Cache/workers: `--utxo-cache-entries 50000`, `--header-verify-workers 1`,
    `--shielded-workers 1`
- `default`
  - No overrides; uses built-in defaults.
- `high` (throughput-oriented)
  - Sync: `--getdata-batch 256`, `--block-peers 6`, `--header-peers 8`, `--tx-peers 4`,
    `--inflight-per-peer 2`
  - Mempool: `--mempool-max-mb 1000`
  - DB: `--db-write-buffer-mb 4096`, `--db-journal-mb 16384`, `--db-memtable-mb 128`,
    `--db-flush-workers 4`, `--db-compaction-workers 6`
  - Cache: `--utxo-cache-entries 1000000`

## Fjall tuning

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

`fluxd` also monitors Fjall write-buffer and journal pressure at runtime and may rotate memtables to
proactively trigger flush + journal GC. When this happens it prints a periodic warning and `/stats`
will show the relevant `db_*` counters.

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

- `--p2p-addr IP:PORT` - bind address for inbound P2P connections (default: `0.0.0.0:<net p2p port>`).
- `--no-p2p-listen` - disable inbound P2P listener (useful for running multiple local instances).
- `--maxconnections N` - maximum total peer connections (inbound + outbound) (default: 125).
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

## Mining

- `--miner-address TADDR`
  - Default miner address used by `getblocktemplate` when the request does not include
    `mineraddress` / `address`.
  - If unset and the request omits `mineraddress`, the daemon uses the first wallet key
    (creating one in `wallet.dat` if the wallet is empty).
  - Equivalent `flux.conf` key: `mineraddress=t1...` (CLI overrides config file).

## Dashboard

- `--dashboard-addr IP:PORT` - enable HTTP dashboard server.

Endpoints:
- `/` - HTML dashboard.
- `/stats` - JSON stats.
- `/metrics` - Prometheus-style plaintext metrics (derived from `/stats`).
- `/healthz` - simple liveness probe.

## Maintenance modes

- `--scan-flatfiles` - scan flatfiles for index mismatches, then exit.
- `--scan-supply` - scan blocks in the local DB and print coinbase totals, then exit.
- `--scan-fluxnodes` - scan fluxnode records in the local DB and print summary stats, then exit.
