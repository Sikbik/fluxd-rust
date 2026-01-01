#!/usr/bin/env bash
set -euo pipefail

addr="127.0.0.1:16125"
timeout_secs="8"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --addr)
      addr="${2:-}"
      shift 2
      ;;
    --timeout-secs)
      timeout_secs="${2:-}"
      shift 2
      ;;
    -h|--help)
      cat <<EOF
Usage: $(basename "$0") [--addr IP:PORT] [--timeout-secs N]

Connects to a Flux P2P port, performs a minimal version/verack handshake,
sends \`mempool\`, waits for \`inv\`, then requests the first tx via \`getdata\`.

Defaults:
  --addr         ${addr}
  --timeout-secs ${timeout_secs}
EOF
      exit 0
      ;;
    *)
      echo "Unknown arg: $1" >&2
      exit 2
      ;;
  esac
done

python3 - "$addr" "$timeout_secs" <<'PY'
import hashlib
import os
import socket
import struct
import sys
import time


def sha256d(data: bytes) -> bytes:
    return hashlib.sha256(hashlib.sha256(data).digest()).digest()


def encode_varint(n: int) -> bytes:
    if n < 0:
        raise ValueError("varint must be >= 0")
    if n < 0xFD:
        return struct.pack("<B", n)
    if n <= 0xFFFF:
        return b"\xFD" + struct.pack("<H", n)
    if n <= 0xFFFFFFFF:
        return b"\xFE" + struct.pack("<I", n)
    return b"\xFF" + struct.pack("<Q", n)


def decode_varint(data: bytes, offset: int = 0):
    if offset >= len(data):
        raise ValueError("varint truncated")
    first = data[offset]
    if first < 0xFD:
        return first, offset + 1
    if first == 0xFD:
        if offset + 3 > len(data):
            raise ValueError("varint truncated")
        return struct.unpack_from("<H", data, offset + 1)[0], offset + 3
    if first == 0xFE:
        if offset + 5 > len(data):
            raise ValueError("varint truncated")
        return struct.unpack_from("<I", data, offset + 1)[0], offset + 5
    if offset + 9 > len(data):
        raise ValueError("varint truncated")
    return struct.unpack_from("<Q", data, offset + 1)[0], offset + 9


def recv_exact(sock: socket.socket, n: int) -> bytes:
    out = bytearray()
    while len(out) < n:
        chunk = sock.recv(n - len(out))
        if not chunk:
            raise EOFError("socket closed")
        out.extend(chunk)
    return bytes(out)


def read_message(sock: socket.socket):
    hdr = recv_exact(sock, 24)
    magic = hdr[0:4]
    cmd_raw = hdr[4:16]
    cmd = cmd_raw.split(b"\x00", 1)[0].decode("ascii", errors="replace")
    length = struct.unpack_from("<I", hdr, 16)[0]
    checksum = hdr[20:24]
    payload = recv_exact(sock, length) if length else b""
    calc = sha256d(payload)[0:4]
    if checksum != calc:
        raise ValueError(f"checksum mismatch for {cmd}: got {checksum.hex()} expected {calc.hex()}")
    return magic, cmd, payload


def send_message(sock: socket.socket, magic: bytes, cmd: str, payload: bytes):
    cmd_b = cmd.encode("ascii")
    if len(cmd_b) > 12:
        raise ValueError("command too long")
    cmd_padded = cmd_b + b"\x00" * (12 - len(cmd_b))
    hdr = bytearray()
    hdr.extend(magic)
    hdr.extend(cmd_padded)
    hdr.extend(struct.pack("<I", len(payload)))
    hdr.extend(sha256d(payload)[0:4])
    sock.sendall(bytes(hdr) + payload)


def build_version_payload(proto_version: int) -> bytes:
    services = 1
    timestamp = int(time.time())
    ip = b"\x00" * 16
    port = 0
    nonce = struct.unpack("<Q", os.urandom(8))[0]
    user_agent = b"/p2p_mempool_probe:0.1/"
    start_height = 0
    relay = 1

    out = bytearray()
    out.extend(struct.pack("<i", proto_version))
    out.extend(struct.pack("<Q", services))
    out.extend(struct.pack("<q", timestamp))
    out.extend(struct.pack("<Q", services) + ip + struct.pack(">H", port))
    out.extend(struct.pack("<Q", services) + ip + struct.pack(">H", port))
    out.extend(struct.pack("<Q", nonce))
    out.extend(encode_varint(len(user_agent)) + user_agent)
    out.extend(struct.pack("<i", start_height))
    out.extend(struct.pack("<B", relay))
    return bytes(out)


def parse_inv(payload: bytes):
    count, off = decode_varint(payload, 0)
    vectors = []
    for _ in range(count):
        if off + 36 > len(payload):
            raise ValueError("inv truncated")
        inv_type = struct.unpack_from("<I", payload, off)[0]
        h = payload[off + 4 : off + 36]  # little-endian
        off += 36
        vectors.append((inv_type, h))
    return count, vectors


def parse_version_proto(payload: bytes) -> int:
    if len(payload) < 4:
        raise ValueError("version payload too short")
    return struct.unpack_from("<i", payload, 0)[0]


def main():
    if len(sys.argv) < 3:
        print("usage: p2p_mempool_probe.sh ADDR TIMEOUT_SECS", file=sys.stderr)
        return 2

    addr = sys.argv[1]
    timeout_secs = float(sys.argv[2])
    host, port_s = addr.rsplit(":", 1)
    port = int(port_s)

    sock = socket.create_connection((host, port), timeout=timeout_secs)
    sock.settimeout(timeout_secs)

    # Read server's initial version to learn magic + proto.
    magic, cmd, payload = read_message(sock)
    if cmd != "version":
        raise RuntimeError(f"expected version, got {cmd}")
    proto = parse_version_proto(payload)

    # Reply with version + verack.
    send_message(sock, magic, "version", build_version_payload(proto))
    send_message(sock, magic, "verack", b"")

    # Drain until we see verack or time out.
    deadline = time.time() + timeout_secs
    while time.time() < deadline:
        try:
            _, cmd, payload = read_message(sock)
        except socket.timeout:
            break
        if cmd == "verack":
            break
        if cmd == "ping":
            send_message(sock, magic, "pong", payload)

    # Request mempool inventory.
    send_message(sock, magic, "mempool", b"")

    inv_vectors = None
    deadline = time.time() + timeout_secs
    while time.time() < deadline:
        try:
            _, cmd, payload = read_message(sock)
        except socket.timeout:
            break
        if cmd == "ping":
            send_message(sock, magic, "pong", payload)
            continue
        if cmd == "inv":
            inv_vectors = parse_inv(payload)
            break

    if inv_vectors is None:
        print("ERROR: did not receive inv in time")
        return 1

    inv_count, vectors = inv_vectors
    tx_vectors = [(t, h) for (t, h) in vectors if t == 1]
    print(f"inv_count={inv_count} tx_inv_count={len(tx_vectors)}")
    if not tx_vectors:
        return 0

    first_txid_le = tx_vectors[0][1]
    print(f"first_txid_le={first_txid_le.hex()}")

    # Request the first tx.
    getdata = encode_varint(1) + struct.pack("<I", 1) + first_txid_le
    send_message(sock, magic, "getdata", getdata)

    deadline = time.time() + timeout_secs
    while time.time() < deadline:
        try:
            _, cmd, payload = read_message(sock)
        except socket.timeout:
            break
        if cmd == "ping":
            send_message(sock, magic, "pong", payload)
            continue
        if cmd == "notfound":
            print("tx_getdata=notfound")
            return 0
        if cmd == "tx":
            tx_sha256d_le = sha256d(payload)
            print(f"tx_payload_bytes={len(payload)} tx_sha256d_le={tx_sha256d_le.hex()}")
            return 0

    print("WARNING: did not receive tx/notfound in time")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as e:
        print(f"ERROR: {e}", file=sys.stderr)
        raise
PY
