use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::ValidationFlags;
use fluxd_consensus::params::ChainParams;
use fluxd_consensus::Hash256;
use fluxd_primitives::transaction::Transaction;
use fluxd_storage::KeyValueStore;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::time::{timeout, Duration};

use crate::mempool;
use crate::p2p::{
    build_addr_payload, build_headers_payload, build_inv_payload, parse_addr, parse_feefilter,
    parse_getheaders, parse_inv, parse_reject, Peer, PeerKind, MSG_BLOCK, MSG_TX,
};
use crate::stats::MempoolMetrics;

const INBOUND_READ_TIMEOUT_SECS: u64 = 120;
const MAX_INBOUND_GETDATA: usize = 256;
const MAX_INBOUND_ADDR: usize = 1000;
const TX_GETDATA_BATCH: usize = 128;
const TX_KNOWN_CAP: usize = 50_000;
const MAX_INBOUND_TX_REQUEST: usize = 2048;

pub async fn serve_inbound_p2p<S: KeyValueStore + Send + Sync + 'static>(
    listener: TcpListener,
    chainstate: Arc<ChainState<S>>,
    params: Arc<ChainParams>,
    addr_book: Arc<crate::AddrBook>,
    peer_registry: Arc<crate::p2p::PeerRegistry>,
    net_totals: Arc<crate::p2p::NetTotals>,
    max_connections: usize,
    mempool: Arc<Mutex<mempool::Mempool>>,
    mempool_policy: Arc<mempool::MempoolPolicy>,
    mempool_metrics: Arc<MempoolMetrics>,
    fee_estimator: Arc<Mutex<crate::fee_estimator::FeeEstimator>>,
    flags: ValidationFlags,
    tx_announce: broadcast::Sender<Hash256>,
) -> Result<(), String> {
    let local_addr = listener.local_addr().ok();
    if let Some(addr) = local_addr {
        log_info!("P2P listening on {}", addr);
    } else {
        log_info!("P2P listening");
    }

    loop {
        let (stream, remote_addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                log_warn!("p2p accept failed: {err}");
                continue;
            }
        };

        let connections = net_totals.snapshot().connections;
        if connections >= max_connections {
            log_debug!(
                "Refusing inbound peer {}: maxconnections reached ({}/{})",
                remote_addr,
                connections,
                max_connections
            );
            drop(stream);
            continue;
        }

        let magic = params.message_start;
        let chainstate = Arc::clone(&chainstate);
        let params = Arc::clone(&params);
        let addr_book = Arc::clone(&addr_book);
        let peer_registry = Arc::clone(&peer_registry);
        let net_totals = Arc::clone(&net_totals);
        let mempool = Arc::clone(&mempool);
        let mempool_policy = Arc::clone(&mempool_policy);
        let mempool_metrics = Arc::clone(&mempool_metrics);
        let fee_estimator = Arc::clone(&fee_estimator);
        let flags = flags.clone();
        let tx_announce = tx_announce.clone();

        tokio::spawn(async move {
            if let Err(err) = handle_inbound_peer(
                stream,
                remote_addr,
                magic,
                chainstate,
                params,
                addr_book,
                peer_registry,
                net_totals,
                mempool,
                mempool_policy,
                mempool_metrics,
                fee_estimator,
                flags,
                tx_announce,
            )
            .await
            {
                log_debug!("inbound peer {} closed: {err}", remote_addr);
            }
        });
    }
}

pub async fn bind_inbound_p2p(bind_addr: SocketAddr) -> Result<TcpListener, String> {
    TcpListener::bind(bind_addr)
        .await
        .map_err(|err| format!("failed to bind p2p listener {bind_addr}: {err}"))
}

