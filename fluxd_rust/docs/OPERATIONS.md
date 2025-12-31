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
There is no cross-process locking for the database / flatfiles, so concurrent writers can corrupt
or stall the node.

If you need a lower-resource run (or are debugging OOM issues), use `--profile low` or override the
individual `--db-*` / worker flags explicitly.

## Data dir notes

The daemon writes a few non-db helper files into `--data-dir`:

- `rpc.cookie` - JSON-RPC auth cookie when not using `--rpc-user`/`--rpc-pass`.
- `peers.dat` - cached peer addresses learned from the network (used to reduce DNS seed reliance).
- `banlist.dat` - cached peer bans (temporary).

## Stop

```bash
ssh <vps-user>@<vps-host> "pkill -x fluxd"
```

Or via RPC (requires Basic Auth):

```bash
ssh <vps-user>@<vps-host> "curl -u \"$(cat <remote-data-dir>/rpc.cookie)\" http://127.0.0.1:16124/daemon/stop"
```

## Logs and monitoring

- Log file: `<remote-log-dir>/longrun-public.log`
- Dashboard: `http://<host>:8080/` and `/healthz`

By default, per-request block download logs are disabled (to avoid log spam). To enable them:

```bash
export FLUXD_LOG_BLOCK_REQUESTS=1
```

Example:

```bash
ssh <vps-user>@<vps-host> "tail -f <remote-log-dir>/longrun-public.log"
```

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
