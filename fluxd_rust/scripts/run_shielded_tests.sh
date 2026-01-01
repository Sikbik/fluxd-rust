#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: run_shielded_tests.sh [options] [-- <extra cargo test args>]

Fetches shielded params (if missing) and runs the ignored shielded verification tests.

Options:
  --params-dir PATH   Shielded params dir (default: ~/.zcash-params)
  --no-fetch          Do not download params if missing
  -h, --help          Show this help

Examples:
  ./scripts/run_shielded_tests.sh
  FLUXD_PARAMS_DIR=~/.zcash-params ./scripts/run_shielded_tests.sh -- --nocapture
USAGE
}

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PARAMS_DIR="${FLUXD_PARAMS_DIR:-${HOME}/.zcash-params}"
NO_FETCH="0"
EXTRA_CARGO_ARGS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --params-dir)
      PARAMS_DIR="$2"
      shift 2
      ;;
    --no-fetch)
      NO_FETCH="1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    --)
      shift
      EXTRA_CARGO_ARGS=("$@")
      break
      ;;
    *)
      echo "Unknown arg: $1" >&2
      echo >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "${CARGO_HOME:-}" ]]; then
  export CARGO_HOME="$ROOT_DIR/.cargo_home"
fi
mkdir -p "$CARGO_HOME"

BASE_URL="https://images.runonflux.io/fluxd/chain-params"

SAPLING_SPEND_NAME="sapling-spend.params"
SAPLING_OUTPUT_NAME="sapling-output.params"
SPROUT_GROTH16_NAME="sprout-groth16.params"

SAPLING_SPEND_SHA256="8e48ffd23abb3a5fd9c5589204f32d9c31285a04b78096ba40a79b75677efc13"
SAPLING_OUTPUT_SHA256="2f0ebbcbb9bb0bcffe95a397e7eba89c29eb4dde6191c339db88570e3f3fb0e4"
SPROUT_GROTH16_SHA256="b685d700c60328498fbde589c8c7c484c722b788b265b72af448a5bf0ee55b50"

sha_ok() {
  local path="$1"
  local expected="$2"
  [[ -f "$path" ]] || return 1
  command -v sha256sum >/dev/null 2>&1 || return 1
  echo "${expected}  ${path}" | sha256sum -c - >/dev/null 2>&1
}

download_param() {
  local name="$1"
  local expected="$2"
  local dest="${PARAMS_DIR}/${name}"

  if sha_ok "$dest" "$expected"; then
    return 0
  fi

  mkdir -p "$PARAMS_DIR"
  local tmp="${dest}.dl"
  local part2="${tmp}.part2"

  rm -f "$tmp" "$part2"
  curl -fsSL "${BASE_URL}/${name}.part.1" -o "$tmp"
  curl -fsSL "${BASE_URL}/${name}.part.2" -o "$part2"
  cat "$part2" >> "$tmp"
  rm -f "$part2"

  if ! sha_ok "$tmp" "$expected"; then
    echo "sha256 mismatch for ${name} (downloaded to ${tmp})" >&2
    exit 1
  fi

  mv "$tmp" "$dest"
}

if [[ "$NO_FETCH" != "1" ]]; then
  missing=0
  for name in "$SAPLING_SPEND_NAME" "$SAPLING_OUTPUT_NAME" "$SPROUT_GROTH16_NAME"; do
    if [[ ! -f "${PARAMS_DIR}/${name}" ]]; then
      missing=1
      break
    fi
  done

  if [[ "$missing" == "1" ]]; then
    echo "Shielded params missing; downloading to ${PARAMS_DIR}" >&2
    download_param "$SAPLING_SPEND_NAME" "$SAPLING_SPEND_SHA256"
    download_param "$SAPLING_OUTPUT_NAME" "$SAPLING_OUTPUT_SHA256"
    download_param "$SPROUT_GROTH16_NAME" "$SPROUT_GROTH16_SHA256"
  fi
fi

export FLUXD_PARAMS_DIR="$PARAMS_DIR"
cd "$ROOT_DIR"

cargo test -p fluxd-shielded
cargo test -p fluxd-shielded -- --ignored "${EXTRA_CARGO_ARGS[@]}"

