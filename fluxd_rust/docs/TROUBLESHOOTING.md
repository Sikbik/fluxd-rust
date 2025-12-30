# Troubleshooting

Common issues seen during sync and operation.

## Headers stall or do not advance

Symptoms:
- `headers` stays flat in status logs.
- `header request timed out` in logs.

Checks:
- Ensure peers have up-to-date heights (`getpeerinfo`).
- Verify DNS seed resolution and connection counts.
- Check `--header-lead` (a very low value can cap header progress).
- Right after startup, allow time for peer discovery; early `header request timed out` messages can happen
  before the address book has enough responsive peers.

Mitigations:
- Increase `--header-peers` and `--header-verify-workers`.
- Increase `--inflight-per-peer` if requests are underutilized.
- Pin known-good header peers with `--header-peer`.
- Temporarily set `--header-lead 0` to remove the cap during initial bootstrap.

## Blocks are far behind headers

Symptoms:
- `gap` grows and block rate is low.

Checks:
- Watch `b/s` (blocks per second) and `ver_ms` / `db_ms` in status logs.
- Verify block peers (`getpeerinfo` and logs).

Mitigations:
- Increase `--block-peers` and `--getdata-batch`.
- Increase `--verify-workers` or `--verify-queue`.
- Ensure storage is not I/O bound (monitor disk and CPU).

## Sync stalls at a specific height

Symptoms:
- Repeated errors around a fixed height or upgrade boundary.

Checks:
- Compare with the C++ reference daemon's consensus behavior.
- Verify the upgrade schedule and activation heights in the internal parity tracker.

Mitigations:
- Inspect the block/height in question via `getblock` or `getblockheader`.
- If the on-disk state may be inconsistent, perform a clean resync.

## Block connect mismatch / reorg fails with missing undo

Symptoms:
- Repeated `block connect mismatch ...; attempting reorg`
- `missing block undo entry; resync required`

Cause:
- The database was created before block undo support existed, so historical blocks
  do not have undo entries. Reorg requires undo data to safely disconnect blocks.

Fix:
- Stop the daemon, remove the data directory, and resync from scratch so undo entries
  are generated during block connect.

## Coinbase / fluxnode payout validation failures

Symptoms:
- `coinbase missing deterministic fluxnode payout`
- `coinbase missing dev fund remainder` (post-PoN)

Newer builds also print a one-line diagnostic with the failing height plus expected payouts and
coinbase outputs.

Likely causes:
- You are running an older `fluxd-rust` build with a deterministic fluxnode payee ordering mismatch
  (older builds could disagree on which fluxnode should be paid when heights tie).
- You are running an older `fluxd-rust` build with incorrect fluxnode confirm expiration handling
  across upgrade boundaries (nodes that expired pre-PoN could be incorrectly treated as eligible
  post-PoN, leading to a deterministic payee mismatch near PoN-era blocks).
- You are reusing a database created by an older `fluxd-rust` build that did not yet track
  fluxnode tier metadata and/or deterministic `last_paid_height` state.

Quick diagnosis:

```bash
./target/release/fluxd --network mainnet --backend fjall --data-dir ./data --scan-fluxnodes
```

If you have the failing height from logs, you can print the expected deterministic fluxnode payouts
at that height:

```bash
./target/release/fluxd --network mainnet --backend fjall --data-dir ./data --debug-fluxnode-payouts <height>
```

For deeper diagnostics on payee selection for a specific tier:

```bash
./target/release/fluxd --network mainnet --backend fjall --data-dir ./data --debug-fluxnode-payee-candidates <tier 1..3> <height>
```

If you see `Tier totals: cumulus=0 nimbus=0 stratus=0` or `last_paid_height range: 0..0` at high
chain heights, the DB is missing required fluxnode payout state.

Fix:
- If you're on an older build, upgrade and restart first (no resync needed if the DB was created by a
  recent build).
