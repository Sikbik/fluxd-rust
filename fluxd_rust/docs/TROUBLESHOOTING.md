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
pkill -f fluxd
rm -rf ./data
./target/release/fluxd --network mainnet --backend fjall --data-dir ./data --fetch-params
```
