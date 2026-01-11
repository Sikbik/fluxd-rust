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
  --listen-p2p         Enable inbound P2P listener (default: disabled)
  --rpc-port PORT      RPC port to bind on 127.0.0.1 (default: 16134)
  --params-dir PATH    Shielded params dir (default: ~/.zcash-params)
  --seed-peers-from DIR  Copy peers.dat/banlist.dat from DIR into the temp data dir
  --timeout-secs N     Max seconds to wait for peers+headers (default: 60)
  --min-peers N        Fail if fewer than N peers are connected (default: 1)
  --min-headers-advance N  Require header height to increase by N (default: 0)
  --min-blocks-advance N   Require block height to increase by N (default: 0)
  --require-headers    Fail if headers do not advance beyond genesis within timeout
  --keep               Do not delete data dir/log on exit (for debugging)
  -h, --help           Show this help

USAGE
}

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT_DIR/target/release/fluxd"
NETWORK="mainnet"
PROFILE="default"
LISTEN_P2P="0"
RPC_PORT="16134"
PARAMS_DIR="${HOME}/.zcash-params"
SEED_PEERS_FROM=""
TIMEOUT_SECS="60"
REQUIRE_HEADERS="0"
MIN_PEERS="1"
MIN_HEADERS_ADVANCE="0"
MIN_BLOCKS_ADVANCE="0"
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
    --listen-p2p)
      LISTEN_P2P="1"
      shift
      ;;
    --rpc-port)
      RPC_PORT="${2:-}"
      shift 2
      ;;
    --params-dir)
      PARAMS_DIR="${2:-}"
      shift 2
      ;;
    --seed-peers-from)
      SEED_PEERS_FROM="${2:-}"
      shift 2
      ;;
    --timeout-secs)
      TIMEOUT_SECS="${2:-}"
      shift 2
      ;;
    --min-peers)
      MIN_PEERS="${2:-}"
      shift 2
      ;;
    --min-headers-advance)
      MIN_HEADERS_ADVANCE="${2:-}"
      shift 2
      ;;
    --min-blocks-advance)
      MIN_BLOCKS_ADVANCE="${2:-}"
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

if [[ -n "$SEED_PEERS_FROM" ]]; then
  if [[ ! -d "$SEED_PEERS_FROM" ]]; then
    echo "--seed-peers-from is not a directory: $SEED_PEERS_FROM" >&2
    exit 2
  fi
  if [[ -f "${SEED_PEERS_FROM}/peers.dat" ]]; then
    cp -f "${SEED_PEERS_FROM}/peers.dat" "${DATA_DIR}/peers.dat"
  fi
  if [[ -f "${SEED_PEERS_FROM}/banlist.dat" ]]; then
    cp -f "${SEED_PEERS_FROM}/banlist.dat" "${DATA_DIR}/banlist.dat"
  fi
fi

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

P2P_ARGS=()
if [[ "$LISTEN_P2P" != "1" ]]; then
  P2P_ARGS=(--no-p2p-listen)
fi

echo "Checking --db-integrity ..."
"$BIN" \
  --network "$NETWORK" \
  --backend fjall \
  --data-dir "$DATA_DIR" \
  --params-dir "$PARAMS_DIR" \
  --log-level error \
  --db-integrity \
  | python3 -c 'import json,sys; obj=json.load(sys.stdin); vc=obj.get("verifychain", {}) or {}; assert vc.get("ok") is True, vc; assert int(vc.get("checklevel", 0)) == 5, vc'

nohup "$BIN" \
  --network "$NETWORK" \
  --backend fjall \
  --data-dir "$DATA_DIR" \
  --params-dir "$PARAMS_DIR" \
  --profile "$PROFILE" \
  --rpc-addr "127.0.0.1:${RPC_PORT}" \
  "${P2P_ARGS[@]}" \
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

rpc_post() {
  local method="$1"
  local body="$2"
  curl -sS --fail -u "$COOKIE" -H 'content-type: application/json' \
    -d "$body" \
    "http://127.0.0.1:${RPC_PORT}/daemon/${method}"
}

