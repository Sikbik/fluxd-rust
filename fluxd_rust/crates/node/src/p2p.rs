use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use fluxd_consensus::constants::PROTOCOL_VERSION;
use fluxd_primitives::block::BlockHeader;
use fluxd_primitives::encoding::{Decoder, Encoder};
use fluxd_primitives::hash::sha256d;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

const MAX_PAYLOAD_SIZE: usize = 4 * 1024 * 1024;
const MAX_HEADERS_RESULTS: usize = 160;
const MAX_ADDR_RESULTS: usize = 1000;
const MAX_INV_RESULTS: usize = 50_000;
const NODE_NETWORK: u64 = 1;
pub const MSG_TX: u32 = 1;
pub const MSG_BLOCK: u32 = 2;
const SEND_TIMEOUT_SECS: u64 = 10;
const HANDSHAKE_READ_TIMEOUT_SECS: u64 = 30;
const USER_AGENT: &str = concat!("/fluxd-rust:", env!("CARGO_PKG_VERSION"), "/");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerKind {
    Block,
    Header,
    Relay,
}

#[derive(Clone, Debug)]
pub struct PeerInfoSnapshot {
    pub addr: SocketAddr,
    pub kind: PeerKind,
    pub inbound: bool,
    pub version: i32,
    pub services: u64,
    pub user_agent: String,
    pub start_height: i32,
    pub connected_since: SystemTime,
    pub last_send: SystemTime,
    pub last_recv: SystemTime,
    pub bytes_sent: u64,
    pub bytes_recv: u64,
}

#[derive(Clone, Debug)]
struct PeerEntry {
    addr: SocketAddr,
    kind: PeerKind,
    inbound: bool,
    version: i32,
    services: u64,
    user_agent: String,
    start_height: i32,
    connected_since: SystemTime,
    last_send: SystemTime,
    last_recv: SystemTime,
    bytes_sent: u64,
    bytes_recv: u64,
}

#[derive(Debug, Default)]
pub struct PeerRegistry {
    peers: Mutex<HashMap<SocketAddr, PeerEntry>>,
    disconnect_requests: Mutex<HashSet<SocketAddr>>,
}

impl PeerRegistry {
    fn register_internal(&self, addr: SocketAddr, kind: PeerKind, inbound: bool) {
        let now = SystemTime::now();
        let entry = PeerEntry {
            addr,
            kind,
            inbound,
            version: 0,
            services: 0,
            user_agent: String::new(),
            start_height: -1,
            connected_since: now,
            last_send: now,
            last_recv: now,
            bytes_sent: 0,
            bytes_recv: 0,
        };
        if let Ok(mut peers) = self.peers.lock() {
            peers.insert(addr, entry);
        }
    }

    pub fn request_disconnect(&self, addr: SocketAddr) {
        if let Ok(mut requests) = self.disconnect_requests.lock() {
            requests.insert(addr);
        }
    }

    pub fn take_disconnect_request(&self, addr: SocketAddr) -> bool {
        self.disconnect_requests
            .lock()
            .map(|mut requests| requests.remove(&addr))
            .unwrap_or(false)
    }

    pub fn register(&self, addr: SocketAddr, kind: PeerKind) {
        self.register_internal(addr, kind, false);
    }

    pub fn register_inbound(&self, addr: SocketAddr, kind: PeerKind) {
        self.register_internal(addr, kind, true);
    }

    pub fn update_version(
        &self,
        addr: SocketAddr,
        version: i32,
        services: u64,
        user_agent: String,
        start_height: i32,
    ) {
        if let Ok(mut peers) = self.peers.lock() {
            if let Some(entry) = peers.get_mut(&addr) {
                entry.version = version;
                entry.services = services;
                entry.user_agent = user_agent;
                entry.start_height = start_height;
            }
        }
    }

    pub fn note_send(&self, addr: SocketAddr, bytes: usize) {
        let now = SystemTime::now();
        if let Ok(mut peers) = self.peers.lock() {
            if let Some(entry) = peers.get_mut(&addr) {
                entry.last_send = now;
                entry.bytes_sent = entry.bytes_sent.saturating_add(bytes as u64);
            }
        }
    }

    pub fn note_recv(&self, addr: SocketAddr, bytes: usize) {
        let now = SystemTime::now();
        if let Ok(mut peers) = self.peers.lock() {
            if let Some(entry) = peers.get_mut(&addr) {
                entry.last_recv = now;
                entry.bytes_recv = entry.bytes_recv.saturating_add(bytes as u64);
            }
        }
    }

