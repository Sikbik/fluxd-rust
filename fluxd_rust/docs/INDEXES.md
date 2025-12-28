# Chainstate indexes

fluxd-rust maintains several on-disk indexes to support fast lookups and RPCs.
All indexes are stored in the selected backend (Fjall or memory). The columns
below correspond to `fluxd_storage::Column`.

## HeaderIndex

- Key: `block_hash` (32 bytes)
- Value: `HeaderEntry` encoded
  - `prev_hash` (32 bytes, LE)
  - `height` (i32)
  - `time` (u32)
  - `bits` (u32)
  - `chainwork` (32 bytes)
  - `status` (has header / has block)

Used to:
- Track header chain, timestamps, and difficulty.
- Compute chainwork and best header selection.

## HeightIndex

- Key: `height` (4 bytes LE)
- Value: `block_hash` (32 bytes)

Maps main chain heights to block hashes.

## BlockIndex

- Key: `block_hash` (32 bytes)
- Value: `FileLocation` (16 bytes)
  - `file_id` (u32 LE)
  - `offset` (u64 LE)
  - `len` (u32 LE)

Points into `blocks/dataNNNNN.dat` flatfiles.

## BlockHeader

- Key: `block_hash` (32 bytes)
- Value: raw block header (consensus encoding)

Written when a header is accepted (header sync) and when a block is connected.
Used by `getblockheader` and header validation caches.
Note: Pre-PoN PoW headers include the Equihash solution (~1.3KB), so this column can contribute a few GB on mainnet.

## TxIndex

- Key: `txid` (32 bytes)
- Value: `TxLocation` (block location + tx index)

Allows `getrawtransaction` lookups without scanning blocks.

## Utxo

- Key: `OutPoint` (txid + vout)
- Value: `UtxoEntry` (value, script_pubkey, height, is_coinbase)

This is the authoritative UTXO set.

## AddressOutpoint

- Key: `sha256(script_pubkey)` (32 bytes) + `OutPoint` (36 bytes)
- Value: empty

This is a script-based address index. It is used for address-level queries and
is always maintained in the Rust daemon.

## Fluxnode

- Key: collateral `OutPoint` (36 bytes)
- Value: `FluxnodeRecord` encoded

Populated by `apply_fluxnode_tx` and used by fluxnode RPCs.

## FluxnodeKey

- Key: `KeyId` bytes
- Value: raw key bytes

Maps stored key ids to actual key material for fluxnode records.

## TimestampIndex

- Key: `logical_timestamp` (4 bytes BE) + `block_hash` (32 bytes)
- Value: empty

This supports `getblockhashes` by time range. Logical timestamps are monotonic:
if a block time is less than or equal to the previous block logical time, it is
bumped to `prev + 1`.

## BlockTimestamp

- Key: `block_hash` (32 bytes)
- Value: `logical_timestamp` (4 bytes BE)

Allows reverse lookup from block hash to logical timestamp.

## BlockUndo

- Key: `block_hash` (32 bytes)
- Value: `BlockUndo` encoded (versioned)
  - previous sprout tree bytes
  - previous sapling tree bytes
  - spent UTXO entries (per input, in connect order)
  - fluxnode record snapshots (per fluxnode tx, in connect order)

This enables fast, correct block disconnect during reorgs by restoring the
pre-block chainstate (UTXO set, txindex, shielded trees, and fluxnode records).

Undo entries are pruned as the chain advances to retain only the most recent
`max_reorg_depth(height)` blocks (plus the current tip), based on consensus rules.

## Anchor and Nullifier sets

- `AnchorSprout` - serialized Sprout tree frontiers (required for JoinSplit anchor chaining).
- `AnchorSapling` - Sapling anchor roots (keys only; values are empty) for existence checks.
- `NullifierSprout`, `NullifierSapling` - spent nullifier sets.

## Meta

- `best_header` / `best_block` hashes
- sprout/sapling tree bytes (used to resume shielded state; updated only when the tree changes)
- UTXO set stats (`utxo_stats_v1`) for `gettxoutsetinfo` (txouts + total_amount)
- Shielded value pools (`value_pools_v1`) for Sprout/Sapling chain supply tracking

## Index lifecycle

Indexes are maintained during block connect. There are no runtime flags to
disable txindex, address index, or timestamp index in the Rust daemon yet.
For a full rebuild of indexes, perform a fresh sync by clearing the data dir.

Note: `BlockUndo` is also generated during block connect. If undo/reorg support
was introduced after an existing database was created, a clean resync is required.
