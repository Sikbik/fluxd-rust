# Fast sync / snapshot evaluation (WIP)

This document outlines options for speeding up initial sync for `fluxd-rust` beyond “download all blocks and index from genesis”.

## Goals

- Reduce time-to-tip for new nodes/indexers.
- Keep correctness and reorg safety equivalent to full validation by default.
- Make faster paths explicit/opt-in, with clear trust and verification boundaries.
- Avoid coupling to a single hardcoded host; support multiple snapshot sources.

## Constraints specific to Flux

- Flux is Zcash-derived: shielded pools introduce additional consensus state (anchors, nullifiers, note commitments).
- The chain transitions from PoW to PoN at height `2,020,000`; header and block validation rules differ across eras.
- `fluxd-rust` persists consensus + index state in Fjall partitions under `db/` and raw blocks/undo in flatfiles under `blocks/`.

Any snapshot strategy must define how much of the following state is trusted vs revalidated:

- Header chain (best header + chainwork/PoN work rules).
- Block data (flatfiles).
- UTXO set.
- Shielded state (Sprout/Sapling anchors, nullifiers).
- Secondary indexes (txindex, spentindex, address deltas/outpoints, block deltas, etc).
- Fluxnode state (deterministic list / PoN producer set).

## Options

### Option A: “Full data-dir snapshot” (fastest to ship)

Ship a compressed archive containing:

- `db/` (Fjall partitions)
- `blocks/` (flatfiles + undo)
- optional: `peers.dat`, `banlist.dat` (non-consensus; can be omitted)

**Pros**
- Extremely fast time-to-tip for users (limited by download + decompress).
- Lowest implementation effort (mostly tooling + integrity checks + docs).
- Works for any chain height (including post-PoN) if produced by a trusted full node.

**Cons**
- Trust-heavy: you are trusting the snapshot producer’s full validation.
- Large artifacts (bandwidth + storage costs).
- Requires careful schema/version matching (`db_schema_version`, index versions, wallet format).

**Minimum recommended validation**
- Verify the snapshot declares:
  - network (`mainnet`/`testnet`)
  - best block hash + height
  - expected `db_schema_version` and secondary index versions
- Verify locally:
  - `best_block_hash` in DB matches the snapshot manifest
  - flatfiles contain the best block and decode cleanly
  - header chain links to the tip (spot-check or full header scan)

**Operational note**
This is suitable as an opt-in “bootstrap” for indexers and infrastructure that already trusts a known snapshot source.

### Option B: “Headers-first + state snapshot” (assumeutxo-style)

Verify headers to a checkpoint height, then import a state snapshot of:

- UTXO set
- shielded anchors + nullifiers
- minimal chain metadata needed to continue validation

Then continue full block validation from that height forward, and optionally backfill historical indexes in the background.

**Pros**
- Much less trust than Option A if the snapshot can be validated against a committed digest at a known height.
- Much smaller than full `blocks/` + `db/`.
- Enables faster “node comes online” experience (serve RPCs earlier).

**Cons**
- Requires designing and committing to a digest scheme:
  - UTXO set hash
  - shielded state digest (anchors + nullifiers)
  - fluxnode/PoN state digest
- Requires more code and careful migration/versioning.

**Implementation sketch**
- Define snapshot manifest:
  - height, best block hash at height
  - digests for UTXO/shielded/fluxnode state
  - snapshot format version + compression
- Bake known-good digests into the binary for a few heights (like checkpoints).
- Allow importing the snapshot only if the local headers match the committed checkpoint hash and the digests validate.

### Option C: “Pruned historical reindex” (serve tip fast, backfill later)

Stay fully validating, but split work into two phases:

1. Reach tip quickly with minimal indexes enabled (keep only consensus DB + minimal RPC).
2. Backfill secondary indexes (txindex/spent/address deltas) in the background up to tip.

**Pros**
- No trust change; still full validation.
- Improves perceived readiness (node can serve core RPCs earlier).

**Cons**
- Requires clearly separating “consensus DB” from “secondary indexes” and gating RPCs that depend on them.
- Requires durable background reindex jobs and progress reporting.

## Recommendation

Near-term (ship quickly):
- Implement Option A with strong version + integrity checks and clear UX (“this is a trusted bootstrap”).

Medium-term (reduce trust):
- Add the primitives needed for Option B (state digests at checkpoint heights).

Always useful:
- Continue improving Option C by making index rebuilds incremental and resumable.

## Next steps (engineering backlog)

- Define a snapshot manifest format + versioning.
- Add CLI flags:
  - `--snapshot <path|url>` (optional)
  - `--snapshot-verify <strict|basic|none>` (default: strict for local files, basic for remote)
- Add a small “snapshot tool” that produces snapshots + manifests from a fully-synced node.
- Decide which RPCs require which indexes, and expose “index progress” in `/stats` and `getblockchaininfo`.
