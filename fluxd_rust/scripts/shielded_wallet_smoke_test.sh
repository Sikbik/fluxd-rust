#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: shielded_wallet_smoke_test.sh [options]

Starts a short-lived fluxd instance (separate data dir + RPC port) and exercises
Sapling wallet RPCs:
  - zgetnewaddress (Sapling)
  - zvalidateaddress (ismine=true)
  - zexportviewingkey / zimportviewingkey (Sapling viewing keys, watch-only)
  - persistence across restart (same data dir)

Options:
  --bin PATH        Path to fluxd binary (default: ../target/release/fluxd)
  --network NAME    mainnet|testnet|regtest (default: regtest)
  --profile NAME    low|default|high (default: low)
  --rpc-port PORT   RPC port to bind on 127.0.0.1 (default: 16135)
  --params-dir PATH Shielded params dir (default: ~/.zcash-params)
  --timeout-secs N  Max seconds to wait for RPC readiness (default: 20)
  --keep            Do not delete data dir/log on exit (for debugging)
  -h, --help        Show this help

Examples:
  ./scripts/shielded_wallet_smoke_test.sh
  ./scripts/shielded_wallet_smoke_test.sh --network testnet
USAGE
}

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT_DIR/target/release/fluxd"
NETWORK="regtest"
PROFILE="low"
RPC_PORT="16135"
PARAMS_DIR="${HOME}/.zcash-params"
TIMEOUT_SECS="20"
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

DATA_DIR="$(mktemp -d "/tmp/fluxd-shielded-wallet.XXXXXX")"
LOG_PATH="${DATA_DIR}.log"
: >"$LOG_PATH"

cleanup() {
  local exit_code=$?
  if [[ -n "${PID:-}" ]]; then
    kill "$PID" >/dev/null 2>&1 || true
    wait "$PID" >/dev/null 2>&1 || true
  fi

  if [[ "$exit_code" -ne 0 ]]; then
    echo "---- tail log ($LOG_PATH) ----" >&2
    tail -n 120 "$LOG_PATH" >&2 || true
  fi

  if [[ "$KEEP" != "1" ]]; then
    rm -rf "$DATA_DIR" "$LOG_PATH" || true
  else
    echo "Kept data dir: $DATA_DIR" >&2
    echo "Kept log: $LOG_PATH" >&2
  fi
}
trap cleanup EXIT

COOKIE=""
PID=""

rpc_get() {
  local method="$1"
  curl -g -sS -u "$COOKIE" "http://127.0.0.1:${RPC_PORT}/daemon/${method}"
}

url_encode() {
  python3 -c 'import urllib.parse,sys; print(urllib.parse.quote(sys.argv[1]))' "$1"
}

start_node() {
  nohup "$BIN" \
    --network "$NETWORK" \
    --backend fjall \
    --data-dir "$DATA_DIR" \
    --params-dir "$PARAMS_DIR" \
    --profile "$PROFILE" \
    --rpc-addr "127.0.0.1:${RPC_PORT}" \
    --status-interval 5 \
    >>"$LOG_PATH" 2>&1 &
  PID=$!

  for _ in $(seq 1 80); do
    [[ -f "${DATA_DIR}/rpc.cookie" ]] && break
    sleep 0.25
  done
  if [[ ! -f "${DATA_DIR}/rpc.cookie" ]]; then
    echo "rpc.cookie not created (RPC failed to start?)" >&2
    exit 1
  fi

  COOKIE="$(cat "${DATA_DIR}/rpc.cookie")"

  local ready=0
  local start_ts
  start_ts="$(date +%s)"
  while true; do
    if rpc_get "getinfo" >/dev/null 2>&1; then
      ready=1
      break
    fi
    local now_ts
    now_ts="$(date +%s)"
    if [[ $((now_ts - start_ts)) -ge "$TIMEOUT_SECS" ]]; then
      break
    fi
    sleep 0.25
  done
  if [[ "$ready" != "1" ]]; then
    echo "RPC did not become reachable on 127.0.0.1:${RPC_PORT} within ${TIMEOUT_SECS}s" >&2
    exit 1
  fi
}

stop_node() {
  if [[ -n "${PID:-}" ]]; then
    kill "$PID" >/dev/null 2>&1 || true
    wait "$PID" >/dev/null 2>&1 || true
    PID=""
  fi
}

