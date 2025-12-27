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

## Run

```bash
ssh <vps-user>@<vps-host> "nohup stdbuf -oL -eL <remote-repo-path>/target/release/fluxd \
  --network mainnet \
  --backend fjall \
  --data-dir <remote-data-dir> \
  --fetch-params \
  --dashboard-addr 0.0.0.0:8080 \
  --getdata-batch 128 \
  --block-peers 3 \
  --header-peers 4 \
  --inflight-per-peer 2 \
  --header-lead 20000 \
  --status-interval 15 \
  --db-cache-mb 256 \
  --db-write-buffer-mb 256 \
  --db-journal-mb 512 \
  --db-memtable-mb 32 \
  --header-verify-workers 6 \
  --verify-workers 4 \
  --shielded-workers 6 \
  > <remote-log-dir>/longrun-public.log 2>&1 &"
```

## Stop

```bash
ssh <vps-user>@<vps-host> "pkill -x fluxd"
```

## Logs and monitoring

- Log file: `<remote-log-dir>/longrun-public.log`
- Dashboard: `http://<host>:8080/` and `/healthz`

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
