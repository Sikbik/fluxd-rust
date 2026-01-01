#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: stats_delta.sh [options]

Fetches two `/stats` snapshots from the dashboard and prints deltas and per-block timings.

Options:
  --stats-addr ADDR     Dashboard address to query (host:port) (default: 127.0.0.1:8080)
  --window-secs N       Seconds between snapshots (default: 60)
  --json                Output JSON (instead of human text)
  -h, --help            Show this help

Examples:
  ./scripts/stats_delta.sh --window-secs 30
  ./scripts/stats_delta.sh --stats-addr 127.0.0.1:8080 --window-secs 90
USAGE
}

STATS_ADDR="127.0.0.1:8080"
WINDOW_SECS="60"
OUTPUT_JSON="0"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --stats-addr)
      STATS_ADDR="${2:-}"
      shift 2
      ;;
    --window-secs)
      WINDOW_SECS="${2:-}"
      shift 2
      ;;
    --json)
      OUTPUT_JSON="1"
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

if ! [[ "$WINDOW_SECS" =~ ^[0-9]+([.][0-9]+)?$ ]]; then
  echo "Invalid --window-secs: $WINDOW_SECS" >&2
  exit 2
fi

URL="http://${STATS_ADDR}/stats"

snap0="$(curl -sS --fail "$URL")"
sleep "$WINDOW_SECS"
snap1="$(curl -sS --fail "$URL")"

PYCODE="$(cat <<'PY'
import json
import math
import sys

window_secs = float(sys.argv[1])
emit_json = sys.argv[2] == "1"

line0 = sys.stdin.readline()
line1 = sys.stdin.readline()
if not line0 or not line1:
  raise SystemExit("expected 2 json snapshots on stdin")

a = json.loads(line0)
b = json.loads(line1)

def num(obj, key, default=0):
  value = obj.get(key, default)
  if value is None:
    return default
  if isinstance(value, bool):
    return int(value)
  if isinstance(value, (int, float)):
    return value
  return default

def delta(key):
  return num(b, key, 0) - num(a, key, 0)

def per_sec(value):
  if window_secs <= 0:
    return 0.0
  return value / window_secs

def ms_per(count_key, us_key):
  count = delta(count_key)
  if count <= 0:
    return None
  us = delta(us_key)
  return (us / 1000.0) / count

height_fields = {
  "best_header_height": ("headers", "header_height"),
  "best_block_height": ("blocks", "block_height"),
  "header_gap": ("gap", "header_gap"),
}

stage_ms_per_block = {
  "download": ms_per("download_blocks", "download_us"),
  "verify": ms_per("verify_blocks", "verify_us"),
  "commit": ms_per("commit_blocks", "commit_us"),
  "validate": ms_per("validate_blocks", "validate_us"),
  "script": ms_per("script_blocks", "script_us"),
  "shielded": ms_per("verify_blocks", "shielded_us"),
  "utxo": ms_per("utxo_blocks", "utxo_us"),
  "index": ms_per("index_blocks", "index_us"),
  "anchor": ms_per("anchor_blocks", "anchor_us"),
  "flatfile": ms_per("flatfile_blocks", "flatfile_us"),
  "undo_encode": ms_per("verify_blocks", "undo_encode_us"),
  "undo_append": ms_per("verify_blocks", "undo_append_us"),
  "payout": ms_per("payout_blocks", "payout_us"),
  "pon_sig": ms_per("pon_sig_blocks", "pon_sig_us"),
  "fluxnode_tx": ms_per("verify_blocks", "fluxnode_tx_us"),
  "fluxnode_sig": ms_per("verify_blocks", "fluxnode_sig_us"),
}

utxo_hits = delta("utxo_cache_hits")
utxo_misses = delta("utxo_cache_misses")
utxo_total = utxo_hits + utxo_misses
utxo_hit_rate = (utxo_hits / utxo_total) if utxo_total > 0 else None