expected_hrp=""
expected_sk_hrp=""
expected_vk_hrp=""
case "$NETWORK" in
  mainnet)
    expected_hrp="za"
    expected_sk_hrp="secret-extended-key-main"
    expected_vk_hrp="zviewa"
    ;;
  testnet)
    expected_hrp="ztestacadia"
    expected_sk_hrp="secret-extended-key-test"
    expected_vk_hrp="zviewtestacadia"
    ;;
  regtest)
    expected_hrp="zregtestsapling"
    expected_sk_hrp="secret-extended-key-regtest"
    expected_vk_hrp="zviewregtestsapling"
    ;;
  *)
    echo "Unknown network: $NETWORK" >&2
    exit 2
    ;;
esac

echo "Starting node (network=$NETWORK profile=$PROFILE rpc=127.0.0.1:${RPC_PORT})"
start_node

echo "Checking zgetnewaddress (sapling) ..."
zaddr1="$(rpc_get "zgetnewaddress" | python3 -c 'import json,sys; obj=json.load(sys.stdin); print(obj.get("result",""))')"
if [[ -z "$zaddr1" ]]; then
  echo "zgetnewaddress returned empty result" >&2
  exit 1
fi
if [[ "$zaddr1" != "${expected_hrp}1"* ]]; then
  echo "unexpected address prefix: got '$zaddr1', expected '${expected_hrp}1...'" >&2
  exit 1
fi

if [[ ! -f "${DATA_DIR}/wallet.dat" ]]; then
  echo "wallet.dat not created in data dir" >&2
  exit 1
fi

echo "Checking zvalidateaddress (ismine) ..."
zaddr1_enc="$(url_encode "$zaddr1")"
rpc_get "zvalidateaddress?zaddr=${zaddr1_enc}" | python3 -c 'import json,sys; addr=sys.argv[1]; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; assert res.get("isvalid") is True, res; assert res.get("type")=="sapling", res; assert res.get("address")==addr, res; assert res.get("ismine") is True, res; assert res.get("iswatchonly") is False, res' "$zaddr1"

echo "Checking zgetbalance returns 0.0 ..."
rpc_get "zgetbalance?zaddr=${zaddr1_enc}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; res=obj.get("result"); assert isinstance(res,(int,float)), res; assert abs(float(res) - 0.0) < 1e-12, res'

echo "Checking zgettotalbalance returns 0.0 totals ..."
rpc_get "zgettotalbalance" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; res=obj.get("result") or {}; assert isinstance(res, dict), res; keys=("transparent","private","total"); assert all(k in res for k in keys), res; assert all(isinstance(res[k],(int,float)) for k in keys), res; assert all(abs(float(res[k]) - 0.0) < 1e-12 for k in keys), res'

echo "Checking zlistunspent returns empty list ..."
rpc_get "zlistunspent" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; res=obj.get("result") or []; assert isinstance(res, list), res; assert len(res) == 0, res'

echo "Checking zexportkey returns a Sapling spending key (no output) ..."
zkey1="$(rpc_get "zexportkey?zaddr=${zaddr1_enc}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); print(obj.get("result",""))')"
if [[ -z "$zkey1" || "$zkey1" == "null" ]]; then
  echo "zexportkey returned empty result" >&2
  exit 1
fi
if [[ "$zkey1" != "${expected_sk_hrp}1"* ]]; then
  echo "unexpected spending key prefix (expected ${expected_sk_hrp}1...)" >&2
  exit 1
fi

echo "Checking zexportviewingkey returns a Sapling viewing key ..."
vkey1="$(rpc_get "zexportviewingkey?zaddr=${zaddr1_enc}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); print(obj.get("result",""))')"
if [[ -z "$vkey1" || "$vkey1" == "null" ]]; then
  echo "zexportviewingkey returned empty result" >&2
  exit 1
fi
if [[ "$vkey1" != "${expected_vk_hrp}1"* ]]; then
  echo "unexpected viewing key prefix (expected ${expected_vk_hrp}1...)" >&2
  exit 1
fi

echo "Checking zlistaddresses contains zaddr1 ..."
rpc_get "zlistaddresses" | python3 -c 'import json,sys; addr=sys.argv[1]; obj=json.load(sys.stdin); res=obj.get("result", []) or []; assert isinstance(res, list), res; assert addr in res, res' "$zaddr1"

echo "Restarting node to confirm persistence ..."
stop_node
sleep 0.2
start_node