    pub fn remove(&self, addr: SocketAddr) {
        if let Ok(mut peers) = self.peers.lock() {
            peers.remove(&addr);
        }
    }

    pub fn count(&self) -> usize {
        self.peers.lock().map(|peers| peers.len()).unwrap_or(0)
    }

    pub fn snapshot(&self) -> Vec<PeerInfoSnapshot> {
        let peers = match self.peers.lock() {
            Ok(peers) => peers,
            Err(_) => return Vec::new(),
        };
        peers
            .values()
            .cloned()
            .map(|entry| PeerInfoSnapshot {
                addr: entry.addr,
                kind: entry.kind,
                inbound: entry.inbound,
                version: entry.version,
                services: entry.services,
                user_agent: entry.user_agent,
                start_height: entry.start_height,
                connected_since: entry.connected_since,
                last_send: entry.last_send,
                last_recv: entry.last_recv,
                bytes_sent: entry.bytes_sent,
                bytes_recv: entry.bytes_recv,
            })
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct NetTotalsSnapshot {
    pub bytes_recv: u64,
    pub bytes_sent: u64,
    pub connections: usize,
}

#[derive(Debug, Default)]
pub struct NetTotals {
    bytes_recv: AtomicU64,
    bytes_sent: AtomicU64,
    connections: AtomicUsize,
}

impl NetTotals {
    pub fn add_recv(&self, bytes: usize) {
        self.bytes_recv.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn add_sent(&self, bytes: usize) {
        self.bytes_sent.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub fn inc_connections(&self) {
        self.connections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn dec_connections(&self) {
        self.connections
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                Some(value.saturating_sub(1))
            })
            .ok();
    }

    pub fn snapshot(&self) -> NetTotalsSnapshot {
        NetTotalsSnapshot {
            bytes_recv: self.bytes_recv.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            connections: self.connections.load(Ordering::Relaxed),
        }
    }
}

pub struct Peer {
    stream: TcpStream,
    magic: [u8; 4],
    remote_height: i32,
    remote_version: i32,
    remote_services: u64,
    remote_user_agent: String,
    kind: PeerKind,
    addr: SocketAddr,
    registry: Arc<PeerRegistry>,
    net_totals: Arc<NetTotals>,
}

impl Peer {
    pub async fn connect(
        addr: SocketAddr,
        magic: [u8; 4],
        kind: PeerKind,
        registry: Arc<PeerRegistry>,
        net_totals: Arc<NetTotals>,
    ) -> Result<Self, String> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|err| err.to_string())?;
        net_totals.inc_connections();
        registry.register(addr, kind);
        Ok(Self {
            stream,
            magic,
            remote_height: -1,
            remote_version: 0,
            remote_services: 0,
            remote_user_agent: String::new(),
            kind,
            addr,
            registry,
            net_totals,
        })
    }

    pub fn from_inbound(
        stream: TcpStream,
        addr: SocketAddr,
        magic: [u8; 4],
        kind: PeerKind,
        registry: Arc<PeerRegistry>,
        net_totals: Arc<NetTotals>,
    ) -> Self {
        net_totals.inc_connections();
        registry.register_inbound(addr, kind);
        Self {
            stream,
            magic,
            remote_height: -1,
            remote_version: 0,
            remote_services: 0,
            remote_user_agent: String::new(),
            kind,
            addr,
            registry,
            net_totals,
        }
    }

    pub async fn send_message(&mut self, command: &str, payload: &[u8]) -> Result<(), String> {
        let mut header = Vec::with_capacity(24 + payload.len());
        header.extend_from_slice(&self.magic);
        let mut command_bytes = [0u8; 12];
        let cmd = command.as_bytes();
        if cmd.len() > 12 {
            return Err("command too long".to_string());
        }
        command_bytes[..cmd.len()].copy_from_slice(cmd);
        header.extend_from_slice(&command_bytes);
        header.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        let checksum = sha256d(payload);
        header.extend_from_slice(&checksum[..4]);
        header.extend_from_slice(payload);
        timeout(
            Duration::from_secs(SEND_TIMEOUT_SECS),
            self.stream.write_all(&header),
        )
        .await
        .map_err(|_| "peer write timed out".to_string())?
        .map_err(|err| err.to_string())?;
        self.net_totals.add_sent(header.len());
        self.registry.note_send(self.addr, header.len());
        Ok(())
    }

    pub async fn read_message(&mut self) -> Result<(String, Vec<u8>), String> {
        let mut header = [0u8; 24];
        self.stream
            .read_exact(&mut header)
            .await
            .map_err(|err| err.to_string())?;
        if header[..4] != self.magic {
            return Err("invalid magic".to_string());
        }
        let command_raw = &header[4..16];
        let command = command_raw
            .iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as char)
            .collect::<String>();
        let length = u32::from_le_bytes([header[16], header[17], header[18], header[19]]) as usize;
        if length > MAX_PAYLOAD_SIZE {
            return Err("payload too large".to_string());
        }
        let checksum = &header[20..24];
        let mut payload = vec![0u8; length];
        self.stream
            .read_exact(&mut payload)
            .await
            .map_err(|err| err.to_string())?;
        let calc = sha256d(&payload);
        if checksum != &calc[..4] {
            return Err("invalid payload checksum".to_string());
        }
        let bytes = 24 + payload.len();
        self.net_totals.add_recv(bytes);
        self.registry.note_recv(self.addr, bytes);
        Ok((command, payload))
    }

