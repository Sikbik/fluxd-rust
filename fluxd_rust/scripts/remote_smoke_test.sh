#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: remote_smoke_test.sh [options]

Runs a short-lived fluxd instance (separate data dir + RPC port), queries a few RPCs,
and confirms it can connect to peers and advance headers.

Options:
  --bin PATH           Path to fluxd binary (default: ../target/release/fluxd)
  --network NAME       mainnet|testnet|regtest (default: mainnet)
  --profile NAME       low|default|high (default: default)
  --rpc-port PORT      RPC port to bind on 127.0.0.1 (default: 16134)
  --params-dir PATH    Shielded params dir (default: ~/.zcash-params)
  --timeout-secs N     Max seconds to wait for peers+headers (default: 60)
  --require-headers    Fail if headers do not advance beyond genesis within timeout
  --keep               Do not delete data dir/log on exit (for debugging)
  -h, --help           Show this help

USAGE
}

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT_DIR/target/release/fluxd"
NETWORK="mainnet"
PROFILE="default"
RPC_PORT="16134"
PARAMS_DIR="${HOME}/.zcash-params"
TIMEOUT_SECS="60"
REQUIRE_HEADERS="0"
KEEP="0"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin)
      BIN="${2:-}"
      shift 2
      ;;
    --network)
      NETWORK="${2:-}"
      shift 2
      ;;
    --profile)
      PROFILE="${2:-}"
      shift 2
      ;;
    --rpc-port)
      RPC_PORT="${2:-}"
      shift 2
      ;;
    --params-dir)
      PARAMS_DIR="${2:-}"
      shift 2
      ;;
    --timeout-secs)
      TIMEOUT_SECS="${2:-}"
      shift 2
      ;;
    --require-headers)
      REQUIRE_HEADERS="1"
      shift
      ;;
    --keep)
      KEEP="1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown arg: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "$BIN" || ! -x "$BIN" ]]; then
  echo "fluxd binary not found or not executable: $BIN" >&2
  echo "Build it first: cargo build -p fluxd --release" >&2
  exit 2
fi

DATA_DIR="$(mktemp -d "/tmp/fluxd-smoke.XXXXXX")"
LOG_PATH="${DATA_DIR}.log"

cleanup() {
  local exit_code=$?
  if [[ -n "${PID:-}" ]]; then
    kill "$PID" >/dev/null 2>&1 || true
    wait "$PID" >/dev/null 2>&1 || true
  fi

  if [[ "$exit_code" -ne 0 ]]; then
    echo "---- tail log ($LOG_PATH) ----" >&2
    tail -n 80 "$LOG_PATH" >&2 || true
  fi

  if [[ "$KEEP" != "1" ]]; then
    rm -rf "$DATA_DIR" "$LOG_PATH" || true
  else
    echo "Kept data dir: $DATA_DIR" >&2
    echo "Kept log: $LOG_PATH" >&2
  fi
}
trap cleanup EXIT

nohup "$BIN" \
  --network "$NETWORK" \
  --backend fjall \
  --data-dir "$DATA_DIR" \
  --params-dir "$PARAMS_DIR" \
  --profile "$PROFILE" \
  --rpc-addr "127.0.0.1:${RPC_PORT}" \
  --status-interval 5 \
  >"$LOG_PATH" 2>&1 &
PID=$!

for _ in $(seq 1 120); do
  [[ -f "${DATA_DIR}/rpc.cookie" ]] && break
  sleep 0.25
done

if [[ ! -f "${DATA_DIR}/rpc.cookie" ]]; then
  echo "rpc.cookie not created (RPC failed to start?)" >&2
  exit 1
fi

COOKIE="$(cat "${DATA_DIR}/rpc.cookie")"

rpc_get() {
  local method="$1"
  curl -sS --fail -u "$COOKIE" "http://127.0.0.1:${RPC_PORT}/daemon/${method}"
}

json_len() {
  python3 -c 'import json,sys; obj=json.load(sys.stdin); value=obj.get("result", []) or []; print(len(value) if isinstance(value, list) else 0)'
}

json_headers() {
  python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; headers=res.get("headers"); headers=res.get("best_header_height", 0) if headers is None else headers; print(int(headers or 0))'
}

echo "PID: $PID"
echo "Data dir: $DATA_DIR"
echo "RPC: 127.0.0.1:${RPC_PORT}"

rpc_ready=0
for _ in $(seq 1 120); do
  if rpc_get "getnetworkinfo" >/dev/null 2>&1; then
    rpc_ready=1
    break
  fi
  sleep 0.25
done
if [[ "$rpc_ready" != "1" ]]; then
  echo "RPC did not become reachable on 127.0.0.1:${RPC_PORT}" >&2
  exit 1
fi

rpc_get "getinfo" >/dev/null

start_ts=$(date +%s)
peers=0
headers=0
while true; do
  peers="$(rpc_get "getpeerinfo" | json_len)"
  headers="$(rpc_get "getblockchaininfo" | json_headers)"
  if [[ "$peers" -ge 1 && ( "$REQUIRE_HEADERS" != "1" || "$headers" -ge 1 ) ]]; then
    break
  fi
  now_ts=$(date +%s)
  if [[ $((now_ts - start_ts)) -ge "$TIMEOUT_SECS" ]]; then
    echo "Timed out waiting for peers/headers (peers=$peers headers=$headers)" >&2
    exit 1
  fi
  sleep 1
done

if [[ "$REQUIRE_HEADERS" != "1" && "$headers" -lt 1 ]]; then
  echo "OK (partial): peers=$peers headers=$headers (headers not required)"
else
  echo "OK: peers=$peers headers=$headers"
fi
echo "Sample getnetworkinfo:"
networkinfo="$(rpc_get "getnetworkinfo")"
echo "${networkinfo:0:1200}"
echo
echo "Sample getpeerinfo:"
peerinfo="$(rpc_get "getpeerinfo")"
echo "${peerinfo:0:1200}"
echo