async fn handle_inbound_peer<S: KeyValueStore + Send + Sync + 'static>(
    stream: tokio::net::TcpStream,
    remote_addr: SocketAddr,
    magic: [u8; 4],
    chainstate: Arc<ChainState<S>>,
    params: Arc<ChainParams>,
    addr_book: Arc<crate::AddrBook>,
    peer_registry: Arc<crate::p2p::PeerRegistry>,
    net_totals: Arc<crate::p2p::NetTotals>,
    mempool: Arc<Mutex<mempool::Mempool>>,
    mempool_policy: Arc<mempool::MempoolPolicy>,
    mempool_metrics: Arc<MempoolMetrics>,
    fee_estimator: Arc<Mutex<crate::fee_estimator::FeeEstimator>>,
    flags: ValidationFlags,
    tx_announce: broadcast::Sender<Hash256>,
) -> Result<(), String> {
    let mut peer = Peer::from_inbound(
        stream,
        remote_addr,
        magic,
        PeerKind::Relay,
        peer_registry,
        net_totals,
    );

    let start_height = crate::start_height(chainstate.as_ref()).unwrap_or(0);
    let handshake = timeout(
        Duration::from_secs(crate::DEFAULT_HANDSHAKE_TIMEOUT_SECS),
        peer.handshake(start_height),
    )
    .await;
    match handshake {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(format!("handshake failed: {err}")),
        Err(_) => return Err("handshake timed out".to_string()),
    }

    let mut announce_rx = tx_announce.subscribe();
    let mut known: HashSet<Hash256> = HashSet::new();
    let mut requested: HashSet<Hash256> = HashSet::new();
    let mut peer_fee_filter_per_kb: i64 = 0;

    let _ = peer
        .send_feefilter(mempool_policy.min_relay_fee_per_kb)
        .await;

    loop {
        if peer.take_disconnect_request() {
            break;
        }

        tokio::select! {
            msg = timeout(Duration::from_secs(INBOUND_READ_TIMEOUT_SECS), peer.read_message()) => {
                let (command, payload) = match msg {
                    Ok(Ok(message)) => message,
                    Ok(Err(err)) => return Err(err),
                    Err(_) => return Err("peer read timed out".to_string()),
                };

                handle_inbound_message(
                    &mut peer,
                    remote_addr,
                    &command,
                    &payload,
                    chainstate.as_ref(),
                    params.as_ref(),
                    addr_book.as_ref(),
                    mempool.as_ref(),
                    mempool_policy.as_ref(),
                    mempool_metrics.as_ref(),
                    fee_estimator.as_ref(),
                    &flags,
                    &tx_announce,
                    &mut known,
                    &mut requested,
                    &mut peer_fee_filter_per_kb,
                ).await?;
            }
            announced = announce_rx.recv() => {
                match announced {
                    Ok(txid) => {
                        if known.contains(&txid) {
                            continue;
                        }
                        if should_announce_tx(mempool.as_ref(), &txid, peer_fee_filter_per_kb) {
                            let _ = touch_known(&mut known, txid);
                            peer.send_inv_tx(&[txid]).await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        }
    }

    Ok(())
}

async fn handle_inbound_message<S: KeyValueStore>(
    peer: &mut Peer,
    remote_addr: SocketAddr,
    command: &str,
    payload: &[u8],
    chainstate: &ChainState<S>,
    params: &ChainParams,
    addr_book: &crate::AddrBook,
    mempool: &Mutex<mempool::Mempool>,
    mempool_policy: &mempool::MempoolPolicy,
    mempool_metrics: &MempoolMetrics,
    fee_estimator: &Mutex<crate::fee_estimator::FeeEstimator>,
    flags: &ValidationFlags,
    tx_announce: &broadcast::Sender<Hash256>,
    known: &mut HashSet<Hash256>,
    requested: &mut HashSet<Hash256>,
    peer_fee_filter_per_kb: &mut i64,
) -> Result<(), String> {
    match command {
        "ping" => peer.send_message("pong", payload).await?,
        "getaddr" => {
            let mut sample = addr_book.sample(MAX_INBOUND_ADDR);
            if sample.len() > MAX_INBOUND_ADDR {
                sample.truncate(MAX_INBOUND_ADDR);
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|value| value.as_secs() as u32)
                .unwrap_or(0);
            let payload = build_addr_payload(&sample, now);
            peer.send_message("addr", &payload).await?;
        }
        "addr" => {
            if let Ok(addrs) = parse_addr(payload) {
                let inserted = addr_book.insert_many(addrs);
                if inserted > 0 {
                    log_debug!(
                        "Addr discovery: learned {} addrs from {} (inbound)",
                        inserted,
                        remote_addr
                    );
                }
            }
        }
        "getheaders" => handle_getheaders(peer, chainstate, params, payload).await?,
        "inv" => handle_inv(peer, mempool, payload, known, requested).await?,
        "tx" => {
            handle_tx(
                chainstate,
                params,
                mempool,
                mempool_policy,
                mempool_metrics,
                fee_estimator,
                flags,
                tx_announce,
                known,
                requested,
                payload,
            )?;
        }
        "mempool" => {
            let txids = mempool_txids(mempool, TX_KNOWN_CAP, *peer_fee_filter_per_kb)?;
            for txid in &txids {
                let _ = touch_known(known, *txid);
            }
            peer.send_inv_tx(&txids).await?;
        }
        "feefilter" => {
            if let Ok(filter) = parse_feefilter(payload) {
                *peer_fee_filter_per_kb = filter;
            }
        }
        "getdata" => handle_getdata(peer, chainstate, mempool, payload).await?,
        "notfound" => {
            if let Ok(vectors) = parse_inv(payload) {
                for vector in vectors {
                    if vector.inv_type == MSG_TX {
                        let _ = requested.remove(&vector.hash);
                    }
                }
            }
        }
        "reject" => {
            if let Ok(reject) = parse_reject(payload) {
                if reject.message == "tx" {
                    if let Some(txid) = reject.data {
                        let _ = requested.remove(&txid);
                    }
                }
            }
        }
        "version" => peer.send_message("verack", &[]).await?,
        _ => {}
    }
    Ok(())
}

async fn handle_inv(
    peer: &mut Peer,
    mempool: &Mutex<mempool::Mempool>,
    payload: &[u8],
    known: &mut HashSet<Hash256>,
    requested: &mut HashSet<Hash256>,
) -> Result<(), String> {
    let vectors = parse_inv(payload)?;
    let mut to_request = Vec::new();
    {
        let guard = mempool
            .lock()
            .map_err(|_| "mempool lock poisoned".to_string())?;
        for vector in vectors {
            if vector.inv_type != MSG_TX {
                continue;
            }
            let _ = touch_known(known, vector.hash);
            if guard.contains(&vector.hash) {
                continue;
            }
            if requested.insert(vector.hash) {
                to_request.push(vector.hash);
                if to_request.len() >= MAX_INBOUND_TX_REQUEST {
                    break;
                }
            }
        }
    }
    request_txids(peer, &to_request).await?;
    Ok(())
}

fn handle_tx<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
    mempool: &Mutex<mempool::Mempool>,
    mempool_policy: &mempool::MempoolPolicy,
    mempool_metrics: &MempoolMetrics,
    fee_estimator: &Mutex<crate::fee_estimator::FeeEstimator>,
    flags: &ValidationFlags,
    tx_announce: &broadcast::Sender<Hash256>,
    known: &mut HashSet<Hash256>,
    requested: &mut HashSet<Hash256>,
    payload: &[u8],
) -> Result<(), String> {
    let tx = Transaction::consensus_decode(payload).map_err(|err| err.to_string())?;
    let txid = tx.txid().map_err(|err| err.to_string())?;
    let raw = payload.to_vec();

    let mempool_prevouts = {
        let guard = mempool
            .lock()
            .map_err(|_| "mempool lock poisoned".to_string())?;
        if guard.contains(&txid) {
            let _ = touch_known(known, txid);
            let _ = requested.remove(&txid);
            return Ok(());
        }
        guard.prevouts_for_tx(&tx)
    };

    let entry = match mempool::build_mempool_entry(
        chainstate,
        &mempool_prevouts,
        params,
        flags,
        mempool_policy,
        tx,
        raw,
    ) {
        Ok(entry) => entry,
        Err(err) => {
            if err.kind == mempool::MempoolErrorKind::MissingInput {
                let _ = requested.remove(&txid);
            }
            if err.kind == mempool::MempoolErrorKind::Internal {
                log_warn!(
                    "mempool reject {}: {}",
                    crate::stats::hash256_to_hex(&txid),
                    err
                );
            }
            mempool_metrics.note_relay_reject();
            return Ok(());
        }
    };

    {
        let mut guard = mempool
            .lock()
            .map_err(|_| "mempool lock poisoned".to_string())?;
        if guard.contains(&txid) {
            return Ok(());
        }
        let should_observe_fee = entry.tx.fluxnode.is_none();
        let entry_fee = entry.fee;
        let entry_size = entry.size();
        match guard.insert(entry) {
            Ok(outcome) => {
                mempool_metrics.note_relay_accept();
                if outcome.evicted > 0 {
                    mempool_metrics.note_evicted(outcome.evicted, outcome.evicted_bytes);
                }
                if should_observe_fee {
                    if let Ok(mut estimator) = fee_estimator.lock() {
                        estimator.observe_tx(entry_fee, entry_size);
                    }
                }
            }
            Err(err) => {
                if err.kind == mempool::MempoolErrorKind::Internal {
                    log_warn!(
                        "mempool insert failed {}: {}",
                        crate::stats::hash256_to_hex(&txid),
                        err
                    );
                }
                mempool_metrics.note_relay_reject();
                return Ok(());
            }
        }
    }

    let _ = touch_known(known, txid);
    let _ = requested.remove(&txid);
    let _ = tx_announce.send(txid);
    Ok(())
}

async fn handle_getheaders<S: KeyValueStore>(
    peer: &mut Peer,
    chainstate: &ChainState<S>,
    params: &ChainParams,
    payload: &[u8],
) -> Result<(), String> {
    let request = parse_getheaders(payload)?;
    let (tip_hash, tip_height) = match chainstate.best_header().map_err(|err| err.to_string())? {
        Some(tip) => (tip.hash, tip.height),
        None => (params.consensus.hash_genesis_block, 0),
    };

    let mut anchor_height = 0i32;
    for candidate in request.locator {
        let Some(entry) = chainstate
            .header_entry(&candidate)
            .map_err(|err| err.to_string())?
        else {
            continue;
        };
        if entry.height < 0 || entry.height > tip_height {
            continue;
        }
        let Some(ancestor) = chainstate
            .header_ancestor_hash(&tip_hash, entry.height)
            .map_err(|err| err.to_string())?
        else {
            continue;
        };
        if ancestor == candidate {
            anchor_height = entry.height;
            break;
        }
    }

    let start_height = anchor_height.saturating_add(1);
    if start_height > tip_height {
        peer.send_message("headers", &build_headers_payload(&[]))
            .await?;
        return Ok(());
    }

    let mut end_height = start_height
        .saturating_add(160)
        .saturating_sub(1)
        .min(tip_height);
    if request.stop != [0u8; 32] {
        if let Some(stop_entry) = chainstate
            .header_entry(&request.stop)
            .map_err(|err| err.to_string())?
        {
            if stop_entry.height >= start_height && stop_entry.height <= tip_height {
                if let Some(ancestor) = chainstate
                    .header_ancestor_hash(&tip_hash, stop_entry.height)
                    .map_err(|err| err.to_string())?
                {
                    if ancestor == request.stop {
                        end_height = end_height.min(stop_entry.height);
                    }
                }
            }
        }
    }

    let mut headers: Vec<Vec<u8>> = Vec::new();
    for height in start_height..=end_height {
        let hash = match chainstate
            .header_ancestor_hash(&tip_hash, height)
            .map_err(|err| err.to_string())?
        {
            Some(hash) => hash,
            None => break,
        };
        let bytes = match chainstate
            .block_header_bytes(&hash)
            .map_err(|err| err.to_string())?
        {
            Some(bytes) => bytes,
            None => break,
        };
        headers.push(bytes);
    }

    peer.send_message("headers", &build_headers_payload(&headers))
        .await?;
    Ok(())
}

async fn handle_getdata<S: KeyValueStore>(
    peer: &mut Peer,
    chainstate: &ChainState<S>,
    mempool: &Mutex<mempool::Mempool>,
    payload: &[u8],
) -> Result<(), String> {
    let invs = parse_inv(payload)?;
    let mut processed = 0usize;
    let mut missing_txs: Vec<Hash256> = Vec::new();
    for inv in invs {
        if processed >= MAX_INBOUND_GETDATA {
            break;
        }
        processed += 1;
        match inv.inv_type {
            MSG_BLOCK => {
                let location = match chainstate
                    .block_location(&inv.hash)
                    .map_err(|err| err.to_string())?
                {
                    Some(location) => location,
                    None => continue,
                };
                let block_bytes = chainstate
                    .read_block(location)
                    .map_err(|err| err.to_string())?;
                peer.send_message("block", &block_bytes).await?;
            }
            MSG_TX => {
                let raw = {
                    let guard = mempool
                        .lock()
                        .map_err(|_| "mempool lock poisoned".to_string())?;
                    guard.get(&inv.hash).map(|entry| entry.raw.clone())
                };
                if let Some(raw) = raw {
                    peer.send_message("tx", &raw).await?;
                } else {
                    missing_txs.push(inv.hash);
                }
            }
            _ => {}
        }
    }

    if !missing_txs.is_empty() {
        peer.send_message("notfound", &build_inv_payload(&missing_txs, MSG_TX))
            .await?;
    }
    Ok(())
}

async fn request_txids(peer: &mut Peer, txids: &[Hash256]) -> Result<(), String> {
    for chunk in txids.chunks(TX_GETDATA_BATCH) {
        peer.send_getdata_txs(chunk).await?;
    }
    Ok(())
}

fn should_announce_tx(
    mempool: &Mutex<mempool::Mempool>,
    txid: &Hash256,
    peer_fee_filter_per_kb: i64,
) -> bool {
    let peer_fee_filter_per_kb = peer_fee_filter_per_kb.max(0);
    let guard = match mempool.lock() {
        Ok(guard) => guard,
        Err(_) => return false,
    };
    let entry = match guard.get(txid) {
        Some(entry) => entry,
        None => return false,
    };
    if peer_fee_filter_per_kb == 0 {
        return true;
    }
    fee_rate_per_kb(entry.fee, entry.size()) >= peer_fee_filter_per_kb
}

fn mempool_txids(
    mempool: &Mutex<mempool::Mempool>,
    limit: usize,
    peer_fee_filter_per_kb: i64,
) -> Result<Vec<Hash256>, String> {
    let guard = mempool
        .lock()
        .map_err(|_| "mempool lock poisoned".to_string())?;
    let peer_fee_filter_per_kb = peer_fee_filter_per_kb.max(0);
    let mut out = Vec::new();
    for entry in guard.entries() {
        if out.len() >= limit {
            break;
        }
        if peer_fee_filter_per_kb > 0
            && fee_rate_per_kb(entry.fee, entry.size()) < peer_fee_filter_per_kb
        {
            continue;
        }
        out.push(entry.txid);
    }
    Ok(out)
}

fn touch_known(known: &mut HashSet<Hash256>, txid: Hash256) -> bool {
    if known.len() >= TX_KNOWN_CAP {
        known.clear();
    }
    known.insert(txid)
}

fn fee_rate_per_kb(fee: i64, size: usize) -> i64 {
    let size = i64::try_from(size.max(1)).unwrap_or(i64::MAX);
    fee.saturating_mul(1000).saturating_div(size)
}