- If the DB is missing payout state, stop the daemon, remove the data directory, and resync from scratch
  so fluxnode tier/paid state is populated deterministically during block connect.

## RPC auth failures

Symptoms:
- HTTP 401 unauthorized.

Fix:
- Check `--data-dir/rpc.cookie` and use it for Basic Auth.
- If you set `--rpc-user`/`--rpc-pass`, ensure your client matches those.

## getblockhashes returns empty

Symptoms:
- `getblockhashes` returns an empty list during a fresh run.

Reason:
- The timestamp index is populated on block connect; it will be empty
  until blocks are indexed.

Fix:
- Wait for blocks to index, or resync from scratch if the index was
  introduced after the existing DB was created.

## Sync stalls / Fjall write throttling

Symptoms:
- Heights stop moving for long periods while the process stays alive.
- Log may include: `Warning: Fjall write_batch commit took ...ms ...`
- Log may include: `Warning: Fjall journal pressure ...`

Cause:
- Fjall is throttling writes due to flush/compaction backpressure (similar to an L0 stall).
  This is most common when running with too-small `--db-*` settings for the current indexing load.
  A second common cause is hitting the Fjall journal / write-buffer limits, which can halt writes until
  background flushes catch up.

Checks:
- Query `/stats` and compare:
  - `db_write_buffer_bytes` vs `db_max_write_buffer_bytes`
  - `db_journal_disk_space_bytes` vs `db_max_journal_bytes`
- If the DB is at/near its max journal size, flush + journal GC may need time to catch up.

Fix:
- Prefer running with no explicit `--db-*` flags first (the daemon auto-tunes/clamps the most dangerous
  combinations), then only override if needed.
- If you do override:
  - `--db-write-buffer-mb` should be at least `partitions × --db-memtable-mb`.
  - `--db-journal-mb` should be at least `2 × partitions × --db-memtable-mb`.
  - If you see sustained journal pressure, increase `--db-journal-mb`, reduce `--db-memtable-mb`, and/or
    increase `--db-flush-workers`.

A known-good mainnet sync configuration is:

```bash
./target/release/fluxd --network mainnet --backend fjall --data-dir ./data --fetch-params \
  --db-cache-mb 256 --db-write-buffer-mb 4096 --db-journal-mb 16384 --db-memtable-mb 128 \
  --db-flush-workers 4 --db-compaction-workers 6
```

## Sync stalls with peers connected (no network progress)

Symptoms:
- `headers`/`blocks` stop increasing and `h/s` + `b/s` stay at `0.00`.
- `getnettotals` stops changing (no bytes in/out).
- `getpeerinfo` shows block/header peers with stale `lastrecv`/`lastsend` while relay peers may remain active.

Checks:
- Confirm peers are still connected (`getconnectioncount`, `getpeerinfo`).
- If you have shell access, inspect socket state (Linux):

```bash
ss -tnp | grep fluxd
```

Mitigations:
- Restart the daemon (it will reconnect peers and resume sync).
- Ensure you are running a recent build: newer versions include bounded P2P send/handshake timeouts and a
  block verify/connect pipeline watchdog to prevent indefinite wedges.

## Memory pressure or OOM

Symptoms:
- Process killed or very slow sync.

Mitigations:
- Reduce Fjall memory usage via `--db-cache-mb`, `--db-write-buffer-mb`,
  and `--db-memtable-mb`.
- Reduce worker counts for validation.

## Dashboard not reachable

Symptoms:
- Cannot load `http://host:8080/`.

Fix:
- Ensure `--dashboard-addr` is set and bound to an accessible interface.
- Check firewall rules.

## Need a clean resync

If you made consensus changes or indexes were added after initial sync,
clean resync is often the fastest way to restore correctness.

```bash
pkill -x fluxd
rm -rf ./data
./target/release/fluxd --network mainnet --backend fjall --data-dir ./data --fetch-params
```

Also ensure there is only one `fluxd` process writing to a given `--data-dir`.
