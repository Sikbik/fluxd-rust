use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::ValidationFlags;
use fluxd_consensus::params::ChainParams;
use fluxd_consensus::Hash256;
use fluxd_primitives::transaction::Transaction;
use fluxd_storage::KeyValueStore;
use tokio::sync::broadcast;
use tokio::task::JoinSet;

use crate::mempool;
use crate::p2p::{parse_inv, InventoryVector, Peer, MSG_TX};

const TX_GETDATA_BATCH: usize = 128;
const TX_KNOWN_CAP: usize = 50_000;
const TX_RECONNECT_DELAY_SECS: u64 = 3;
const TX_REJECT_LOG_INTERVAL_SECS: u64 = 60;

pub async fn tx_relay_loop<S: KeyValueStore + 'static>(
    chainstate: Arc<ChainState<S>>,
    params: Arc<ChainParams>,
    addr_book: Arc<crate::AddrBook>,
    peer_ctx: crate::PeerContext,
    mempool: Arc<Mutex<mempool::Mempool>>,
    flags: ValidationFlags,
    tx_announce: broadcast::Sender<Hash256>,
    peer_target: usize,
) -> Result<(), String> {
    if peer_target == 0 {
        return Ok(());
    }

    let mut join_set: JoinSet<Result<(), String>> = JoinSet::new();
    loop {
        while join_set.len() < peer_target {
            let start_height = crate::start_height(chainstate.as_ref())?;
            let min_height = chainstate
                .best_header()
                .map_err(|err| err.to_string())?
                .map(|tip| tip.height)
                .unwrap_or(start_height);
            let need = peer_target - join_set.len();
            match crate::connect_to_peers(
                params.as_ref(),
                need,
                start_height,
                min_height,
                Some(addr_book.as_ref()),
                &peer_ctx,
            )
            .await
            {
                Ok(peers) => {
                    for peer in peers {
                        let chainstate = Arc::clone(&chainstate);
                        let params = Arc::clone(&params);
                        let mempool = Arc::clone(&mempool);
                        let flags = flags.clone();
                        let tx_announce = tx_announce.clone();
                        join_set.spawn(async move {
                            let addr = peer.addr();
                            let result = tx_relay_peer(
                                peer,
                                chainstate,
                                params,
                                mempool,
                                flags,
                                tx_announce,
                            )
                            .await;
                            if let Err(err) = &result {
                                eprintln!("tx relay peer {addr} stopped: {err}");
                            }
                            result
                        });
                    }
                }
                Err(err) => {
                    eprintln!("tx relay connect failed: {err}");
                    tokio::time::sleep(Duration::from_secs(TX_RECONNECT_DELAY_SECS)).await;
                    break;
                }
            }
        }

        match join_set.join_next().await {
            Some(Ok(_)) => {}
            Some(Err(err)) => {
                eprintln!("tx relay join failed: {err}");
            }
            None => {
                tokio::time::sleep(Duration::from_secs(TX_RECONNECT_DELAY_SECS)).await;
            }
        }
    }
}

