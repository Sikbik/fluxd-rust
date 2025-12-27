# Architecture overview

This document summarizes the high-level structure of the fluxd-rust daemon and how data
flows from the P2P layer into chainstate.

## Process layout

The fluxd-rust daemon runs a single process with multiple async tasks and worker threads.
Key subsystems:

- P2P networking (header and block peers).
- Header sync pipeline.
- Block download and validation pipeline.
- Chainstate storage and indexing.
- RPC server and dashboard.

## Crate map

- `node` - top-level daemon, CLI, networking, RPC, dashboard.
- `chainstate` - consensus validation and state updates.
- `consensus` - network parameters, upgrades, monetary rules.
- `pow` - PoW difficulty and header validation.
- `pon` - PoN header and signature validation.
- `fluxnode` - fluxnode indexing and cache.
- `script` - script interpreter and classification.
- `primitives` - core types and encoding.
- `shielded` - sprout/sapling structures and verification.
- `storage` - key-value store backends.

## Data directories

- `--data-dir/db` - Fjall keyspace for indexes and metadata.
- `--data-dir/blocks` - flatfiles for raw blocks.
- `--data-dir/rpc.cookie` - RPC auth cookie (if auto-generated).

Flatfiles are named `data00000.dat`, `data00001.dat`, etc and store blocks
with a 4-byte length prefix per block. Locations are tracked in the block index.

## Chainstate and indexes

The chainstate layer owns consensus checks and indexes. Key responsibilities:

- Insert and validate headers, track best header and chainwork.
- Connect blocks in strict order and update UTXO set.
- Maintain indexes for headers, blocks, txs, address outpoints, fluxnodes,
  and timestamps.
- Verify scripts (parallelizable) and shielded transitions.

See `INDEXES.md` for schema details.

## Header sync

The header sync pipeline:

1. Connect to header peers.
2. Request header batches.
3. Validate PoW or PoN rules and difficulty.
4. Insert header entries into the header index.
5. Track best header (most chainwork).

Header verification uses `--header-verify-workers` threads. The target header
lead over blocks is controlled by `--header-lead`.

## Block download and validation

Blocks are downloaded in parallel, but validated in strict height order:

1. Build a download plan from missing blocks.
2. Request block batches from multiple block peers.
3. Pre-validate blocks in worker threads.
4. Perform shielded validation in separate workers.
5. Connect blocks sequentially and commit a write batch.

Block connect runs on blocking threads so the async runtime can keep serving RPC
and dashboard requests during high-throughput sync.

Worker counts and queue depths are controlled via:
- `--verify-workers` / `--verify-queue`
- `--shielded-workers` / `--shielded-queue`

## Reorg handling

fluxd-rust tracks two tips:

- **best header**: the most-chainwork header chain (headers-only view).
- **best block**: the fully connected chainstate tip (indexed blocks).

During sync, the daemon expects the best block chain to follow the best header chain.
If the best header chain diverges (e.g. the node indexed blocks on a lower-work fork),
the daemon disconnects blocks back to the common ancestor and then downloads/connects
the blocks needed to reach the best header tip.

Block disconnect is powered by per-block undo data stored in the database
(`block_undo`). Undo entries are generated on connect and pruned to a bounded
reorg depth. If you introduce undo/reorg support after an existing sync, a clean
resync is required to populate undo entries for historical blocks.

## Consensus rules

Consensus rules are enforced in `chainstate`, `pow`, and `pon` crates.
The C++ reference daemon is the reference for parity, and changes must be tracked
in the internal parity tracker.

Key consensus behaviors:

- Header checks for difficulty, timestamps, and upgrades.
- PoN headers require operator signature validation.
- Block checks include coinbase funding rules and shielded tree validation.
- UTXO set and address index updates are part of block connect.

## Observability

- RPC server: JSON-RPC and `/daemon` endpoints.
- Dashboard server: `/`, `/stats`, `/healthz`.
- Status logs with throughput and timing metrics.

## Storage backends

- Fjall (default): persistent key-value store with configurable caching.
- Memory: in-memory store for tests and short-lived runs.

The storage backend is selected via `--backend` and affects all indexes.
