# Operations runbook

This document captures the standard VPS workflow for building, running, and
monitoring the fluxd-rust daemon.

## VPS details

- Host: `<vps-user>@<vps-host>` (see private ops notes)
- Rust repo path: `<remote-repo-path>`
- Build as: `dev` user

Replace placeholder values (e.g., `<remote-repo-path>`) with your environment paths.

## Sync and build

From your local machine (repo root):

```bash
rsync -az --exclude '.cargo' --exclude 'target' --exclude 'data*' --exclude 'logs' \
  <local-repo-path>/fluxd_rust/ \
  <vps-user>@<vps-host>:<remote-repo-path>/
```

Build on VPS:

```bash
ssh <vps-user>@<vps-host> "su - dev -c 'bash -lc \"cd <remote-repo-path> && /home/dev/.cargo/bin/cargo build -p fluxd --release\"'"
```

## Smoke test (recommended)

After building, run a short-lived smoke test instance (separate data dir + RPC port):

```bash
ssh <vps-user>@<vps-host> "su - dev -c 'bash -lc \"cd <remote-repo-path> && ./scripts/remote_smoke_test.sh --profile high\"'"
```

Use `--keep` to preserve the temporary data dir/log for debugging.
If peer discovery is slow on your VPS, seed the smoke test from an existing data dir:

```bash
ssh <vps-user>@<vps-host> "su - dev -c 'bash -lc \"cd <remote-repo-path> && ./scripts/remote_smoke_test.sh --profile high --seed-peers-from <remote-data-dir> --min-headers-advance 1\"'"
```

## P2P mempool probe (inbound tx relay)

To validate inbound tx relay on the P2P port (without needing RPC credentials), run:

```bash
ssh <vps-user>@<vps-host> "su - dev -c 'bash -lc \"cd <remote-repo-path> && ./scripts/p2p_mempool_probe.sh --addr 127.0.0.1:16125\"'"
```

Expected output includes an `inv_count=...` line and a `tx_payload_bytes=...` line.

## Shielded wallet smoke test (regtest/testnet)

To exercise the Sapling wallet RPCs (without relying on mainnet policy around t→z), run:

```bash
ssh <vps-user>@<vps-host> "su - dev -c 'bash -lc \"cd <remote-repo-path> && ./scripts/shielded_wallet_smoke_test.sh --network regtest\"'"
```

This script validates:
- `zgetnewaddress` returns a Sapling address for the selected network
- `zvalidateaddress` reports `ismine=true`
- the wallet persists across a restart (same data dir)

## Progress gate (stall detection)

When doing a fresh sync (and expecting steady progress), you can run a simple RPC-based
progress gate against the long-running instance:

```bash
ssh <vps-user>@<vps-host> "su - dev -c 'bash -lc \"cd <remote-repo-path> && ./scripts/progress_gate.sh --data-dir <remote-data-dir> --window-secs 120 --min-blocks-advance 1\"'"
```

This command exits non-zero if the node is behind the peer best height (or has a headers>blocks gap)
but fails to advance blocks during the observation window.

## Watchdog loop (progress + log patterns)

For a long-running sync test, you can loop the progress gate and also fail fast on severe log
messages:

```bash
ssh <vps-user>@<vps-host> "su - dev -c 'bash -lc \"cd <remote-repo-path> && ./scripts/longrun_watchdog.sh --data-dir <remote-data-dir> --log-file <remote-log-dir>/longrun-public.log --window-secs 120\"'"
```

Use `--fail-pattern` to add additional fatal regexes, and `--loops N` for a finite run.

## Run

```bash
ssh <vps-user>@<vps-host> "nohup stdbuf -oL -eL <remote-repo-path>/target/release/fluxd \
  --network mainnet \
  --backend fjall \
  --data-dir <remote-data-dir> \
  --fetch-params \
  --profile high \
  --dashboard-addr 0.0.0.0:8080 \
  > <remote-log-dir>/longrun-public.log 2>&1 &"
```

Note: do not run multiple `fluxd` instances pointing at the same `--data-dir` at the same time.
`fluxd` takes an exclusive lock on `--data-dir/.lock`; a second process using the same `--data-dir`
will exit with an error.

If you need a lower-resource run (or are debugging OOM issues), use `--profile low` or override the
individual `--db-*` / worker flags explicitly.

## Data dir notes

The daemon writes a few non-db helper files into `--data-dir`:

- `.lock` - prevents multiple `fluxd` processes from using the same data dir.
- `rpc.cookie` - JSON-RPC auth cookie when not using `--rpc-user`/`--rpc-pass`.
- `peers.dat` - cached peer addresses learned from the network (used to reduce DNS seed reliance).
- `banlist.dat` - cached peer bans (temporary).

## Stop

```bash
ssh <vps-user>@<vps-host> "pkill -x fluxd"
```

`pkill` sends SIGTERM; `fluxd` handles SIGTERM/CTRL-C and will shut down cleanly.

Or via RPC (requires Basic Auth):

```bash
ssh <vps-user>@<vps-host> "curl -u \"$(cat <remote-data-dir>/rpc.cookie)\" http://127.0.0.1:16124/daemon/stop"
```

## Logs and monitoring

- Log file: `<remote-log-dir>/longrun-public.log`
- Dashboard: `http://<host>:8080/` and `/healthz`

Logging controls:
- Default verbosity is `info`. Increase to `debug`/`trace` to see peer/headers details: `--log-level debug`.
- For structured logs, use `--log-format json` (one JSON object per line).

By default, per-request block download logs are disabled (to avoid log spam). To enable them:

```bash
export FLUXD_LOG_BLOCK_REQUESTS=1
```

Example:

```bash
ssh <vps-user>@<vps-host> "tail -f <remote-log-dir>/longrun-public.log"
```

## Reindex (keep blocks)

If `blocks/` is present but `db/` needs rebuilding (corruption, index changes), reindex from existing
flatfiles (no network download):

```bash
ssh <vps-user>@<vps-host> "pkill -x fluxd"
ssh <vps-user>@<vps-host> "<remote-fluxd-bin> --data-dir <remote-data-dir> --reindex"
```

For selective rebuilds (avoid touching other state), use:

```bash
ssh <vps-user>@<vps-host> "pkill -x fluxd"
ssh <vps-user>@<vps-host> "<remote-fluxd-bin> --data-dir <remote-data-dir> --reindex-txindex --reindex-spentindex --reindex-addressindex"
```

`--reindex-spentindex` uses `txindex` to populate satoshis/address metadata; include `--reindex-txindex` if txindex is missing or stale.

To wipe `blocks/` too (clean download + index), use `--resync` or remove `<remote-data-dir>`.

## Clean resync

```bash
ssh <vps-user>@<vps-host> "pkill -x fluxd"
ssh <vps-user>@<vps-host> "rm -rf <remote-data-dir> && mkdir -p <remote-data-dir>"
```

Then rebuild and run as usual.

## Database size

```bash
ssh <vps-user>@<vps-host> "du -sh <remote-data-dir>"
```

## RPC auth on VPS

```bash
ssh <vps-user>@<vps-host> "cat <remote-data-dir>/rpc.cookie"
```

Use the cookie with curl from a trusted host.

## Shielded proof tests

Shielded proof verification tests are marked `#[ignore]` because they require Sapling/Sprout params
and are CPU-heavy.

Run them locally from `fluxd_rust/`:

```bash
./scripts/run_shielded_tests.sh
```

Override the params location:

```bash
./scripts/run_shielded_tests.sh --params-dir /path/to/zcash-params
```