async fn tx_relay_peer<S: KeyValueStore>(
    mut peer: Peer,
    chainstate: Arc<ChainState<S>>,
    params: Arc<ChainParams>,
    mempool: Arc<Mutex<mempool::Mempool>>,
    flags: ValidationFlags,
    tx_announce: broadcast::Sender<Hash256>,
) -> Result<(), String> {
    let mut announce_rx = tx_announce.subscribe();
    let mut known: HashSet<Hash256> = HashSet::new();
    let mut requested: HashSet<Hash256> = HashSet::new();
    let mut reject_stats = TxRejectStats::new();

    let _ = peer.send_mempool().await;

    loop {
        tokio::select! {
            msg = peer.read_message() => {
                let (command, payload) = msg?;
                handle_peer_message(
                    &mut peer,
                    &command,
                    &payload,
                    chainstate.as_ref(),
                    params.as_ref(),
                    mempool.as_ref(),
                    &flags,
                    &tx_announce,
                    &mut known,
                    &mut requested,
                    &mut reject_stats,
                )
                .await?;
            }
            announced = announce_rx.recv() => {
                match announced {
                    Ok(txid) => {
                        if known.contains(&txid) {
                            continue;
                        }
                        if mempool_has_tx(mempool.as_ref(), &txid) {
                            if touch_known(&mut known, txid) {
                                peer.send_inv_tx(&[txid]).await?;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        }
    }
}

async fn handle_peer_message<S: KeyValueStore>(
    peer: &mut Peer,
    command: &str,
    payload: &[u8],
    chainstate: &ChainState<S>,
    params: &ChainParams,
    mempool: &Mutex<mempool::Mempool>,
    flags: &ValidationFlags,
    tx_announce: &broadcast::Sender<Hash256>,
    known: &mut HashSet<Hash256>,
    requested: &mut HashSet<Hash256>,
    reject_stats: &mut TxRejectStats,
) -> Result<(), String> {
    match command {
        "inv" => {
            let vectors = parse_inv(payload)?;
            let txids = inventory_txids(&vectors);
            if txids.is_empty() {
                return Ok(());
            }
            let mut to_request = Vec::new();
            {
                let guard = mempool
                    .lock()
                    .map_err(|_| "mempool lock poisoned".to_string())?;
                for txid in txids {
                    let _ = touch_known(known, txid);
                    if guard.contains(&txid) {
                        continue;
                    }
                    if requested.insert(txid) {
                        to_request.push(txid);
                    }
                }
            }
            request_txids(peer, &to_request).await?;
        }
        "tx" => {
            let tx = Transaction::consensus_decode(payload).map_err(|err| err.to_string())?;
            let txid = tx.txid().map_err(|err| err.to_string())?;
            let raw = payload.to_vec();

            let entry = match mempool::build_mempool_entry(chainstate, params, flags, tx, raw) {
                Ok(entry) => entry,
                Err(err) => {
                    if err.kind == mempool::MempoolErrorKind::Internal {
                        eprintln!(
                            "mempool reject {}: {}",
                            crate::stats::hash256_to_hex(&txid),
                            err
                        );
                    }
                    reject_stats.note_build_error(err.kind);
                    reject_stats.maybe_log(peer.addr());
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
                if let Err(err) = guard.insert(entry) {
                    if err.kind == mempool::MempoolErrorKind::Internal {
                        eprintln!(
                            "mempool insert failed {}: {}",
                            crate::stats::hash256_to_hex(&txid),
                            err
                        );
                    }
                    reject_stats.note_insert_error(err.kind);
                    reject_stats.maybe_log(peer.addr());
                    return Ok(());
                }
            }

            let _ = touch_known(known, txid);
            let _ = requested.remove(&txid);
            let _ = tx_announce.send(txid);
        }
        "getdata" => {
            let vectors = parse_inv(payload)?;
            let txids = inventory_txids(&vectors);
            if txids.is_empty() {
                return Ok(());
            }
            let raws = mempool_raws(mempool, &txids)?;
            for raw in raws {
                peer.send_message("tx", &raw).await?;
            }
        }
        "mempool" => {
            let txids = mempool_txids(mempool, TX_KNOWN_CAP)?;
            if !txids.is_empty() {
                for txid in &txids {
                    let _ = touch_known(known, *txid);
                }
                peer.send_inv_tx(&txids).await?;
            }
        }
        "notfound" => {}
        "ping" => peer.send_message("pong", payload).await?,
        "version" => peer.send_message("verack", &[]).await?,
        _ => {}
    }
    Ok(())
}

fn inventory_txids(vectors: &[InventoryVector]) -> Vec<Hash256> {
    vectors
        .iter()
        .filter(|vector| vector.inv_type == MSG_TX)
        .map(|vector| vector.hash)
        .collect()
}

async fn request_txids(peer: &mut Peer, txids: &[Hash256]) -> Result<(), String> {
    for chunk in txids.chunks(TX_GETDATA_BATCH) {
        peer.send_getdata_txs(chunk).await?;
    }
    Ok(())
}

fn mempool_has_tx(mempool: &Mutex<mempool::Mempool>, txid: &Hash256) -> bool {
    mempool
        .lock()
        .map(|guard| guard.contains(txid))
        .unwrap_or(false)
}

fn mempool_raws(
    mempool: &Mutex<mempool::Mempool>,
    txids: &[Hash256],
) -> Result<Vec<Vec<u8>>, String> {
    let guard = mempool
        .lock()
        .map_err(|_| "mempool lock poisoned".to_string())?;
    let mut raws = Vec::new();
    for txid in txids {
        if let Some(entry) = guard.get(txid) {
            raws.push(entry.raw.clone());
        }
    }
    Ok(raws)
}

fn mempool_txids(mempool: &Mutex<mempool::Mempool>, limit: usize) -> Result<Vec<Hash256>, String> {
    let guard = mempool
        .lock()
        .map_err(|_| "mempool lock poisoned".to_string())?;
    let mut out = Vec::new();
    out.extend(guard.entries().map(|entry| entry.txid).take(limit));
    Ok(out)
}

fn touch_known(known: &mut HashSet<Hash256>, txid: Hash256) -> bool {
    if known.len() >= TX_KNOWN_CAP {
        known.clear();
    }
    known.insert(txid)
}

#[derive(Clone, Debug)]
struct TxRejectStats {
    build_missing_input: u64,
    build_conflicting_input: u64,
    build_invalid_transaction: u64,
    build_invalid_script: u64,
    build_invalid_shielded: u64,
    build_internal: u64,
    insert_already_in_mempool: u64,
    insert_conflicting_input: u64,
    insert_other: u64,
    last_log: Instant,
}

impl TxRejectStats {
    fn new() -> Self {
        Self {
            build_missing_input: 0,
            build_conflicting_input: 0,
            build_invalid_transaction: 0,
            build_invalid_script: 0,
            build_invalid_shielded: 0,
            build_internal: 0,
            insert_already_in_mempool: 0,
            insert_conflicting_input: 0,
            insert_other: 0,
            last_log: Instant::now(),
        }
    }

    fn note_build_error(&mut self, kind: mempool::MempoolErrorKind) {
        match kind {
            mempool::MempoolErrorKind::MissingInput => self.build_missing_input += 1,
            mempool::MempoolErrorKind::ConflictingInput => self.build_conflicting_input += 1,
            mempool::MempoolErrorKind::InvalidTransaction => self.build_invalid_transaction += 1,
            mempool::MempoolErrorKind::InvalidScript => self.build_invalid_script += 1,
            mempool::MempoolErrorKind::InvalidShielded => self.build_invalid_shielded += 1,
            mempool::MempoolErrorKind::Internal => self.build_internal += 1,
            mempool::MempoolErrorKind::AlreadyInMempool => self.insert_already_in_mempool += 1,
        }
    }

    fn note_insert_error(&mut self, kind: mempool::MempoolErrorKind) {
        match kind {
            mempool::MempoolErrorKind::AlreadyInMempool => self.insert_already_in_mempool += 1,
            mempool::MempoolErrorKind::ConflictingInput => self.insert_conflicting_input += 1,
            _ => self.insert_other += 1,
        }
    }

    fn maybe_log(&mut self, addr: std::net::SocketAddr) {
        if self.last_log.elapsed() < Duration::from_secs(TX_REJECT_LOG_INTERVAL_SECS) {
            return;
        }
        self.last_log = Instant::now();

        let total = self.build_missing_input
            + self.build_conflicting_input
            + self.build_invalid_transaction
            + self.build_invalid_script
            + self.build_invalid_shielded
            + self.build_internal
            + self.insert_already_in_mempool
            + self.insert_conflicting_input
            + self.insert_other;
        if total == 0 {
            return;
        }

        eprintln!(
            "tx relay peer {addr}: rejected {total} tx(s) (build_missing_input {} build_conflict {} build_invalid_tx {} build_invalid_script {} build_invalid_shielded {} build_internal {} insert_dupe {} insert_conflict {} insert_other {})",
            self.build_missing_input,
            self.build_conflicting_input,
            self.build_invalid_transaction,
            self.build_invalid_script,
            self.build_invalid_shielded,
            self.build_internal,
            self.insert_already_in_mempool,
            self.insert_conflicting_input,
            self.insert_other,
        );

        self.build_missing_input = 0;
        self.build_conflicting_input = 0;
        self.build_invalid_transaction = 0;
        self.build_invalid_script = 0;
        self.build_invalid_shielded = 0;
        self.build_internal = 0;
        self.insert_already_in_mempool = 0;
        self.insert_conflicting_input = 0;
        self.insert_other = 0;
    }
}