json_len() {
  python3 -c 'import json,sys; obj=json.load(sys.stdin); value=obj.get("result", []) or []; print(len(value) if isinstance(value, list) else 0)'
}

json_headers() {
  python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; headers=res.get("headers"); headers=res.get("best_header_height", 0) if headers is None else headers; print(int(headers or 0))'
}

json_blocks() {
  python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; blocks=res.get("blocks"); blocks=res.get("best_block_height", 0) if blocks is None else blocks; print(int(blocks or 0))'
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

echo "Checking verifychain ..."
rpc_get "verifychain?checklevel=5&numblocks=1" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; assert obj.get("result") is True, obj'

echo "Checking getinfo ..."
rpc_get "getinfo" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; req=("version","protocolversion","walletversion","balance","blocks","timeoffset","connections","proxy","difficulty","testnet","keypoololdest","keypoolsize","paytxfee","relayfee","errors"); missing=[k for k in req if k not in res]; assert not missing, f"missing keys: {missing}"; assert isinstance(res.get("version"), int); assert isinstance(res.get("protocolversion"), int); assert isinstance(res.get("walletversion"), int); assert isinstance(res.get("blocks"), int); assert isinstance(res.get("connections"), int)'

echo "Checking help includes p2shstarttx RPCs ..."
rpc_get "help" | python3 -c 'import json,sys; obj=json.load(sys.stdin); methods=obj.get("result", []) or []; need=("createp2shstarttx","signp2shstarttx","sendp2shstarttx","startfluxnodewithdelegates","startfluxnodeasdelegate","startp2shasdelegate"); missing=[m for m in need if m not in methods]; assert not missing, f"missing methods: {missing}"'

echo "Checking createdelegatekeypair ..."
rpc_get "createdelegatekeypair" | python3 -c 'import json,sys,re; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; req=("private_key","public_key_compressed","public_key_uncompressed"); missing=[k for k in req if k not in res]; assert not missing, f"missing keys: {missing}"; pkc=res.get("public_key_compressed",""); pku=res.get("public_key_uncompressed",""); assert isinstance(res.get("private_key"), str) and len(res.get("private_key"))>0; assert isinstance(pkc,str) and re.fullmatch(r"[0-9a-fA-F]+", pkc) and len(pkc)==66; assert isinstance(pku,str) and re.fullmatch(r"[0-9a-fA-F]+", pku) and len(pku)==130'

echo "Checking validateaddress ..."
taddr="$(rpc_get "getnewaddress" | python3 -c 'import json,sys; obj=json.load(sys.stdin); print(obj.get("result",""))')"
if [[ -z "$taddr" ]]; then
  echo "getnewaddress returned empty result" >&2
  exit 1
fi
taddr2="$(rpc_get "getnewaddress" | python3 -c 'import json,sys; obj=json.load(sys.stdin); print(obj.get("result",""))')"
if [[ -z "$taddr2" ]]; then
  echo "getnewaddress returned empty result" >&2
  exit 1
fi
rpc_get "validateaddress?address=${taddr}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; req=("isvalid","address","scriptPubKey","ismine","iswatchonly","isscript"); missing=[k for k in req if k not in res]; assert not missing, f"missing keys: {missing}"; assert res.get("isvalid") is True; assert res.get("ismine") is True; assert res.get("iswatchonly") is False'
rpc_get "validateaddress?address=notanaddress" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; assert res.get("isvalid") is False, res'

echo "Checking wallet encryption/locking ..."
rpc_post "encryptwallet" '["test-passphrase"]' | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj'
rpc_get "dumpprivkey?address=${taddr}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); err=obj.get("error") or {}; assert err.get("code")==-13, obj'
rpc_post "walletpassphrase" '["test-passphrase", 15]' | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj'
wif1="$(rpc_get "dumpprivkey?address=${taddr}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; print(obj.get("result",""))')"
if [[ -z "$wif1" ]]; then
  echo "dumpprivkey returned empty WIF" >&2
  exit 1
fi
wif2="$(rpc_get "dumpprivkey?address=${taddr2}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; print(obj.get("result",""))')"
if [[ -z "$wif2" ]]; then
  echo "dumpprivkey returned empty WIF" >&2
  exit 1
fi
rpc_get "walletlock" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj'
rpc_get "dumpprivkey?address=${taddr}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); err=obj.get("error") or {}; assert err.get("code")==-13, obj'

echo "Checking startfluxnode schema ..."
fluxnode_wif="$(rpc_get "createfluxnodekey" | python3 -c 'import json,sys; obj=json.load(sys.stdin); print(obj.get("result",""))')"
if [[ -z "$fluxnode_wif" ]]; then
  echo "createfluxnodekey returned empty result" >&2
  exit 1
fi
cat > "${DATA_DIR}/fluxnode.conf" <<EOF
fn1 127.0.0.1:16125 ${fluxnode_wif} 0000000000000000000000000000000000000000000000000000000000000000 0 ${fluxnode_wif}
EOF
rpc_get "startfluxnode?set=all&lockwallet=false" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; res=obj.get("result", {}) or {}; assert isinstance(res, dict), res; assert "overall" in res and "detail" in res, res; detail=res.get("detail"); assert isinstance(detail, list) and len(detail)==1, detail; entry=detail[0] if detail else {}; assert isinstance(entry, dict), entry; req=("alias","outpoint","result","transaction_built","transaction_signed","transaction_commited","errorMessage"); missing=[k for k in req if k not in entry]; assert not missing, f"missing keys: {missing}"'

echo "Checking startdeterministicfluxnode schema ..."
rpc_get "startdeterministicfluxnode?alias=fn1&lockwallet=false" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; res=obj.get("result", {}) or {}; assert isinstance(res, dict), res; assert "overall" in res and "detail" in res, res; detail=res.get("detail"); assert isinstance(detail, list) and len(detail)==1, detail; entry=detail[0] if detail else {}; assert isinstance(entry, dict), entry; req=("alias","outpoint","result","transaction_built","transaction_signed","transaction_commited","errorMessage"); missing=[k for k in req if k not in entry]; assert not missing, f"missing keys: {missing}"; assert entry.get("alias") == "fn1"'

echo "Checking multisig RPCs while wallet locked ..."
ms="$(rpc_post "createmultisig" "[2, [\"${taddr}\", \"${taddr2}\"]]" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; assert "address" in res and "redeemScript" in res, res; import json as j; print(j.dumps(res))')"
ms_addr="$(python3 -c "import json,sys; obj=json.loads(sys.argv[1]); print(obj.get('address',''))" "$ms")"
ms_redeem="$(python3 -c "import json,sys; obj=json.loads(sys.argv[1]); print(obj.get('redeemScript',''))" "$ms")"
if [[ -z "$ms_addr" || -z "$ms_redeem" ]]; then
  echo "createmultisig returned empty fields" >&2
  exit 1
fi
rpc_post "addmultisigaddress" "[2, [\"${taddr}\", \"${taddr2}\"]]" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result"); assert isinstance(res, str) and len(res) > 0, res'

echo "Checking signrawtransaction for P2SH multisig (wallet locked, privkeys provided) ..."
ms_spk="$(rpc_get "validateaddress?address=${ms_addr}" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; spk=res.get("scriptPubKey",""); assert isinstance(spk,str) and len(spk)>0, res; print(spk)')"
fake_txid="$(python3 -c 'import os,binascii; print(binascii.hexlify(os.urandom(32)).decode())')"
raw_tx="$(rpc_post "createrawtransaction" "[[{\"txid\":\"${fake_txid}\",\"vout\":0}], {\"${taddr}\": 0.1}]" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result",""); assert isinstance(res,str) and len(res)>0, obj; print(res)')"
rpc_post "signrawtransaction" "[\"${raw_tx}\", [{\"txid\":\"${fake_txid}\",\"vout\":0,\"scriptPubKey\":\"${ms_spk}\",\"redeemScript\":\"${ms_redeem}\",\"amount\":1.0}], [\"${wif1}\", \"${wif2}\"]]" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; assert res.get("complete") is True, obj; assert isinstance(res.get("hex",""), str) and len(res.get("hex",""))>0, obj'

echo "Checking gettxoutsetinfo ..."
rpc_get "gettxoutsetinfo" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; req=("transactions","txouts","bytes_serialized","hash_serialized","total_amount"); missing=[k for k in req if k not in res]; assert not missing, f"missing keys: {missing}"'

echo "Checking getblocktemplate ..."
rpc_get "getblocktemplate" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; req=("previousblockhash","coinbasetxn","transactions","height"); missing=[k for k in req if k not in res]; assert not missing, f"missing keys: {missing}"'

echo "Checking submitblock duplicate ..."
genesis_hash="$(rpc_get "getblockhash?height=0" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; print(obj.get("result",""))')"
if [[ -z "$genesis_hash" ]]; then
  echo "getblockhash returned empty result" >&2
  exit 1
fi
genesis_hex="$(rpc_get "getblock?hash=${genesis_hash}&verbosity=0" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; res=obj.get("result",""); assert isinstance(res,str) and len(res)>0, obj; print(res)')"
if [[ -z "$genesis_hex" ]]; then
  echo "getblock verbosity=0 returned empty result" >&2
  exit 1
fi
rpc_post "submitblock" "[\"${genesis_hex}\"]" | python3 -c 'import json,sys; obj=json.load(sys.stdin); assert obj.get("error") is None, obj; assert obj.get("result") == "duplicate", obj'

echo "Checking getnetworksolps/getnetworkhashps ..."
rpc_get "getnetworksolps" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result"); assert isinstance(res, (int,float)), res; assert res >= 0'
rpc_get "getnetworkhashps" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result"); assert isinstance(res, (int,float)), res; assert res >= 0'

echo "Checking getlocalsolps ..."
rpc_get "getlocalsolps" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result"); assert isinstance(res, (int,float)), res; assert res >= 0'

echo "Checking getmininginfo ..."
rpc_get "getmininginfo" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; assert isinstance(res, dict), res; assert isinstance(res.get("localsolps"), (int,float)), res; assert res.get("localsolps") >= 0'

echo "Checking zvalidateaddress ..."
rpc_get "zvalidateaddress?zaddr=notanaddress" | python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; assert res.get("isvalid") is False'

start_ts=$(date +%s)
peers=0
headers=0
blocks=0
start_headers="$(rpc_get "getblockchaininfo" | json_headers)"
start_blocks="$(rpc_get "getblockchaininfo" | json_blocks)"
if [[ "$REQUIRE_HEADERS" == "1" ]]; then
  if [[ "$MIN_HEADERS_ADVANCE" -lt 1 ]]; then
    MIN_HEADERS_ADVANCE="1"
  fi
fi
while true; do
  peers="$(rpc_get "getpeerinfo" | json_len)"
  headers="$(rpc_get "getblockchaininfo" | json_headers)"
  blocks="$(rpc_get "getblockchaininfo" | json_blocks)"
  headers_advance=$((headers - start_headers))
  blocks_advance=$((blocks - start_blocks))
  if [[ "$peers" -ge "$MIN_PEERS" && "$headers_advance" -ge "$MIN_HEADERS_ADVANCE" && "$blocks_advance" -ge "$MIN_BLOCKS_ADVANCE" ]]; then
    break
  fi
  now_ts=$(date +%s)
  if [[ $((now_ts - start_ts)) -ge "$TIMEOUT_SECS" ]]; then
    echo "Timed out waiting for smoke test conditions:" >&2
    echo "  peers=$peers (min $MIN_PEERS)" >&2
    echo "  headers=$headers (start $start_headers, +$headers_advance, min +$MIN_HEADERS_ADVANCE)" >&2
    echo "  blocks=$blocks (start $start_blocks, +$blocks_advance, min +$MIN_BLOCKS_ADVANCE)" >&2
    exit 1
  fi
  sleep 1
done

echo "OK: peers=$peers headers=$headers (start $start_headers, +$headers_advance) blocks=$blocks (start $start_blocks, +$blocks_advance)"
echo "Sample getnetworkinfo:"
networkinfo="$(rpc_get "getnetworkinfo")"
echo "${networkinfo:0:1200}"
echo
echo "Sample getpeerinfo:"
peerinfo="$(rpc_get "getpeerinfo")"
echo "${peerinfo:0:1200}"
echo
