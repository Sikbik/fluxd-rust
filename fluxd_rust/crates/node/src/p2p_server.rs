use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use fluxd_chainstate::state::ChainState;
use fluxd_consensus::params::ChainParams;
use fluxd_storage::KeyValueStore;
use tokio::net::TcpListener;
use tokio::time::{timeout, Duration};

use crate::p2p::{
    build_addr_payload, build_headers_payload, parse_addr, parse_getheaders, parse_inv, Peer,
    PeerKind, MSG_BLOCK,
};

const INBOUND_READ_TIMEOUT_SECS: u64 = 120;
const MAX_INBOUND_GETDATA: usize = 256;
const MAX_INBOUND_ADDR: usize = 1000;

pub async fn serve_inbound_p2p<S: KeyValueStore + Send + Sync + 'static>(
    listener: TcpListener,
    chainstate: Arc<ChainState<S>>,
    params: Arc<ChainParams>,
    addr_book: Arc<crate::AddrBook>,
    peer_registry: Arc<crate::p2p::PeerRegistry>,
    net_totals: Arc<crate::p2p::NetTotals>,
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

        let magic = params.message_start;
        let chainstate = Arc::clone(&chainstate);
        let params = Arc::clone(&params);
        let addr_book = Arc::clone(&addr_book);
        let peer_registry = Arc::clone(&peer_registry);
        let net_totals = Arc::clone(&net_totals);

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

    loop {
        if peer.take_disconnect_request() {
            break;
        }

        let (command, payload) = match timeout(
            Duration::from_secs(INBOUND_READ_TIMEOUT_SECS),
            peer.read_message(),
        )
        .await
        {
            Ok(Ok(message)) => message,
            Ok(Err(err)) => return Err(err),
            Err(_) => return Err("peer read timed out".to_string()),
        };

        match command.as_str() {
            "ping" => {
                peer.send_message("pong", &payload).await?;
            }
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
                if let Ok(addrs) = parse_addr(&payload) {
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
            "getheaders" => {
                handle_getheaders(&mut peer, chainstate.as_ref(), params.as_ref(), &payload)
                    .await?;
            }
            "getdata" => {
                handle_getdata_blocks(&mut peer, chainstate.as_ref(), &payload).await?;
            }
            _ => {}
        }
    }

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

async fn handle_getdata_blocks<S: KeyValueStore>(
    peer: &mut Peer,
    chainstate: &ChainState<S>,
    payload: &[u8],
) -> Result<(), String> {
    let invs = parse_inv(payload)?;
    let mut served = 0usize;
    for inv in invs {
        if served >= MAX_INBOUND_GETDATA {
            break;
        }
        if inv.inv_type != MSG_BLOCK {
            continue;
        }
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
        served += 1;
    }
    Ok(())
}