out = {
  "window_secs": window_secs,
  "start": {k: a.get(k) for k in ("best_header_height", "best_block_height", "header_gap", "sync_state", "uptime_secs")},
  "end": {k: b.get(k) for k in ("best_header_height", "best_block_height", "header_gap", "sync_state", "uptime_secs")},
  "delta": {
    "download_blocks": delta("download_blocks"),
    "verify_blocks": delta("verify_blocks"),
    "commit_blocks": delta("commit_blocks"),
    "download_us": delta("download_us"),
    "verify_us": delta("verify_us"),
    "commit_us": delta("commit_us"),
  },
  "rates_per_sec": {
    "download_blocks_per_sec": per_sec(delta("download_blocks")),
    "verify_blocks_per_sec": per_sec(delta("verify_blocks")),
    "commit_blocks_per_sec": per_sec(delta("commit_blocks")),
  },
  "ms_per_block": {k: v for k, v in stage_ms_per_block.items() if v is not None},
  "utxo_cache": {
    "hits": utxo_hits,
    "misses": utxo_misses,
    "hit_rate": utxo_hit_rate,
  },
  "db": {
    "write_buffer_bytes": num(b, "db_write_buffer_bytes", None),
    "max_write_buffer_bytes": num(b, "db_max_write_buffer_bytes", None),
    "active_compactions": num(b, "db_active_compactions", None),
    "compactions_completed_delta": delta("db_compactions_completed"),
    "flushes_completed_delta": delta("db_flushes_completed"),
  },
}

if emit_json:
  print(json.dumps(out, sort_keys=True))
  raise SystemExit(0)

def fmt_ms(value):
  if value is None:
    return "-"
  if math.isnan(value) or math.isinf(value):
    return "-"
  return f"{value:.2f}"

def fmt_rate(value):
  if math.isnan(value) or math.isinf(value):
    return "-"
  return f"{value:.2f}"

headers0 = num(a, "best_header_height", 0)
headers1 = num(b, "best_header_height", 0)
blocks0 = num(a, "best_block_height", 0)
blocks1 = num(b, "best_block_height", 0)
gap0 = num(a, "header_gap", 0)
gap1 = num(b, "header_gap", 0)

print(f"window: {window_secs:.0f}s")
print(f"heights: headers {headers0} -> {headers1} ({headers1-headers0:+})  blocks {blocks0} -> {blocks1} ({blocks1-blocks0:+})  gap {gap0} -> {gap1}")
print(f"rates: verify {fmt_rate(out['rates_per_sec']['verify_blocks_per_sec'])} blk/s  commit {fmt_rate(out['rates_per_sec']['commit_blocks_per_sec'])} blk/s  download {fmt_rate(out['rates_per_sec']['download_blocks_per_sec'])} blk/s")
print(
  "ms/block:"
  f" dl {fmt_ms(stage_ms_per_block['download'])}"
  f" ver {fmt_ms(stage_ms_per_block['verify'])}"
  f" db {fmt_ms(stage_ms_per_block['commit'])}"
  f" script {fmt_ms(stage_ms_per_block['script'])}"
  f" shield {fmt_ms(stage_ms_per_block['shielded'])}"
  f" utxo {fmt_ms(stage_ms_per_block['utxo'])}"
  f" idx {fmt_ms(stage_ms_per_block['index'])}"
  f" payout {fmt_ms(stage_ms_per_block['payout'])}"
)

if utxo_hit_rate is None:
  print("utxo cache: -")
else:
  print(f"utxo cache: hit_rate={utxo_hit_rate:.3f} (hits {utxo_hits}, misses {utxo_misses})")

max_wb = out["db"]["max_write_buffer_bytes"]
cur_wb = out["db"]["write_buffer_bytes"]
if isinstance(max_wb, (int, float)) and max_wb > 0 and isinstance(cur_wb, (int, float)):
  pct = (cur_wb / max_wb) * 100.0
  print(f"db: write_buffer={int(cur_wb)}/{int(max_wb)} bytes ({pct:.1f}%) compactions_delta={out['db']['compactions_completed_delta']} flushes_delta={out['db']['flushes_completed_delta']}")
else:
  print(f"db: compactions_delta={out['db']['compactions_completed_delta']} flushes_delta={out['db']['flushes_completed_delta']}")
PY
)"

printf '%s\n%s\n' "$snap0" "$snap1" | python3 -c "$PYCODE" "$WINDOW_SECS" "$OUTPUT_JSON"