echo "Re-checking zvalidateaddress (ismine persists) ..."
rpc_get "zvalidateaddress?zaddr=${zaddr1_enc}" | python3 -c 'import json,sys; addr=sys.argv[1]; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; assert res.get("isvalid") is True, res; assert res.get("type")=="sapling", res; assert res.get("address")==addr, res; assert res.get("ismine") is True, res; assert res.get("iswatchonly") is False, res' "$zaddr1"

echo "Checking zgetnewaddress returns a new address ..."
zaddr2="$(rpc_get "zgetnewaddress" | python3 -c 'import json,sys; obj=json.load(sys.stdin); print(obj.get("result",""))')"
if [[ -z "$zaddr2" ]]; then
  echo "zgetnewaddress returned empty result (second call)" >&2
  exit 1
fi
if [[ "$zaddr2" == "$zaddr1" ]]; then
  echo "expected a new Sapling address, but got the same address twice" >&2
  exit 1
fi
if [[ "$zaddr2" != "${expected_hrp}1"* ]]; then
  echo "unexpected address prefix: got '$zaddr2', expected '${expected_hrp}1...'" >&2
  exit 1
fi

echo "Checking zlistaddresses contains both addresses ..."
rpc_get "zlistaddresses" | python3 -c 'import json,sys; a1=sys.argv[1]; a2=sys.argv[2]; obj=json.load(sys.stdin); res=obj.get("result", []) or []; assert isinstance(res, list), res; assert a1 in res, res; assert a2 in res, res' "$zaddr1" "$zaddr2"

echo "Importing viewing key into a fresh wallet (watch-only) ..."
stop_node
if [[ "$KEEP" != "1" ]]; then
  rm -rf "$DATA_DIR" "$LOG_PATH" || true
fi
DATA_DIR="$(mktemp -d "/tmp/fluxd-shielded-wallet-vk-import.XXXXXX")"
LOG_PATH="${DATA_DIR}.log"
: >"$LOG_PATH"
start_node

vkey1_enc="$(url_encode "$vkey1")"
rpc_get "zimportviewingkey?vkey=${vkey1_enc}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj'

echo "Checking zlistaddresses excludes watch-only by default ..."
rpc_get "zlistaddresses" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", []) or []; assert isinstance(res, list), res; assert len(res) == 0, res'

echo "Checking zlistaddresses(includeWatchonly=true) includes zaddr1 ..."
rpc_get "zlistaddresses?params=[true]" | python3 -c 'import json,sys; addr=sys.argv[1]; obj=json.load(sys.stdin); res=obj.get("result", []) or []; assert isinstance(res, list), res; assert addr in res, res' "$zaddr1"

echo "Checking zvalidateaddress ismine=false for watch-only wallet ..."
rpc_get "zvalidateaddress?zaddr=${zaddr1_enc}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; assert res.get("isvalid") is True, res; assert res.get("type")=="sapling", res; assert res.get("ismine") is False, res; assert res.get("iswatchonly") is True, res'

echo "Checking zgetbalance errors without includeWatchonly on watch-only wallet ..."
rpc_get "zgetbalance?zaddr=${zaddr1_enc}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); err=obj.get("error") or {}; assert err.get("code")==-4, obj'

echo "Checking zgetbalance succeeds with includeWatchonly=true on watch-only wallet ..."
rpc_get "zgetbalance?zaddr=${zaddr1_enc}&minconf=1&includeWatchonly=true" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; res=obj.get("result"); assert isinstance(res,(int,float)), res; assert abs(float(res) - 0.0) < 1e-12, res'

echo "Importing zkey into a fresh wallet (no output) ..."
stop_node
if [[ "$KEEP" != "1" ]]; then
  rm -rf "$DATA_DIR" "$LOG_PATH" || true
fi
DATA_DIR="$(mktemp -d "/tmp/fluxd-shielded-wallet-import.XXXXXX")"
LOG_PATH="${DATA_DIR}.log"
: >"$LOG_PATH"
start_node

import_file="${DATA_DIR}/zimportwallet.txt"
printf '%s\n' "$zkey1" >"$import_file"
import_file_enc="$(url_encode "$import_file")"
rpc_get "zimportwallet?filename=${import_file_enc}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj'
rm -f "$import_file" || true

import_addr="$(rpc_get "zgetnewaddress" | python3 -c 'import json,sys; obj=json.load(sys.stdin); print(obj.get("result",""))')"
if [[ "$import_addr" != "$zaddr1" ]]; then
  echo "imported key derived unexpected first address" >&2
  exit 1
fi

echo "OK: zaddr1=$zaddr1"
echo "OK: zaddr2=$zaddr2"
