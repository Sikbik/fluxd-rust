# Quickstart

This guide covers a clean local run of the Rust daemon and basic sanity checks.

## Prereqs

- Rust toolchain (stable).
- Disk space for chain data.
- Network access to Flux peers.

## Build

From `fluxd_rust/`:

```bash
cargo build -p fluxd --release
```

## First run (mainnet)

```bash
./target/release/fluxd \
  --network mainnet \
  --backend fjall \
  --data-dir ./data \
  --fetch-params \
  --dashboard-addr 127.0.0.1:8080
```

What this does:
- Creates `./data/db` (Fjall) and `./data/blocks` (flatfiles).
- Downloads shielded parameters to `~/.zcash-params` unless overridden.
- Starts RPC on `127.0.0.1:16124` by default.
- Starts a local dashboard at `http://127.0.0.1:8080/`.

## RPC authentication

If you do not set `--rpc-user` and `--rpc-pass`, a cookie is generated:

```bash
cat ./data/rpc.cookie
# __cookie__:hexpass
```

Use it directly with curl:

```bash
curl -u "$(cat ./data/rpc.cookie)" http://127.0.0.1:16124/daemon/getinfo
```

## JSON-RPC example

```bash
curl -u "$(cat ./data/rpc.cookie)" \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"1.0","id":"curl","method":"getblockcount","params":[]}' \
  http://127.0.0.1:16124/
```

## /daemon example

```bash
curl -u "$(cat ./data/rpc.cookie)" \
  http://127.0.0.1:16124/daemon/getblockheader?params=["<blockhash>",true]
```

Note: `/daemon` expects parameters as a JSON array via the `params` query key. See `RPC.md` for details.

## Dashboard

- `GET /` - HTML dashboard
- `GET /stats` - JSON stats
- `GET /healthz` - simple liveness probe

Example:

```bash
curl http://127.0.0.1:8080/healthz
```

## Quick sanity checks

- `getblockcount` and `getblockchaininfo` should increase over time.
- Status logs should show increasing headers and blocks.
- `getpeerinfo` should show active peers.

## Clean resync

Stop the daemon, delete the data dir, and restart:

```bash
pkill -f fluxd
rm -rf ./data
./target/release/fluxd --network mainnet --backend fjall --data-dir ./data --fetch-params
```

## Testnet or regtest

```bash
./target/release/fluxd --network testnet --data-dir ./data-testnet
```

RPC defaults to port `26124` for testnet/regtest.
