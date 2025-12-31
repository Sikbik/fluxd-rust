#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: longrun_watchdog.sh [options]

Runs `progress_gate.sh` in a loop and optionally scans a log file for fatal patterns.
Intended for long-running sync/regression validation on a server.

Options (progress gate passthrough):
  --rpc-addr ADDR        RPC bind address to query (default: 127.0.0.1:16124)
  --data-dir DIR         Data dir containing rpc.cookie (preferred)
  --cookie-file PATH     Path to rpc.cookie (overrides --data-dir)
  --window-secs N        Seconds to wait between checks (default: 90)
  --min-peers N          Minimum connected peers required (default: 1)
  --min-blocks-advance N Minimum block height increase when behind tip (default: 1)
  --min-headers-advance N Minimum header height increase when behind tip (default: 0)
  --tip-lag N            Treat node as "behind" if peer best height - local height > N (default: 2)

Additional options:
  --log-file PATH        If set, scan recent log lines for fatal patterns
  --tail-lines N         Number of log lines to scan each loop (default: 2000)
  --fail-pattern REGEX   Add a fatal regex (repeatable)
  --loops N              Stop after N successful loops (default: 0, run forever)
  -h, --help             Show this help

Default fatal patterns (when --log-file is set):
  - deterministic fluxnode payout mismatch
  - coinbase missing deterministic fluxnode payout
  - CorruptIndex
  - panicked at

USAGE
}

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
GATE="${ROOT_DIR}/scripts/progress_gate.sh"

RPC_ADDR="127.0.0.1:16124"
DATA_DIR=""
COOKIE_FILE=""
WINDOW_SECS="90"
MIN_PEERS="1"
MIN_BLOCKS_ADVANCE="1"
MIN_HEADERS_ADVANCE="0"
TIP_LAG="2"

LOG_FILE=""
TAIL_LINES="2000"
LOOPS="0"

FAIL_PATTERNS=()

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
    --log-file)
      LOG_FILE="${2:-}"
      shift 2
      ;;
    --tail-lines)
      TAIL_LINES="${2:-}"
      shift 2
      ;;
    --fail-pattern)
      FAIL_PATTERNS+=("${2:-}")
      shift 2
      ;;
    --loops)
      LOOPS="${2:-}"
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

if [[ ! -x "$GATE" ]]; then
  echo "progress gate script not found: $GATE" >&2
  exit 2
fi

if [[ -n "$LOG_FILE" && ${#FAIL_PATTERNS[@]} -eq 0 ]]; then
  FAIL_PATTERNS+=("deterministic fluxnode payout mismatch")
  FAIL_PATTERNS+=("coinbase missing deterministic fluxnode payout")
  FAIL_PATTERNS+=("CorruptIndex")
  FAIL_PATTERNS+=("panicked at")
fi

gate_args=(
  --rpc-addr "$RPC_ADDR"
  --window-secs "$WINDOW_SECS"
  --min-peers "$MIN_PEERS"
  --min-blocks-advance "$MIN_BLOCKS_ADVANCE"
  --min-headers-advance "$MIN_HEADERS_ADVANCE"
  --tip-lag "$TIP_LAG"
)
if [[ -n "$COOKIE_FILE" ]]; then
  gate_args+=(--cookie-file "$COOKIE_FILE")
elif [[ -n "$DATA_DIR" ]]; then
  gate_args+=(--data-dir "$DATA_DIR")
else
  echo "Missing --data-dir or --cookie-file" >&2
  usage >&2
  exit 2
fi

check_logs() {
  if [[ -z "$LOG_FILE" ]]; then
    return 0
  fi
  if [[ ! -f "$LOG_FILE" ]]; then
    echo "Log file not found: $LOG_FILE" >&2
    return 1
  fi

  for pattern in "${FAIL_PATTERNS[@]}"; do
    if tail -n "$TAIL_LINES" "$LOG_FILE" | grep -Ein "$pattern" >/dev/null 2>&1; then
      echo "FAIL: log matched pattern: $pattern" >&2
      tail -n "$TAIL_LINES" "$LOG_FILE" | grep -Ein "$pattern" | tail -n 25 >&2 || true
      return 1
    fi
  done
  return 0
}

loops_done=0
while true; do
  loops_done=$((loops_done + 1))
  check_logs
  "$GATE" "${gate_args[@]}"
  check_logs

  if [[ "$LOOPS" != "0" && "$loops_done" -ge "$LOOPS" ]]; then
    echo "OK: completed $loops_done watchdog loop(s)"
    exit 0
  fi
done

