**WORK IN PROGRESS**

# fluxd-rust

fluxd-rust is a Rust rewrite of the Flux daemon (fluxd). It targets full consensus parity with the C++ reference daemon while modernizing the storage layer (Fjall), sync pipeline, and operator UX. The C++ reference daemon remains the source of truth for consensus behavior and is maintained separately.

This project is actively evolving. When changing consensus or validation behavior, update the internal parity tracker and keep parity with the C++ reference daemon.

## Goals

- Full consensus parity with the C++ daemon.
- Faster header and block sync while keeping strict validation order.
- Modern storage with Fjall and explicit indexing.
- Clear and compatible RPC surface for existing Flux infrastructure.
- Strong observability (status logs, dashboard, RPC stats).

## Project layout

- `fluxd_rust/` - Rust workspace.
- `fluxd_rust/crates/`
  - `node` - main daemon (binary name `fluxd`).
  - `chainstate` - block/UTXO state, indexes, validation.
  - `consensus` - network params, upgrades, monetary rules.
  - `pow` - PoW validation and difficulty.
  - `pon` - PoN validation.
  - `fluxnode` - fluxnode state/indexing.
  - `script` - script classification and interpreter.
  - `primitives` - core types (blocks, txs, outpoints, address enc/dec).
  - `shielded` - sapling/sprout support.
  - `storage` - storage backends (Fjall, memory).
- `fluxd_rust/ROADMAP.md` - living parity and feature roadmap.
- `fluxd_rust/data/` - local chain data (do not commit).
- `fluxd_rust/target/` - build output (do not commit).

## Quick start (mainnet)

From `fluxd_rust/`:

```bash
cargo build -p fluxd --release
./target/release/fluxd \
  --network mainnet \
  --backend fjall \
  --data-dir ./data \
  --fetch-params \
  --dashboard-addr 127.0.0.1:8080
```

Notes:
- The first run will download shielded params into `~/.zcash-params` unless you override with `--params-dir`.
- Block data is stored in flatfiles under `./data/blocks` and metadata/indexes in `./data/db`.
- For faster local iteration, `--backend memory` uses an in-memory store (not persistent).

## RPC and dashboard

- RPC uses HTTP Basic Auth. A cookie is written to `data/rpc.cookie` if you do not pass `--rpc-user` and `--rpc-pass`.
- JSON-RPC and `/daemon` HTTP endpoints are both supported. See `fluxd_rust/docs/RPC.md`.
- Dashboard (optional): `--dashboard-addr 127.0.0.1:8080` serves `/` (UI), `/stats`, and `/healthz`.

Example (JSON-RPC):

```bash
curl -u "$(cat ./data/rpc.cookie)" \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"1.0","id":"curl","method":"getinfo","params":[]}' \
  http://127.0.0.1:16124/
```

Example (`/daemon`):

```bash
curl -u "$(cat ./data/rpc.cookie)" \
  http://127.0.0.1:16124/daemon/getblockcount
```

## Status logs

Every `--status-interval` seconds the daemon prints a compact status line:

- `headers` and `blocks` - best header and best block heights.
- `gap` - header height minus block height.
- `h/s` and `b/s` - headers per second and blocks per second.
- `dl_ms`, `ver_ms`, `db_ms` - download, verification, and commit timings.
- `hdr_*` - header request/validation/commit timings.
- `val_ms`, `script_ms`, `shield_ms` - block validation timing buckets.
- `utxo_ms`, `idx_ms`, `anchor_ms`, `flat_ms` - chainstate update timings.

## Development

From `fluxd_rust/`:

```bash
cargo build
cargo test
cargo fmt
cargo clippy -p fluxd --all-targets
```

Consensus-critical logic lives in `consensus`, `chainstate`, `script`, `pow`, `pon`, or `fluxnode`. Add regression vectors for consensus changes.

## Documentation

- `fluxd_rust/docs/README.md` - documentation index.
- `fluxd_rust/docs/QUICKSTART.md` - step-by-step setup and usage.
- `fluxd_rust/docs/CONFIGURATION.md` - CLI flags and tuning.
- `fluxd_rust/docs/RPC.md` - RPC interface and method reference.
- `fluxd_rust/docs/RPC_PARITY.md` - parity checklist vs C++ RPC surface.
- `fluxd_rust/docs/ARCHITECTURE.md` - internal design and data flow.
- `fluxd_rust/docs/INDEXES.md` - on-disk indexes and schema.
- `fluxd_rust/docs/OPERATIONS.md` - ops runbook and tasks.
- `fluxd_rust/docs/TELEMETRY.md` - metrics and performance debugging.
- `fluxd_rust/docs/TROUBLESHOOTING.md` - common issues and fixes.
