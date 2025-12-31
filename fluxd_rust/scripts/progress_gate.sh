#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: progress_gate.sh [options]

Checks a running fluxd instance via RPC and fails if it appears to be stalled while
behind the network tip.

Options:
  --rpc-addr ADDR        RPC bind address to query (default: 127.0.0.1:16124)
  --data-dir DIR         Data dir containing rpc.cookie (preferred)
  --cookie-file PATH     Path to rpc.cookie (overrides --data-dir)
  --window-secs N        Seconds to wait between checks (default: 90)
  --min-peers N          Minimum connected peers required (default: 1)
  --min-blocks-advance N Minimum block height increase when behind tip (default: 1)
  --min-headers-advance N Minimum header height increase when behind tip (default: 0)
  --tip-lag N            Treat node as "behind" if peer best height - local height > N (default: 2)
  -h, --help             Show this help

Notes:
- This gate only enforces progress when the node is behind the peer best height or
  still has a headers > blocks gap. When already at tip, it only validates RPC and
  peer connectivity (progress may legitimately be 0 within the window).

USAGE
}

RPC_ADDR="127.0.0.1:16124"
DATA_DIR=""
COOKIE_FILE=""
WINDOW_SECS="90"
MIN_PEERS="1"
MIN_BLOCKS_ADVANCE="1"
MIN_HEADERS_ADVANCE="0"
TIP_LAG="2"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --rpc-addr)
      RPC_ADDR="${2:-}"
      shift 2
      ;;
    --data-dir)
      DATA_DIR="${2:-}"
      shift 2
      ;;
    --cookie-file)
      COOKIE_FILE="${2:-}"
      shift 2
      ;;
    --window-secs)
      WINDOW_SECS="${2:-}"
      shift 2
      ;;
    --min-peers)
      MIN_PEERS="${2:-}"
      shift 2
      ;;
    --min-blocks-advance)
      MIN_BLOCKS_ADVANCE="${2:-}"
      shift 2
      ;;
    --min-headers-advance)
      MIN_HEADERS_ADVANCE="${2:-}"
      shift 2
      ;;
    --tip-lag)
      TIP_LAG="${2:-}"
      shift 2
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

if [[ -z "$COOKIE_FILE" ]]; then
  if [[ -z "$DATA_DIR" ]]; then
    echo "Missing --data-dir or --cookie-file" >&2
    usage >&2
    exit 2
  fi
  COOKIE_FILE="${DATA_DIR%/}/rpc.cookie"
fi

if [[ ! -f "$COOKIE_FILE" ]]; then
  echo "rpc.cookie not found: $COOKIE_FILE" >&2
  exit 1
fi

COOKIE="$(cat "$COOKIE_FILE")"

rpc_get() {
  local method="$1"
  curl -sS --fail -u "$COOKIE" "http://${RPC_ADDR}/daemon/${method}"
}

json_blocks() {
  python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; blocks=res.get("blocks"); blocks=res.get("best_block_height", 0) if blocks is None else blocks; print(int(blocks or 0))'
}

json_headers() {
  python3 -c 'import json,sys; obj=json.load(sys.stdin); res=obj.get("result", {}) or {}; headers=res.get("headers"); headers=res.get("best_header_height", 0) if headers is None else headers; print(int(headers or 0))'
}

json_peer_count() {
  python3 -c 'import json,sys; obj=json.load(sys.stdin); peers=obj.get("result", []) or []; print(len(peers) if isinstance(peers, list) else 0)'
}

json_peer_best_height() {
  python3 -c 'import json,sys; obj=json.load(sys.stdin); peers=obj.get("result", []) or []; print(max((p.get("startingheight") or 0) for p in peers) if isinstance(peers, list) and peers else 0)'
}

if ! rpc_get "getnetworkinfo" >/dev/null 2>&1; then
  echo "RPC not reachable at http://${RPC_ADDR}/daemon/*" >&2
  exit 1
fi

peerinfo_0="$(rpc_get "getpeerinfo")"
peers_0="$(printf '%s' "$peerinfo_0" | json_peer_count)"
peer_best_0="$(printf '%s' "$peerinfo_0" | json_peer_best_height)"
chaininfo_0="$(rpc_get "getblockchaininfo")"
blocks_0="$(printf '%s' "$chaininfo_0" | json_blocks)"
headers_0="$(printf '%s' "$chaininfo_0" | json_headers)"

if [[ "$peers_0" -lt "$MIN_PEERS" ]]; then
  echo "FAIL: peers=$peers_0 (min $MIN_PEERS)" >&2
  exit 1
fi

behind_by_blocks=$((peer_best_0 - blocks_0))
behind_by_headers=$((peer_best_0 - headers_0))
gap_0=$((headers_0 - blocks_0))
needs_progress=0
if [[ "$gap_0" -gt 0 ]]; then
  needs_progress=1
elif [[ "$behind_by_blocks" -gt "$TIP_LAG" ]]; then
  needs_progress=1
elif [[ "$behind_by_headers" -gt "$TIP_LAG" ]]; then
  needs_progress=1
fi

sleep "$WINDOW_SECS"

peerinfo_1="$(rpc_get "getpeerinfo")"
peers_1="$(printf '%s' "$peerinfo_1" | json_peer_count)"
peer_best_1="$(printf '%s' "$peerinfo_1" | json_peer_best_height)"
chaininfo_1="$(rpc_get "getblockchaininfo")"
blocks_1="$(printf '%s' "$chaininfo_1" | json_blocks)"
headers_1="$(printf '%s' "$chaininfo_1" | json_headers)"

blocks_advance=$((blocks_1 - blocks_0))
headers_advance=$((headers_1 - headers_0))
gap_1=$((headers_1 - blocks_1))

echo "peers: $peers_0 -> $peers_1 (min $MIN_PEERS)"
echo "peer_best_height: $peer_best_0 -> $peer_best_1"
echo "headers: $headers_0 -> $headers_1 (+$headers_advance) gap $gap_0 -> $gap_1"
echo "blocks:  $blocks_0 -> $blocks_1 (+$blocks_advance)"

if [[ "$peers_1" -lt "$MIN_PEERS" ]]; then
  echo "FAIL: peers=$peers_1 (min $MIN_PEERS)" >&2
  exit 1
fi
if [[ "$blocks_1" -lt "$blocks_0" || "$headers_1" -lt "$headers_0" ]]; then
  echo "FAIL: heights regressed (blocks $blocks_0->$blocks_1 headers $headers_0->$headers_1)" >&2
  exit 1
fi

if [[ "$needs_progress" == "1" ]]; then
  if [[ "$blocks_advance" -lt "$MIN_BLOCKS_ADVANCE" ]]; then
    echo "FAIL: no block progress while behind (need +$MIN_BLOCKS_ADVANCE)" >&2
    exit 1
  fi
  if [[ "$headers_advance" -lt "$MIN_HEADERS_ADVANCE" ]]; then
    echo "FAIL: no header progress while behind (need +$MIN_HEADERS_ADVANCE)" >&2
    exit 1
  fi
  echo "OK: progress observed while behind tip"
else
  echo "OK: at/near tip (progress not required within window)"
fi