    pub async fn handshake(&mut self, start_height: i32) -> Result<(), String> {
        let relay = matches!(self.kind, PeerKind::Relay);
        let payload = build_version_payload(start_height, relay);
        self.send_message("version", &payload).await?;

        let mut got_verack = false;
        let mut got_version = false;
        while !(got_verack && got_version) {
            let (command, payload) = timeout(
                Duration::from_secs(HANDSHAKE_READ_TIMEOUT_SECS),
                self.read_message(),
            )
            .await
            .map_err(|_| "peer handshake timed out".to_string())??;
            match command.as_str() {
                "version" => {
                    got_version = true;
                    self.send_message("verack", &[]).await?;
                    if let Ok(info) = parse_version(&payload) {
                        self.remote_height = info.start_height;
                        self.remote_version = info.version;
                        self.remote_services = info.services;
                        self.remote_user_agent = info.user_agent;
                        self.registry.update_version(
                            self.addr,
                            self.remote_version,
                            self.remote_services,
                            self.remote_user_agent.clone(),
                            self.remote_height,
                        );
                    }
                }
                "verack" => {
                    got_verack = true;
                }
                "ping" => {
                    self.send_message("pong", &payload).await?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    pub fn take_disconnect_request(&self) -> bool {
        self.registry.take_disconnect_request(self.addr)
    }

    pub fn remote_height(&self) -> i32 {
        self.remote_height
    }

    pub fn bump_remote_height(&mut self, height: i32) {
        self.remote_height = self.remote_height.max(height);
    }

    pub fn remote_version(&self) -> i32 {
        self.remote_version
    }

    pub fn remote_user_agent(&self) -> &str {
        &self.remote_user_agent
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub async fn send_getheaders(
        &mut self,
        locator: &[fluxd_consensus::Hash256],
    ) -> Result<(), String> {
        let payload = build_getheaders_payload(locator);
        self.send_message("getheaders", &payload).await
    }

    pub async fn send_getdata_blocks(
        &mut self,
        hashes: &[fluxd_consensus::Hash256],
    ) -> Result<(), String> {
        let payload = build_getdata_payload(hashes, MSG_BLOCK);
        self.send_message("getdata", &payload).await
    }

    pub async fn send_getdata_txs(
        &mut self,
        hashes: &[fluxd_consensus::Hash256],
    ) -> Result<(), String> {
        let payload = build_getdata_payload(hashes, MSG_TX);
        self.send_message("getdata", &payload).await
    }

    pub async fn send_getaddr(&mut self) -> Result<(), String> {
        self.send_message("getaddr", &[]).await
    }

    pub async fn send_mempool(&mut self) -> Result<(), String> {
        self.send_message("mempool", &[]).await
    }

    pub async fn send_inv_tx(&mut self, hashes: &[fluxd_consensus::Hash256]) -> Result<(), String> {
        let payload = build_inv_payload(hashes, MSG_TX);
        self.send_message("inv", &payload).await
    }

    pub async fn send_feefilter(&mut self, fee_rate_per_kb: i64) -> Result<(), String> {
        let payload = build_feefilter_payload(fee_rate_per_kb);
        self.send_message("feefilter", &payload).await
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.net_totals.dec_connections();
        self.registry.remove(self.addr);
    }
}

pub fn parse_headers(payload: &[u8]) -> Result<Vec<BlockHeader>, String> {
    let mut decoder = Decoder::new(payload);
    let count = decoder.read_varint().map_err(|err| err.to_string())?;
    let count = usize::try_from(count).map_err(|_| "header count too large".to_string())?;
    if count > MAX_HEADERS_RESULTS {
        return Err("header count too large".to_string());
    }
    let mut headers = Vec::with_capacity(count);
    for _ in 0..count {
        let header = BlockHeader::consensus_decode_from(&mut decoder, true)
            .map_err(|err| err.to_string())?;
        let _tx_count = decoder.read_varint().map_err(|err| err.to_string())?;
        headers.push(header);
    }
    Ok(headers)
}

#[derive(Clone, Debug)]
pub struct GetHeadersRequest {
    pub locator: Vec<fluxd_consensus::Hash256>,
    pub stop: fluxd_consensus::Hash256,
}

pub fn parse_getheaders(payload: &[u8]) -> Result<GetHeadersRequest, String> {
    let mut decoder = Decoder::new(payload);
    let _protocol_version = decoder.read_i32_le().map_err(|err| err.to_string())?;
    let count = decoder.read_varint().map_err(|err| err.to_string())?;
    let count = usize::try_from(count).map_err(|_| "locator count too large".to_string())?;
    if count > 128 {
        return Err("locator count too large".to_string());
    }
    let mut locator = Vec::with_capacity(count);
    for _ in 0..count {
        let hash = decoder.read_hash_le().map_err(|err| err.to_string())?;
        locator.push(hash);
    }
    let stop = decoder.read_hash_le().map_err(|err| err.to_string())?;
    if !decoder.is_empty() {
        return Err("trailing bytes in getheaders payload".to_string());
    }
    Ok(GetHeadersRequest { locator, stop })
}

pub fn parse_addr(payload: &[u8]) -> Result<Vec<SocketAddr>, String> {
    let mut decoder = Decoder::new(payload);
    let count = decoder.read_varint().map_err(|err| err.to_string())?;
    let count = usize::try_from(count).map_err(|_| "addr count too large".to_string())?;
    if count > MAX_ADDR_RESULTS {
        return Err("addr count too large".to_string());
    }
    let mut addrs = Vec::with_capacity(count);
    for _ in 0..count {
        let _time = decoder.read_u32_le().map_err(|err| err.to_string())?;
        let _services = decoder.read_u64_le().map_err(|err| err.to_string())?;
        let ip_bytes = decoder.read_fixed::<16>().map_err(|err| err.to_string())?;
        let port_bytes = decoder.read_bytes(2).map_err(|err| err.to_string())?;
        let port = u16::from_be_bytes([port_bytes[0], port_bytes[1]]);
        if port == 0 {
            continue;
        }
        let ip6 = Ipv6Addr::from(ip_bytes);
        let ip = if let Some(ip4) = ip6.to_ipv4_mapped() {
            IpAddr::V4(ip4)
        } else {
            IpAddr::V6(ip6)
        };
        if ip.is_unspecified() || ip.is_loopback() {
            continue;
        }
        addrs.push(SocketAddr::new(ip, port));
    }
    Ok(addrs)
}

pub fn build_headers_payload(headers: &[Vec<u8>]) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.write_varint(headers.len() as u64);
    for header_bytes in headers {
        encoder.write_bytes(header_bytes);
        encoder.write_varint(0);
    }
    encoder.into_inner()
}

pub fn build_addr_payload(addrs: &[SocketAddr], now: u32) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.write_varint(addrs.len() as u64);
    for addr in addrs {
        encoder.write_u32_le(now);
        encoder.write_u64_le(NODE_NETWORK);
        let ip_bytes = match addr.ip() {
            IpAddr::V4(ip4) => {
                let mut out = [0u8; 16];
                out[10] = 0xff;
                out[11] = 0xff;
                out[12..16].copy_from_slice(&ip4.octets());
                out
            }
            IpAddr::V6(ip6) => ip6.octets(),
        };
        encoder.write_bytes(&ip_bytes);
        encoder.write_bytes(&addr.port().to_be_bytes());
    }
    encoder.into_inner()
}

#[derive(Clone, Copy, Debug)]
pub struct InventoryVector {
    pub inv_type: u32,
    pub hash: fluxd_consensus::Hash256,
}

pub fn parse_inv(payload: &[u8]) -> Result<Vec<InventoryVector>, String> {
    let mut decoder = Decoder::new(payload);
    let count = decoder.read_varint().map_err(|err| err.to_string())?;
    let count = usize::try_from(count).map_err(|_| "inv count too large".to_string())?;
    if count > MAX_INV_RESULTS {
        return Err("inv count too large".to_string());
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let inv_type = decoder.read_u32_le().map_err(|err| err.to_string())?;
        let hash = decoder.read_hash_le().map_err(|err| err.to_string())?;
        out.push(InventoryVector { inv_type, hash });
    }
    if !decoder.is_empty() {
        return Err("trailing bytes in inv payload".to_string());
    }
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct RejectMessage {
    pub message: String,
    pub code: u8,
    pub reason: String,
    pub data: Option<fluxd_consensus::Hash256>,
}

pub fn parse_reject(payload: &[u8]) -> Result<RejectMessage, String> {
    let mut decoder = Decoder::new(payload);
    let message = decoder.read_var_str().map_err(|err| err.to_string())?;
    let code = decoder.read_u8().map_err(|err| err.to_string())?;
    let reason = decoder.read_var_str().map_err(|err| err.to_string())?;
    let data = match decoder.remaining() {
        0 => None,
        32 => Some(decoder.read_hash_le().map_err(|err| err.to_string())?),
        _ => None,
    };
    Ok(RejectMessage {
        message,
        code,
        reason,
        data,
    })
}

pub fn build_inv_payload(hashes: &[fluxd_consensus::Hash256], inv_type: u32) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.write_varint(hashes.len() as u64);
    for hash in hashes {
        encoder.write_u32_le(inv_type);
        encoder.write_hash_le(hash);
    }
    encoder.into_inner()
}

pub fn parse_feefilter(payload: &[u8]) -> Result<i64, String> {
    let mut decoder = Decoder::new(payload);
    let raw = decoder.read_u64_le().map_err(|err| err.to_string())?;
    if !decoder.is_empty() {
        return Err("trailing bytes in feefilter payload".to_string());
    }
    let fee = i64::try_from(raw).unwrap_or(i64::MAX);
    Ok(fee.max(0))
}

fn build_version_payload(start_height: i32, relay: bool) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.write_i32_le(PROTOCOL_VERSION);
    encoder.write_u64_le(NODE_NETWORK);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    encoder.write_i64_le(timestamp);
    write_net_addr(&mut encoder, NODE_NETWORK, [0u8; 16], 0);
    write_net_addr(&mut encoder, NODE_NETWORK, [0u8; 16], 0);
    encoder.write_u64_le(rand::random());
    encoder.write_var_str(USER_AGENT);
    encoder.write_i32_le(start_height);
    encoder.write_u8(relay as u8);
    encoder.into_inner()
}

fn build_feefilter_payload(fee_rate_per_kb: i64) -> Vec<u8> {
    let fee_rate_per_kb = fee_rate_per_kb.max(0);
    let mut encoder = Encoder::new();
    encoder.write_i64_le(fee_rate_per_kb);
    encoder.into_inner()
}

fn build_getheaders_payload(locator: &[fluxd_consensus::Hash256]) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.write_i32_le(PROTOCOL_VERSION);
    encoder.write_varint(locator.len() as u64);
    for hash in locator {
        encoder.write_hash_le(hash);
    }
    encoder.write_hash_le(&[0u8; 32]);
    encoder.into_inner()
}

fn build_getdata_payload(hashes: &[fluxd_consensus::Hash256], inv_type: u32) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.write_varint(hashes.len() as u64);
    for hash in hashes {
        encoder.write_u32_le(inv_type);
        encoder.write_hash_le(hash);
    }
    encoder.into_inner()
}

fn write_net_addr(encoder: &mut Encoder, services: u64, ip: [u8; 16], port: u16) {
    encoder.write_u64_le(services);
    encoder.write_bytes(&ip);
    encoder.write_bytes(&port.to_be_bytes());
}

struct VersionInfo {
    version: i32,
    services: u64,
    user_agent: String,
    start_height: i32,
}

fn parse_version(payload: &[u8]) -> Result<VersionInfo, String> {
    let mut decoder = Decoder::new(payload);
    let version = decoder.read_i32_le().map_err(|err| err.to_string())?;
    let services = decoder.read_u64_le().map_err(|err| err.to_string())?;
    let _timestamp = decoder.read_i64_le().map_err(|err| err.to_string())?;
    read_net_addr(&mut decoder)?;
    read_net_addr(&mut decoder)?;
    let _nonce = decoder.read_u64_le().map_err(|err| err.to_string())?;
    let user_agent = decoder.read_var_str().map_err(|err| err.to_string())?;
    let start_height = decoder.read_i32_le().map_err(|err| err.to_string())?;
    Ok(VersionInfo {
        version,
        services,
        user_agent,
        start_height,
    })
}

fn read_net_addr(decoder: &mut Decoder) -> Result<(), String> {
    let _services = decoder.read_u64_le().map_err(|err| err.to_string())?;
    let _ip = decoder.read_fixed::<16>().map_err(|err| err.to_string())?;
    let _port = decoder.read_bytes(2).map_err(|err| err.to_string())?;
    Ok(())
}
