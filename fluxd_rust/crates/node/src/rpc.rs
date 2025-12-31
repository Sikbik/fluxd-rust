use std::collections::{BinaryHeap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::RngCore;
use serde_json::{json, Number, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch};

use fluxd_chainstate::index::HeaderEntry;
use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::ValidationFlags;
use fluxd_consensus::constants::{
    FLUXNODE_DOS_REMOVE_AMOUNT, FLUXNODE_DOS_REMOVE_AMOUNT_V2, FLUXNODE_START_TX_EXPIRATION_HEIGHT,
    FLUXNODE_START_TX_EXPIRATION_HEIGHT_V2, MAX_BLOCK_SIGOPS, MAX_BLOCK_SIZE, PROTOCOL_VERSION,
};
use fluxd_consensus::money::COIN;
use fluxd_consensus::params::{hash256_from_hex, ChainParams, Network};
use fluxd_consensus::upgrades::{
    current_epoch_branch_id, network_upgrade_active, network_upgrade_state, UpgradeIndex,
    UpgradeState, ALL_UPGRADES, NETWORK_UPGRADE_INFO,
};
use fluxd_consensus::Hash256;
use fluxd_consensus::{
    block_subsidy, exchange_fund_amount, foundation_fund_amount, swap_pool_amount,
};
use fluxd_fluxnode::storage::FluxnodeRecord;
use fluxd_pow::difficulty::compact_to_u256;
use fluxd_primitives::block::{Block, CURRENT_VERSION, PON_VERSION};
use fluxd_primitives::hash::{hash160, sha256d};
use fluxd_primitives::merkleblock::{MerkleBlock, PartialMerkleTree};
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::transaction::{
    FluxnodeStartV5, FluxnodeStartV6, FluxnodeStartVariantV6, FluxnodeTx, FluxnodeTxV5,
    FluxnodeTxV6, Transaction, TxIn, TxOut, FLUXNODE_INTERNAL_NORMAL_TX_VERSION,
    FLUXNODE_INTERNAL_P2SH_TX_VERSION, FLUXNODE_TX_UPGRADEABLE_VERSION, FLUXNODE_TX_VERSION,
    SAPLING_VERSION_GROUP_ID,
};
use fluxd_primitives::{
    address_to_script_pubkey, script_pubkey_to_address, secret_key_to_wif, wif_to_secret_key,
    AddressError,
};
use fluxd_script::message::{recover_signed_message_pubkey, signed_message_hash};
use fluxd_script::standard::{classify_script_pubkey, ScriptType};
use primitive_types::U256;
use secp256k1::{ecdsa::RecoverableSignature, Message, PublicKey, Secp256k1, SecretKey};

use crate::fee_estimator::FeeEstimator;
use crate::mempool::{build_mempool_entry, Mempool, MempoolErrorKind, MempoolPolicy};
use crate::p2p::{NetTotals, PeerKind, PeerRegistry};
use crate::peer_book::HeaderPeerBook;
use crate::stats::{hash256_to_hex, MempoolMetrics};
use crate::AddrBook;
use crate::{db_info, Backend, Store};

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const RPC_REALM: &str = "fluxd";
pub(crate) const RPC_COOKIE_FILE: &str = "rpc.cookie";

const RPC_INVALID_PARAMETER: i64 = -8;
const RPC_TYPE_ERROR: i64 = -3;
const RPC_WALLET_ERROR: i64 = -4;
const RPC_INVALID_ADDRESS_OR_KEY: i64 = -5;
const RPC_DESERIALIZATION_ERROR: i64 = -22;
const RPC_TRANSACTION_ERROR: i64 = -25;
const RPC_TRANSACTION_REJECTED: i64 = -26;
const RPC_TRANSACTION_ALREADY_IN_CHAIN: i64 = -27;
const RPC_METHOD_NOT_FOUND: i64 = -32601;
const RPC_INVALID_REQUEST: i64 = -32600;
const RPC_PARSE_ERROR: i64 = -32700;
const RPC_INTERNAL_ERROR: i64 = -32603;

const RPC_METHODS: &[&str] = &[
    "help",
    "getinfo",
    "ping",
    "stop",
    "restart",
    "reindex",
    "rescanblockchain",
    "getdbinfo",
    "getblockcount",
    "getbestblockhash",
    "getblockhash",
    "getblockheader",
    "getblock",
    "getblockchaininfo",
    "getdifficulty",
    "getchaintips",
    "getblocksubsidy",
    "getblockhashes",
    "createrawtransaction",
    "decoderawtransaction",
    "decodescript",
    "validateaddress",
    "verifymessage",
    "createmultisig",
    "getrawtransaction",
    "sendrawtransaction",
    "getmempoolinfo",
    "getrawmempool",
    "gettxout",
    "gettxoutproof",
    "verifytxoutproof",
    "gettxoutsetinfo",
    "getblockdeltas",
    "getspentinfo",
    "getaddressutxos",
    "getaddressbalance",
    "getaddressdeltas",
    "getaddresstxids",
    "getaddressmempool",
    "getmininginfo",
    "getblocktemplate",
    "submitblock",
    "getnetworkhashps",
    "getnetworksolps",
    "getlocalsolps",
    "estimatefee",
    "getconnectioncount",
    "getnettotals",
    "listbanned",
    "getnetworkinfo",
    "getpeerinfo",
    "getdeprecationinfo",
    "getfluxnodecount",
    "listfluxnodes",
    "viewdeterministicfluxnodelist",
    "fluxnodecurrentwinner",
    "getfluxnodestatus",
    "getdoslist",
    "getstartlist",
    "createfluxnodekey",
    "createzelnodekey",
    "listfluxnodeconf",
    "listzelnodeconf",
    "getfluxnodeoutputs",
    "getzelnodeoutputs",
    "startfluxnode",
    "startzelnode",
    "startdeterministicfluxnode",
    "startdeterministiczelnode",
    "verifychain",
    "addnode",
    "disconnectnode",
    "getaddednodeinfo",
    "setban",
    "clearbanned",
];
pub struct RpcAuth {
    user: String,
    pass: String,
}

pub fn load_or_create_auth(
    user: Option<String>,
    pass: Option<String>,
    data_dir: &Path,
) -> Result<RpcAuth, String> {
    if user.is_some() || pass.is_some() {
        let user =
            user.ok_or_else(|| "missing --rpc-user (required with --rpc-pass)".to_string())?;
        let pass =
            pass.ok_or_else(|| "missing --rpc-pass (required with --rpc-user)".to_string())?;
        return Ok(RpcAuth { user, pass });
    }

    let cookie_path = data_dir.join(RPC_COOKIE_FILE);
    if cookie_path.exists() {
        let contents = fs::read_to_string(&cookie_path).map_err(|err| err.to_string())?;
        if let Some((user, pass)) = contents.trim().split_once(':') {
            return Ok(RpcAuth {
                user: user.to_string(),
                pass: pass.to_string(),
            });
        }
    }

    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let user = "__cookie__".to_string();
    let pass = hex_bytes(&bytes);
    write_cookie(&cookie_path, &user, &pass)?;
    println!("RPC auth cookie: {}", cookie_path.display());
    Ok(RpcAuth { user, pass })
}

#[allow(clippy::too_many_arguments)]
pub async fn serve_rpc<S: fluxd_storage::KeyValueStore + Send + Sync + 'static>(
    addr: SocketAddr,
    auth: RpcAuth,
    chainstate: Arc<ChainState<S>>,
    store: Arc<Store>,
    write_lock: Arc<Mutex<()>>,
    mempool: Arc<Mutex<Mempool>>,
    mempool_policy: Arc<MempoolPolicy>,
    mempool_metrics: Arc<MempoolMetrics>,
    fee_estimator: Arc<Mutex<FeeEstimator>>,
    mempool_flags: ValidationFlags,
    miner_address: Option<String>,
    params: ChainParams,
    data_dir: PathBuf,
    net_totals: Arc<NetTotals>,
    peer_registry: Arc<PeerRegistry>,
    header_peer_book: Arc<HeaderPeerBook>,
    addr_book: Arc<AddrBook>,
    added_nodes: Arc<Mutex<HashSet<SocketAddr>>>,
    tx_announce: broadcast::Sender<Hash256>,
    shutdown_tx: watch::Sender<bool>,
) -> Result<(), String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|err| format!("rpc bind failed: {err}"))?;
    println!("RPC listening on http://{addr}");

    let auth = Arc::new(auth);
    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|err| format!("rpc accept failed: {err}"))?;
        let auth = Arc::clone(&auth);
        let chainstate = Arc::clone(&chainstate);
        let store = Arc::clone(&store);
        let write_lock = Arc::clone(&write_lock);
        let mempool = Arc::clone(&mempool);
        let mempool_policy = Arc::clone(&mempool_policy);
        let mempool_metrics = Arc::clone(&mempool_metrics);
        let fee_estimator = Arc::clone(&fee_estimator);
        let mempool_flags = mempool_flags.clone();
        let miner_address = miner_address.clone();
        let params = params.clone();
        let data_dir = data_dir.clone();
        let net_totals = Arc::clone(&net_totals);
        let peer_registry = Arc::clone(&peer_registry);
        let header_peer_book = Arc::clone(&header_peer_book);
        let addr_book = Arc::clone(&addr_book);
        let added_nodes = Arc::clone(&added_nodes);
        let tx_announce = tx_announce.clone();
        let shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(
                stream,
                auth,
                chainstate,
                store,
                write_lock,
                mempool,
                mempool_policy,
                mempool_metrics,
                fee_estimator,
                mempool_flags,
                miner_address,
                params,
                data_dir,
                net_totals,
                peer_registry,
                header_peer_book,
                addr_book,
                added_nodes,
                tx_announce,
                shutdown_tx,
            )
            .await
            {
                eprintln!("rpc error: {err}");
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_connection<S: fluxd_storage::KeyValueStore + Send + Sync + 'static>(
    mut stream: tokio::net::TcpStream,
    auth: Arc<RpcAuth>,
    chainstate: Arc<ChainState<S>>,
    store: Arc<Store>,
    write_lock: Arc<Mutex<()>>,
    mempool: Arc<Mutex<Mempool>>,
    mempool_policy: Arc<MempoolPolicy>,
    mempool_metrics: Arc<MempoolMetrics>,
    fee_estimator: Arc<Mutex<FeeEstimator>>,
    mempool_flags: ValidationFlags,
    miner_address: Option<String>,
    chain_params: ChainParams,
    data_dir: PathBuf,
    net_totals: Arc<NetTotals>,
    peer_registry: Arc<PeerRegistry>,
    header_peer_book: Arc<HeaderPeerBook>,
    addr_book: Arc<AddrBook>,
    added_nodes: Arc<Mutex<HashSet<SocketAddr>>>,
    tx_announce: broadcast::Sender<Hash256>,
    shutdown_tx: watch::Sender<bool>,
) -> Result<(), String> {
    let request = read_http_request(&mut stream).await?;
    let is_daemon = request.path.starts_with("/daemon/");
    if !is_daemon && request.method != "POST" {
        let response = build_response("405 Method Not Allowed", "text/plain", "method not allowed");
        stream
            .write_all(&response)
            .await
            .map_err(|err| err.to_string())?;
        return Ok(());
    }

    if !auth.check(
        request
            .headers
            .get("authorization")
            .map(|value| value.as_str()),
    ) {
        let response = build_unauthorized();
        stream
            .write_all(&response)
            .await
            .map_err(|err| err.to_string())?;
        return Ok(());
    }

    if is_daemon {
        let method = request
            .path
            .trim_start_matches("/daemon/")
            .trim_matches('/');
        if method.is_empty() {
            let response = build_response("404 Not Found", "text/plain", "not found");
            stream
                .write_all(&response)
                .await
                .map_err(|err| err.to_string())?;
            return Ok(());
        }
        let rpc_response = if method == "getblocktemplate" {
            let params = if request.method == "GET" {
                parse_query_params(request.query.as_deref().unwrap_or(""))
            } else if request.method == "POST" {
                parse_body_params(&request.body)
            } else {
                Err(RpcError::new(RPC_INVALID_REQUEST, "method not allowed"))
            };

            match params {
                Err(err) => rpc_error(Value::Null, err.code, err.message),
                Ok(params) => {
                    let mut longpoll_error: Option<RpcError> = None;
                    if let Some((watched_hash, watched_revision)) =
                        longpoll_state_from_params(&params)
                    {
                        match current_longpoll_state(chainstate.as_ref(), mempool.as_ref()) {
                            Ok((current_hash, current_revision))
                                if current_hash == watched_hash
                                    && current_revision == watched_revision =>
                            {
                                if let Err(err) = wait_longpoll(
                                    chainstate.as_ref(),
                                    mempool.as_ref(),
                                    watched_hash,
                                    watched_revision,
                                )
                                .await
                                {
                                    longpoll_error = Some(err);
                                }
                            }
                            Ok(_) => {}
                            Err(err) => longpoll_error = Some(err),
                        }
                    }

                    if let Some(err) = longpoll_error {
                        rpc_error(Value::Null, err.code, err.message)
                    } else {
                        match dispatch_method(
                            method,
                            params,
                            chainstate.as_ref(),
                            write_lock.as_ref(),
                            mempool.as_ref(),
                            mempool_policy.as_ref(),
                            mempool_metrics.as_ref(),
                            fee_estimator.as_ref(),
                            &mempool_flags,
                            &chain_params,
                            miner_address.as_deref(),
                            &data_dir,
                            store.as_ref(),
                            &net_totals,
                            &peer_registry,
                            &header_peer_book,
                            addr_book.as_ref(),
                            added_nodes.as_ref(),
                            &tx_announce,
                            &shutdown_tx,
                        ) {
                            Ok(value) => rpc_ok(Value::Null, value),
                            Err(err) => rpc_error(Value::Null, err.code, err.message),
                        }
                    }
                }
            }
        } else {
            match handle_daemon_request(
                method,
                &request,
                chainstate.as_ref(),
                write_lock.as_ref(),
                mempool.as_ref(),
                mempool_policy.as_ref(),
                mempool_metrics.as_ref(),
                fee_estimator.as_ref(),
                &mempool_flags,
                &chain_params,
                miner_address.as_deref(),
                &data_dir,
                store.as_ref(),
                &net_totals,
                &peer_registry,
                &header_peer_book,
                addr_book.as_ref(),
                added_nodes.as_ref(),
                &tx_announce,
                &shutdown_tx,
            ) {
                Ok(value) => rpc_ok(Value::Null, value),
                Err(err) => rpc_error(Value::Null, err.code, err.message),
            }
        };
        let body = rpc_response.to_string();
        let response = build_response("200 OK", "application/json", &body);
        stream
            .write_all(&response)
            .await
            .map_err(|err| err.to_string())?;
        return Ok(());
    }

    if let Some(rpc_response) = handle_json_rpc_getblocktemplate_longpoll(
        &request.body,
        chainstate.as_ref(),
        write_lock.as_ref(),
        mempool.as_ref(),
        mempool_policy.as_ref(),
        mempool_metrics.as_ref(),
        fee_estimator.as_ref(),
        &mempool_flags,
        &chain_params,
        miner_address.as_deref(),
        &data_dir,
        store.as_ref(),
        &net_totals,
        &peer_registry,
        &header_peer_book,
        addr_book.as_ref(),
        added_nodes.as_ref(),
        &tx_announce,
        &shutdown_tx,
    )
    .await?
    {
        let body = rpc_response.to_string();
        let response = build_response("200 OK", "application/json", &body);
        stream
            .write_all(&response)
            .await
            .map_err(|err| err.to_string())?;
        return Ok(());
    }

    let rpc_response = match handle_rpc_request(
        &request.body,
        chainstate.as_ref(),
        write_lock.as_ref(),
        mempool.as_ref(),
        mempool_policy.as_ref(),
        mempool_metrics.as_ref(),
        fee_estimator.as_ref(),
        &mempool_flags,
        &chain_params,
        miner_address.as_deref(),
        &data_dir,
        store.as_ref(),
        &net_totals,
        &peer_registry,
        &header_peer_book,
        addr_book.as_ref(),
        added_nodes.as_ref(),
        &tx_announce,
        &shutdown_tx,
    ) {
        Ok(value) => value,
        Err(err) => err,
    };
    let body = rpc_response.to_string();
    let response = build_response("200 OK", "application/json", &body);
    stream
        .write_all(&response)
        .await
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn parse_longpollid(value: &str) -> Option<(Hash256, u64)> {
    if value.len() < 64 {
        return None;
    }
    let (hash_hex, rest) = value.split_at(64);
    let hash = hash256_from_hex(hash_hex).ok()?;
    let revision = rest.parse::<u64>().ok()?;
    Some((hash, revision))
}

fn longpoll_state_from_params(params: &[Value]) -> Option<(Hash256, u64)> {
    let obj = params.get(0)?.as_object()?;
    let value = obj.get("longpollid")?.as_str()?;
    parse_longpollid(value)
}

fn current_longpoll_state<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
) -> Result<(Hash256, u64), RpcError> {
    let tip_hash = chainstate
        .best_block()
        .map_err(map_internal)?
        .map(|tip| tip.hash)
        .unwrap_or([0u8; 32]);
    let revision = mempool
        .lock()
        .map_err(|_| map_internal("mempool lock poisoned"))?
        .revision();
    Ok((tip_hash, revision))
}

async fn wait_longpoll<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
    watched_hash: Hash256,
    watched_revision: u64,
) -> Result<(), RpcError> {
    loop {
        let (current_hash, current_revision) = current_longpoll_state(chainstate, mempool)?;
        if current_hash != watched_hash || current_revision != watched_revision {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_json_rpc_getblocktemplate_longpoll<S: fluxd_storage::KeyValueStore>(
    body: &[u8],
    chainstate: &ChainState<S>,
    write_lock: &Mutex<()>,
    mempool: &Mutex<Mempool>,
    mempool_policy: &MempoolPolicy,
    mempool_metrics: &MempoolMetrics,
    fee_estimator: &Mutex<FeeEstimator>,
    mempool_flags: &ValidationFlags,
    chain_params: &ChainParams,
    miner_address: Option<&str>,
    data_dir: &Path,
    store: &Store,
    net_totals: &NetTotals,
    peer_registry: &PeerRegistry,
    header_peer_book: &HeaderPeerBook,
    addr_book: &AddrBook,
    added_nodes: &Mutex<HashSet<SocketAddr>>,
    tx_announce: &broadcast::Sender<Hash256>,
    shutdown_tx: &watch::Sender<bool>,
) -> Result<Option<Value>, String> {
    let value: Value = match serde_json::from_slice(body) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };

    if value.is_array() {
        return Ok(None);
    }

    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let method = match value.get("method").and_then(|value| value.as_str()) {
        Some(method) => method,
        None => return Ok(None),
    };
    if method != "getblocktemplate" {
        return Ok(None);
    }

    let params_value = value
        .get("params")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    let params = match params_value {
        Value::Array(values) => values,
        Value::Null => Vec::new(),
        _ => {
            return Ok(Some(rpc_error(
                id,
                RPC_INVALID_REQUEST,
                "params must be an array",
            )))
        }
    };

    if let Some((watched_hash, watched_revision)) = longpoll_state_from_params(&params) {
        let (current_hash, current_revision) = match current_longpoll_state(chainstate, mempool) {
            Ok(state) => state,
            Err(err) => return Ok(Some(rpc_error(id, err.code, err.message))),
        };
        if current_hash == watched_hash && current_revision == watched_revision {
            if let Err(err) =
                wait_longpoll(chainstate, mempool, watched_hash, watched_revision).await
            {
                return Ok(Some(rpc_error(id, err.code, err.message)));
            }
        }
    }

    let rpc_response = match dispatch_method(
        method,
        params,
        chainstate,
        write_lock,
        mempool,
        mempool_policy,
        mempool_metrics,
        fee_estimator,
        mempool_flags,
        chain_params,
        miner_address,
        data_dir,
        store,
        net_totals,
        peer_registry,
        header_peer_book,
        addr_book,
        added_nodes,
        tx_announce,
        shutdown_tx,
    ) {
        Ok(value) => rpc_ok(id, value),
        Err(err) => rpc_error(id, err.code, err.message),
    };
    Ok(Some(rpc_response))
}

#[allow(clippy::too_many_arguments)]
fn handle_daemon_request<S: fluxd_storage::KeyValueStore>(
    method: &str,
    request: &HttpRequest,
    chainstate: &ChainState<S>,
    write_lock: &Mutex<()>,
    mempool: &Mutex<Mempool>,
    mempool_policy: &MempoolPolicy,
    mempool_metrics: &MempoolMetrics,
    fee_estimator: &Mutex<FeeEstimator>,
    mempool_flags: &ValidationFlags,
    chain_params: &ChainParams,
    miner_address: Option<&str>,
    data_dir: &Path,
    store: &Store,
    net_totals: &NetTotals,
    peer_registry: &PeerRegistry,
    header_peer_book: &HeaderPeerBook,
    addr_book: &AddrBook,
    added_nodes: &Mutex<HashSet<SocketAddr>>,
    tx_announce: &broadcast::Sender<Hash256>,
    shutdown_tx: &watch::Sender<bool>,
) -> Result<Value, RpcError> {
    let params = if request.method == "GET" {
        parse_query_params(request.query.as_deref().unwrap_or(""))?
    } else if request.method == "POST" {
        parse_body_params(&request.body)?
    } else {
        return Err(RpcError::new(RPC_INVALID_REQUEST, "method not allowed"));
    };
    dispatch_method(
        method,
        params,
        chainstate,
        write_lock,
        mempool,
        mempool_policy,
        mempool_metrics,
        fee_estimator,
        mempool_flags,
        chain_params,
        miner_address,
        data_dir,
        store,
        net_totals,
        peer_registry,
        header_peer_book,
        addr_book,
        added_nodes,
        tx_announce,
        shutdown_tx,
    )
}

fn handle_rpc_request<S: fluxd_storage::KeyValueStore>(
    body: &[u8],
    chainstate: &ChainState<S>,
    write_lock: &Mutex<()>,
    mempool: &Mutex<Mempool>,
    mempool_policy: &MempoolPolicy,
    mempool_metrics: &MempoolMetrics,
    fee_estimator: &Mutex<FeeEstimator>,
    mempool_flags: &ValidationFlags,
    chain_params: &ChainParams,
    miner_address: Option<&str>,
    data_dir: &Path,
    store: &Store,
    net_totals: &NetTotals,
    peer_registry: &PeerRegistry,
    header_peer_book: &HeaderPeerBook,
    addr_book: &AddrBook,
    added_nodes: &Mutex<HashSet<SocketAddr>>,
    tx_announce: &broadcast::Sender<Hash256>,
    shutdown_tx: &watch::Sender<bool>,
) -> Result<Value, Value> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|err| rpc_error(Value::Null, RPC_PARSE_ERROR, format!("parse error: {err}")))?;

    if value.is_array() {
        return Err(rpc_error(
            Value::Null,
            RPC_INVALID_REQUEST,
            "batch requests are not supported",
        ));
    }

    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let method = value
        .get("method")
        .and_then(|value| value.as_str())
        .ok_or_else(|| rpc_error(id.clone(), RPC_INVALID_REQUEST, "missing method"))?;
    let params_value = value
        .get("params")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    let params = match params_value {
        Value::Array(values) => values,
        Value::Null => Vec::new(),
        _ => {
            return Err(rpc_error(
                id,
                RPC_INVALID_REQUEST,
                "params must be an array",
            ))
        }
    };

    let result = dispatch_method(
        method,
        params,
        chainstate,
        write_lock,
        mempool,
        mempool_policy,
        mempool_metrics,
        fee_estimator,
        mempool_flags,
        chain_params,
        miner_address,
        data_dir,
        store,
        net_totals,
        peer_registry,
        header_peer_book,
        addr_book,
        added_nodes,
        tx_announce,
        shutdown_tx,
    );

    match result {
        Ok(value) => Ok(rpc_ok(id, value)),
        Err(err) => Err(rpc_error(id, err.code, err.message)),
    }
}

fn parse_body_params(body: &[u8]) -> Result<Vec<Value>, RpcError> {
    if body.is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = serde_json::from_slice(body)
        .map_err(|err| RpcError::new(RPC_PARSE_ERROR, format!("parse error: {err}")))?;
    match value {
        Value::Array(values) => Ok(values),
        Value::Object(mut map) => {
            if let Some(params_value) = map.remove("params") {
                match params_value {
                    Value::Array(values) => Ok(values),
                    Value::Null => Ok(Vec::new()),
                    other => Ok(vec![other]),
                }
            } else {
                Ok(vec![Value::Object(map)])
            }
        }
        Value::Null => Ok(Vec::new()),
        other => Ok(vec![other]),
    }
}

fn parse_query_params(query: &str) -> Result<Vec<Value>, RpcError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut params = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut parts = pair.splitn(2, '=');
        let key_raw = parts.next().unwrap_or("");
        let value_raw = parts.next().unwrap_or("");
        let key = percent_decode(key_raw)?;
        let value = percent_decode(value_raw)?;
        if key == "params" {
            let value = if value.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&value).map_err(|err| {
                    RpcError::new(RPC_INVALID_REQUEST, format!("invalid params: {err}"))
                })?
            };
            return match value {
                Value::Array(values) => Ok(values),
                Value::Null => Ok(Vec::new()),
                other => Ok(vec![other]),
            };
        }
        params.push(parse_query_value(&value));
    }
    Ok(params)
}

fn parse_query_value(value: &str) -> Value {
    let trimmed = value.trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower == "true" {
        return Value::Bool(true);
    }
    if lower == "false" {
        return Value::Bool(false);
    }
    if let Ok(int_val) = trimmed.parse::<i64>() {
        return Value::Number(int_val.into());
    }
    if trimmed.contains('.') || trimmed.contains('e') || trimmed.contains('E') {
        if let Ok(float_val) = trimmed.parse::<f64>() {
            if let Some(number) = Number::from_f64(float_val) {
                return Value::Number(number);
            }
        }
    }
    Value::String(trimmed.to_string())
}

fn percent_decode(input: &str) -> Result<String, RpcError> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'%' => {
                if idx + 2 >= bytes.len() {
                    return Err(RpcError::new(
                        RPC_INVALID_REQUEST,
                        "invalid percent-encoding",
                    ));
                }
                let hi = hex_value(bytes[idx + 1])?;
                let lo = hex_value(bytes[idx + 2])?;
                out.push((hi << 4) | lo);
                idx += 3;
            }
            b'+' => {
                out.push(b' ');
                idx += 1;
            }
            byte => {
                out.push(byte);
                idx += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| RpcError::new(RPC_INVALID_REQUEST, "invalid utf8 in query"))
}

fn hex_value(byte: u8) -> Result<u8, RpcError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(RpcError::new(
            RPC_INVALID_REQUEST,
            "invalid percent-encoding",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_method<S: fluxd_storage::KeyValueStore>(
    method: &str,
    params: Vec<Value>,
    chainstate: &ChainState<S>,
    write_lock: &Mutex<()>,
    mempool: &Mutex<Mempool>,
    mempool_policy: &MempoolPolicy,
    mempool_metrics: &MempoolMetrics,
    fee_estimator: &Mutex<FeeEstimator>,
    mempool_flags: &ValidationFlags,
    chain_params: &ChainParams,
    miner_address: Option<&str>,
    data_dir: &Path,
    store: &Store,
    net_totals: &NetTotals,
    peer_registry: &PeerRegistry,
    header_peer_book: &HeaderPeerBook,
    addr_book: &AddrBook,
    added_nodes: &Mutex<HashSet<SocketAddr>>,
    tx_announce: &broadcast::Sender<Hash256>,
    shutdown_tx: &watch::Sender<bool>,
) -> Result<Value, RpcError> {
    match method {
        "help" => rpc_help(params),
        "getinfo" => rpc_getinfo(
            chainstate,
            params,
            chain_params,
            data_dir,
            net_totals,
            peer_registry,
            mempool_policy,
        ),
        "ping" => rpc_ping(params),
        "stop" => rpc_stop(params, shutdown_tx),
        "restart" => rpc_restart(params, shutdown_tx),
        "reindex" => rpc_reindex(params, data_dir, shutdown_tx),
        "rescanblockchain" => rpc_rescanblockchain(params),
        "getdbinfo" => rpc_getdbinfo(chainstate, store, params, data_dir),
        "getblockcount" => rpc_getblockcount(chainstate, params),
        "getbestblockhash" => rpc_getbestblockhash(chainstate, params),
        "getblockhash" => rpc_getblockhash(chainstate, params),
        "getblockheader" => rpc_getblockheader(chainstate, params, chain_params),
        "getblock" => rpc_getblock(chainstate, params, chain_params),
        "getblockchaininfo" => rpc_getblockchaininfo(chainstate, params, chain_params, data_dir),
        "getdifficulty" => rpc_getdifficulty(chainstate, params, chain_params),
        "getchaintips" => rpc_getchaintips(chainstate, params),
        "getblocksubsidy" => rpc_getblocksubsidy(chainstate, params, chain_params),
        "getblockhashes" => rpc_getblockhashes(chainstate, params),
        "createrawtransaction" => rpc_createrawtransaction(chainstate, params, chain_params),
        "decoderawtransaction" => rpc_decoderawtransaction(params, chain_params),
        "decodescript" => rpc_decodescript(params, chain_params),
        "getrawtransaction" => rpc_getrawtransaction(chainstate, mempool, params, chain_params),
        "sendrawtransaction" => rpc_sendrawtransaction(
            chainstate,
            mempool,
            mempool_policy,
            mempool_metrics,
            fee_estimator,
            mempool_flags,
            params,
            chain_params,
            tx_announce,
        ),
        "getmempoolinfo" => rpc_getmempoolinfo(params, mempool),
        "getrawmempool" => rpc_getrawmempool(params, mempool),
        "gettxout" => rpc_gettxout(chainstate, mempool, params, chain_params),
        "gettxoutproof" => rpc_gettxoutproof(chainstate, params),
        "verifytxoutproof" => rpc_verifytxoutproof(chainstate, params),
        "gettxoutsetinfo" => rpc_gettxoutsetinfo(chainstate, params, data_dir),
        "getblockdeltas" => rpc_getblockdeltas(chainstate, params, chain_params),
        "getspentinfo" => rpc_getspentinfo(chainstate, params),
        "getaddressutxos" => rpc_getaddressutxos(chainstate, params, chain_params),
        "getaddressbalance" => rpc_getaddressbalance(chainstate, params, chain_params),
        "getaddressdeltas" => rpc_getaddressdeltas(chainstate, params, chain_params),
        "getaddresstxids" => rpc_getaddresstxids(chainstate, params, chain_params),
        "getaddressmempool" => rpc_getaddressmempool(chainstate, mempool, params, chain_params),
        "getmininginfo" => rpc_getmininginfo(chainstate, mempool, params, chain_params),
        "getblocktemplate" => rpc_getblocktemplate(
            chainstate,
            mempool,
            params,
            chain_params,
            mempool_flags,
            miner_address,
        ),
        "submitblock" => {
            rpc_submitblock(chainstate, write_lock, params, chain_params, mempool_flags)
        }
        "getnetworkhashps" => rpc_getnetworkhashps(params),
        "getnetworksolps" => rpc_getnetworksolps(params),
        "getlocalsolps" => rpc_getlocalsolps(params),
        "estimatefee" => rpc_estimatefee(params, fee_estimator),
        "getconnectioncount" => rpc_getconnectioncount(params, peer_registry, net_totals),
        "getnettotals" => rpc_getnettotals(params, net_totals),
        "getnetworkinfo" => rpc_getnetworkinfo(params, peer_registry, net_totals, mempool_policy),
        "getpeerinfo" => rpc_getpeerinfo(params, peer_registry),
        "getdeprecationinfo" => rpc_getdeprecationinfo(params),
        "listbanned" => rpc_listbanned(params, header_peer_book),
        "clearbanned" => rpc_clearbanned(params, header_peer_book),
        "setban" => rpc_setban(params, chain_params, peer_registry, header_peer_book),
        "disconnectnode" => rpc_disconnectnode(params, chain_params, peer_registry),
        "addnode" => rpc_addnode(params, chain_params, addr_book, added_nodes),
        "getaddednodeinfo" => {
            rpc_getaddednodeinfo(params, chain_params, peer_registry, added_nodes)
        }
        "getfluxnodecount" => rpc_getfluxnodecount(chainstate, params),
        "listfluxnodes" => rpc_viewdeterministicfluxnodelist(chainstate, params),
        "viewdeterministicfluxnodelist" => rpc_viewdeterministicfluxnodelist(chainstate, params),
        "fluxnodecurrentwinner" => rpc_fluxnodecurrentwinner(chainstate, params),
        "getfluxnodestatus" => rpc_getfluxnodestatus(chainstate, params, chain_params, data_dir),
        "getdoslist" => rpc_getdoslist(chainstate, params, chain_params),
        "getstartlist" => rpc_getstartlist(chainstate, params, chain_params),
        "createfluxnodekey" | "createzelnodekey" => rpc_createfluxnodekey(params, chain_params),
        "listfluxnodeconf" | "listzelnodeconf" => {
            rpc_listfluxnodeconf(chainstate, params, chain_params, data_dir)
        }
        "getfluxnodeoutputs" | "getzelnodeoutputs" => {
            rpc_getfluxnodeoutputs(chainstate, params, chain_params, data_dir)
        }
        "startfluxnode" | "startzelnode" => rpc_startfluxnode(
            chainstate,
            mempool,
            mempool_policy,
            mempool_metrics,
            fee_estimator,
            mempool_flags,
            params,
            chain_params,
            tx_announce,
            data_dir,
        ),
        "startdeterministicfluxnode" | "startdeterministiczelnode" => {
            rpc_startdeterministicfluxnode(
                chainstate,
                mempool,
                mempool_policy,
                mempool_metrics,
                fee_estimator,
                mempool_flags,
                params,
                chain_params,
                tx_announce,
                data_dir,
            )
        }
        "verifychain" => rpc_verifychain(chainstate, params),
        "validateaddress" => rpc_validateaddress(params, chain_params),
        "verifymessage" => rpc_verifymessage(params, chain_params),
        "createmultisig" => rpc_createmultisig(params, chain_params),
        _ => Err(RpcError::new(RPC_METHOD_NOT_FOUND, "method not found")),
    }
}

fn rpc_ping(params: Vec<Value>) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    Ok(Value::Null)
}

fn rpc_stop(params: Vec<Value>, shutdown_tx: &watch::Sender<bool>) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    shutdown_tx
        .send(true)
        .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "shutdown channel closed"))?;
    Ok(Value::String("fluxd stopping".to_string()))
}

fn rpc_restart(params: Vec<Value>, shutdown_tx: &watch::Sender<bool>) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    shutdown_tx
        .send(true)
        .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "shutdown channel closed"))?;
    Ok(Value::String(
        "fluxd restarting (exit requested; restart requires a supervisor)".to_string(),
    ))
}

fn rpc_reindex(
    params: Vec<Value>,
    data_dir: &Path,
    shutdown_tx: &watch::Sender<bool>,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let flag_path = data_dir.join(crate::REINDEX_REQUEST_FILE_NAME);
    fs::write(&flag_path, b"reindex\n").map_err(|err| {
        RpcError::new(
            RPC_INTERNAL_ERROR,
            &format!("failed to write {}: {err}", flag_path.display()),
        )
    })?;
    shutdown_tx
        .send(true)
        .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "shutdown channel closed"))?;
    Ok(Value::String(
        "fluxd reindex requested (exit requested; restart required)".to_string(),
    ))
}

fn rpc_rescanblockchain(params: Vec<Value>) -> Result<Value, RpcError> {
    if params.len() > 2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "rescanblockchain expects 0 to 2 parameters",
        ));
    }
    Err(RpcError::new(RPC_WALLET_ERROR, "wallet not implemented"))
}

fn rpc_getdbinfo<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    store: &Store,
    params: Vec<Value>,
    data_dir: &Path,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let backend = match store {
        Store::Fjall(_) => Backend::Fjall,
        Store::Memory(_) => Backend::Memory,
    };
    db_info::collect_db_info(chainstate, store, data_dir, backend, false)
        .map_err(|err| RpcError::new(RPC_INTERNAL_ERROR, err))
}

fn rpc_getdeprecationinfo(params: Vec<Value>) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    Ok(json!({
        "deprecated": false,
        "version": node_version(),
        "subversion": format!("/fluxd-rust:{}/", env!("CARGO_PKG_VERSION")),
        "warnings": "",
    }))
}

fn rpc_help(params: Vec<Value>) -> Result<Value, RpcError> {
    if params.is_empty() {
        let methods = RPC_METHODS
            .iter()
            .map(|name| Value::String((*name).to_string()))
            .collect::<Vec<_>>();
        return Ok(Value::Array(methods));
    }
    if params.len() > 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "help expects 0 or 1 parameter",
        ));
    }
    let name = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "method name must be a string"))?;
    if RPC_METHODS.contains(&name) {
        Ok(Value::String(format!("{name} is supported")))
    } else {
        Err(RpcError::new(RPC_METHOD_NOT_FOUND, "method not found"))
    }
}

fn rpc_getinfo<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    rpc_params: Vec<Value>,
    chain_params: &ChainParams,
    _data_dir: &Path,
    net_totals: &NetTotals,
    peer_registry: &PeerRegistry,
    mempool_policy: &MempoolPolicy,
) -> Result<Value, RpcError> {
    ensure_no_params(&rpc_params)?;
    let connections = net_totals.snapshot().connections.max(peer_registry.count());
    let best_block = chainstate
        .best_block()
        .map_err(map_internal)?
        .map(|tip| tip.height)
        .unwrap_or(0);
    let best_header = chainstate
        .best_header()
        .map_err(map_internal)?
        .map(|tip| tip.hash);
    let difficulty = match best_header {
        Some(hash) => {
            let entry = chainstate
                .header_entry(&hash)
                .map_err(map_internal)?
                .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "missing header entry"))?;
            difficulty_from_bits(entry.bits, chain_params).unwrap_or(0.0)
        }
        None => 0.0,
    };

    Ok(json!({
        "version": node_version(),
        "protocolversion": PROTOCOL_VERSION,
        "blocks": best_block,
        "timeoffset": 0,
        "connections": connections,
        "proxy": "",
        "difficulty": difficulty,
        "testnet": chain_params.network != Network::Mainnet,
        "relayfee": amount_to_value(mempool_policy.min_relay_fee_per_kb),
        "errors": ""
    }))
}

fn rpc_getblockcount<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let height = chainstate
        .best_block()
        .map_err(map_internal)?
        .map(|tip| tip.height)
        .unwrap_or(0);
    Ok(Value::Number(height.into()))
}

fn rpc_getbestblockhash<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let best = chainstate
        .best_block()
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "best block not found"))?;
    Ok(Value::String(hash256_to_hex(&best.hash)))
}

fn rpc_getblockhash<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getblockhash expects 1 parameter",
        ));
    }
    let height = parse_height(&params[0])?;
    if height < 0 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "block height out of range",
        ));
    }
    let best_height = chainstate
        .best_block()
        .map_err(map_internal)?
        .map(|tip| tip.height)
        .unwrap_or(0);
    if height > best_height {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "block height out of range",
        ));
    }
    let hash = chainstate
        .height_hash(height)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "block not found"))?;
    Ok(Value::String(hash256_to_hex(&hash)))
}

fn rpc_getblockheader<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.is_empty() || params.len() > 2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getblockheader expects 1 or 2 parameters",
        ));
    }
    let hash = parse_hash(&params[0])?;
    let verbose = if params.len() > 1 {
        parse_bool(&params[1])?
    } else {
        true
    };
    let entry = chainstate
        .header_entry(&hash)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "block not found"))?;
    let header_bytes = chainstate
        .block_header_bytes(&hash)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "block not found"))?;
    let header = fluxd_primitives::block::BlockHeader::consensus_decode(&header_bytes)
        .map_err(map_internal)?;

    if !verbose {
        return Ok(Value::String(hex_bytes(&header_bytes)));
    }

    let best_height = best_block_height(chainstate)?;
    let confirmations = confirmations_for_height(chainstate, entry.height, best_height, &hash)?;
    let next_block_hash = next_hash_for_height(chainstate, entry.height, best_height, &hash)?;
    let mut result = json!({
        "hash": hash256_to_hex(&hash),
        "confirmations": confirmations,
        "height": entry.height,
        "version": header.version,
        "merkleroot": hash256_to_hex(&header.merkle_root),
        "finalsaplingroot": hash256_to_hex(&header.final_sapling_root),
        "time": header.time,
        "bits": format!("{:08x}", header.bits),
        "difficulty": difficulty_from_bits(header.bits, chain_params).unwrap_or(0.0),
        "chainwork": hex_bytes(&entry.chainwork),
    });

    if header.is_pon() {
        result["type"] = Value::String("PON".to_string());
        result["collateral"] = Value::String(format_outpoint(&header.nodes_collateral));
        result["blocksig"] = Value::String(hex_bytes(&header.block_sig));
    } else {
        result["type"] = Value::String("POW".to_string());
        result["nonce"] = Value::String(hash256_to_hex(&header.nonce));
        result["solution"] = Value::String(hex_bytes(&header.solution));
    }

    if entry.height > 0 {
        result["previousblockhash"] = Value::String(hash256_to_hex(&entry.prev_hash));
    }
    if let Some(next_hash) = next_block_hash {
        result["nextblockhash"] = Value::String(hash256_to_hex(&next_hash));
    }

    Ok(result)
}

fn rpc_getblock<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.is_empty() || params.len() > 2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getblock expects 1 or 2 parameters",
        ));
    }
    let (hash, entry) = resolve_block_hash(chainstate, &params[0])?;
    let verbosity = if params.len() > 1 {
        parse_verbosity(&params[1])?
    } else {
        1
    };

    let location = chainstate
        .block_location(&hash)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "block not found"))?;
    let bytes = chainstate.read_block(location).map_err(map_internal)?;
    let block = fluxd_primitives::block::Block::consensus_decode(&bytes).map_err(map_internal)?;

    if verbosity == 0 {
        return Ok(Value::String(hex_bytes(&bytes)));
    }

    let best_height = best_block_height(chainstate)?;
    let confirmations = confirmations_for_height(chainstate, entry.height, best_height, &hash)?;
    let next_block_hash = next_hash_for_height(chainstate, entry.height, best_height, &hash)?;

    let mut txs = Vec::with_capacity(block.transactions.len());
    for tx in &block.transactions {
        if verbosity >= 2 {
            txs.push(tx_to_json(tx, chain_params.network)?);
        } else {
            let txid = tx.txid().map_err(map_internal)?;
            txs.push(Value::String(hash256_to_hex(&txid)));
        }
    }

    let mut result = json!({
        "hash": hash256_to_hex(&hash),
        "confirmations": confirmations,
        "size": bytes.len(),
        "height": entry.height,
        "version": block.header.version,
        "merkleroot": hash256_to_hex(&block.header.merkle_root),
        "finalsaplingroot": hash256_to_hex(&block.header.final_sapling_root),
        "tx": Value::Array(txs),
        "time": block.header.time,
        "bits": format!("{:08x}", block.header.bits),
        "difficulty": difficulty_from_bits(block.header.bits, chain_params).unwrap_or(0.0),
        "chainwork": hex_bytes(&entry.chainwork),
    });

    if block.header.is_pon() {
        result["type"] = Value::String("PON".to_string());
        result["collateral"] = Value::String(format_outpoint(&block.header.nodes_collateral));
        result["blocksig"] = Value::String(hex_bytes(&block.header.block_sig));
    } else {
        result["type"] = Value::String("POW".to_string());
        result["nonce"] = Value::String(hash256_to_hex(&block.header.nonce));
        result["solution"] = Value::String(hex_bytes(&block.header.solution));
    }

    if entry.height > 0 {
        result["previousblockhash"] = Value::String(hash256_to_hex(&entry.prev_hash));
    }
    if let Some(next_hash) = next_block_hash {
        result["nextblockhash"] = Value::String(hash256_to_hex(&next_hash));
    }

    Ok(result)
}

fn rpc_getblockchaininfo<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
    data_dir: &Path,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let best_header = chainstate.best_header().map_err(map_internal)?;
    let best_block = chainstate.best_block().map_err(map_internal)?;
    let best_header_height = best_header.as_ref().map(|tip| tip.height).unwrap_or(-1);
    let best_block_height = best_block.as_ref().map(|tip| tip.height).unwrap_or(-1);
    let best_block_hash = best_block.as_ref().map(|tip| tip.hash);
    let best_header_hash = best_header.as_ref().map(|tip| tip.hash);
    let difficulty = match best_header_hash {
        Some(hash) => {
            let entry = chainstate
                .header_entry(&hash)
                .map_err(map_internal)?
                .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "missing header entry"))?;
            difficulty_from_bits(entry.bits, chain_params).unwrap_or(0.0)
        }
        None => 0.0,
    };
    let chainwork = best_block
        .as_ref()
        .map(|tip| hex_bytes(&tip.chainwork))
        .unwrap_or_else(|| "00".to_string());
    let size_on_disk = db_info::dir_size_cached(data_dir, Duration::from_secs(30)).unwrap_or(0);
    let verificationprogress = if best_header_height > 0 && best_block_height >= 0 {
        (best_block_height as f64 / best_header_height as f64).min(1.0)
    } else {
        0.0
    };

    let upgrades = build_upgrade_info(chain_params, best_block_height);
    let consensus = json!({
        "chaintip": format!("{:08x}", current_epoch_branch_id(best_block_height, &chain_params.consensus.upgrades)),
        "nextblock": format!("{:08x}", current_epoch_branch_id(best_block_height + 1, &chain_params.consensus.upgrades)),
    });

    let commitments = chainstate.sprout_commitment_count().map_err(map_internal)?;
    let softforks = build_softfork_info(chainstate, best_block_hash, &chain_params.consensus)?;
    let value_pools = chainstate.value_pools_or_compute().map_err(map_internal)?;
    let utxo_stats = chainstate.utxo_stats_or_compute().map_err(map_internal)?;
    let shielded_total = value_pools
        .sprout
        .checked_add(value_pools.sapling)
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "shielded value pool overflow"))?;
    let total_supply = utxo_stats
        .total_amount
        .checked_add(shielded_total)
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "total supply overflow"))?;

    Ok(json!({
        "chain": network_name(chain_params.network),
        "blocks": best_block_height.max(0),
        "headers": best_header_height.max(0),
        "bestblockhash": best_block_hash.map(|hash| hash256_to_hex(&hash)),
        "difficulty": difficulty,
        "verificationprogress": verificationprogress,
        "chainwork": chainwork,
        "pruned": false,
        "size_on_disk": size_on_disk,
        "commitments": commitments,
        "valuePools": [
            {
                "id": "sprout",
                "monitored": true,
                "chainValue": amount_to_value(value_pools.sprout),
                "chainValueZat": value_pools.sprout,
            },
            {
                "id": "sapling",
                "monitored": true,
                "chainValue": amount_to_value(value_pools.sapling),
                "chainValueZat": value_pools.sapling,
            }
        ],
        "total_supply": amount_to_value(total_supply),
        "total_supply_zat": total_supply,
        "softforks": softforks,
        "upgrades": upgrades,
        "consensus": consensus
    }))
}

fn rpc_getdifficulty<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let best_header = chainstate
        .best_header()
        .map_err(map_internal)?
        .map(|tip| tip.hash);
    let difficulty = match best_header {
        Some(hash) => {
            let entry = chainstate
                .header_entry(&hash)
                .map_err(map_internal)?
                .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "missing header entry"))?;
            difficulty_from_bits(entry.bits, chain_params).unwrap_or(0.0)
        }
        None => 0.0,
    };
    Number::from_f64(difficulty)
        .map(Value::Number)
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "difficulty out of range"))
}

fn rpc_getchaintips<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let entries = chainstate.scan_headers().map_err(map_internal)?;
    if entries.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    let best_block = chainstate.best_block().map_err(map_internal)?;
    let best_hash = best_block.as_ref().map(|tip| tip.hash);
    let _best_height = best_block.as_ref().map(|tip| tip.height).unwrap_or(-1);

    let mut prevs = HashSet::new();
    for (_, entry) in &entries {
        prevs.insert(entry.prev_hash);
    }

    let mut tips = Vec::new();
    for (hash, entry) in entries {
        if prevs.contains(&hash) {
            continue;
        }
        let status = if Some(hash) == best_hash {
            "active"
        } else if entry.has_block() {
            "valid-fork"
        } else {
            "headers-only"
        };
        let branchlen = if status == "active" {
            0
        } else {
            branch_len(chainstate, &hash, entry.height)?
        };
        tips.push(json!({
            "height": entry.height,
            "hash": hash256_to_hex(&hash),
            "branchlen": branchlen,
            "status": status,
        }));
    }
    Ok(Value::Array(tips))
}

fn rpc_getblocksubsidy<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.len() > 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getblocksubsidy expects 0 or 1 parameter",
        ));
    }
    let height = if params.is_empty() {
        chainstate
            .best_block()
            .map_err(map_internal)?
            .map(|tip| tip.height)
            .unwrap_or(0)
    } else {
        parse_height(&params[0])?
    };
    if height < 0 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "block height out of range",
        ));
    }
    let subsidy = block_subsidy(height, &chain_params.consensus);
    Ok(json!({
        "miner": amount_to_value(subsidy),
    }))
}

fn rpc_createrawtransaction<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    const DEFAULT_TX_EXPIRY_DELTA: u32 = 20;
    const TX_EXPIRING_SOON_THRESHOLD: u32 = 3;

    if params.len() < 2 || params.len() > 4 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "createrawtransaction expects 2 to 4 parameters",
        ));
    }
    let inputs = params[0]
        .as_array()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "transactions must be a json array"))?;
    let outputs = params[1]
        .as_object()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "addresses must be a json object"))?;

    let next_height = chainstate
        .best_block()
        .map_err(map_internal)?
        .map(|tip| tip.height.saturating_add(1))
        .unwrap_or(1);
    let sapling_active = network_upgrade_active(
        next_height,
        &chain_params.consensus.upgrades,
        UpgradeIndex::Acadia,
    );

    let mut tx = Transaction {
        f_overwintered: sapling_active,
        version: if sapling_active { 4 } else { 1 },
        version_group_id: if sapling_active {
            SAPLING_VERSION_GROUP_ID
        } else {
            0
        },
        vin: Vec::new(),
        vout: Vec::new(),
        lock_time: 0,
        expiry_height: if sapling_active {
            (next_height as u32).saturating_add(DEFAULT_TX_EXPIRY_DELTA)
        } else {
            0
        },
        value_balance: 0,
        shielded_spends: Vec::new(),
        shielded_outputs: Vec::new(),
        join_splits: Vec::new(),
        join_split_pub_key: [0u8; 32],
        join_split_sig: [0u8; 64],
        binding_sig: [0u8; 64],
        fluxnode: None,
    };

    if params.len() > 2 && !params[2].is_null() {
        tx.lock_time = parse_u32(&params[2], "locktime")?;
    }
    if params.len() > 3 && !params[3].is_null() {
        if !sapling_active {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "expiryheight can only be used if sapling is active when the transaction is mined",
            ));
        }
        let expiry_height = parse_u32(&params[3], "expiryheight")?;
        if expiry_height >= fluxd_consensus::constants::TX_EXPIRY_HEIGHT_THRESHOLD {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "expiryheight too high",
            ));
        }
        if expiry_height != 0
            && (next_height as u32).saturating_add(TX_EXPIRING_SOON_THRESHOLD) > expiry_height
        {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "expiryheight too soon",
            ));
        }
        tx.expiry_height = expiry_height;
    }

    let default_sequence = if tx.lock_time != 0 {
        u32::MAX.saturating_sub(1)
    } else {
        u32::MAX
    };
    for input in inputs {
        let obj = input.as_object().ok_or_else(|| {
            RpcError::new(
                RPC_INVALID_PARAMETER,
                "transactions entries must be objects",
            )
        })?;
        let txid_value = obj
            .get("txid")
            .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "missing txid key"))?;
        let vout_value = obj
            .get("vout")
            .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "missing vout key"))?;
        let txid = parse_hash(txid_value)?;
        let vout = parse_u32(vout_value, "vout")?;

        let sequence = match obj.get("sequence") {
            Some(value) => parse_u32(value, "sequence")?,
            None => default_sequence,
        };

        tx.vin.push(TxIn {
            prevout: OutPoint {
                hash: txid,
                index: vout,
            },
            script_sig: Vec::new(),
            sequence,
        });
    }

    for (address, amount) in outputs {
        let script_pubkey =
            address_to_script_pubkey(address, chain_params.network).map_err(|err| match err {
                AddressError::InvalidLength
                | AddressError::InvalidCharacter
                | AddressError::InvalidChecksum
                | AddressError::UnknownPrefix => RpcError::new(
                    RPC_INVALID_ADDRESS_OR_KEY,
                    format!("Invalid Flux address: {address}"),
                ),
            })?;
        let value = parse_amount(amount)?;
        tx.vout.push(TxOut {
            value,
            script_pubkey,
        });
    }

    let encoded = tx.consensus_encode().map_err(map_internal)?;
    Ok(Value::String(hex_bytes(&encoded)))
}

fn rpc_decoderawtransaction(
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "decoderawtransaction expects 1 parameter",
        ));
    }
    let hex = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "hexstring must be a string"))?;
    let raw = bytes_from_hex(hex)
        .ok_or_else(|| RpcError::new(RPC_DESERIALIZATION_ERROR, "TX decode failed"))?;
    let tx = Transaction::consensus_decode(&raw)
        .map_err(|_| RpcError::new(RPC_DESERIALIZATION_ERROR, "TX decode failed"))?;
    tx_to_json(&tx, chain_params.network)
}

fn rpc_decodescript(params: Vec<Value>, chain_params: &ChainParams) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "decodescript expects 1 parameter",
        ));
    }
    let hex = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "hexstring must be a string"))?;
    let script = if hex.is_empty() {
        Vec::new()
    } else {
        bytes_from_hex(hex)
            .ok_or_else(|| RpcError::new(RPC_DESERIALIZATION_ERROR, "script decode failed"))?
    };

    let mut map = match script_pubkey_json(&script, chain_params.network) {
        Value::Object(map) => map,
        _ => return Err(RpcError::new(RPC_INTERNAL_ERROR, "invalid script json")),
    };
    map.insert(
        "p2sh".to_string(),
        Value::String(script_p2sh_address(&script, chain_params.network)),
    );
    Ok(Value::Object(map))
}

fn rpc_validateaddress(params: Vec<Value>, chain_params: &ChainParams) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "validateaddress expects 1 parameter",
        ));
    }
    let address = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "address must be a string"))?;
    let script_pubkey = match address_to_script_pubkey(address, chain_params.network) {
        Ok(script) => script,
        Err(_) => {
            return Ok(json!({
                "isvalid": false,
            }))
        }
    };
    Ok(json!({
        "isvalid": true,
        "address": address,
        "scriptPubKey": hex_bytes(&script_pubkey),
    }))
}

fn rpc_verifymessage(params: Vec<Value>, chain_params: &ChainParams) -> Result<Value, RpcError> {
    if params.len() != 3 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "verifymessage expects 3 parameters",
        ));
    }
    let address = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "address must be a string"))?;
    let signature = params[1]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "signature must be a string"))?;
    let message = params[2]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "message must be a string"))?;

    let script_pubkey = address_to_script_pubkey(address, chain_params.network)
        .map_err(|_| RpcError::new(RPC_TYPE_ERROR, "Invalid address"))?;
    if classify_script_pubkey(&script_pubkey) != ScriptType::P2Pkh {
        return Err(RpcError::new(
            RPC_TYPE_ERROR,
            "Address does not refer to key",
        ));
    }
    if script_pubkey.len() < 23 {
        return Err(RpcError::new(RPC_INTERNAL_ERROR, "invalid scriptPubKey"));
    }
    let expected_key_hash: [u8; 20] = script_pubkey[3..23]
        .try_into()
        .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "invalid scriptPubKey"))?;

    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(signature.as_bytes())
        .map_err(|_| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Malformed base64 encoding"))?;

    let recovered_pubkey = match recover_signed_message_pubkey(&sig_bytes, message.as_bytes()) {
        Ok(pubkey) => pubkey,
        Err(_) => return Ok(Value::Bool(false)),
    };
    let recovered_hash = hash160(&recovered_pubkey);
    Ok(Value::Bool(recovered_hash == expected_key_hash))
}

fn rpc_createmultisig(params: Vec<Value>, chain_params: &ChainParams) -> Result<Value, RpcError> {
    const MAX_PUBKEYS: usize = 16;
    const MAX_SCRIPT_ELEMENT_SIZE: usize = 520;

    if params.len() != 2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "createmultisig expects 2 parameters",
        ));
    }
    let required = parse_u32(&params[0], "nrequired")? as usize;
    if required < 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "a multisignature address must require at least one key to redeem",
        ));
    }
    let keys = params[1].as_array().ok_or_else(|| {
        RpcError::new(
            RPC_INVALID_PARAMETER,
            "keys must be a json array of strings",
        )
    })?;
    if keys.len() < required {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!(
                "not enough keys supplied (got {} keys, but need at least {required} to redeem)",
                keys.len()
            ),
        ));
    }
    if keys.len() > MAX_PUBKEYS {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "Number of addresses involved in the multisignature address creation > 16\nReduce the number",
        ));
    }

    let mut pubkeys = Vec::with_capacity(keys.len());
    for key in keys {
        let key = key
            .as_str()
            .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "keys must be strings"))?;
        let bytes = bytes_from_hex(key).ok_or_else(|| {
            RpcError::new(RPC_INVALID_PARAMETER, format!(" Invalid public key: {key}"))
        })?;
        PublicKey::from_slice(&bytes).map_err(|_| {
            RpcError::new(RPC_INVALID_PARAMETER, format!(" Invalid public key: {key}"))
        })?;
        pubkeys.push(bytes);
    }

    let mut redeem_script = Vec::new();
    redeem_script.push(multisig_small_int_opcode(required)?);
    for pubkey in &pubkeys {
        redeem_script.push(pubkey.len() as u8);
        redeem_script.extend_from_slice(pubkey);
    }
    redeem_script.push(multisig_small_int_opcode(pubkeys.len())?);
    redeem_script.push(0xae);

    if redeem_script.len() > MAX_SCRIPT_ELEMENT_SIZE {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            format!(
                "redeemScript exceeds size limit: {} > {}",
                redeem_script.len(),
                MAX_SCRIPT_ELEMENT_SIZE
            ),
        ));
    }

    Ok(json!({
        "address": script_p2sh_address(&redeem_script, chain_params.network),
        "redeemScript": hex_bytes(&redeem_script),
    }))
}

fn rpc_getrawtransaction<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.is_empty() || params.len() > 2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getrawtransaction expects 1 or 2 parameters",
        ));
    }
    let txid = parse_hash(&params[0])?;
    let verbose = if params.len() > 1 {
        parse_verbose_flag(&params[1])?
    } else {
        false
    };
    if let Ok(guard) = mempool.lock() {
        if let Some(entry) = guard.get(&txid) {
            if !verbose {
                return Ok(Value::String(hex_bytes(&entry.raw)));
            }
            let mut obj = match tx_to_json(&entry.tx, chain_params.network)? {
                Value::Object(map) => map,
                _ => return Err(RpcError::new(RPC_INTERNAL_ERROR, "invalid tx json")),
            };
            obj.insert("hex".to_string(), Value::String(hex_bytes(&entry.raw)));
            return Ok(Value::Object(obj));
        }
    }
    let location = chainstate
        .tx_location(&txid)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "transaction not found"))?;
    let bytes = chainstate
        .read_block(location.block)
        .map_err(map_internal)?;
    let block = fluxd_primitives::block::Block::consensus_decode(&bytes).map_err(map_internal)?;
    let tx_index = location.index as usize;
    let tx = block
        .transactions
        .get(tx_index)
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "transaction index out of range"))?;
    let encoded = tx.consensus_encode().map_err(map_internal)?;
    if !verbose {
        return Ok(Value::String(hex_bytes(&encoded)));
    }
    let mut obj = match tx_to_json(tx, chain_params.network)? {
        Value::Object(map) => map,
        _ => return Err(RpcError::new(RPC_INTERNAL_ERROR, "invalid tx json")),
    };
    obj.insert("hex".to_string(), Value::String(hex_bytes(&encoded)));
    let block_hash = block.header.hash();
    if let Ok(Some(entry)) = chainstate.header_entry(&block_hash) {
        let best_height = best_block_height(chainstate)?;
        let confirmations =
            confirmations_for_height(chainstate, entry.height, best_height, &block_hash)?;
        obj.insert(
            "blockhash".to_string(),
            Value::String(hash256_to_hex(&block_hash)),
        );
        obj.insert(
            "confirmations".to_string(),
            Value::Number(confirmations.into()),
        );
        obj.insert("time".to_string(), Value::Number(block.header.time.into()));
        obj.insert(
            "blocktime".to_string(),
            Value::Number(block.header.time.into()),
        );
        obj.insert("height".to_string(), Value::Number(entry.height.into()));
    }
    Ok(Value::Object(obj))
}

fn rpc_sendrawtransaction<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
    mempool_policy: &MempoolPolicy,
    mempool_metrics: &MempoolMetrics,
    fee_estimator: &Mutex<FeeEstimator>,
    mempool_flags: &ValidationFlags,
    params: Vec<Value>,
    chain_params: &ChainParams,
    tx_announce: &broadcast::Sender<Hash256>,
) -> Result<Value, RpcError> {
    let result = (|| {
        if params.is_empty() || params.len() > 2 {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "sendrawtransaction expects 1 or 2 parameters",
            ));
        }
        let hex = params[0]
            .as_str()
            .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "hexstring must be a string"))?;
        let _allow_high_fees = if params.len() > 1 {
            parse_bool(&params[1])?
        } else {
            false
        };
        let raw = bytes_from_hex(hex)
            .ok_or_else(|| RpcError::new(RPC_DESERIALIZATION_ERROR, "TX decode failed"))?;
        let tx = Transaction::consensus_decode(&raw)
            .map_err(|_| RpcError::new(RPC_DESERIALIZATION_ERROR, "TX decode failed"))?;
        let txid = tx
            .txid()
            .map_err(|_| RpcError::new(RPC_DESERIALIZATION_ERROR, "TX decode failed"))?;

        if chainstate
            .tx_location(&txid)
            .map_err(map_internal)?
            .is_some()
        {
            return Err(RpcError::new(
                RPC_TRANSACTION_ALREADY_IN_CHAIN,
                "transaction already in block chain",
            ));
        }

        let mempool_prevouts = {
            let guard = mempool
                .lock()
                .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "mempool lock poisoned"))?;
            if guard.contains(&txid) {
                let _ = tx_announce.send(txid);
                return Ok(Value::String(hash256_to_hex(&txid)));
            }
            for input in &tx.vin {
                if let Some(spender) = guard.spender(&input.prevout) {
                    return Err(RpcError::new(
                        RPC_TRANSACTION_REJECTED,
                        format!("input already spent by {}", hash256_to_hex(&spender)),
                    ));
                }
            }
            guard.prevouts_for_tx(&tx)
        };

        let entry = build_mempool_entry(
            chainstate,
            &mempool_prevouts,
            chain_params,
            mempool_flags,
            mempool_policy,
            tx,
            raw,
        )
        .map_err(|err| match err.kind {
            MempoolErrorKind::MissingInput => {
                RpcError::new(RPC_TRANSACTION_ERROR, "Missing inputs")
            }
            MempoolErrorKind::ConflictingInput => {
                RpcError::new(RPC_TRANSACTION_REJECTED, err.message)
            }
            MempoolErrorKind::InsufficientFee => {
                RpcError::new(RPC_TRANSACTION_REJECTED, err.message)
            }
            MempoolErrorKind::MempoolFull => RpcError::new(RPC_TRANSACTION_REJECTED, err.message),
            MempoolErrorKind::NonStandard => RpcError::new(RPC_TRANSACTION_REJECTED, err.message),
            MempoolErrorKind::InvalidTransaction
            | MempoolErrorKind::InvalidScript
            | MempoolErrorKind::InvalidShielded => {
                RpcError::new(RPC_TRANSACTION_REJECTED, err.message)
            }
            MempoolErrorKind::AlreadyInMempool => {
                RpcError::new(RPC_INTERNAL_ERROR, "unexpected mempool duplicate")
            }
            MempoolErrorKind::Internal => RpcError::new(RPC_INTERNAL_ERROR, err.message),
        })?;

        let should_observe_fee = entry.tx.fluxnode.is_none();
        let entry_fee = entry.fee;
        let entry_size = entry.size();
        let txid = entry.txid;
        let mut guard = mempool
            .lock()
            .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "mempool lock poisoned"))?;
        match guard.insert(entry) {
            Ok(outcome) => {
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
                if err.kind != MempoolErrorKind::AlreadyInMempool {
                    return Err(RpcError::new(RPC_TRANSACTION_REJECTED, err.message));
                }
            }
        }
        let _ = tx_announce.send(txid);
        Ok(Value::String(hash256_to_hex(&txid)))
    })();

    match result {
        Ok(value) => {
            mempool_metrics.note_rpc_accept();
            Ok(value)
        }
        Err(err) => {
            mempool_metrics.note_rpc_reject();
            Err(err)
        }
    }
}

fn rpc_getmempoolinfo(params: Vec<Value>, mempool: &Mutex<Mempool>) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let guard = mempool
        .lock()
        .map_err(|_| map_internal("mempool lock poisoned"))?;
    Ok(json!({
        "size": guard.size(),
        "bytes": guard.bytes(),
        "usage": guard.usage(),
    }))
}

fn rpc_getrawmempool(params: Vec<Value>, mempool: &Mutex<Mempool>) -> Result<Value, RpcError> {
    if params.len() > 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getrawmempool expects 0 or 1 parameter",
        ));
    }
    let verbose = if params.is_empty() {
        false
    } else {
        parse_verbose_flag(&params[0])?
    };
    let guard = mempool
        .lock()
        .map_err(|_| map_internal("mempool lock poisoned"))?;
    if !verbose {
        return Ok(Value::Array(
            guard
                .txids()
                .into_iter()
                .map(|txid| Value::String(hash256_to_hex(&txid)))
                .collect(),
        ));
    }
    let mut out = serde_json::Map::new();
    for entry in guard.entries() {
        out.insert(
            hash256_to_hex(&entry.txid),
            json!({
                "size": entry.size(),
                "fee": amount_to_value(entry.fee),
                "time": entry.time,
                "height": entry.height.max(0),
                "startingpriority": 0,
                "currentpriority": 0,
                "depends": [],
            }),
        );
    }
    Ok(Value::Object(out))
}

fn rpc_gettxout<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.len() < 2 || params.len() > 3 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "gettxout expects 2 or 3 parameters",
        ));
    }
    let txid = parse_hash(&params[0])?;
    let index = params[1]
        .as_u64()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "vout must be numeric"))?;
    let outpoint = fluxd_primitives::outpoint::OutPoint {
        hash: txid,
        index: index as u32,
    };
    let include_mempool = if params.len() > 2 {
        parse_verbose_flag(&params[2])?
    } else {
        true
    };
    if include_mempool {
        let guard = mempool
            .lock()
            .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "mempool lock poisoned"))?;
        if guard.is_spent(&outpoint) {
            return Ok(Value::Null);
        }
        if let Some(entry) = guard.get(&txid) {
            let out_index = outpoint.index as usize;
            let Some(output) = entry.tx.vout.get(out_index) else {
                return Ok(Value::Null);
            };
            let best = chainstate
                .best_block()
                .map_err(map_internal)?
                .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "best block not found"))?;
            let script = script_pubkey_json(&output.script_pubkey, chain_params.network);
            return Ok(json!({
                "bestblock": hash256_to_hex(&best.hash),
                "confirmations": 0,
                "value": amount_to_value(output.value),
                "scriptPubKey": script,
                "version": entry.tx.version,
                "coinbase": false,
            }));
        }
    }
    let entry = match chainstate.utxo_entry(&outpoint).map_err(map_internal)? {
        Some(entry) => entry,
        None => return Ok(Value::Null),
    };
    let tx_version = {
        let location = chainstate
            .tx_location(&txid)
            .map_err(map_internal)?
            .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "transaction not found"))?;
        let bytes = chainstate
            .read_block(location.block)
            .map_err(map_internal)?;
        let block = Block::consensus_decode(&bytes).map_err(map_internal)?;
        block
            .transactions
            .get(location.index as usize)
            .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "transaction index out of range"))?
            .version
    };
    let best = chainstate
        .best_block()
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "best block not found"))?;
    let confirmations = if best.height >= entry.height as i32 {
        (best.height - entry.height as i32 + 1).max(0)
    } else {
        0
    };
    let script = script_pubkey_json(&entry.script_pubkey, chain_params.network);
    Ok(json!({
        "bestblock": hash256_to_hex(&best.hash),
        "confirmations": confirmations,
        "value": amount_to_value(entry.value),
        "scriptPubKey": script,
        "version": tx_version,
        "coinbase": entry.is_coinbase,
    }))
}

fn rpc_gettxoutproof<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    if params.is_empty() || params.len() > 2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "gettxoutproof expects 1 or 2 parameters",
        ));
    }

    let txids_value = params[0]
        .as_array()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "txids must be an array"))?;
    if txids_value.is_empty() {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "txids must be a non-empty array",
        ));
    }

    let mut set_txids = HashSet::with_capacity(txids_value.len());
    let mut one_txid = None;
    for value in txids_value {
        let text = value
            .as_str()
            .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "txid must be a string"))?;
        if text.len() != 64 {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("Invalid txid {text}"),
            ));
        }
        let txid = hash256_from_hex(text)
            .map_err(|_| RpcError::new(RPC_INVALID_PARAMETER, format!("Invalid txid {text}")))?;
        if !set_txids.insert(txid) {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                format!("Invalid parameter, duplicated txid: {text}"),
            ));
        }
        one_txid = Some(txid);
    }
    let one_txid = one_txid.expect("non-empty txids array");

    let location = if params.len() > 1 {
        let block_hash = parse_hash(&params[1])?;
        chainstate
            .block_location(&block_hash)
            .map_err(map_internal)?
            .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"))?
    } else {
        let tx_location = chainstate
            .tx_location(&one_txid)
            .map_err(map_internal)?
            .ok_or_else(|| {
                RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Transaction not yet in block")
            })?;
        tx_location.block
    };

    let bytes = chainstate.read_block(location).map_err(map_internal)?;
    let block = Block::consensus_decode(&bytes).map_err(map_internal)?;

    let mut txids = Vec::with_capacity(block.transactions.len());
    let mut matches = Vec::with_capacity(block.transactions.len());
    let mut found = 0usize;
    for tx in &block.transactions {
        let txid = tx.txid().map_err(map_internal)?;
        let matched = set_txids.contains(&txid);
        if matched {
            found = found.saturating_add(1);
        }
        txids.push(txid);
        matches.push(matched);
    }

    if found != set_txids.len() {
        return Err(RpcError::new(
            RPC_INVALID_ADDRESS_OR_KEY,
            "(Not all) transactions not found in specified block",
        ));
    }

    let tree = PartialMerkleTree::from_txids(&txids, &matches).map_err(map_internal)?;
    let proof = MerkleBlock {
        header: block.header,
        txn: tree,
    };
    Ok(Value::String(hex_bytes(&proof.consensus_encode())))
}

fn rpc_verifytxoutproof<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "verifytxoutproof expects 1 parameter",
        ));
    }

    let hex = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "proof must be a string"))?;
    let bytes = bytes_from_hex(hex)
        .ok_or_else(|| RpcError::new(RPC_DESERIALIZATION_ERROR, "Merkle block decode failed"))?;
    let merkle_block = MerkleBlock::consensus_decode(&bytes)
        .map_err(|_| RpcError::new(RPC_DESERIALIZATION_ERROR, "Merkle block decode failed"))?;

    let (root, matches) = match merkle_block.txn.extract_matches() {
        Some(value) => value,
        None => return Ok(Value::Array(Vec::new())),
    };
    if root != merkle_block.header.merkle_root {
        return Ok(Value::Array(Vec::new()));
    }

    let block_hash = merkle_block.header.hash();
    let entry = chainstate
        .header_entry(&block_hash)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found in chain"))?;
    let best_at_height = chainstate
        .height_hash(entry.height)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found in chain"))?;
    if best_at_height != block_hash {
        return Err(RpcError::new(
            RPC_INVALID_ADDRESS_OR_KEY,
            "Block not found in chain",
        ));
    }

    let mut out = Vec::with_capacity(matches.len());
    for txid in matches {
        out.push(Value::String(hash256_to_hex(&txid)));
    }
    Ok(Value::Array(out))
}

fn rpc_createfluxnodekey(
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;

    let mut rng = rand::rngs::OsRng;
    let mut seed = [0u8; 32];
    for _ in 0..100 {
        rng.fill_bytes(&mut seed);
        if let Ok(secret) = SecretKey::from_slice(&seed) {
            let wif = secret_key_to_wif(&secret.secret_bytes(), chain_params.network, false);
            return Ok(Value::String(wif));
        }
    }

    Err(RpcError::new(
        RPC_INTERNAL_ERROR,
        "failed to generate secret key",
    ))
}

fn rpc_gettxoutsetinfo<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    data_dir: &Path,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let best = chainstate.best_block().map_err(map_internal)?;
    let (height, best_hash) = match best {
        Some(tip) => (tip.height, tip.hash),
        None => (0, [0u8; 32]),
    };
    let utxo_info = chainstate.utxo_set_info().map_err(map_internal)?;
    let value_pools = chainstate.value_pools_or_compute().map_err(map_internal)?;
    let shielded_total = value_pools
        .sprout
        .checked_add(value_pools.sapling)
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "shielded value pool overflow"))?;
    let total_supply = utxo_info
        .total_amount
        .checked_add(shielded_total)
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "total supply overflow"))?;
    let disk_size =
        db_info::dir_size_cached(&data_dir.join("db"), Duration::from_secs(30)).unwrap_or(0);
    Ok(json!({
        "height": height.max(0),
        "bestblock": hash256_to_hex(&best_hash),
        "transactions": utxo_info.transactions,
        "txouts": utxo_info.txouts,
        "bytes_serialized": utxo_info.bytes_serialized,
        "hash_serialized": hash256_to_hex(&utxo_info.hash_serialized),
        "disk_size": disk_size,
        "total_amount": amount_to_value(utxo_info.total_amount),
        "total_amount_zat": utxo_info.total_amount,
        "sprout_pool": amount_to_value(value_pools.sprout),
        "sprout_pool_zat": value_pools.sprout,
        "sapling_pool": amount_to_value(value_pools.sapling),
        "sapling_pool_zat": value_pools.sapling,
        "shielded_amount": amount_to_value(shielded_total),
        "shielded_amount_zat": shielded_total,
        "total_supply": amount_to_value(total_supply),
        "total_supply_zat": total_supply,
    }))
}

fn rpc_verifychain<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    if params.len() > 2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "verifychain expects 0 to 2 parameters",
        ));
    }

    let checklevel = if let Some(value) = params.get(0) {
        parse_u32(value, "checklevel")?
    } else {
        3
    };
    if checklevel > 4 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "checklevel must be 0-4",
        ));
    }

    let numblocks = if let Some(value) = params.get(1) {
        parse_u32(value, "numblocks")?
    } else {
        288
    };

    let result = verify_chain_impl(chainstate, checklevel, numblocks);
    if let Err(reason) = result.as_ref() {
        eprintln!("verifychain failed: {reason}");
    }
    Ok(Value::Bool(result.is_ok()))
}

fn verify_chain_impl<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    checklevel: u32,
    numblocks: u32,
) -> Result<(), String> {
    if checklevel == 0 {
        return Ok(());
    }

    let best = chainstate.best_block().map_err(|err| err.to_string())?;
    let Some(best) = best else {
        return Ok(());
    };
    let best_height = best.height.max(0) as u32;
    let mut remaining = if numblocks == 0 {
        best_height.saturating_add(1)
    } else {
        numblocks.min(best_height.saturating_add(1))
    };

    let mut current_hash = best.hash;
    while remaining > 0 {
        let entry = chainstate
            .header_entry(&current_hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing header entry {}", hash256_to_hex(&current_hash)))?;
        if !entry.has_block() {
            return Err(format!(
                "missing block data at height {}",
                entry.height.max(0)
            ));
        }
        let main_hash = chainstate
            .height_hash(entry.height)
            .map_err(|err| err.to_string())?;
        if main_hash.as_ref() != Some(&current_hash) {
            return Err(format!("height index mismatch at {}", entry.height.max(0)));
        }

        if checklevel >= 1 {
            let block_location = chainstate
                .block_location(&current_hash)
                .map_err(|err| err.to_string())?
                .ok_or_else(|| format!("missing block index {}", hash256_to_hex(&current_hash)))?;
            let bytes = chainstate
                .read_block(block_location)
                .map_err(|err| err.to_string())?;
            let block = fluxd_primitives::block::Block::consensus_decode(&bytes)
                .map_err(|err| err.to_string())?;
            if block.header.hash() != current_hash {
                return Err(format!(
                    "block hash mismatch at height {}",
                    entry.height.max(0)
                ));
            }
            if block.header.prev_block != entry.prev_hash {
                return Err(format!(
                    "block prev-hash mismatch at height {}",
                    entry.height.max(0)
                ));
            }

            if checklevel >= 2 {
                let mut txids = Vec::with_capacity(block.transactions.len());
                for tx in &block.transactions {
                    txids.push(tx.txid().map_err(|err| err.to_string())?);
                }
                let root = compute_merkle_root(&txids);
                if root != block.header.merkle_root {
                    return Err(format!(
                        "merkle root mismatch at height {}",
                        entry.height.max(0)
                    ));
                }

                if checklevel >= 3 {
                    for (index, txid) in txids.into_iter().enumerate() {
                        let tx_location = chainstate
                            .tx_location(&txid)
                            .map_err(|err| err.to_string())?
                            .ok_or_else(|| {
                                format!("missing txindex entry {}", hash256_to_hex(&txid))
                            })?;
                        if tx_location.block != block_location {
                            return Err(format!(
                                "txindex block location mismatch {}",
                                hash256_to_hex(&txid)
                            ));
                        }
                        if tx_location.index != index as u32 {
                            return Err(format!(
                                "txindex position mismatch {}",
                                hash256_to_hex(&txid)
                            ));
                        }
                    }
                }
            }
        }

        remaining = remaining.saturating_sub(1);
        if entry.height == 0 {
            break;
        }
        current_hash = entry.prev_hash;
    }

    Ok(())
}

fn compute_merkle_root(txids: &[Hash256]) -> Hash256 {
    if txids.is_empty() {
        return [0u8; 32];
    }
    let mut layer = txids.to_vec();
    while layer.len() > 1 {
        if layer.len() % 2 == 1 {
            let last = *layer.last().expect("non-empty");
            layer.push(last);
        }
        let mut next = Vec::with_capacity((layer.len() + 1) / 2);
        for pair in layer.chunks(2) {
            next.push(merkle_hash_pair(&pair[0], &pair[1]));
        }
        layer = next;
    }
    layer[0]
}

fn merkle_hash_pair(left: &Hash256, right: &Hash256) -> Hash256 {
    let mut buf = [0u8; 64];
    buf[0..32].copy_from_slice(left);
    buf[32..64].copy_from_slice(right);
    sha256d(&buf)
}

fn rpc_getblockdeltas<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getblockdeltas expects 1 parameter",
        ));
    }
    let (hash, entry) = resolve_block_hash(chainstate, &params[0])?;

    let best_height = best_block_height(chainstate)?;
    let main_hash = chainstate.height_hash(entry.height).map_err(map_internal)?;
    if main_hash.as_ref() != Some(&hash) {
        return Err(RpcError::new(
            RPC_INVALID_ADDRESS_OR_KEY,
            "Block is an orphan",
        ));
    }

    let location = chainstate
        .block_location(&hash)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Block not found"))?;
    let bytes = chainstate.read_block(location).map_err(map_internal)?;
    let block = fluxd_primitives::block::Block::consensus_decode(&bytes).map_err(map_internal)?;

    let mut deltas = Vec::with_capacity(block.transactions.len());
    let mut tx_cache: HashMap<Hash256, Transaction> = HashMap::new();

    for (tx_index, tx) in block.transactions.iter().enumerate() {
        let txid = tx.txid().map_err(map_internal)?;
        let mut entry_obj = serde_json::Map::new();
        entry_obj.insert("txid".to_string(), Value::String(hash256_to_hex(&txid)));
        entry_obj.insert("index".to_string(), Value::Number((tx_index as i64).into()));

        let mut inputs = Vec::new();
        let is_coinbase =
            tx.vin.len() == 1 && tx.vin[0].prevout == fluxd_primitives::outpoint::OutPoint::null();
        if !is_coinbase {
            for (vin_index, input) in tx.vin.iter().enumerate() {
                let mut delta = serde_json::Map::new();

                let (satoshis, address) = match chainstate
                    .spent_info(&input.prevout)
                    .map_err(map_internal)?
                {
                    Some(spent) => {
                        if let Some(details) = spent.details {
                            (
                                details.satoshis,
                                spent_details_address(
                                    details.address_type,
                                    &details.address_hash,
                                    chain_params.network,
                                ),
                            )
                        } else {
                            resolve_prevout_via_txindex(
                                chainstate,
                                &mut tx_cache,
                                &input.prevout,
                                chain_params.network,
                            )?
                        }
                    }
                    None => resolve_prevout_via_txindex(
                        chainstate,
                        &mut tx_cache,
                        &input.prevout,
                        chain_params.network,
                    )?,
                };

                if let Some(address) = address {
                    delta.insert("address".to_string(), Value::String(address));
                }

                delta.insert("satoshis".to_string(), Value::from(-satoshis));
                delta.insert(
                    "index".to_string(),
                    Value::Number((vin_index as i64).into()),
                );
                delta.insert(
                    "prevtxid".to_string(),
                    Value::String(hash256_to_hex(&input.prevout.hash)),
                );
                delta.insert(
                    "prevout".to_string(),
                    Value::Number((input.prevout.index as i64).into()),
                );
                inputs.push(Value::Object(delta));
            }
        }
        entry_obj.insert("inputs".to_string(), Value::Array(inputs));

        let mut outputs = Vec::with_capacity(tx.vout.len());
        for (vout_index, output) in tx.vout.iter().enumerate() {
            let address = script_pubkey_to_address(&output.script_pubkey, chain_params.network)
                .unwrap_or_default();
            outputs.push(json!({
                "address": address,
                "satoshis": output.value,
                "index": vout_index,
            }));
        }
        entry_obj.insert("outputs".to_string(), Value::Array(outputs));
        deltas.push(Value::Object(entry_obj));
    }

    let confirmations = best_height - entry.height + 1;
    let next_hash = next_hash_for_height(chainstate, entry.height, best_height, &hash)?;
    let mediantime = median_time_past(chainstate, entry.height)?;

    let mut result = json!({
        "hash": hash256_to_hex(&hash),
        "confirmations": confirmations,
        "size": bytes.len(),
        "height": entry.height,
        "version": block.header.version,
        "merkleroot": hash256_to_hex(&block.header.merkle_root),
        "deltas": Value::Array(deltas),
        "time": block.header.time,
        "mediantime": mediantime,
        "nonce": hash256_to_hex(&block.header.nonce),
        "bits": format!("{:08x}", block.header.bits),
        "difficulty": difficulty_from_bits(block.header.bits, chain_params).unwrap_or(0.0),
        "chainwork": hex_bytes(&entry.chainwork),
    });

    if entry.height > 0 {
        result["previousblockhash"] = Value::String(hash256_to_hex(&entry.prev_hash));
    }
    if let Some(next_hash) = next_hash {
        result["nextblockhash"] = Value::String(hash256_to_hex(&next_hash));
    }

    Ok(result)
}

fn rpc_getspentinfo<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    let (txid, index) = match params.as_slice() {
        [Value::Object(map)] => {
            let txid_value = map
                .get("txid")
                .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "getspentinfo missing txid"))?;
            let index_value = map.get("index").ok_or_else(|| {
                RpcError::new(RPC_INVALID_PARAMETER, "getspentinfo missing index")
            })?;
            (parse_hash(txid_value)?, parse_u32(index_value, "index")?)
        }
        [txid_value, index_value] => (parse_hash(txid_value)?, parse_u32(index_value, "index")?),
        _ => {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "getspentinfo expects {\"txid\": \"...\", \"index\": n}",
            ))
        }
    };

    let outpoint = fluxd_primitives::outpoint::OutPoint { hash: txid, index };
    let spent = chainstate
        .spent_info(&outpoint)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Unable to get spent info"))?;

    Ok(json!({
        "txid": hash256_to_hex(&spent.txid),
        "index": spent.input_index,
        "height": spent.block_height,
    }))
}

fn rpc_getaddressutxos<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getaddressutxos expects 1 parameter",
        ));
    }
    let (addresses, opts) = parse_addresses_param(&params[0])?;
    let include_chain_info = parse_chain_info_flag(opts)?;
    let address_scripts = decode_address_scripts(addresses, chain_params.network)?;

    #[derive(Clone)]
    struct UtxoRow {
        address: String,
        txid: Hash256,
        output_index: u32,
        script: Vec<u8>,
        satoshis: i64,
        height: u32,
    }

    let mut rows = Vec::new();
    for (address, script_pubkey) in address_scripts {
        let outpoints = chainstate
            .address_outpoints(&script_pubkey)
            .map_err(map_internal)?;
        for outpoint in outpoints {
            let entry = chainstate
                .utxo_entry(&outpoint)
                .map_err(map_internal)?
                .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "missing utxo entry"))?;
            rows.push(UtxoRow {
                address: address.clone(),
                txid: outpoint.hash,
                output_index: outpoint.index,
                script: entry.script_pubkey,
                satoshis: entry.value,
                height: entry.height,
            });
        }
    }

    rows.sort_by(|a, b| {
        a.height
            .cmp(&b.height)
            .then_with(|| a.txid.cmp(&b.txid))
            .then_with(|| a.output_index.cmp(&b.output_index))
    });

    let utxos = rows
        .into_iter()
        .map(|row| {
            json!({
                "address": row.address,
                "txid": hash256_to_hex(&row.txid),
                "outputIndex": row.output_index,
                "script": hex_bytes(&row.script),
                "satoshis": row.satoshis,
                "height": row.height,
            })
        })
        .collect::<Vec<_>>();

    if !include_chain_info {
        return Ok(Value::Array(utxos));
    }

    let best = chainstate.best_block().map_err(map_internal)?;
    let (height, hash) = match best {
        Some(tip) => (tip.height, tip.hash),
        None => (0, [0u8; 32]),
    };

    Ok(json!({
        "utxos": utxos,
        "hash": hash256_to_hex(&hash),
        "height": height.max(0),
    }))
}

fn rpc_getaddressbalance<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getaddressbalance expects 1 parameter",
        ));
    }
    let (addresses, _opts) = parse_addresses_param(&params[0])?;
    let address_scripts = decode_address_scripts(addresses, chain_params.network)?;

    let mut balance = 0i64;
    let mut received = 0i64;
    for (_address, script_pubkey) in address_scripts {
        let mut visitor = |delta: fluxd_chainstate::address_deltas::AddressDeltaEntry| {
            if delta.satoshis > 0 {
                received = received.checked_add(delta.satoshis).ok_or_else(|| {
                    fluxd_storage::StoreError::Backend("address balance overflow".to_string())
                })?;
            }
            balance = balance.checked_add(delta.satoshis).ok_or_else(|| {
                fluxd_storage::StoreError::Backend("address balance overflow".to_string())
            })?;
            Ok(())
        };
        chainstate
            .for_each_address_delta(&script_pubkey, &mut visitor)
            .map_err(map_internal)?;
    }

    Ok(json!({
        "balance": balance,
        "received": received,
    }))
}

fn rpc_getaddressdeltas<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getaddressdeltas expects 1 parameter",
        ));
    }
    let (addresses, opts) = parse_addresses_param(&params[0])?;
    let include_chain_info = parse_chain_info_flag(opts)?;
    let range = parse_height_range(chainstate, opts)?;
    let address_scripts = decode_address_scripts(addresses, chain_params.network)?;

    #[derive(Clone)]
    struct DeltaRow {
        address: String,
        height: u32,
        tx_index: u32,
        txid: Hash256,
        index: u32,
        satoshis: i64,
    }

    let mut rows = Vec::new();
    for (address, script_pubkey) in address_scripts {
        let deltas = chainstate
            .address_deltas(&script_pubkey)
            .map_err(map_internal)?;
        for delta in deltas {
            if let Some((start, end)) = range {
                if delta.height < start || delta.height > end {
                    continue;
                }
            }
            rows.push(DeltaRow {
                address: address.clone(),
                height: delta.height,
                tx_index: delta.tx_index,
                txid: delta.txid,
                index: delta.index,
                satoshis: delta.satoshis,
            });
        }
    }

    rows.sort_by(|a, b| {
        a.height
            .cmp(&b.height)
            .then_with(|| a.tx_index.cmp(&b.tx_index))
            .then_with(|| a.txid.cmp(&b.txid))
            .then_with(|| a.index.cmp(&b.index))
            .then_with(|| a.address.cmp(&b.address))
    });

    let deltas = rows
        .into_iter()
        .map(|row| {
            json!({
                "address": row.address,
                "blockindex": row.tx_index,
                "height": row.height,
                "index": row.index,
                "satoshis": row.satoshis,
                "txid": hash256_to_hex(&row.txid),
            })
        })
        .collect::<Vec<_>>();

    let Some((start, end)) = range else {
        return Ok(Value::Array(deltas));
    };
    if !include_chain_info {
        return Ok(Value::Array(deltas));
    }

    let start_height = i32::try_from(start).map_err(|_| {
        RpcError::new(
            RPC_INVALID_ADDRESS_OR_KEY,
            "Start or end is outside chain range",
        )
    })?;
    let end_height = i32::try_from(end).map_err(|_| {
        RpcError::new(
            RPC_INVALID_ADDRESS_OR_KEY,
            "Start or end is outside chain range",
        )
    })?;
    let start_hash = chainstate
        .height_hash(start_height)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "block not found"))?;
    let end_hash = chainstate
        .height_hash(end_height)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "block not found"))?;

    Ok(json!({
        "deltas": deltas,
        "start": {
            "hash": hash256_to_hex(&start_hash),
            "height": start,
        },
        "end": {
            "hash": hash256_to_hex(&end_hash),
            "height": end,
        },
    }))
}

fn rpc_getaddresstxids<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getaddresstxids expects 1 parameter",
        ));
    }
    let (addresses, opts) = parse_addresses_param(&params[0])?;
    let range = parse_height_range(chainstate, opts)?;
    let address_scripts = decode_address_scripts(addresses, chain_params.network)?;

    let mut txids = std::collections::BTreeSet::<(u32, Hash256)>::new();
    for (_address, script_pubkey) in address_scripts {
        let deltas = chainstate
            .address_deltas(&script_pubkey)
            .map_err(map_internal)?;
        for delta in deltas {
            if let Some((start, end)) = range {
                if delta.height < start || delta.height > end {
                    continue;
                }
            }
            txids.insert((delta.height, delta.txid));
        }
    }

    Ok(Value::Array(
        txids
            .into_iter()
            .map(|(_height, txid)| Value::String(hash256_to_hex(&txid)))
            .collect(),
    ))
}

fn rpc_getaddressmempool<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getaddressmempool expects 1 parameter",
        ));
    }
    let (addresses, _opts) = parse_addresses_param(&params[0])?;
    let address_scripts = decode_address_scripts(addresses, chain_params.network)?;

    let mut script_to_address = HashMap::new();
    for (address, script_pubkey) in address_scripts {
        script_to_address.insert(script_pubkey, address);
    }

    #[derive(Clone)]
    struct DeltaRow {
        address: String,
        txid: Hash256,
        index: u32,
        satoshis: i64,
        timestamp: u64,
        prevtxid: Option<Hash256>,
        prevout: Option<u32>,
    }

    let guard = mempool
        .lock()
        .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "mempool lock poisoned"))?;
    let mut rows = Vec::new();
    for entry in guard.entries() {
        for (index, output) in entry.tx.vout.iter().enumerate() {
            if let Some(address) = script_to_address.get(&output.script_pubkey) {
                rows.push(DeltaRow {
                    address: address.clone(),
                    txid: entry.txid,
                    index: index as u32,
                    satoshis: output.value,
                    timestamp: entry.time,
                    prevtxid: None,
                    prevout: None,
                });
            }
        }
        for (index, input) in entry.tx.vin.iter().enumerate() {
            let prevout_entry = match chainstate
                .utxo_entry(&input.prevout)
                .map_err(map_internal)?
            {
                Some(entry) => entry,
                None => continue,
            };
            if let Some(address) = script_to_address.get(&prevout_entry.script_pubkey) {
                rows.push(DeltaRow {
                    address: address.clone(),
                    txid: entry.txid,
                    index: index as u32,
                    satoshis: -prevout_entry.value,
                    timestamp: entry.time,
                    prevtxid: Some(input.prevout.hash),
                    prevout: Some(input.prevout.index),
                });
            }
        }
    }

    rows.sort_by(|a, b| {
        a.timestamp
            .cmp(&b.timestamp)
            .then_with(|| a.txid.cmp(&b.txid))
            .then_with(|| a.index.cmp(&b.index))
            .then_with(|| a.address.cmp(&b.address))
    });

    Ok(Value::Array(
        rows.into_iter()
            .map(|row| {
                let mut obj = serde_json::Map::new();
                obj.insert("address".to_string(), Value::String(row.address));
                obj.insert("txid".to_string(), Value::String(hash256_to_hex(&row.txid)));
                obj.insert("index".to_string(), Value::Number(row.index.into()));
                obj.insert("satoshis".to_string(), Value::Number(row.satoshis.into()));
                obj.insert("timestamp".to_string(), Value::Number(row.timestamp.into()));
                if row.satoshis < 0 {
                    if let Some(prevtxid) = row.prevtxid {
                        obj.insert(
                            "prevtxid".to_string(),
                            Value::String(hash256_to_hex(&prevtxid)),
                        );
                    }
                    if let Some(prevout) = row.prevout {
                        obj.insert("prevout".to_string(), Value::Number(prevout.into()));
                    }
                }
                Value::Object(obj)
            })
            .collect(),
    ))
}

fn rpc_getblocktemplate<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
    params: Vec<Value>,
    chain_params: &ChainParams,
    flags: &ValidationFlags,
    default_miner_address: Option<&str>,
) -> Result<Value, RpcError> {
    if params.len() > 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getblocktemplate expects 0 or 1 parameter",
        ));
    }

    let request = match params.get(0) {
        None => None,
        Some(value) => Some(value.as_object().ok_or_else(|| {
            RpcError::new(
                RPC_INVALID_PARAMETER,
                "getblocktemplate param must be an object",
            )
        })?),
    };
    let mode = request
        .and_then(|obj| obj.get("mode"))
        .and_then(|value| value.as_str())
        .unwrap_or("template");

    if mode == "proposal" {
        let Some(obj) = request else {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "getblocktemplate param must be an object",
            ));
        };
        let hex = obj
            .get("data")
            .and_then(|val| val.as_str())
            .ok_or_else(|| RpcError::new(RPC_TYPE_ERROR, "Missing data String key for proposal"))?;
        let bytes = bytes_from_hex(hex)
            .ok_or_else(|| RpcError::new(RPC_DESERIALIZATION_ERROR, "Block decode failed"))?;
        let block = Block::consensus_decode(&bytes)
            .map_err(|_| RpcError::new(RPC_DESERIALIZATION_ERROR, "Block decode failed"))?;
        let hash = block.header.hash();

        if let Some(entry) = chainstate.header_entry(&hash).map_err(map_internal)? {
            if entry.has_block() {
                return Ok(Value::String("duplicate".to_string()));
            }
            return Ok(Value::String("duplicate-inconclusive".to_string()));
        }

        let best = chainstate.best_block().map_err(map_internal)?;
        if let Some(tip) = best {
            if block.header.prev_block != tip.hash {
                return Ok(Value::String("inconclusive-not-best-prevblk".to_string()));
            }

            let height = tip.height + 1;
            return match chainstate.connect_block(
                &block,
                height,
                chain_params,
                flags,
                false,
                None,
                None,
                Some(bytes.as_slice()),
            ) {
                Ok(_) => Ok(Value::Null),
                Err(fluxd_chainstate::state::ChainStateError::Validation(err)) => {
                    Ok(Value::String(err.to_string()))
                }
                Err(fluxd_chainstate::state::ChainStateError::InvalidHeader(msg)) => {
                    Ok(Value::String(msg.to_string()))
                }
                Err(err) => Err(map_internal(err.to_string())),
            };
        } else {
            if block.header.prev_block != [0u8; 32] {
                return Ok(Value::String("inconclusive-not-best-prevblk".to_string()));
            }

            return match chainstate.connect_block(
                &block,
                0,
                chain_params,
                flags,
                false,
                None,
                None,
                Some(bytes.as_slice()),
            ) {
                Ok(_) => Ok(Value::Null),
                Err(fluxd_chainstate::state::ChainStateError::Validation(err)) => {
                    Ok(Value::String(err.to_string()))
                }
                Err(fluxd_chainstate::state::ChainStateError::InvalidHeader(msg)) => {
                    Ok(Value::String(msg.to_string()))
                }
                Err(err) => Err(map_internal(err.to_string())),
            };
        }
    }

    if mode != "template" {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "Invalid mode"));
    }

    let miner_address = request
        .and_then(|obj| {
            obj.get("mineraddress")
                .or_else(|| obj.get("address"))
                .and_then(|val| val.as_str())
        })
        .or(default_miner_address)
        .ok_or_else(|| {
            RpcError::new(
                RPC_METHOD_NOT_FOUND,
                "wallet support not implemented and mineraddress not set",
            )
        })?;

    let miner_script_pubkey = address_to_script_pubkey(miner_address, chain_params.network)
        .map_err(|err| match err {
            AddressError::UnknownPrefix => RpcError::new(
                RPC_INVALID_ADDRESS_OR_KEY,
                "miner address has invalid prefix",
            ),
            _ => RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "invalid miner address"),
        })?;

    let best = chainstate.best_block().map_err(map_internal)?;
    let (prev_hash, height) = match best {
        Some(tip) => (tip.hash, tip.height + 1),
        None => ([0u8; 32], 0),
    };

    let mintime = median_time_past(chainstate, height - 1)?
        .saturating_add(1)
        .max(0);
    let mintime_u32 = u32::try_from(mintime).unwrap_or(0);

    let mut curtime = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(u32::MAX as u64) as u32;
    curtime = curtime.max(mintime_u32);

    let bits = chainstate
        .next_work_required_bits(&prev_hash, height, curtime as i64, &chain_params.consensus)
        .map_err(map_internal)?;

    let pon_active =
        network_upgrade_active(height, &chain_params.consensus.upgrades, UpgradeIndex::Pon);
    let sapling_active = network_upgrade_active(
        height,
        &chain_params.consensus.upgrades,
        UpgradeIndex::Acadia,
    );

    let payouts = chainstate
        .deterministic_fluxnode_payouts(height, chain_params)
        .map_err(map_internal)?;

    let subsidy = block_subsidy(height, &chain_params.consensus);
    let payout_sum = payouts
        .iter()
        .try_fold(0i64, |acc, (_, _, _, amount)| acc.checked_add(*amount))
        .ok_or_else(|| map_internal("fluxnode payout sum out of range"))?;
    let remainder = subsidy
        .checked_sub(payout_sum)
        .ok_or_else(|| map_internal("fluxnode payout remainder out of range"))?;

    let exchange_fund = exchange_fund_amount(height, &chain_params.funding);
    let foundation_fund = foundation_fund_amount(height, &chain_params.funding);
    let swap_pool = swap_pool_amount(height as i64, &chain_params.swap_pool);

    let make_coinbase = |miner_value: i64| -> Result<Transaction, RpcError> {
        let mut outputs = Vec::new();
        outputs.push(TxOut {
            value: miner_value,
            script_pubkey: miner_script_pubkey.clone(),
        });
        for (_, _, script_pubkey, amount) in &payouts {
            outputs.push(TxOut {
                value: *amount,
                script_pubkey: script_pubkey.clone(),
            });
        }

        if pon_active {
            let dev_script = address_to_script_pubkey(
                chain_params.funding.dev_fund_address,
                chain_params.network,
            )
            .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "invalid dev fund address"))?;
            outputs.push(TxOut {
                value: remainder,
                script_pubkey: dev_script,
            });
        }

        if exchange_fund > 0 {
            let exchange_script = address_to_script_pubkey(
                chain_params.funding.exchange_address,
                chain_params.network,
            )
            .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "invalid exchange fund address"))?;
            outputs.push(TxOut {
                value: exchange_fund,
                script_pubkey: exchange_script,
            });
        }
        if foundation_fund > 0 {
            let foundation_script = address_to_script_pubkey(
                chain_params.funding.foundation_address,
                chain_params.network,
            )
            .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "invalid foundation fund address"))?;
            outputs.push(TxOut {
                value: foundation_fund,
                script_pubkey: foundation_script,
            });
        }
        if swap_pool > 0 {
            let swap_script =
                address_to_script_pubkey(chain_params.swap_pool.address, chain_params.network)
                    .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "invalid swap pool address"))?;
            outputs.push(TxOut {
                value: swap_pool,
                script_pubkey: swap_script,
            });
        }

        let mut script_sig = script_push_int(height as i64);
        crate::push_data(&mut script_sig, b"fluxd-rust");

        Ok(Transaction {
            f_overwintered: sapling_active,
            version: if sapling_active { 4 } else { 1 },
            version_group_id: if sapling_active {
                SAPLING_VERSION_GROUP_ID
            } else {
                0
            },
            vin: vec![TxIn {
                prevout: OutPoint::null(),
                script_sig,
                sequence: u32::MAX,
            }],
            vout: outputs,
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: None,
        })
    };

    fn legacy_sigops(script: &[u8]) -> u32 {
        const OP_CHECKSIG: u8 = 0xac;
        const OP_CHECKSIGVERIFY: u8 = 0xad;
        const OP_CHECKMULTISIG: u8 = 0xae;
        const OP_CHECKMULTISIGVERIFY: u8 = 0xaf;
        const OP_PUSHDATA1: u8 = 0x4c;
        const OP_PUSHDATA2: u8 = 0x4d;
        const OP_PUSHDATA4: u8 = 0x4e;

        let mut count = 0u32;
        let mut cursor = 0usize;
        while cursor < script.len() {
            let opcode = script[cursor];
            cursor = cursor.saturating_add(1);
            match opcode {
                OP_CHECKSIG | OP_CHECKSIGVERIFY => count = count.saturating_add(1),
                OP_CHECKMULTISIG | OP_CHECKMULTISIGVERIFY => count = count.saturating_add(20),
                0x01..=0x4b => cursor = cursor.saturating_add(opcode as usize),
                OP_PUSHDATA1 => {
                    if let Some(len) = script.get(cursor).copied() {
                        cursor = cursor.saturating_add(1 + len as usize);
                    } else {
                        break;
                    }
                }
                OP_PUSHDATA2 => {
                    if cursor + 1 >= script.len() {
                        break;
                    }
                    let len = u16::from_le_bytes([script[cursor], script[cursor + 1]]) as usize;
                    cursor = cursor.saturating_add(2 + len);
                }
                OP_PUSHDATA4 => {
                    if cursor + 3 >= script.len() {
                        break;
                    }
                    let len = u32::from_le_bytes([
                        script[cursor],
                        script[cursor + 1],
                        script[cursor + 2],
                        script[cursor + 3],
                    ]) as usize;
                    cursor = cursor.saturating_add(4 + len);
                }
                _ => {}
            }
            if cursor > script.len() {
                break;
            }
        }
        count
    }

    fn tx_sigops(tx: &Transaction) -> u32 {
        let input_ops: u32 = tx
            .vin
            .iter()
            .map(|input| legacy_sigops(&input.script_sig))
            .sum();
        let output_ops: u32 = tx
            .vout
            .iter()
            .map(|output| legacy_sigops(&output.script_pubkey))
            .sum();
        input_ops.saturating_add(output_ops)
    }

    #[derive(Clone)]
    struct TemplateTx {
        fee: i64,
        size: usize,
        parents: Vec<Hash256>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct FeeRateTx {
        txid: Hash256,
        fee: i64,
        size: usize,
    }

    impl Ord for FeeRateTx {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            let fee_a = i128::from(self.fee);
            let fee_b = i128::from(other.fee);
            let size_a = i128::try_from(self.size.max(1)).unwrap_or(i128::MAX);
            let size_b = i128::try_from(other.size.max(1)).unwrap_or(i128::MAX);
            let left = fee_a.saturating_mul(size_b);
            let right = fee_b.saturating_mul(size_a);
            match left.cmp(&right) {
                std::cmp::Ordering::Equal => self.txid.cmp(&other.txid),
                other => other,
            }
        }
    }

    impl PartialOrd for FeeRateTx {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    let coinbase_size = make_coinbase(0)
        .and_then(|tx| tx.consensus_encode().map_err(map_internal))
        .map(|bytes| bytes.len())?;
    let coinbase_overhead_bytes = 1024usize;
    let mut block_bytes_limit = usize::try_from(MAX_BLOCK_SIZE).unwrap_or(0);
    block_bytes_limit = block_bytes_limit
        .saturating_sub(coinbase_size)
        .saturating_sub(coinbase_overhead_bytes);

    let (mempool_revision, selected_fees, transactions_json, sapling_commitments) = {
        let mempool_snapshot = mempool
            .lock()
            .map_err(|_| map_internal("mempool lock poisoned"))?;
        let mempool_revision = mempool_snapshot.revision();

        let mut templates: HashMap<Hash256, TemplateTx> = HashMap::new();
        for entry in mempool_snapshot.entries() {
            templates.insert(
                entry.txid,
                TemplateTx {
                    fee: entry.fee,
                    size: entry.size(),
                    parents: entry.parents.clone(),
                },
            );
        }

        let mut children: HashMap<Hash256, Vec<Hash256>> = HashMap::new();
        let mut remaining_parents: HashMap<Hash256, usize> = HashMap::new();
        for (txid, tx) in &templates {
            let in_mempool_parents = tx
                .parents
                .iter()
                .filter(|parent| templates.contains_key(*parent))
                .count();
            remaining_parents.insert(*txid, in_mempool_parents);
            for parent in &tx.parents {
                if templates.contains_key(parent) {
                    children.entry(*parent).or_default().push(*txid);
                }
            }
        }

        let mut heap: BinaryHeap<FeeRateTx> = BinaryHeap::new();
        for (txid, tx) in &templates {
            let parent_count = remaining_parents
                .get(txid)
                .copied()
                .unwrap_or(tx.parents.len());
            if parent_count == 0 {
                heap.push(FeeRateTx {
                    txid: *txid,
                    fee: tx.fee,
                    size: tx.size,
                });
            }
        }

        let mut selected: Vec<Hash256> = Vec::new();
        let mut selected_set: HashSet<Hash256> = HashSet::new();
        let mut selected_fees: i64 = 0;
        let mut selected_bytes: usize = 0;

        while let Some(candidate) = heap.pop() {
            if selected_set.contains(&candidate.txid) {
                continue;
            }
            let Some(entry) = templates.get(&candidate.txid) else {
                continue;
            };

            if selected_bytes.saturating_add(entry.size) > block_bytes_limit {
                continue;
            }

            selected_fees = selected_fees
                .checked_add(entry.fee)
                .ok_or_else(|| map_internal("mempool fee overflow"))?;
            selected_bytes = selected_bytes.saturating_add(entry.size);
            selected_set.insert(candidate.txid);
            selected.push(candidate.txid);

            if let Some(outgoing) = children.get(&candidate.txid) {
                for child in outgoing {
                    let Some(count) = remaining_parents.get_mut(child) else {
                        continue;
                    };
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        if let Some(child_tx) = templates.get(child) {
                            heap.push(FeeRateTx {
                                txid: *child,
                                fee: child_tx.fee,
                                size: child_tx.size,
                            });
                        }
                    }
                }
            }
        }

        let mut tx_index_by_id: HashMap<Hash256, usize> = HashMap::new();
        for (idx, txid) in selected.iter().copied().enumerate() {
            tx_index_by_id.insert(txid, idx + 1);
        }

        let mut transactions_json = Vec::with_capacity(selected.len());
        let mut sapling_commitments = Vec::new();
        for txid in &selected {
            let Some(entry) = mempool_snapshot.get(txid) else {
                continue;
            };
            let depends = entry
                .parents
                .iter()
                .filter_map(|parent| tx_index_by_id.get(parent).copied())
                .collect::<Vec<_>>();
            for output in &entry.tx.shielded_outputs {
                sapling_commitments.push(output.cm);
            }
            transactions_json.push(json!({
                "data": hex_bytes(&entry.raw),
                "hash": hash256_to_hex(txid),
                "fee": entry.fee,
                "depends": depends,
                "sigops": tx_sigops(&entry.tx),
            }));
        }

        Ok::<_, RpcError>((
            mempool_revision,
            selected_fees,
            transactions_json,
            sapling_commitments,
        ))
    }?;

    let miner_value = if pon_active {
        selected_fees
    } else {
        remainder
            .checked_add(selected_fees)
            .ok_or_else(|| map_internal("coinbase value out of range"))?
    };

    let coinbase = make_coinbase(miner_value)?;

    let coinbase_bytes = coinbase.consensus_encode().map_err(map_internal)?;
    let coinbase_txid = coinbase.txid().map_err(map_internal)?;
    let miner_reward = coinbase.vout.first().map(|out| out.value).unwrap_or(0);
    let coinbase_sigops = tx_sigops(&coinbase);

    let target = compact_to_u256(bits).map_err(|err| map_internal(err.to_string()))?;
    let target_hex = hex_bytes(&target.to_big_endian());

    let final_sapling_root = chainstate
        .sapling_root_after_commitments(&sapling_commitments)
        .map_err(map_internal)?;

    let longpollid = format!("{}{}", hash256_to_hex(&prev_hash), mempool_revision);

    let mut result = serde_json::Map::new();
    result.insert("capabilities".to_string(), json!(["proposal"]));
    result.insert(
        "version".to_string(),
        Value::Number(Number::from(if pon_active {
            PON_VERSION
        } else {
            CURRENT_VERSION
        })),
    );
    result.insert(
        "previousblockhash".to_string(),
        Value::String(hash256_to_hex(&prev_hash)),
    );
    result.insert(
        "finalsaplingroothash".to_string(),
        Value::String(hash256_to_hex(&final_sapling_root)),
    );
    result.insert("transactions".to_string(), Value::Array(transactions_json));
    result.insert(
        "coinbasetxn".to_string(),
        json!({
            "data": hex_bytes(&coinbase_bytes),
            "hash": hash256_to_hex(&coinbase_txid),
            "depends": [],
            "fee": -selected_fees,
            "sigops": coinbase_sigops,
            "required": true,
        }),
    );
    result.insert("longpollid".to_string(), Value::String(longpollid));
    result.insert("target".to_string(), Value::String(target_hex));
    result.insert("mintime".to_string(), Value::Number(Number::from(mintime)));
    result.insert(
        "mutable".to_string(),
        json!(["time", "transactions", "prevblock"]),
    );
    result.insert(
        "noncerange".to_string(),
        Value::String("00000000ffffffff".to_string()),
    );
    result.insert(
        "sigoplimit".to_string(),
        Value::Number(Number::from(MAX_BLOCK_SIGOPS)),
    );
    result.insert(
        "sizelimit".to_string(),
        Value::Number(Number::from(MAX_BLOCK_SIZE)),
    );
    result.insert("curtime".to_string(), Value::Number(Number::from(curtime)));
    result.insert("bits".to_string(), Value::String(format!("{:08x}", bits)));
    result.insert("height".to_string(), Value::Number(Number::from(height)));
    result.insert(
        "miner_reward".to_string(),
        Value::Number(Number::from(miner_reward)),
    );

    for (tier, _outpoint, script_pubkey, amount) in &payouts {
        let (legacy_name, renamed) = match *tier {
            2 => ("super", "nimbus"),
            3 => ("bamf", "stratus"),
            _ => ("basic", "cumulus"),
        };
        let address =
            script_pubkey_to_address(script_pubkey, chain_params.network).unwrap_or_default();
        result.insert(
            format!("{renamed}_fluxnode_address"),
            Value::String(address.clone()),
        );
        result.insert(
            format!("{renamed}_fluxnode_payout"),
            Value::Number(Number::from(*amount)),
        );

        result.insert(
            format!("{legacy_name}_zelnode_address"),
            Value::String(address.clone()),
        );
        result.insert(
            format!("{legacy_name}_zelnode_payout"),
            Value::Number(Number::from(*amount)),
        );
        result.insert(
            format!("{renamed}_zelnode_address"),
            Value::String(address.clone()),
        );
        result.insert(
            format!("{renamed}_zelnode_payout"),
            Value::Number(Number::from(*amount)),
        );
    }

    if exchange_fund > 0 {
        result.insert(
            "flux_creation_address".to_string(),
            Value::String(chain_params.funding.exchange_address.to_string()),
        );
        result.insert(
            "flux_creation_amount".to_string(),
            Value::Number(Number::from(exchange_fund)),
        );
    } else if foundation_fund > 0 {
        result.insert(
            "flux_creation_address".to_string(),
            Value::String(chain_params.funding.foundation_address.to_string()),
        );
        result.insert(
            "flux_creation_amount".to_string(),
            Value::Number(Number::from(foundation_fund)),
        );
    } else if swap_pool > 0 {
        result.insert(
            "flux_creation_address".to_string(),
            Value::String(chain_params.swap_pool.address.to_string()),
        );
        result.insert(
            "flux_creation_amount".to_string(),
            Value::Number(Number::from(swap_pool)),
        );
    }

    Ok(Value::Object(result))
}

fn script_push_int(value: i64) -> Vec<u8> {
    const OP_0: u8 = 0x00;
    const OP_1NEGATE: u8 = 0x4f;
    const OP_1: u8 = 0x51;

    if value == 0 {
        return vec![OP_0];
    }
    if value == -1 {
        return vec![OP_1NEGATE];
    }
    if (1..=16).contains(&value) {
        return vec![OP_1 + (value as u8 - 1)];
    }

    let data = crate::script_num_to_vec(value);
    let mut script = Vec::new();
    crate::push_data(&mut script, &data);
    script
}

fn rpc_getmininginfo<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;

    let best_block = chainstate
        .best_block()
        .map_err(map_internal)?
        .map(|tip| tip.height)
        .unwrap_or(0);

    let best_header = chainstate
        .best_header()
        .map_err(map_internal)?
        .map(|tip| tip.hash);
    let difficulty = match best_header {
        Some(hash) => {
            let entry = chainstate
                .header_entry(&hash)
                .map_err(map_internal)?
                .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "missing header entry"))?;
            difficulty_from_bits(entry.bits, chain_params).unwrap_or(0.0)
        }
        None => 0.0,
    };

    let pooledtx = mempool
        .lock()
        .map_err(|_| map_internal("mempool lock poisoned"))?
        .size();

    Ok(json!({
        "blocks": best_block,
        "currentblocksize": 0,
        "currentblocktx": 0,
        "difficulty": difficulty,
        "errors": "",
        "generate": false,
        "genproclimit": -1,
        "localsolps": 0.0,
        "networksolps": 0.0,
        "networkhashps": 0.0,
        "pooledtx": pooledtx,
        "testnet": chain_params.network != Network::Mainnet,
        "chain": network_name(chain_params.network),
        "ponminter": false,
    }))
}

fn rpc_submitblock<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    write_lock: &Mutex<()>,
    params: Vec<Value>,
    chain_params: &ChainParams,
    flags: &ValidationFlags,
) -> Result<Value, RpcError> {
    if params.is_empty() || params.len() > 2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "submitblock expects 1 or 2 parameters",
        ));
    }
    let hex = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "block hex must be a string"))?;
    let bytes = bytes_from_hex(hex)
        .ok_or_else(|| RpcError::new(RPC_DESERIALIZATION_ERROR, "Block decode failed"))?;
    let block = Block::consensus_decode(&bytes)
        .map_err(|_| RpcError::new(RPC_DESERIALIZATION_ERROR, "Block decode failed"))?;
    let hash = block.header.hash();

    if let Some(entry) = chainstate.header_entry(&hash).map_err(map_internal)? {
        if entry.has_block() {
            return Ok(Value::String("duplicate".to_string()));
        }
    }

    let prev_hash = block.header.prev_block;
    let best = chainstate.best_block().map_err(map_internal)?;
    if let Some(tip) = best.as_ref() {
        if tip.hash == hash {
            return Ok(Value::String("duplicate".to_string()));
        }
        if tip.hash != prev_hash {
            return Ok(Value::String("inconclusive".to_string()));
        }
    } else if !(prev_hash == [0u8; 32] && hash == chain_params.consensus.hash_genesis_block) {
        return Ok(Value::String("inconclusive".to_string()));
    }

    let height = if prev_hash == [0u8; 32] && hash == chain_params.consensus.hash_genesis_block {
        0
    } else {
        best.as_ref()
            .map(|tip| tip.height + 1)
            .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "missing best block"))?
    };

    let batch = match chainstate.connect_block(
        &block,
        height,
        chain_params,
        flags,
        false,
        None,
        None,
        Some(bytes.as_slice()),
    ) {
        Ok(batch) => batch,
        Err(fluxd_chainstate::state::ChainStateError::InvalidHeader(
            "block does not extend best block tip",
        ))
        | Err(fluxd_chainstate::state::ChainStateError::InvalidHeader(
            "block height does not match header index",
        )) => return Ok(Value::String("inconclusive".to_string())),
        Err(fluxd_chainstate::state::ChainStateError::Validation(err)) => {
            return Ok(Value::String(err.to_string()))
        }
        Err(err) => return Err(map_internal(err.to_string())),
    };

    let _guard = write_lock
        .lock()
        .map_err(|_| map_internal("write lock poisoned"))?;
    let current_tip = chainstate.best_block().map_err(map_internal)?;
    if let Some(tip) = current_tip {
        if tip.hash == hash {
            return Ok(Value::String("duplicate".to_string()));
        }
        if tip.hash != prev_hash {
            return Ok(Value::String("inconclusive".to_string()));
        }
    }
    chainstate.commit_batch(batch).map_err(map_internal)?;
    Ok(Value::Null)
}

fn rpc_getfluxnodecount<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let records = chainstate.fluxnode_records().map_err(map_internal)?;
    let total = records.len() as i64;
    Ok(json!({
        "total": total,
        "stable": total,
        "enabled": total,
        "inqueue": 0,
    }))
}

fn rpc_viewdeterministicfluxnodelist<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    if params.len() > 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "viewdeterministicfluxnodelist expects 0 or 1 parameter",
        ));
    }
    let filter = if params.is_empty() {
        String::new()
    } else {
        params[0].as_str().unwrap_or_default().to_ascii_lowercase()
    };
    let records = chainstate.fluxnode_records().map_err(map_internal)?;
    let mut out = Vec::new();
    for record in records {
        let operator_pubkey = chainstate
            .fluxnode_key(record.operator_pubkey)
            .map_err(map_internal)?
            .unwrap_or_default();
        let collateral_pubkey = match record.collateral_pubkey {
            Some(key) => chainstate
                .fluxnode_key(key)
                .map_err(map_internal)?
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let p2sh_script = match record.p2sh_script {
            Some(key) => chainstate
                .fluxnode_key(key)
                .map_err(map_internal)?
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let outpoint_str = format_outpoint(&record.collateral);
        let txhash = hash256_to_hex(&record.collateral.hash);
        let operator_pubkey_b64 = if operator_pubkey.is_empty() {
            String::new()
        } else {
            base64::engine::general_purpose::STANDARD.encode(&operator_pubkey)
        };
        let collateral_pubkey_b64 = if collateral_pubkey.is_empty() {
            String::new()
        } else {
            base64::engine::general_purpose::STANDARD.encode(&collateral_pubkey)
        };
        let p2sh_hex = if p2sh_script.is_empty() {
            String::new()
        } else {
            hex_bytes(&p2sh_script)
        };
        if !filter.is_empty() {
            let haystack = format!(
                "{} {} {} {} {}",
                outpoint_str, txhash, operator_pubkey_b64, collateral_pubkey_b64, p2sh_hex
            )
            .to_ascii_lowercase();
            if !haystack.contains(&filter) {
                continue;
            }
        }
        out.push(json!({
            "collateral": outpoint_str,
            "txhash": txhash,
            "outidx": record.collateral.index,
            "ip": "",
            "network": "",
            "added_height": record.start_height,
            "confirmed_height": record.start_height,
            "last_confirmed_height": record.last_confirmed_height,
            "last_paid_height": record.last_paid_height,
            "tier": fluxnode_tier_name(record.tier),
            "payment_address": "",
            "pubkey": operator_pubkey_b64,
            "collateral_pubkey": collateral_pubkey_b64,
            "redeemscript": p2sh_hex,
            "activesince": 0,
            "lastpaid": 0,
            "rank": 0
        }));
    }
    Ok(Value::Array(out))
}

fn rpc_fluxnodecurrentwinner<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let records = chainstate.fluxnode_records().map_err(map_internal)?;
    let mut result = serde_json::Map::new();
    for (tier_label, tier_id) in [
        ("CUMULUS Winner", 1u8),
        ("NIMBUS Winner", 2u8),
        ("STRATUS Winner", 3u8),
    ] {
        let winner = select_fluxnode_winner(&records, tier_id);
        if let Some(record) = winner {
            result.insert(
                tier_label.to_string(),
                json!({
                    "collateral": format_outpoint(&record.collateral),
                    "added_height": record.start_height,
                    "confirmed_height": record.start_height,
                    "last_confirmed_height": record.last_confirmed_height,
                    "last_paid_height": record.last_paid_height,
                    "tier": fluxnode_tier_name(record.tier),
                }),
            );
        } else {
            result.insert(tier_label.to_string(), Value::Null);
        }
    }
    Ok(Value::Object(result))
}

fn rpc_listfluxnodeconf<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
    data_dir: &Path,
) -> Result<Value, RpcError> {
    if params.len() > 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "listfluxnodeconf expects 0 or 1 parameter",
        ));
    }
    let filter = params
        .get(0)
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let conf_entries = read_fluxnode_conf(data_dir)?;
    if conf_entries.is_empty() {
        return Ok(Value::Array(Vec::new()));
    }

    let best_height = best_block_height(chainstate)?;
    let pon_active = network_upgrade_active(
        best_height,
        &chain_params.consensus.upgrades,
        UpgradeIndex::Pon,
    );
    let expiration = if pon_active {
        FLUXNODE_START_TX_EXPIRATION_HEIGHT_V2
    } else {
        FLUXNODE_START_TX_EXPIRATION_HEIGHT
    };
    let dos_remove = if pon_active {
        FLUXNODE_DOS_REMOVE_AMOUNT_V2
    } else {
        FLUXNODE_DOS_REMOVE_AMOUNT
    };

    let mut records_by_outpoint: HashMap<OutPoint, FluxnodeRecord> = HashMap::new();
    for record in chainstate.fluxnode_records().map_err(map_internal)? {
        records_by_outpoint.insert(record.collateral.clone(), record);
    }

    let mut out = Vec::with_capacity(conf_entries.len());
    for entry in conf_entries {
        let record = records_by_outpoint.get(&entry.collateral);
        let status = match record {
            Some(record) if record.confirmed_height != 0 => "CONFIRMED",
            Some(record) if best_height >= record.start_height as i32 => {
                let age = best_height.saturating_sub(record.start_height as i32);
                if age <= expiration {
                    "STARTED"
                } else if age <= dos_remove {
                    "DOS"
                } else {
                    "OFFLINE"
                }
            }
            Some(_) => "STARTED",
            None => "OFFLINE",
        };

        let (ip, network) = fluxnode_network_info(&entry.address);
        let collateral_str = format_outpoint(&entry.collateral);
        let txhash_hex = hash256_to_hex(&entry.collateral.hash);

        let mut obj = serde_json::Map::new();
        obj.insert("alias".to_string(), Value::String(entry.alias));
        obj.insert("status".to_string(), Value::String(status.to_string()));
        obj.insert(
            "collateral".to_string(),
            Value::String(collateral_str.clone()),
        );
        obj.insert("txHash".to_string(), Value::String(txhash_hex.clone()));
        obj.insert(
            "outputIndex".to_string(),
            Value::Number((entry.collateral.index as i64).into()),
        );
        obj.insert("privateKey".to_string(), Value::String(entry.privkey));
        obj.insert("address".to_string(), Value::String(entry.address));

        obj.insert("ip".to_string(), Value::String(ip));
        obj.insert("network".to_string(), Value::String(network));

        if let Some(record) = record {
            let payment_address =
                fluxnode_payment_address(chainstate, record, chain_params.network)?
                    .unwrap_or_else(|| "UNKNOWN".to_string());
            obj.insert(
                "added_height".to_string(),
                Value::Number((record.start_height as i64).into()),
            );
            obj.insert(
                "confirmed_height".to_string(),
                Value::Number((record.confirmed_height as i64).into()),
            );
            obj.insert(
                "last_confirmed_height".to_string(),
                Value::Number((record.last_confirmed_height as i64).into()),
            );
            obj.insert(
                "last_paid_height".to_string(),
                Value::Number((record.last_paid_height as i64).into()),
            );
            obj.insert(
                "tier".to_string(),
                Value::String(fluxnode_tier_name(record.tier).to_string()),
            );
            obj.insert(
                "payment_address".to_string(),
                Value::String(payment_address),
            );
            let activesince = header_time_at_height(chainstate, record.start_height as i32)
                .unwrap_or_default() as i64;
            let lastpaid =
                if record.last_paid_height == 0 || best_height < record.last_paid_height as i32 {
                    0
                } else {
                    header_time_at_height(chainstate, record.last_paid_height as i32)
                        .unwrap_or_default() as i64
                };
            obj.insert("activesince".to_string(), Value::Number(activesince.into()));
            obj.insert("lastpaid".to_string(), Value::Number(lastpaid.into()));
        } else {
            obj.insert("added_height".to_string(), Value::Number(0.into()));
            obj.insert("confirmed_height".to_string(), Value::Number(0.into()));
            obj.insert("last_confirmed_height".to_string(), Value::Number(0.into()));
            obj.insert("last_paid_height".to_string(), Value::Number(0.into()));
            obj.insert("tier".to_string(), Value::String("UNKNOWN".to_string()));
            obj.insert(
                "payment_address".to_string(),
                Value::String("UNKNOWN".to_string()),
            );
            obj.insert("activesince".to_string(), Value::Number(0.into()));
            obj.insert("lastpaid".to_string(), Value::Number(0.into()));
        }

        if !filter.is_empty() {
            let haystack = format!(
                "{} {} {} {} {}",
                obj.get("alias")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default(),
                obj.get("address")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default(),
                txhash_hex,
                status,
                collateral_str
            )
            .to_ascii_lowercase();
            if !haystack.contains(&filter) {
                continue;
            }
        }

        out.push(Value::Object(obj));
    }

    Ok(Value::Array(out))
}

fn rpc_getfluxnodeoutputs<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
    data_dir: &Path,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;

    let conf_entries = read_fluxnode_conf(data_dir)?;
    if conf_entries.is_empty() {
        return Err(RpcError::new(
            RPC_INTERNAL_ERROR,
            "This is not a Flux Node (no fluxnode.conf entry found)",
        ));
    }

    let best_height = best_block_height(chainstate)?;
    let best_u32 = u32::try_from(best_height).unwrap_or(0);

    let mut out = Vec::new();
    for entry in conf_entries {
        let utxo = chainstate
            .utxo_entry(&entry.collateral)
            .map_err(map_internal)?;
        let Some(utxo) = utxo else {
            continue;
        };
        if utxo.is_coinbase {
            continue;
        }
        if fluxd_consensus::fluxnode_tier_from_collateral(
            best_height,
            utxo.value,
            &chain_params.fluxnode,
        )
        .is_none()
        {
            continue;
        }

        let confirmations = best_u32.saturating_sub(utxo.height).saturating_add(1);
        out.push(json!({
            "txhash": hash256_to_hex(&entry.collateral.hash),
            "outputidx": entry.collateral.index,
            "Flux Amount": amount_to_value(utxo.value),
            "Confirmations": confirmations,
        }));
    }

    Ok(Value::Array(out))
}

fn mempool_contains_fluxnode_outpoint(mempool: &Mempool, outpoint: &OutPoint) -> bool {
    mempool.entries().any(|entry| {
        let Some(fluxnode) = entry.tx.fluxnode.as_ref() else {
            return false;
        };
        match fluxnode {
            FluxnodeTx::V5(FluxnodeTxV5::Start(start)) => &start.collateral == outpoint,
            FluxnodeTx::V6(FluxnodeTxV6::Start(start)) => match &start.variant {
                FluxnodeStartVariantV6::Normal { collateral, .. } => collateral == outpoint,
                FluxnodeStartVariantV6::P2sh { collateral, .. } => collateral == outpoint,
            },
            FluxnodeTx::V5(FluxnodeTxV5::Confirm(confirm))
            | FluxnodeTx::V6(FluxnodeTxV6::Confirm(confirm)) => &confirm.collateral == outpoint,
        }
    })
}

fn fluxnode_start_blocked_reason(
    best_height: i32,
    record: &FluxnodeRecord,
    expiration: i32,
    dos_remove: i32,
) -> Option<&'static str> {
    if record.confirmed_height != 0 {
        return Some("Fluxnode already confirmed and in fluxnode list");
    }
    if best_height < record.start_height as i32 {
        return Some("Fluxnode already started, waiting to be confirmed");
    }
    let age = best_height.saturating_sub(record.start_height as i32);
    if age <= expiration {
        return Some("Fluxnode already started, waiting to be confirmed");
    }
    if age <= dos_remove {
        return Some("Fluxnode already started then not confirmed, in DoS tracker. Must wait until out of DoS tracker to start");
    }
    None
}

fn extract_p2pkh_hash(script_pubkey: &[u8]) -> Option<[u8; 20]> {
    if script_pubkey.len() != 25 {
        return None;
    }
    if script_pubkey[0] != 0x76
        || script_pubkey[1] != 0xa9
        || script_pubkey[2] != 0x14
        || script_pubkey[23] != 0x88
        || script_pubkey[24] != 0xac
    {
        return None;
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&script_pubkey[3..23]);
    Some(out)
}

fn extract_p2sh_hash(script_pubkey: &[u8]) -> Option<[u8; 20]> {
    if script_pubkey.len() != 23 {
        return None;
    }
    if script_pubkey[0] != 0xa9 || script_pubkey[1] != 0x14 || script_pubkey[22] != 0x87 {
        return None;
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&script_pubkey[2..22]);
    Some(out)
}

fn parse_multisig_redeem_script(script: &[u8]) -> Option<Vec<Vec<u8>>> {
    const OP_1: u8 = 0x51;
    const OP_16: u8 = 0x60;
    const OP_CHECKMULTISIG: u8 = 0xae;
    const OP_PUSHDATA1: u8 = 0x4c;
    const OP_PUSHDATA2: u8 = 0x4d;

    if script.len() < 3 {
        return None;
    }
    let mut cursor = 0usize;
    let required_opcode = *script.get(cursor)?;
    cursor += 1;
    if !(OP_1..=OP_16).contains(&required_opcode) {
        return None;
    }
    let required = required_opcode - OP_1 + 1;

    let mut pubkeys: Vec<Vec<u8>> = Vec::new();
    while cursor < script.len() {
        let op = *script.get(cursor)?;
        if (OP_1..=OP_16).contains(&op) {
            break;
        }
        cursor += 1;
        let len = if op <= 75 {
            op as usize
        } else if op == OP_PUSHDATA1 {
            let len = *script.get(cursor)? as usize;
            cursor += 1;
            len
        } else if op == OP_PUSHDATA2 {
            let lo = *script.get(cursor)? as u16;
            let hi = *script.get(cursor + 1)? as u16;
            cursor += 2;
            u16::from_le_bytes([lo as u8, hi as u8]) as usize
        } else {
            return None;
        };
        if cursor + len > script.len() {
            return None;
        }
        let data = &script[cursor..cursor + len];
        cursor += len;
        if matches!(data.len(), 33 | 65) {
            pubkeys.push(data.to_vec());
        }
    }

    let total_opcode = *script.get(cursor)?;
    cursor += 1;
    if !(OP_1..=OP_16).contains(&total_opcode) {
        return None;
    }
    let total = total_opcode - OP_1 + 1;

    if cursor >= script.len() || script[cursor] != OP_CHECKMULTISIG {
        return None;
    }
    cursor += 1;
    if cursor != script.len() {
        return None;
    }
    if total as usize != pubkeys.len() || required > total {
        return None;
    }
    Some(pubkeys)
}

fn build_fluxnode_start_tx<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    entry: &FluxnodeConfEntry,
    chain_params: &ChainParams,
    collateral_wif: &str,
    redeem_script_hex: Option<&str>,
) -> Result<Transaction, RpcError> {
    let best_height = best_block_height(chainstate)?;
    let next_height = best_height.saturating_add(1);

    if !network_upgrade_active(
        next_height,
        &chain_params.consensus.upgrades,
        UpgradeIndex::Kamata,
    ) {
        return Err(RpcError::new(
            RPC_INTERNAL_ERROR,
            "deterministic fluxnodes transactions is not active yet",
        ));
    }

    let p2sh_active = network_upgrade_active(
        next_height,
        &chain_params.consensus.upgrades,
        UpgradeIndex::P2ShNodes,
    );

    let (operator_secret, operator_compressed) =
        parse_wif_secret_key(&entry.privkey, chain_params.network)?;
    let operator_pubkey = secret_key_pubkey_bytes(&operator_secret, operator_compressed);

    let (collateral_secret, _collateral_wif_compressed) =
        parse_wif_secret_key(collateral_wif, chain_params.network)?;

    let utxo = chainstate
        .utxo_entry(&entry.collateral)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "collateral output not found"))?;

    let sig_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .try_into()
        .unwrap_or(u32::MAX);

    match classify_script_pubkey(&utxo.script_pubkey) {
        ScriptType::P2Pkh => {
            let pubkey_hash = extract_p2pkh_hash(&utxo.script_pubkey)
                .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "invalid p2pkh script"))?;

            let compressed_pubkey = secret_key_pubkey_bytes(&collateral_secret, true);
            let uncompressed_pubkey = secret_key_pubkey_bytes(&collateral_secret, false);

            let (collateral_pubkey, collateral_compressed) =
                if hash160(&compressed_pubkey) == pubkey_hash {
                    (compressed_pubkey, true)
                } else if hash160(&uncompressed_pubkey) == pubkey_hash {
                    (uncompressed_pubkey, false)
                } else {
                    return Err(RpcError::new(
                        RPC_INVALID_ADDRESS_OR_KEY,
                        "collateral private key does not match collateral output",
                    ));
                };

            let mut tx = if p2sh_active {
                Transaction {
                    f_overwintered: false,
                    version: FLUXNODE_TX_UPGRADEABLE_VERSION,
                    version_group_id: 0,
                    vin: Vec::new(),
                    vout: Vec::new(),
                    lock_time: 0,
                    expiry_height: 0,
                    value_balance: 0,
                    shielded_spends: Vec::new(),
                    shielded_outputs: Vec::new(),
                    join_splits: Vec::new(),
                    join_split_pub_key: [0u8; 32],
                    join_split_sig: [0u8; 64],
                    binding_sig: [0u8; 64],
                    fluxnode: Some(FluxnodeTx::V6(FluxnodeTxV6::Start(FluxnodeStartV6 {
                        flux_tx_version: FLUXNODE_INTERNAL_NORMAL_TX_VERSION,
                        variant: FluxnodeStartVariantV6::Normal {
                            collateral: entry.collateral.clone(),
                            collateral_pubkey: collateral_pubkey.clone(),
                            pubkey: operator_pubkey,
                            sig_time,
                            sig: Vec::new(),
                        },
                        using_delegates: false,
                        delegates: None,
                    }))),
                }
            } else {
                Transaction {
                    f_overwintered: false,
                    version: FLUXNODE_TX_VERSION,
                    version_group_id: 0,
                    vin: Vec::new(),
                    vout: Vec::new(),
                    lock_time: 0,
                    expiry_height: 0,
                    value_balance: 0,
                    shielded_spends: Vec::new(),
                    shielded_outputs: Vec::new(),
                    join_splits: Vec::new(),
                    join_split_pub_key: [0u8; 32],
                    join_split_sig: [0u8; 64],
                    binding_sig: [0u8; 64],
                    fluxnode: Some(FluxnodeTx::V5(FluxnodeTxV5::Start(FluxnodeStartV5 {
                        collateral: entry.collateral.clone(),
                        collateral_pubkey: collateral_pubkey.clone(),
                        pubkey: operator_pubkey,
                        sig_time,
                        sig: Vec::new(),
                    }))),
                }
            };

            let txid = tx.txid().map_err(map_internal)?;
            let message = hash256_to_hex(&txid).into_bytes();
            let sig_bytes =
                sign_compact_message(&collateral_secret, collateral_compressed, &message)?;

            match tx.fluxnode.as_mut() {
                Some(FluxnodeTx::V5(FluxnodeTxV5::Start(start))) => {
                    start.sig = sig_bytes.to_vec();
                }
                Some(FluxnodeTx::V6(FluxnodeTxV6::Start(start))) => {
                    if let FluxnodeStartVariantV6::Normal { sig: sig_field, .. } =
                        &mut start.variant
                    {
                        *sig_field = sig_bytes.to_vec();
                    }
                }
                _ => {}
            }

            Ok(tx)
        }
        ScriptType::P2Sh => {
            if !p2sh_active {
                return Err(RpcError::new(
                    RPC_INTERNAL_ERROR,
                    "p2sh collateral requires P2SH nodes activation",
                ));
            }

            let redeem_script_hex = redeem_script_hex.ok_or_else(|| {
                RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "missing redeem script for p2sh collateral",
                )
            })?;
            let redeem_script = bytes_from_hex(redeem_script_hex).ok_or_else(|| {
                RpcError::new(RPC_DESERIALIZATION_ERROR, "redeem script decode failed")
            })?;

            let script_hash = extract_p2sh_hash(&utxo.script_pubkey)
                .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "invalid p2sh script"))?;
            if hash160(&redeem_script) != script_hash {
                return Err(RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "redeem script hash mismatch",
                ));
            }

            let pubkeys = parse_multisig_redeem_script(&redeem_script).ok_or_else(|| {
                RpcError::new(RPC_INVALID_PARAMETER, "redeem script not multisig")
            })?;

            let compressed_pubkey = secret_key_pubkey_bytes(&collateral_secret, true);
            let uncompressed_pubkey = secret_key_pubkey_bytes(&collateral_secret, false);

            let collateral_compressed = if pubkeys.iter().any(|pk| pk == &compressed_pubkey) {
                true
            } else if pubkeys.iter().any(|pk| pk == &uncompressed_pubkey) {
                false
            } else {
                return Err(RpcError::new(
                    RPC_INVALID_ADDRESS_OR_KEY,
                    "collateral private key not present in redeem script",
                ));
            };

            let mut tx = Transaction {
                f_overwintered: false,
                version: FLUXNODE_TX_UPGRADEABLE_VERSION,
                version_group_id: 0,
                vin: Vec::new(),
                vout: Vec::new(),
                lock_time: 0,
                expiry_height: 0,
                value_balance: 0,
                shielded_spends: Vec::new(),
                shielded_outputs: Vec::new(),
                join_splits: Vec::new(),
                join_split_pub_key: [0u8; 32],
                join_split_sig: [0u8; 64],
                binding_sig: [0u8; 64],
                fluxnode: Some(FluxnodeTx::V6(FluxnodeTxV6::Start(FluxnodeStartV6 {
                    flux_tx_version: FLUXNODE_INTERNAL_P2SH_TX_VERSION,
                    variant: FluxnodeStartVariantV6::P2sh {
                        collateral: entry.collateral.clone(),
                        pubkey: operator_pubkey,
                        redeem_script,
                        sig_time,
                        sig: Vec::new(),
                    },
                    using_delegates: false,
                    delegates: None,
                }))),
            };

            let txid = tx.txid().map_err(map_internal)?;
            let message = hash256_to_hex(&txid).into_bytes();
            let sig = sign_compact_message(&collateral_secret, collateral_compressed, &message)?;

            if let Some(FluxnodeTx::V6(FluxnodeTxV6::Start(start))) = tx.fluxnode.as_mut() {
                if let FluxnodeStartVariantV6::P2sh { sig: sig_field, .. } = &mut start.variant {
                    *sig_field = sig.to_vec();
                }
            }

            Ok(tx)
        }
        _ => Err(RpcError::new(
            RPC_INVALID_ADDRESS_OR_KEY,
            "collateral output script unsupported",
        )),
    }
}

fn rpc_startdeterministicfluxnode<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
    mempool_policy: &MempoolPolicy,
    mempool_metrics: &MempoolMetrics,
    fee_estimator: &Mutex<FeeEstimator>,
    mempool_flags: &ValidationFlags,
    params: Vec<Value>,
    chain_params: &ChainParams,
    tx_announce: &broadcast::Sender<Hash256>,
    data_dir: &Path,
) -> Result<Value, RpcError> {
    if params.len() < 2 || params.len() > 4 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "startdeterministicfluxnode expects 2 to 4 parameters",
        ));
    }

    let alias = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "alias must be a string"))?
        .to_string();
    let _lockwallet = parse_bool(&params[1])?;
    let collateral_wif_override = params.get(2).and_then(|value| value.as_str());
    let redeem_script_override = params.get(3).and_then(|value| value.as_str());

    let conf_entries = read_fluxnode_conf(data_dir)?;
    let selected = conf_entries.iter().find(|entry| entry.alias == alias);

    let mut detail = serde_json::Map::new();
    detail.insert("alias".to_string(), Value::String(alias.clone()));

    let mut successful = 0;
    let mut failed = 0;

    let Some(entry) = selected else {
        failed = 1;
        detail.insert("result".to_string(), Value::String("failed".to_string()));
        detail.insert(
            "error".to_string(),
            Value::String(
                "could not find alias in config. Verify with listfluxnodeconf.".to_string(),
            ),
        );
        return Ok(json!({
            "overall": format!("Successfully started {successful} fluxnodes, failed to start {failed}, total {}", successful + failed),
            "detail": [Value::Object(detail)],
        }));
    };

    let best_height = best_block_height(chainstate)?;
    let pon_active = network_upgrade_active(
        best_height,
        &chain_params.consensus.upgrades,
        UpgradeIndex::Pon,
    );
    let expiration = if pon_active {
        FLUXNODE_START_TX_EXPIRATION_HEIGHT_V2
    } else {
        FLUXNODE_START_TX_EXPIRATION_HEIGHT
    };
    let dos_remove = if pon_active {
        FLUXNODE_DOS_REMOVE_AMOUNT_V2
    } else {
        FLUXNODE_DOS_REMOVE_AMOUNT
    };

    let record = chainstate
        .fluxnode_records()
        .map_err(map_internal)?
        .into_iter()
        .find(|record| record.collateral == entry.collateral);
    if let Some(record) = record {
        if let Some(reason) =
            fluxnode_start_blocked_reason(best_height, &record, expiration, dos_remove)
        {
            failed = 1;
            detail.insert("result".to_string(), Value::String("failed".to_string()));
            detail.insert("reason".to_string(), Value::String(reason.to_string()));
            return Ok(json!({
                "overall": format!("Successfully started {successful} fluxnodes, failed to start {failed}, total {}", successful + failed),
                "detail": [Value::Object(detail)],
            }));
        }
    }

    {
        let guard = mempool
            .lock()
            .map_err(|_| map_internal("mempool lock poisoned"))?;
        if mempool_contains_fluxnode_outpoint(&guard, &entry.collateral) {
            failed = 1;
            detail.insert("result".to_string(), Value::String("failed".to_string()));
            detail.insert(
                "reason".to_string(),
                Value::String(
                    "Mempool already has a fluxnode transaction using this outpoint".to_string(),
                ),
            );
            return Ok(json!({
                "overall": format!("Successfully started {successful} fluxnodes, failed to start {failed}, total {}", successful + failed),
                "detail": [Value::Object(detail)],
            }));
        }
    }

    let collateral_wif = collateral_wif_override
        .or(entry.collateral_privkey.as_deref())
        .ok_or_else(|| {
            RpcError::new(
                RPC_INVALID_PARAMETER,
                "missing collateral private key (provide as 3rd parameter or in fluxnode.conf)",
            )
        })?;
    let redeem_script_hex = redeem_script_override.or(entry.redeem_script.as_deref());

    let tx = match build_fluxnode_start_tx(
        chainstate,
        entry,
        chain_params,
        collateral_wif,
        redeem_script_hex,
    ) {
        Ok(tx) => tx,
        Err(err) => {
            failed = 1;
            detail.insert("result".to_string(), Value::String("failed".to_string()));
            detail.insert("errorMessage".to_string(), Value::String(err.message));
            return Ok(json!({
                "overall": format!("Successfully started {successful} fluxnodes, failed to start {failed}, total {}", successful + failed),
                "detail": [Value::Object(detail)],
            }));
        }
    };

    let raw = tx.consensus_encode().map_err(map_internal)?;
    let raw_hex = hex_bytes(&raw);

    match rpc_sendrawtransaction(
        chainstate,
        mempool,
        mempool_policy,
        mempool_metrics,
        fee_estimator,
        mempool_flags,
        vec![Value::String(raw_hex)],
        chain_params,
        tx_announce,
    ) {
        Ok(txid) => {
            successful = 1;
            detail.insert(
                "result".to_string(),
                Value::String("successful".to_string()),
            );
            detail.insert("txid".to_string(), txid);
        }
        Err(err) => {
            failed = 1;
            detail.insert("result".to_string(), Value::String("failed".to_string()));
            detail.insert("errorMessage".to_string(), Value::String(err.message));
        }
    }

    Ok(json!({
        "overall": format!("Successfully started {successful} fluxnodes, failed to start {failed}, total {}", successful + failed),
        "detail": [Value::Object(detail)],
    }))
}

fn rpc_startfluxnode<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
    mempool_policy: &MempoolPolicy,
    mempool_metrics: &MempoolMetrics,
    fee_estimator: &Mutex<FeeEstimator>,
    mempool_flags: &ValidationFlags,
    params: Vec<Value>,
    chain_params: &ChainParams,
    tx_announce: &broadcast::Sender<Hash256>,
    data_dir: &Path,
) -> Result<Value, RpcError> {
    if params.len() < 2 || params.len() > 3 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "startfluxnode expects 2 or 3 parameters",
        ));
    }
    let set = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "set must be a string"))?;
    let _lockwallet = parse_bool(&params[1])?;

    let conf_entries = read_fluxnode_conf(data_dir)?;
    if conf_entries.is_empty() {
        return Err(RpcError::new(
            RPC_INTERNAL_ERROR,
            "This is not a Flux Node (no fluxnode.conf entry found)",
        ));
    }

    let targets: Vec<&FluxnodeConfEntry> = match set {
        "all" => conf_entries.iter().collect(),
        "alias" => {
            let alias = params
                .get(2)
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    RpcError::new(RPC_INVALID_PARAMETER, "alias required for set=alias")
                })?;
            let filtered: Vec<_> = conf_entries.iter().filter(|e| e.alias == alias).collect();
            if filtered.is_empty() {
                return Ok(json!({
                    "overall": "Successfully started 0 fluxnodes, failed to start 1, total 1",
                    "detail": [{
                        "alias": alias,
                        "result": "failed",
                        "error": "could not find alias in config. Verify with listfluxnodeconf.",
                    }],
                }));
            }
            filtered
        }
        _ => {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "set must be \"all\" or \"alias\"",
            ))
        }
    };

    let mut successful = 0;
    let mut failed = 0;
    let mut detail = Vec::new();

    for entry in targets {
        let mut obj = serde_json::Map::new();
        obj.insert("alias".to_string(), Value::String(entry.alias.clone()));
        obj.insert(
            "outpoint".to_string(),
            Value::String(format_outpoint(&entry.collateral)),
        );

        let collateral_wif = match entry.collateral_privkey.as_deref() {
            Some(wif) => wif,
            None => {
                failed += 1;
                obj.insert("result".to_string(), Value::String("failed".to_string()));
                obj.insert(
                    "errorMessage".to_string(),
                    Value::String(
                        "missing collateral private key in fluxnode.conf (wallet not implemented)"
                            .to_string(),
                    ),
                );
                detail.push(Value::Object(obj));
                continue;
            }
        };
        let redeem_script_hex = entry.redeem_script.as_deref();

        let tx = match build_fluxnode_start_tx(
            chainstate,
            entry,
            chain_params,
            collateral_wif,
            redeem_script_hex,
        ) {
            Ok(tx) => tx,
            Err(err) => {
                failed += 1;
                obj.insert("result".to_string(), Value::String("failed".to_string()));
                obj.insert("errorMessage".to_string(), Value::String(err.message));
                detail.push(Value::Object(obj));
                continue;
            }
        };

        let raw = match tx.consensus_encode() {
            Ok(bytes) => bytes,
            Err(err) => {
                failed += 1;
                obj.insert("result".to_string(), Value::String("failed".to_string()));
                obj.insert("errorMessage".to_string(), Value::String(err.to_string()));
                detail.push(Value::Object(obj));
                continue;
            }
        };

        let raw_hex = hex_bytes(&raw);
        match rpc_sendrawtransaction(
            chainstate,
            mempool,
            mempool_policy,
            mempool_metrics,
            fee_estimator,
            mempool_flags,
            vec![Value::String(raw_hex)],
            chain_params,
            tx_announce,
        ) {
            Ok(txid) => {
                successful += 1;
                obj.insert(
                    "result".to_string(),
                    Value::String("successful".to_string()),
                );
                obj.insert("txid".to_string(), txid);
            }
            Err(err) => {
                failed += 1;
                obj.insert("result".to_string(), Value::String("failed".to_string()));
                obj.insert("errorMessage".to_string(), Value::String(err.message));
            }
        }

        detail.push(Value::Object(obj));
    }

    Ok(json!({
        "overall": format!("Successfully started {successful} fluxnodes, failed to start {failed}, total {}", successful + failed),
        "detail": detail,
    }))
}

fn rpc_getfluxnodestatus<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
    data_dir: &Path,
) -> Result<Value, RpcError> {
    if params.len() > 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getfluxnodestatus expects 0 or 1 parameter",
        ));
    }

    let conf_entries = read_fluxnode_conf(data_dir)?;
    let (collateral, selected_conf): (OutPoint, Option<&FluxnodeConfEntry>) = match params.first() {
        Some(value) => {
            let arg = value
                .as_str()
                .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "argument must be a string"))?;
            if arg.contains(':') {
                let outpoint = parse_outpoint(arg)?;
                let selected = conf_entries
                    .iter()
                    .find(|entry| entry.collateral == outpoint);
                (outpoint, selected)
            } else {
                let entry = conf_entries
                    .iter()
                    .find(|entry| entry.alias == arg)
                    .ok_or_else(|| {
                        RpcError::new(RPC_INVALID_PARAMETER, "unknown fluxnode alias")
                    })?;
                (entry.collateral.clone(), Some(entry))
            }
        }
        None => {
            if conf_entries.is_empty() {
                return Err(RpcError::new(
                    RPC_INTERNAL_ERROR,
                    "This is not a Flux Node (no fluxnode.conf entry found)",
                ));
            }
            if conf_entries.len() > 1 {
                return Err(RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "Multiple entries in fluxnode.conf; pass an alias or collateral outpoint",
                ));
            }
            (conf_entries[0].collateral.clone(), Some(&conf_entries[0]))
        }
    };

    let record = chainstate
        .fluxnode_records()
        .map_err(map_internal)?
        .into_iter()
        .find(|record| record.collateral == collateral);

    let outpoint_str = format_outpoint(&collateral);
    if record.is_none() {
        return Ok(json!({
            "status": "expired",
            "collateral": outpoint_str,
        }));
    }
    let record = record.expect("checked");

    let best_height = best_block_height(chainstate)?;
    let pon_active = network_upgrade_active(
        best_height,
        &chain_params.consensus.upgrades,
        UpgradeIndex::Pon,
    );
    let expiration = if pon_active {
        FLUXNODE_START_TX_EXPIRATION_HEIGHT_V2
    } else {
        FLUXNODE_START_TX_EXPIRATION_HEIGHT
    };
    let dos_remove = if pon_active {
        FLUXNODE_DOS_REMOVE_AMOUNT_V2
    } else {
        FLUXNODE_DOS_REMOVE_AMOUNT
    };

    let status = if record.confirmed_height != 0 {
        "CONFIRMED"
    } else if best_height >= record.start_height as i32 {
        let age = best_height.saturating_sub(record.start_height as i32);
        if age <= expiration {
            "STARTED"
        } else if age <= dos_remove {
            "DOS"
        } else {
            "OFFLINE"
        }
    } else {
        "STARTED"
    };

    let payment_address =
        fluxnode_payment_address(chainstate, &record, chain_params.network)?.unwrap_or_default();
    let pubkey = chainstate
        .fluxnode_key(record.operator_pubkey)
        .map_err(map_internal)?
        .unwrap_or_default();

    let activesince =
        header_time_at_height(chainstate, record.start_height as i32).unwrap_or_default() as i64;
    let lastpaid = if record.last_paid_height == 0 || best_height < record.last_paid_height as i32 {
        0
    } else {
        header_time_at_height(chainstate, record.last_paid_height as i32).unwrap_or_default() as i64
    };

    let mut info = serde_json::Map::new();
    info.insert("status".to_string(), Value::String(status.to_string()));
    info.insert("collateral".to_string(), Value::String(outpoint_str));
    info.insert(
        "txhash".to_string(),
        Value::String(hash256_to_hex(&record.collateral.hash)),
    );
    info.insert(
        "outidx".to_string(),
        Value::Number((record.collateral.index as i64).into()),
    );
    if let Some(entry) = selected_conf {
        let (ip, network) = fluxnode_network_info(&entry.address);
        info.insert("ip".to_string(), Value::String(ip));
        info.insert("network".to_string(), Value::String(network));
    } else {
        info.insert("ip".to_string(), Value::String(String::new()));
        info.insert("network".to_string(), Value::String(String::new()));
    }
    info.insert(
        "added_height".to_string(),
        Value::Number((record.start_height as i64).into()),
    );
    info.insert(
        "confirmed_height".to_string(),
        Value::Number((record.confirmed_height as i64).into()),
    );
    info.insert(
        "last_confirmed_height".to_string(),
        Value::Number((record.last_confirmed_height as i64).into()),
    );
    info.insert(
        "last_paid_height".to_string(),
        Value::Number((record.last_paid_height as i64).into()),
    );
    info.insert(
        "tier".to_string(),
        Value::String(fluxnode_tier_name(record.tier).to_string()),
    );
    info.insert(
        "payment_address".to_string(),
        Value::String(payment_address),
    );
    info.insert("pubkey".to_string(), Value::String(hex_bytes(&pubkey)));
    info.insert("activesince".to_string(), Value::Number(activesince.into()));
    info.insert("lastpaid".to_string(), Value::Number(lastpaid.into()));

    if record.collateral_value > 0 {
        info.insert(
            "amount".to_string(),
            amount_to_value(record.collateral_value),
        );
    }

    Ok(Value::Object(info))
}

fn rpc_getdoslist<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let best_height = best_block_height(chainstate)?;
    let pon_active = network_upgrade_active(
        best_height,
        &chain_params.consensus.upgrades,
        UpgradeIndex::Pon,
    );
    let expiration = if pon_active {
        FLUXNODE_START_TX_EXPIRATION_HEIGHT_V2
    } else {
        FLUXNODE_START_TX_EXPIRATION_HEIGHT
    };
    let dos_remove = if pon_active {
        FLUXNODE_DOS_REMOVE_AMOUNT_V2
    } else {
        FLUXNODE_DOS_REMOVE_AMOUNT
    };

    let mut entries = Vec::new();
    for record in chainstate.fluxnode_records().map_err(map_internal)? {
        if record.confirmed_height != 0 {
            continue;
        }
        let Ok(start_height) = i32::try_from(record.start_height) else {
            continue;
        };
        if best_height < start_height {
            continue;
        }
        let age = best_height - start_height;
        if age <= expiration || age > dos_remove {
            continue;
        }
        let eligible_in = dos_remove - age;
        let payment_address = fluxnode_payment_address(chainstate, &record, chain_params.network)?
            .unwrap_or_default();
        let mut obj = serde_json::Map::new();
        obj.insert(
            "collateral".to_string(),
            Value::String(format_outpoint(&record.collateral)),
        );
        obj.insert(
            "added_height".to_string(),
            Value::Number((record.start_height as i64).into()),
        );
        obj.insert(
            "payment_address".to_string(),
            Value::String(payment_address),
        );
        obj.insert(
            "eligible_in".to_string(),
            Value::Number((eligible_in as i64).into()),
        );
        if record.collateral_value > 0 {
            obj.insert(
                "amount".to_string(),
                amount_to_value(record.collateral_value),
            );
        }
        entries.push(Value::Object(obj));
    }
    entries.sort_by(|a, b| {
        let left = a
            .get("eligible_in")
            .and_then(|value| value.as_i64())
            .unwrap_or(i64::MAX);
        let right = b
            .get("eligible_in")
            .and_then(|value| value.as_i64())
            .unwrap_or(i64::MAX);
        left.cmp(&right)
    });
    Ok(Value::Array(entries))
}

fn rpc_getstartlist<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
    chain_params: &ChainParams,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let best_height = best_block_height(chainstate)?;
    let pon_active = network_upgrade_active(
        best_height,
        &chain_params.consensus.upgrades,
        UpgradeIndex::Pon,
    );
    let expiration = if pon_active {
        FLUXNODE_START_TX_EXPIRATION_HEIGHT_V2
    } else {
        FLUXNODE_START_TX_EXPIRATION_HEIGHT
    };

    let mut entries = Vec::new();
    for record in chainstate.fluxnode_records().map_err(map_internal)? {
        if record.confirmed_height != 0 {
            continue;
        }
        let Ok(start_height) = i32::try_from(record.start_height) else {
            continue;
        };
        if best_height < start_height {
            continue;
        }
        let age = best_height - start_height;
        if age > expiration {
            continue;
        }
        let expires_in = expiration - age;
        let payment_address = fluxnode_payment_address(chainstate, &record, chain_params.network)?
            .unwrap_or_default();
        let mut obj = serde_json::Map::new();
        obj.insert(
            "collateral".to_string(),
            Value::String(format_outpoint(&record.collateral)),
        );
        obj.insert(
            "added_height".to_string(),
            Value::Number((record.start_height as i64).into()),
        );
        obj.insert(
            "payment_address".to_string(),
            Value::String(payment_address),
        );
        obj.insert(
            "expires_in".to_string(),
            Value::Number((expires_in as i64).into()),
        );
        if record.collateral_value > 0 {
            obj.insert(
                "amount".to_string(),
                amount_to_value(record.collateral_value),
            );
        }
        entries.push(Value::Object(obj));
    }
    entries.sort_by(|a, b| {
        let left = a
            .get("expires_in")
            .and_then(|value| value.as_i64())
            .unwrap_or(i64::MAX);
        let right = b
            .get("expires_in")
            .and_then(|value| value.as_i64())
            .unwrap_or(i64::MAX);
        left.cmp(&right)
    });
    Ok(Value::Array(entries))
}

fn rpc_getblockhashes<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    params: Vec<Value>,
) -> Result<Value, RpcError> {
    if params.len() < 2 || params.len() > 3 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getblockhashes expects 2 or 3 parameters",
        ));
    }
    let high = parse_u32(&params[0], "high")?;
    let low = parse_u32(&params[1], "low")?;
    let mut no_orphans = false;
    let mut logical_times = false;
    if params.len() > 2 {
        let options = params[2]
            .as_object()
            .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "options must be an object"))?;
        if let Some(value) = options.get("noOrphans") {
            no_orphans = parse_bool(value)?;
        }
        if let Some(value) = options.get("logicalTimes") {
            logical_times = parse_bool(value)?;
        }
    }

    let mut entries = chainstate.scan_timestamp_index().map_err(map_internal)?;
    entries.retain(|(timestamp, _)| *timestamp >= low && *timestamp < high);
    entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut out = Vec::new();
    for (timestamp, hash) in entries {
        if no_orphans {
            let entry = match chainstate.header_entry(&hash).map_err(map_internal)? {
                Some(entry) => entry,
                None => continue,
            };
            let main_hash = chainstate.height_hash(entry.height).map_err(map_internal)?;
            if main_hash != Some(hash) {
                continue;
            }
        }
        if logical_times {
            out.push(json!({
                "blockhash": hash256_to_hex(&hash),
                "logicalts": timestamp,
            }));
        } else {
            out.push(Value::String(hash256_to_hex(&hash)));
        }
    }
    Ok(Value::Array(out))
}

fn rpc_getnetworkhashps(params: Vec<Value>) -> Result<Value, RpcError> {
    if params.len() > 2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getnetworkhashps expects 0, 1, or 2 parameters",
        ));
    }
    Ok(json!(0.0))
}

fn rpc_getnetworksolps(params: Vec<Value>) -> Result<Value, RpcError> {
    if params.len() > 2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "getnetworksolps expects 0, 1, or 2 parameters",
        ));
    }
    Ok(json!(0.0))
}

fn rpc_getlocalsolps(params: Vec<Value>) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    Ok(json!(0.0))
}

fn rpc_estimatefee(
    params: Vec<Value>,
    fee_estimator: &Mutex<FeeEstimator>,
) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "estimatefee expects 1 parameter",
        ));
    }
    let mut blocks = parse_u32(&params[0], "nblocks")?;
    if blocks < 1 {
        blocks = 1;
    }
    let estimate = fee_estimator
        .lock()
        .map_err(|_| map_internal("fee estimator lock poisoned"))?
        .estimate_fee_per_kb(blocks);
    match estimate {
        Some(amount) => Ok(amount_to_value(amount)),
        None => Ok(Number::from_f64(-1.0)
            .map(Value::Number)
            .unwrap_or(Value::Number((-1).into()))),
    }
}

fn rpc_getnetworkinfo(
    params: Vec<Value>,
    peer_registry: &PeerRegistry,
    net_totals: &NetTotals,
    mempool_policy: &MempoolPolicy,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let local_services: u64 = 1;
    let local_services_hex = format!("{:016x}", local_services);
    let snapshot = net_totals.snapshot();
    let connections = snapshot.connections.max(peer_registry.count());
    let networks = json!([
        {"name": "ipv4", "limited": false, "reachable": true, "proxy": ""},
        {"name": "ipv6", "limited": false, "reachable": true, "proxy": ""},
        {"name": "onion", "limited": true, "reachable": false, "proxy": ""}
    ]);
    Ok(json!({
        "version": node_version(),
        "subversion": format!("/fluxd-rust:{}/", env!("CARGO_PKG_VERSION")),
        "protocolversion": PROTOCOL_VERSION,
        "localservices": local_services_hex,
        "localservicesnames": service_flag_names(local_services),
        "timeoffset": 0,
        "connections": connections,
        "networks": networks,
        "relayfee": amount_to_value(mempool_policy.min_relay_fee_per_kb),
        "localaddresses": [],
        "warnings": ""
    }))
}

fn rpc_getconnectioncount(
    params: Vec<Value>,
    peer_registry: &PeerRegistry,
    net_totals: &NetTotals,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let snapshot = net_totals.snapshot();
    let count = snapshot.connections.max(peer_registry.count());
    Ok(Value::Number((count as i64).into()))
}

fn rpc_getnettotals(params: Vec<Value>, net_totals: &NetTotals) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let snapshot = net_totals.snapshot();
    let timemillis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0);
    Ok(json!({
        "totalbytesrecv": snapshot.bytes_recv,
        "totalbytessent": snapshot.bytes_sent,
        "timemillis": timemillis,
    }))
}

fn rpc_getpeerinfo(params: Vec<Value>, peer_registry: &PeerRegistry) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let peers = peer_registry.snapshot();
    let mut out = Vec::with_capacity(peers.len());
    for peer in peers {
        let services_hex = format!("{:016x}", peer.services);
        let services_names = service_flag_names(peer.services);
        out.push(json!({
            "addr": peer.addr.to_string(),
            "subver": peer.user_agent,
            "version": peer.version,
            "services": services_hex,
            "servicesnames": services_names,
            "startingheight": peer.start_height,
            "conntime": system_time_to_unix(peer.connected_since),
            "lastsend": system_time_to_unix(peer.last_send),
            "lastrecv": system_time_to_unix(peer.last_recv),
            "bytessent": peer.bytes_sent,
            "bytesrecv": peer.bytes_recv,
            "inbound": false,
            "kind": peer_kind_name(peer.kind),
        }));
    }
    Ok(Value::Array(out))
}

fn service_flag_names(services: u64) -> Vec<&'static str> {
    const NODE_NETWORK: u64 = 1;
    const NODE_GETUTXO: u64 = 1 << 1;
    const NODE_BLOOM: u64 = 1 << 2;
    const NODE_WITNESS: u64 = 1 << 3;
    const NODE_COMPACT_FILTERS: u64 = 1 << 6;
    const NODE_NETWORK_LIMITED: u64 = 1 << 10;
    const NODE_P2P_V2: u64 = 1 << 11;

    let mut out = Vec::new();
    if services & NODE_NETWORK != 0 {
        out.push("NETWORK");
    }
    if services & NODE_GETUTXO != 0 {
        out.push("GETUTXO");
    }
    if services & NODE_BLOOM != 0 {
        out.push("BLOOM");
    }
    if services & NODE_WITNESS != 0 {
        out.push("WITNESS");
    }
    if services & NODE_COMPACT_FILTERS != 0 {
        out.push("COMPACT_FILTERS");
    }
    if services & NODE_NETWORK_LIMITED != 0 {
        out.push("NETWORK_LIMITED");
    }
    if services & NODE_P2P_V2 != 0 {
        out.push("P2P_V2");
    }
    out
}

fn rpc_listbanned(
    params: Vec<Value>,
    header_peer_book: &HeaderPeerBook,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    let banned = header_peer_book.banned_peers();
    let mut out = Vec::with_capacity(banned.len());
    for entry in banned {
        out.push(json!({
            "address": entry.addr.to_string(),
            "banned_until": system_time_to_unix(entry.banned_until),
        }));
    }
    Ok(Value::Array(out))
}

fn parse_socket_addr_with_default(value: &str, default_port: u16) -> Result<SocketAddr, RpcError> {
    if let Ok(addr) = value.parse::<SocketAddr>() {
        return Ok(addr);
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, default_port));
    }
    Err(RpcError::new(
        RPC_INVALID_PARAMETER,
        "invalid address (expected ip or ip:port)",
    ))
}

fn rpc_clearbanned(
    params: Vec<Value>,
    header_peer_book: &HeaderPeerBook,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    header_peer_book.clear_banned();
    Ok(Value::Null)
}

fn rpc_setban(
    params: Vec<Value>,
    chain_params: &ChainParams,
    peer_registry: &PeerRegistry,
    header_peer_book: &HeaderPeerBook,
) -> Result<Value, RpcError> {
    if params.len() < 2 || params.len() > 4 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "setban expects 2 to 4 parameters",
        ));
    }
    let addr_raw = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "address must be a string"))?;
    let addr = parse_socket_addr_with_default(addr_raw, chain_params.default_port)?;
    let command = params[1]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "command must be a string"))?;

    match command {
        "add" => {
            const DEFAULT_BAN_SECS: u64 = 24 * 60 * 60;

            let mut bantime: u64 = DEFAULT_BAN_SECS;
            let mut absolute = false;
            if params.len() >= 3 && !params[2].is_null() {
                let value = params[2].as_i64().ok_or_else(|| {
                    RpcError::new(RPC_INVALID_PARAMETER, "bantime must be an integer")
                })?;
                bantime = value.max(0) as u64;
            }
            if params.len() == 4 && !params[3].is_null() {
                absolute = params[3].as_bool().ok_or_else(|| {
                    RpcError::new(RPC_INVALID_PARAMETER, "absolute must be a boolean")
                })?;
            }
            if bantime == 0 {
                bantime = DEFAULT_BAN_SECS;
            }
            if absolute {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if bantime <= now {
                    header_peer_book.unban(addr);
                    return Ok(Value::Null);
                }
                bantime = bantime.saturating_sub(now);
            }

            header_peer_book.ban_for(addr, bantime);
            peer_registry.request_disconnect(addr);
        }
        "remove" => {
            header_peer_book.unban(addr);
        }
        _ => {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "command must be 'add' or 'remove'",
            ))
        }
    }

    Ok(Value::Null)
}

fn rpc_disconnectnode(
    params: Vec<Value>,
    chain_params: &ChainParams,
    peer_registry: &PeerRegistry,
) -> Result<Value, RpcError> {
    if params.len() != 1 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "disconnectnode expects 1 parameter",
        ));
    }
    let addr_raw = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "address must be a string"))?;
    let addr = parse_socket_addr_with_default(addr_raw, chain_params.default_port)?;
    peer_registry.request_disconnect(addr);
    Ok(Value::Null)
}

fn rpc_addnode(
    params: Vec<Value>,
    chain_params: &ChainParams,
    addr_book: &AddrBook,
    added_nodes: &Mutex<HashSet<SocketAddr>>,
) -> Result<Value, RpcError> {
    if params.len() != 2 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "addnode expects 2 parameters",
        ));
    }
    let addr_raw = params[0]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "node must be a string"))?;
    let addr = parse_socket_addr_with_default(addr_raw, chain_params.default_port)?;
    let command = params[1]
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "command must be a string"))?;

    match command {
        "add" => {
            let Ok(mut guard) = added_nodes.lock() else {
                return Err(RpcError::new(
                    RPC_INTERNAL_ERROR,
                    "added nodes lock poisoned",
                ));
            };
            guard.insert(addr);
            let _ = addr_book.insert_many(vec![addr]);
        }
        "remove" => {
            if let Ok(mut guard) = added_nodes.lock() {
                guard.remove(&addr);
            }
        }
        "onetry" => {
            addr_book.record_attempt(addr);
            let _ = addr_book.insert_many(vec![addr]);
        }
        _ => {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "command must be 'add', 'remove', or 'onetry'",
            ))
        }
    }

    Ok(Value::Null)
}

fn rpc_getaddednodeinfo(
    params: Vec<Value>,
    chain_params: &ChainParams,
    peer_registry: &PeerRegistry,
    added_nodes: &Mutex<HashSet<SocketAddr>>,
) -> Result<Value, RpcError> {
    let (node_filter, _dns) = match params.len() {
        0 => (None, false),
        1 => {
            if let Some(value) = params[0].as_bool() {
                (None, value)
            } else if let Some(value) = params[0].as_str() {
                (Some(value.to_string()), false)
            } else {
                return Err(RpcError::new(
                    RPC_INVALID_PARAMETER,
                    "getaddednodeinfo expects a boolean or node string",
                ));
            }
        }
        2 => {
            let dns = params[0]
                .as_bool()
                .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "dns must be a boolean"))?;
            let node = params[1]
                .as_str()
                .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "node must be a string"))?;
            (Some(node.to_string()), dns)
        }
        _ => {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "getaddednodeinfo expects 0 to 2 parameters",
            ))
        }
    };

    let mut nodes = {
        let Ok(guard) = added_nodes.lock() else {
            return Err(RpcError::new(
                RPC_INTERNAL_ERROR,
                "added nodes lock poisoned",
            ));
        };
        guard.iter().copied().collect::<Vec<_>>()
    };
    nodes.sort_by_key(|addr| addr.to_string());

    if let Some(filter) = node_filter.as_deref() {
        let addr = parse_socket_addr_with_default(filter, chain_params.default_port)?;
        if !nodes.contains(&addr) {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "node is not in added node list",
            ));
        }
        nodes = vec![addr];
    }

    let connected: HashSet<SocketAddr> = peer_registry
        .snapshot()
        .into_iter()
        .map(|entry| entry.addr)
        .collect();

    let mut out = Vec::with_capacity(nodes.len());
    for addr in nodes {
        let is_connected = connected.contains(&addr);
        out.push(json!({
            "addednode": addr.to_string(),
            "connected": is_connected,
            "addresses": [{
                "address": addr.to_string(),
                "connected": is_connected,
            }]
        }));
    }
    Ok(Value::Array(out))
}

fn branch_len<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    tip_hash: &Hash256,
    tip_height: i32,
) -> Result<i32, RpcError> {
    let mut height = tip_height;
    let mut hash = *tip_hash;
    loop {
        if let Some(main_hash) = chainstate.height_hash(height).map_err(map_internal)? {
            if main_hash == hash {
                return Ok(tip_height - height);
            }
        }
        if height == 0 {
            break;
        }
        let entry = chainstate
            .header_entry(&hash)
            .map_err(map_internal)?
            .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "missing header entry"))?;
        hash = entry.prev_hash;
        height -= 1;
    }
    Ok(tip_height)
}

fn tx_to_json(tx: &Transaction, network: Network) -> Result<Value, RpcError> {
    let txid = tx.txid().map_err(map_internal)?;
    let encoded = tx.consensus_encode().map_err(map_internal)?;
    let mut entry = json!({
        "txid": hash256_to_hex(&txid),
        "version": tx.version,
        "size": encoded.len(),
        "overwintered": tx.f_overwintered,
        "locktime": tx.lock_time,
    });

    if tx.f_overwintered {
        entry["versiongroupid"] = Value::String(format!("{:08x}", tx.version_group_id));
        entry["expiryheight"] = Value::Number(tx.expiry_height.into());
    }

    let mut vin = Vec::with_capacity(tx.vin.len());
    for input in &tx.vin {
        let mut map = serde_json::Map::new();
        if input.prevout.hash == [0u8; 32] && input.prevout.index == u32::MAX {
            map.insert(
                "coinbase".to_string(),
                Value::String(hex_bytes(&input.script_sig)),
            );
        } else {
            map.insert(
                "txid".to_string(),
                Value::String(hash256_to_hex(&input.prevout.hash)),
            );
            map.insert(
                "vout".to_string(),
                Value::Number(input.prevout.index.into()),
            );
            map.insert(
                "scriptSig".to_string(),
                json!({
                    "asm": script_to_asm(&input.script_sig),
                    "hex": hex_bytes(&input.script_sig),
                }),
            );
        }
        map.insert("sequence".to_string(), Value::Number(input.sequence.into()));
        vin.push(Value::Object(map));
    }
    entry["vin"] = Value::Array(vin);

    let mut vout = Vec::with_capacity(tx.vout.len());
    for (index, output) in tx.vout.iter().enumerate() {
        let script = script_pubkey_json(&output.script_pubkey, network);
        let out = json!({
            "value": amount_to_value(output.value),
            "n": index,
            "scriptPubKey": script,
        });
        vout.push(out);
    }
    entry["vout"] = Value::Array(vout);
    Ok(entry)
}

fn script_type_name(script: &[u8]) -> &'static str {
    match classify_script_pubkey(script) {
        ScriptType::P2Pk => "pubkey",
        ScriptType::P2Pkh => "pubkeyhash",
        ScriptType::P2Sh => "scripthash",
        ScriptType::P2Wpkh => "witness_v0_keyhash",
        ScriptType::P2Wsh => "witness_v0_scripthash",
        ScriptType::Unknown => "nonstandard",
    }
}

fn script_pubkey_json(script: &[u8], network: Network) -> Value {
    let script_type = script_type_name(script);
    let mut map = serde_json::Map::new();
    map.insert("asm".to_string(), Value::String(script_to_asm(script)));
    map.insert("hex".to_string(), Value::String(hex_bytes(script)));
    map.insert("type".to_string(), Value::String(script_type.to_string()));
    if let Some(req_sigs) = script_req_sigs(script_type) {
        map.insert("reqSigs".to_string(), Value::Number(req_sigs.into()));
    }
    if let Some(address) = script_pubkey_to_address(script, network) {
        map.insert(
            "addresses".to_string(),
            Value::Array(vec![Value::String(address)]),
        );
    }
    Value::Object(map)
}

fn script_p2sh_address(script: &[u8], network: Network) -> String {
    let hash = hash160(script);
    let mut script_pubkey = Vec::with_capacity(23);
    script_pubkey.push(0xa9);
    script_pubkey.push(0x14);
    script_pubkey.extend_from_slice(&hash);
    script_pubkey.push(0x87);
    script_pubkey_to_address(&script_pubkey, network).unwrap_or_default()
}

fn multisig_small_int_opcode(value: usize) -> Result<u8, RpcError> {
    match value {
        1..=16 => Ok(0x50u8 + value as u8),
        _ => Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "multisig param out of range",
        )),
    }
}

fn script_req_sigs(script_type: &str) -> Option<i64> {
    match script_type {
        "pubkeyhash" | "scripthash" | "witness_v0_keyhash" => Some(1),
        _ => None,
    }
}

fn script_to_asm(script: &[u8]) -> String {
    let mut parts = Vec::new();
    let mut idx = 0usize;
    while idx < script.len() {
        let opcode = script[idx];
        idx += 1;
        match opcode {
            0x00 => parts.push("0".to_string()),
            0x01..=0x4b => {
                let len = opcode as usize;
                if idx + len > script.len() {
                    parts.push("OP_INVALID_PUSH".to_string());
                    break;
                }
                parts.push(hex_bytes(&script[idx..idx + len]));
                idx += len;
            }
            0x4c => {
                if idx >= script.len() {
                    parts.push("OP_PUSHDATA1".to_string());
                    break;
                }
                let len = script[idx] as usize;
                idx += 1;
                if idx + len > script.len() {
                    parts.push("OP_INVALID_PUSHDATA1".to_string());
                    break;
                }
                parts.push(hex_bytes(&script[idx..idx + len]));
                idx += len;
            }
            0x4d => {
                if idx + 1 >= script.len() {
                    parts.push("OP_PUSHDATA2".to_string());
                    break;
                }
                let len = u16::from_le_bytes([script[idx], script[idx + 1]]) as usize;
                idx += 2;
                if idx + len > script.len() {
                    parts.push("OP_INVALID_PUSHDATA2".to_string());
                    break;
                }
                parts.push(hex_bytes(&script[idx..idx + len]));
                idx += len;
            }
            0x4e => {
                if idx + 3 >= script.len() {
                    parts.push("OP_PUSHDATA4".to_string());
                    break;
                }
                let len = u32::from_le_bytes([
                    script[idx],
                    script[idx + 1],
                    script[idx + 2],
                    script[idx + 3],
                ]) as usize;
                idx += 4;
                if idx + len > script.len() {
                    parts.push("OP_INVALID_PUSHDATA4".to_string());
                    break;
                }
                parts.push(hex_bytes(&script[idx..idx + len]));
                idx += len;
            }
            0x51..=0x60 => parts.push((opcode - 0x50).to_string()),
            opcode => parts.push(opcode_name(opcode)),
        }
    }
    parts.join(" ")
}

fn opcode_name(opcode: u8) -> String {
    match opcode {
        0x61 => "OP_NOP".to_string(),
        0x63 => "OP_IF".to_string(),
        0x64 => "OP_NOTIF".to_string(),
        0x67 => "OP_ELSE".to_string(),
        0x68 => "OP_ENDIF".to_string(),
        0x69 => "OP_VERIFY".to_string(),
        0x6a => "OP_RETURN".to_string(),
        0x76 => "OP_DUP".to_string(),
        0xa9 => "OP_HASH160".to_string(),
        0x87 => "OP_EQUAL".to_string(),
        0x88 => "OP_EQUALVERIFY".to_string(),
        0xac => "OP_CHECKSIG".to_string(),
        0xae => "OP_CHECKMULTISIG".to_string(),
        _ => format!("0x{opcode:02x}"),
    }
}

fn fluxnode_tier_name(tier: u8) -> &'static str {
    match tier {
        1 => "CUMULUS",
        2 => "NIMBUS",
        3 => "STRATUS",
        _ => "UNKNOWN",
    }
}

fn select_fluxnode_winner(records: &[FluxnodeRecord], tier: u8) -> Option<&FluxnodeRecord> {
    let mut candidates: Vec<&FluxnodeRecord> = records
        .iter()
        .filter(|record| {
            if tier == 1 && record.tier == 0 {
                return true;
            }
            record.tier == tier
        })
        .collect();
    candidates.sort_by(|a, b| {
        a.last_paid_height
            .cmp(&b.last_paid_height)
            .then_with(|| a.start_height.cmp(&b.start_height))
            .then_with(|| a.collateral.hash.cmp(&b.collateral.hash))
            .then_with(|| a.collateral.index.cmp(&b.collateral.index))
    });
    candidates.into_iter().next()
}

fn amount_to_value(amount: i64) -> Value {
    let value = amount as f64 / COIN as f64;
    Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Number(0.into()))
}

fn parse_amount(value: &Value) -> Result<i64, RpcError> {
    let text = match value {
        Value::Number(num) => num.to_string(),
        Value::String(text) => text.clone(),
        _ => {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "amount must be a number or string",
            ))
        }
    };
    let text = text.trim();
    if text.is_empty() {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "amount is empty"));
    }

    let negative = text.starts_with('-');
    let text = text.strip_prefix('-').unwrap_or(text);

    let (whole, fractional) = match text.split_once('.') {
        Some((whole, fractional)) => (whole, fractional),
        None => (text, ""),
    };
    if whole.is_empty() && fractional.is_empty() {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "invalid amount"));
    }
    if !whole.is_empty() && !whole.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "invalid amount"));
    }
    if !fractional.is_empty() && !fractional.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "invalid amount"));
    }
    if fractional.len() > 8 {
        return Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "amount has too many decimal places",
        ));
    }

    let whole_value = if whole.is_empty() {
        0i64
    } else {
        whole
            .parse::<i64>()
            .map_err(|_| RpcError::new(RPC_INVALID_PARAMETER, "invalid amount"))?
    };
    let fractional_value = if fractional.is_empty() {
        0i64
    } else {
        let parsed = fractional
            .parse::<i64>()
            .map_err(|_| RpcError::new(RPC_INVALID_PARAMETER, "invalid amount"))?;
        let scale = 10i64.pow(8u32.saturating_sub(fractional.len() as u32));
        parsed
            .checked_mul(scale)
            .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "invalid amount"))?
    };

    let amount = whole_value
        .checked_mul(COIN)
        .and_then(|value| value.checked_add(fractional_value))
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "invalid amount"))?;
    let amount = if negative {
        amount
            .checked_neg()
            .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "invalid amount"))?
    } else {
        amount
    };
    if amount < 0 {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "amount out of range"));
    }
    if !fluxd_consensus::money::money_range(amount) {
        return Err(RpcError::new(RPC_INVALID_PARAMETER, "amount out of range"));
    }
    Ok(amount)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mempool::MempoolEntry;
    use fluxd_chainstate::flatfiles::FlatFileStore;
    use fluxd_chainstate::validation::ValidationFlags;
    use fluxd_consensus::params::{chain_params, Network};
    use fluxd_fluxnode::storage::dedupe_key;
    use fluxd_primitives::block::BlockHeader;
    use fluxd_storage::memory::MemoryStore;
    use fluxd_storage::{Column, WriteBatch};
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_data_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
    }

    fn is_hex_64(value: &str) -> bool {
        if value.len() != 64 {
            return false;
        }
        value
            .as_bytes()
            .iter()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F'))
    }

    fn setup_regtest_chainstate() -> (
        ChainState<MemoryStore>,
        fluxd_consensus::params::ChainParams,
        PathBuf,
    ) {
        let data_dir = temp_data_dir("fluxd-rpc-test");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        let blocks_dir = data_dir.join("blocks");
        let blocks = FlatFileStore::new(&blocks_dir, 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(&blocks_dir, "undo", 10_000_000).expect("flatfiles");
        let store = Arc::new(MemoryStore::new());
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let params = chain_params(Network::Regtest);
        let flags = ValidationFlags::default();
        let write_lock = Mutex::new(());
        crate::ensure_genesis(&chainstate, &params, &flags, None, &write_lock)
            .expect("insert genesis");

        (chainstate, params, data_dir)
    }

    fn extend_regtest_chain_to_height(
        chainstate: &ChainState<MemoryStore>,
        params: &fluxd_consensus::params::ChainParams,
        target_height: i32,
    ) {
        let flags = ValidationFlags::default();
        loop {
            let tip = chainstate
                .best_block()
                .expect("best block")
                .expect("best block present");
            if tip.height >= target_height {
                break;
            }
            let tip_entry = chainstate
                .header_entry(&tip.hash)
                .expect("header entry")
                .expect("header entry present");
            let height = tip.height + 1;
            let time = tip_entry.time.saturating_add(1);
            let bits = chainstate
                .next_work_required_bits(&tip.hash, height, time as i64, &params.consensus)
                .expect("next bits");

            let miner_value = block_subsidy(height, &params.consensus);
            let exchange_amount = exchange_fund_amount(height, &params.funding);
            let foundation_amount = foundation_fund_amount(height, &params.funding);
            let swap_amount = swap_pool_amount(height as i64, &params.swap_pool);

            let mut vout = Vec::new();
            vout.push(TxOut {
                value: miner_value,
                script_pubkey: Vec::new(),
            });
            if exchange_amount > 0 {
                let script =
                    address_to_script_pubkey(params.funding.exchange_address, params.network)
                        .expect("exchange address script");
                vout.push(TxOut {
                    value: exchange_amount,
                    script_pubkey: script,
                });
            }
            if foundation_amount > 0 {
                let script =
                    address_to_script_pubkey(params.funding.foundation_address, params.network)
                        .expect("foundation address script");
                vout.push(TxOut {
                    value: foundation_amount,
                    script_pubkey: script,
                });
            }
            if swap_amount > 0 {
                let script = address_to_script_pubkey(params.swap_pool.address, params.network)
                    .expect("swap pool address script");
                vout.push(TxOut {
                    value: swap_amount,
                    script_pubkey: script,
                });
            }
            let coinbase = Transaction {
                f_overwintered: false,
                version: 1,
                version_group_id: 0,
                vin: vec![TxIn {
                    prevout: OutPoint::null(),
                    script_sig: Vec::new(),
                    sequence: u32::MAX,
                }],
                vout,
                lock_time: 0,
                expiry_height: 0,
                value_balance: 0,
                shielded_spends: Vec::new(),
                shielded_outputs: Vec::new(),
                join_splits: Vec::new(),
                join_split_pub_key: [0u8; 32],
                join_split_sig: [0u8; 64],
                binding_sig: [0u8; 64],
                fluxnode: None,
            };

            let coinbase_txid = coinbase.txid().expect("coinbase txid");
            let header = BlockHeader {
                version: CURRENT_VERSION,
                prev_block: tip.hash,
                merkle_root: coinbase_txid,
                final_sapling_root: [0u8; 32],
                time,
                bits,
                nonce: [0u8; 32],
                solution: Vec::new(),
                nodes_collateral: OutPoint::null(),
                block_sig: Vec::new(),
            };

            let mut header_batch = WriteBatch::new();
            chainstate
                .insert_headers_batch_with_pow(
                    &[header.clone()],
                    &params.consensus,
                    &mut header_batch,
                    false,
                )
                .expect("insert header");
            chainstate
                .commit_batch(header_batch)
                .expect("commit header");

            let block = Block {
                header,
                transactions: vec![coinbase],
            };
            let block_bytes = block.consensus_encode().expect("encode block");
            let batch = chainstate
                .connect_block(
                    &block,
                    height,
                    params,
                    &flags,
                    true,
                    None,
                    None,
                    Some(block_bytes.as_slice()),
                )
                .expect("connect block");
            chainstate.commit_batch(batch).expect("commit block");
        }
    }

    fn p2pkh_script(pubkey_hash: [u8; 20]) -> Vec<u8> {
        let mut script = Vec::with_capacity(25);
        script.extend_from_slice(&[0x76, 0xa9, 0x14]);
        script.extend_from_slice(&pubkey_hash);
        script.extend_from_slice(&[0x88, 0xac]);
        script
    }

    fn setup_regtest_chain_with_p2pkh_utxo() -> (
        ChainState<MemoryStore>,
        fluxd_consensus::params::ChainParams,
        PathBuf,
        String,
        Hash256,
        u32,
    ) {
        let (chainstate, params, data_dir) = setup_regtest_chainstate();

        let tip = chainstate
            .best_block()
            .expect("best block")
            .expect("best block present");
        let tip_entry = chainstate
            .header_entry(&tip.hash)
            .expect("header entry")
            .expect("header entry present");
        let height = tip.height + 1;
        let time = tip_entry.time.saturating_add(1);
        let bits = chainstate
            .next_work_required_bits(&tip.hash, height, time as i64, &params.consensus)
            .expect("next bits");

        let pubkey_hash = [0x11u8; 20];
        let script_pubkey = p2pkh_script(pubkey_hash);
        let address =
            script_pubkey_to_address(&script_pubkey, params.network).expect("p2pkh address");

        let miner_value = block_subsidy(height, &params.consensus);
        let coinbase = Transaction {
            f_overwintered: false,
            version: 1,
            version_group_id: 0,
            vin: vec![TxIn {
                prevout: OutPoint::null(),
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![TxOut {
                value: miner_value,
                script_pubkey: script_pubkey.clone(),
            }],
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: None,
        };

        let coinbase_txid = coinbase.txid().expect("coinbase txid");
        let header = BlockHeader {
            version: CURRENT_VERSION,
            prev_block: tip.hash,
            merkle_root: coinbase_txid,
            final_sapling_root: [0u8; 32],
            time,
            bits,
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint::null(),
            block_sig: Vec::new(),
        };

        let mut header_batch = WriteBatch::new();
        chainstate
            .insert_headers_batch_with_pow(
                &[header.clone()],
                &params.consensus,
                &mut header_batch,
                false,
            )
            .expect("insert header");
        chainstate
            .commit_batch(header_batch)
            .expect("commit header");

        let block = Block {
            header,
            transactions: vec![coinbase],
        };
        let block_bytes = block.consensus_encode().expect("encode block");
        let flags = ValidationFlags::default();
        let batch = chainstate
            .connect_block(
                &block,
                height,
                &params,
                &flags,
                true,
                None,
                None,
                Some(block_bytes.as_slice()),
            )
            .expect("connect block");
        chainstate.commit_batch(batch).expect("commit block");

        (chainstate, params, data_dir, address, coinbase_txid, 0)
    }

    fn add_fluxnode_record_to_batch(
        batch: &mut WriteBatch,
        outpoint: OutPoint,
        tier: u8,
        start_height: u32,
        operator_pubkey: Vec<u8>,
        collateral_pubkey: Vec<u8>,
        collateral_value: i64,
    ) -> FluxnodeRecord {
        let operator_pubkey_key = dedupe_key(&operator_pubkey);
        let collateral_pubkey_key = dedupe_key(&collateral_pubkey);

        batch.put(Column::FluxnodeKey, &operator_pubkey_key.0, operator_pubkey);
        batch.put(
            Column::FluxnodeKey,
            &collateral_pubkey_key.0,
            collateral_pubkey,
        );

        let record = FluxnodeRecord {
            collateral: outpoint.clone(),
            tier,
            start_height,
            confirmed_height: 0,
            last_confirmed_height: start_height,
            last_paid_height: 0,
            collateral_value,
            operator_pubkey: operator_pubkey_key,
            collateral_pubkey: Some(collateral_pubkey_key),
            p2sh_script: None,
        };
        let key = fluxd_chainstate::utxo::outpoint_key_bytes(&outpoint);
        batch.put(Column::Fluxnode, key.as_bytes(), record.encode());
        record
    }

    #[test]
    fn parse_amount_accepts_basic_inputs() {
        assert_eq!(parse_amount(&json!(0)).unwrap(), 0);
        assert_eq!(parse_amount(&json!(1)).unwrap(), COIN);
        assert_eq!(parse_amount(&json!("1")).unwrap(), COIN);
        assert_eq!(parse_amount(&json!("1.00000001")).unwrap(), COIN + 1);
        assert_eq!(parse_amount(&json!("0.1")).unwrap(), COIN / 10);
        assert_eq!(parse_amount(&json!(".1")).unwrap(), COIN / 10);
        assert_eq!(parse_amount(&json!("1.")).unwrap(), COIN);
    }

    #[test]
    fn parse_amount_rejects_invalid_inputs() {
        assert!(parse_amount(&json!(-1)).is_err());
        assert!(parse_amount(&json!("-1")).is_err());
        assert!(parse_amount(&json!("1.000000001")).is_err());
        assert!(parse_amount(&json!("foo")).is_err());
        assert!(parse_amount(&json!("1e-8")).is_err());
        assert!(parse_amount(&json!("")).is_err());
    }

    #[test]
    fn getblockchaininfo_has_cpp_schema_keys() {
        let (chainstate, params, data_dir) = setup_regtest_chainstate();
        let value =
            rpc_getblockchaininfo(&chainstate, Vec::new(), &params, &data_dir).expect("rpc");
        let obj = value.as_object().expect("object");

        let chain = obj.get("chain").and_then(Value::as_str).unwrap_or("");
        assert_eq!(chain, "regtest");

        for key in [
            "blocks",
            "headers",
            "bestblockhash",
            "difficulty",
            "verificationprogress",
            "chainwork",
            "pruned",
            "size_on_disk",
            "commitments",
            "softforks",
            "valuePools",
            "upgrades",
            "consensus",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }

        assert!(obj.get("bestblockhash").and_then(Value::as_str).is_some());
        let best_block = obj.get("bestblockhash").and_then(Value::as_str).unwrap();
        assert!(is_hex_64(best_block));
        assert!(obj.get("softforks").and_then(Value::as_array).is_some());
        assert!(obj.get("commitments").and_then(Value::as_u64).is_some());
    }

    #[test]
    fn gettxoutsetinfo_has_cpp_schema_keys() {
        let (chainstate, _params, data_dir) = setup_regtest_chainstate();
        let value = rpc_gettxoutsetinfo(&chainstate, Vec::new(), &data_dir).expect("rpc");
        let obj = value.as_object().expect("object");

        for key in [
            "height",
            "bestblock",
            "transactions",
            "txouts",
            "bytes_serialized",
            "hash_serialized",
            "total_amount",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }

        assert!(obj.get("bestblock").and_then(Value::as_str).is_some());
        let best_block = obj.get("bestblock").and_then(Value::as_str).unwrap();
        assert!(is_hex_64(best_block));

        let hash_serialized = obj.get("hash_serialized").and_then(Value::as_str).unwrap();
        assert!(is_hex_64(hash_serialized));
    }

    #[test]
    fn txoutproof_roundtrip_returns_txids() {
        let (chainstate, _params, _data_dir, _address, txid, _vout) =
            setup_regtest_chain_with_p2pkh_utxo();

        let proof = rpc_gettxoutproof(
            &chainstate,
            vec![Value::Array(vec![Value::String(hash256_to_hex(&txid))])],
        )
        .expect("rpc");
        let proof_hex = proof.as_str().expect("hex string").to_string();
        assert!(bytes_from_hex(&proof_hex).is_some(), "proof should be hex");

        let matches =
            rpc_verifytxoutproof(&chainstate, vec![Value::String(proof_hex)]).expect("rpc");
        let txids = matches.as_array().expect("array");
        assert_eq!(txids.len(), 1);
        assert_eq!(txids[0].as_str(), Some(hash256_to_hex(&txid).as_str()));
    }

    #[test]
    fn getinfo_has_cpp_schema_keys() {
        let (chainstate, params, data_dir) = setup_regtest_chainstate();
        let net_totals = NetTotals::default();
        let peer_registry = PeerRegistry::default();
        let mempool_policy = MempoolPolicy::standard(0, false);

        let value = rpc_getinfo(
            &chainstate,
            Vec::new(),
            &params,
            &data_dir,
            &net_totals,
            &peer_registry,
            &mempool_policy,
        )
        .expect("rpc");
        let obj = value.as_object().expect("object");

        for key in [
            "version",
            "protocolversion",
            "blocks",
            "timeoffset",
            "connections",
            "proxy",
            "difficulty",
            "testnet",
            "relayfee",
            "errors",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }

        assert!(obj.get("version").and_then(Value::as_i64).is_some());
        assert!(obj.get("protocolversion").and_then(Value::as_i64).is_some());
        assert!(obj.get("blocks").and_then(Value::as_i64).is_some());
        assert!(obj.get("connections").and_then(Value::as_i64).is_some());
    }

    #[test]
    fn getnetworkinfo_has_cpp_schema_keys() {
        let net_totals = NetTotals::default();
        let peer_registry = PeerRegistry::default();
        let mempool_policy = MempoolPolicy::standard(0, false);

        let value = rpc_getnetworkinfo(Vec::new(), &peer_registry, &net_totals, &mempool_policy)
            .expect("rpc");
        let obj = value.as_object().expect("object");

        for key in [
            "version",
            "subversion",
            "protocolversion",
            "localservices",
            "timeoffset",
            "connections",
            "networks",
            "relayfee",
            "localaddresses",
            "warnings",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }

        let networks = obj
            .get("networks")
            .and_then(Value::as_array)
            .expect("networks array");
        assert!(!networks.is_empty(), "networks should not be empty");
        let network = networks[0].as_object().expect("network object");
        for key in ["name", "limited", "reachable", "proxy"] {
            assert!(network.contains_key(key), "missing networks key {key}");
        }
    }

    #[test]
    fn getnettotals_has_cpp_schema_keys() {
        let net_totals = NetTotals::default();
        let value = rpc_getnettotals(Vec::new(), &net_totals).expect("rpc");
        let obj = value.as_object().expect("object");
        for key in ["totalbytesrecv", "totalbytessent", "timemillis"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn getpeerinfo_has_cpp_schema_keys() {
        let peer_registry = PeerRegistry::default();
        let addr: std::net::SocketAddr = "127.0.0.1:12345".parse().expect("addr");
        peer_registry.register(addr, PeerKind::Header);
        peer_registry.update_version(addr, 170020, 0x5, "/MagicBean:9.0.6/".to_string(), 123);

        let value = rpc_getpeerinfo(Vec::new(), &peer_registry).expect("rpc");
        let peers = value.as_array().expect("array");
        assert_eq!(peers.len(), 1);
        let obj = peers[0].as_object().expect("object");
        for key in [
            "addr",
            "subver",
            "version",
            "services",
            "servicesnames",
            "startingheight",
            "conntime",
            "lastsend",
            "lastrecv",
            "bytessent",
            "bytesrecv",
            "inbound",
            "kind",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn getconnectioncount_has_cpp_schema() {
        let net_totals = NetTotals::default();
        let peer_registry = PeerRegistry::default();
        let addr: std::net::SocketAddr = "127.0.0.1:12345".parse().expect("addr");
        peer_registry.register(addr, PeerKind::Header);
        let value = rpc_getconnectioncount(Vec::new(), &peer_registry, &net_totals).expect("rpc");
        assert_eq!(value.as_i64(), Some(1));
    }

    #[test]
    fn getdeprecationinfo_has_cpp_schema_keys() {
        let value = rpc_getdeprecationinfo(Vec::new()).expect("rpc");
        let obj = value.as_object().expect("object");
        for key in ["deprecated", "version", "subversion", "warnings"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn listbanned_has_cpp_schema_keys() {
        let book = HeaderPeerBook::default();
        let addr: std::net::SocketAddr = "127.0.0.1:12345".parse().expect("addr");
        book.ban_for(addr, 60);

        let value = rpc_listbanned(Vec::new(), &book).expect("rpc");
        let entries = value.as_array().expect("array");
        assert_eq!(entries.len(), 1);
        let obj = entries[0].as_object().expect("object");
        for key in ["address", "banned_until"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn addnode_and_getaddednodeinfo_have_cpp_schema_keys() {
        let (_chainstate, params, _data_dir) = setup_regtest_chainstate();

        let addr_book = AddrBook::default();
        let added_nodes = Mutex::new(HashSet::new());

        rpc_addnode(
            vec![
                Value::String("127.0.0.1:16125".to_string()),
                Value::String("add".to_string()),
            ],
            &params,
            &addr_book,
            &added_nodes,
        )
        .expect("addnode");

        let peer_registry = PeerRegistry::default();
        let addr: std::net::SocketAddr = "127.0.0.1:16125".parse().expect("addr");
        peer_registry.register(addr, PeerKind::Header);

        let value = rpc_getaddednodeinfo(Vec::new(), &params, &peer_registry, &added_nodes)
            .expect("getaddednodeinfo");
        let list = value.as_array().expect("array");
        assert_eq!(list.len(), 1);
        let obj = list[0].as_object().expect("object");
        for key in ["addednode", "connected", "addresses"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        assert_eq!(obj.get("connected").and_then(Value::as_bool), Some(true));
    }

    #[test]
    fn disconnectnode_has_cpp_schema() {
        let (_chainstate, params, _data_dir) = setup_regtest_chainstate();
        let peer_registry = PeerRegistry::default();
        let value = rpc_disconnectnode(
            vec![Value::String("127.0.0.1".to_string())],
            &params,
            &peer_registry,
        )
        .expect("disconnectnode");
        assert!(value.is_null());
    }

    #[test]
    fn setban_and_clearbanned_have_cpp_schema() {
        let (_chainstate, params, _data_dir) = setup_regtest_chainstate();
        let peer_registry = PeerRegistry::default();
        let book = HeaderPeerBook::default();

        rpc_setban(
            vec![
                Value::String("127.0.0.1".to_string()),
                Value::String("add".to_string()),
                json!(60),
            ],
            &params,
            &peer_registry,
            &book,
        )
        .expect("setban");

        let banned = rpc_listbanned(Vec::new(), &book).expect("listbanned");
        let banned = banned.as_array().expect("array");
        assert_eq!(banned.len(), 1);

        rpc_clearbanned(Vec::new(), &book).expect("clearbanned");
        let banned = rpc_listbanned(Vec::new(), &book).expect("listbanned");
        let banned = banned.as_array().expect("array");
        assert!(banned.is_empty());
    }

    #[test]
    fn getblockcount_has_cpp_schema() {
        let (chainstate, _params, _data_dir) = setup_regtest_chainstate();
        let value = rpc_getblockcount(&chainstate, Vec::new()).expect("rpc");
        assert!(value.as_i64().is_some());
    }

    #[test]
    fn getbestblockhash_has_cpp_schema() {
        let (chainstate, _params, _data_dir) = setup_regtest_chainstate();
        let value = rpc_getbestblockhash(&chainstate, Vec::new()).expect("rpc");
        let hash = value.as_str().expect("hash string");
        assert!(is_hex_64(hash));
    }

    #[test]
    fn getblockhash_has_cpp_schema() {
        let (chainstate, _params, _data_dir) = setup_regtest_chainstate();
        let best = rpc_getbestblockhash(&chainstate, Vec::new())
            .expect("rpc")
            .as_str()
            .expect("hash")
            .to_string();
        let value = rpc_getblockhash(&chainstate, vec![json!(0)]).expect("rpc");
        assert_eq!(value.as_str().unwrap(), best);
    }

    #[test]
    fn getdifficulty_has_cpp_schema() {
        let (chainstate, params, _data_dir) = setup_regtest_chainstate();
        let value = rpc_getdifficulty(&chainstate, Vec::new(), &params).expect("rpc");
        assert!(value.as_f64().is_some());
    }

    #[test]
    fn getchaintips_has_cpp_schema_keys() {
        let (chainstate, _params, _data_dir) = setup_regtest_chainstate();
        let value = rpc_getchaintips(&chainstate, Vec::new()).expect("rpc");
        let tips = value.as_array().expect("array");
        assert!(!tips.is_empty());
        let tip = tips[0].as_object().expect("object");
        for key in ["height", "hash", "branchlen", "status"] {
            assert!(tip.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn getblockheader_has_cpp_schema_keys() {
        let (chainstate, params, _data_dir) = setup_regtest_chainstate();
        let best = chainstate
            .best_block()
            .expect("best block")
            .expect("best block present");
        let hash_hex = hash256_to_hex(&best.hash);
        let value = rpc_getblockheader(&chainstate, vec![Value::String(hash_hex.clone())], &params)
            .expect("rpc");
        let obj = value.as_object().expect("object");
        for key in [
            "hash",
            "confirmations",
            "height",
            "version",
            "merkleroot",
            "finalsaplingroot",
            "time",
            "bits",
            "difficulty",
            "chainwork",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        assert_eq!(obj.get("hash").and_then(Value::as_str).unwrap(), hash_hex);
    }

    #[test]
    fn getblock_has_cpp_schema_keys() {
        let (chainstate, params, _data_dir) = setup_regtest_chainstate();
        let best = chainstate
            .best_block()
            .expect("best block")
            .expect("best block present");
        let value = rpc_getblock(&chainstate, vec![json!(best.height)], &params).expect("rpc");
        let obj = value.as_object().expect("object");
        for key in [
            "hash",
            "confirmations",
            "size",
            "height",
            "version",
            "merkleroot",
            "finalsaplingroot",
            "tx",
            "time",
            "bits",
            "difficulty",
            "chainwork",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        assert!(obj.get("tx").and_then(Value::as_array).is_some());
    }

    #[test]
    fn getmempoolinfo_has_cpp_schema_keys() {
        let mempool = Mutex::new(Mempool::new(0));
        let value = rpc_getmempoolinfo(Vec::new(), &mempool).expect("rpc");
        let obj = value.as_object().expect("object");
        for key in ["size", "bytes", "usage"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn getrawmempool_has_cpp_schema() {
        let mempool = Mutex::new(Mempool::new(0));
        let value = rpc_getrawmempool(Vec::new(), &mempool).expect("rpc");
        let txids = value.as_array().expect("array");
        assert!(txids.is_empty());
    }

    #[test]
    fn getrawmempool_verbose_has_cpp_schema() {
        let mempool = Mutex::new(Mempool::new(0));
        let value = rpc_getrawmempool(vec![json!(true)], &mempool).expect("rpc");
        assert!(value.as_object().is_some());
    }

    #[test]
    fn gettxout_has_cpp_schema_keys() {
        let (chainstate, params, _data_dir, _address, txid, vout) =
            setup_regtest_chain_with_p2pkh_utxo();
        let mempool = Mutex::new(Mempool::new(0));
        let value = rpc_gettxout(
            &chainstate,
            &mempool,
            vec![Value::String(hash256_to_hex(&txid)), json!(vout)],
            &params,
        )
        .expect("rpc");
        let obj = value.as_object().expect("object");
        for key in [
            "bestblock",
            "confirmations",
            "value",
            "scriptPubKey",
            "version",
            "coinbase",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        let bestblock = obj.get("bestblock").and_then(Value::as_str).unwrap();
        assert!(is_hex_64(bestblock));
    }

    #[test]
    fn gettxout_can_serve_mempool_outputs() {
        let (chainstate, params, _data_dir) = setup_regtest_chainstate();

        let script_pubkey = p2pkh_script([0x22u8; 20]);
        let tx = Transaction {
            f_overwintered: false,
            version: 1,
            version_group_id: 0,
            vin: vec![TxIn {
                prevout: OutPoint {
                    hash: [0x33u8; 32],
                    index: 0,
                },
                script_sig: Vec::new(),
                sequence: 0,
            }],
            vout: vec![TxOut {
                value: 123,
                script_pubkey,
            }],
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: None,
        };
        let txid = tx.txid().expect("txid");
        let raw = tx.consensus_encode().expect("encode tx");

        let entry = MempoolEntry {
            txid,
            tx,
            raw,
            time: 0,
            height: 0,
            fee: 0,
            spent_outpoints: Vec::new(),
            parents: Vec::new(),
        };
        let mut inner = Mempool::new(0);
        inner.insert(entry).expect("insert mempool tx");
        let mempool = Mutex::new(inner);

        let value = rpc_gettxout(
            &chainstate,
            &mempool,
            vec![Value::String(hash256_to_hex(&txid)), json!(0)],
            &params,
        )
        .expect("rpc");
        let obj = value.as_object().expect("object");
        assert_eq!(obj.get("confirmations").and_then(Value::as_i64), Some(0));
        assert_eq!(obj.get("coinbase").and_then(Value::as_bool), Some(false));
        assert_eq!(obj.get("version").and_then(Value::as_i64), Some(1));
    }

    #[test]
    fn getspentinfo_has_cpp_schema_keys() {
        let (chainstate, _params, _data_dir, _address, txid, vout) =
            setup_regtest_chain_with_p2pkh_utxo();

        let outpoint = OutPoint {
            hash: txid,
            index: vout,
        };
        let key = fluxd_chainstate::utxo::outpoint_key_bytes(&outpoint);
        let spent = fluxd_chainstate::spentindex::SpentIndexValue {
            txid: [0x44u8; 32],
            input_index: 7,
            block_height: 123,
            details: None,
        };
        let mut batch = WriteBatch::new();
        batch.put(Column::SpentIndex, key.as_bytes(), spent.encode());
        chainstate.commit_batch(batch).expect("insert spent index");

        let object_form = rpc_getspentinfo(
            &chainstate,
            vec![json!({"txid": hash256_to_hex(&txid), "index": vout })],
        )
        .expect("rpc");
        let obj = object_form.as_object().expect("object");
        for key in ["txid", "index", "height"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }

        let positional = rpc_getspentinfo(
            &chainstate,
            vec![Value::String(hash256_to_hex(&txid)), json!(vout)],
        )
        .expect("rpc");
        assert_eq!(positional, object_form);

        let not_found =
            rpc_getspentinfo(&chainstate, vec![Value::String("00".repeat(32)), json!(0)])
                .unwrap_err();
        assert_eq!(not_found.code, RPC_INVALID_ADDRESS_OR_KEY);
    }

    #[test]
    fn getaddressutxos_has_cpp_schema_keys() {
        let (chainstate, params, _data_dir, address, _txid, _vout) =
            setup_regtest_chain_with_p2pkh_utxo();
        let value = rpc_getaddressutxos(&chainstate, vec![Value::String(address.clone())], &params)
            .expect("rpc");
        let utxos = value.as_array().expect("array");
        assert!(!utxos.is_empty());
        let first = utxos[0].as_object().expect("object");
        for key in [
            "address",
            "txid",
            "outputIndex",
            "script",
            "satoshis",
            "height",
        ] {
            assert!(first.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn getaddressutxos_chaininfo_has_cpp_schema_keys() {
        let (chainstate, params, _data_dir, address, _txid, _vout) =
            setup_regtest_chain_with_p2pkh_utxo();
        let value = rpc_getaddressutxos(
            &chainstate,
            vec![json!({"addresses": [address], "chainInfo": true })],
            &params,
        )
        .expect("rpc");
        let obj = value.as_object().expect("object");
        for key in ["utxos", "hash", "height"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn getaddressbalance_has_cpp_schema_keys() {
        let (chainstate, params, _data_dir, address, _txid, _vout) =
            setup_regtest_chain_with_p2pkh_utxo();
        let value =
            rpc_getaddressbalance(&chainstate, vec![Value::String(address)], &params).expect("rpc");
        let obj = value.as_object().expect("object");
        for key in ["balance", "received"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn getaddressdeltas_has_cpp_schema_keys() {
        let (chainstate, params, _data_dir, address, _txid, _vout) =
            setup_regtest_chain_with_p2pkh_utxo();
        let value =
            rpc_getaddressdeltas(&chainstate, vec![Value::String(address)], &params).expect("rpc");
        let deltas = value.as_array().expect("array");
        assert!(!deltas.is_empty());
        let first = deltas[0].as_object().expect("object");
        for key in [
            "address",
            "blockindex",
            "height",
            "index",
            "satoshis",
            "txid",
        ] {
            assert!(first.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn getaddresstxids_has_cpp_schema() {
        let (chainstate, params, _data_dir, address, txid, _vout) =
            setup_regtest_chain_with_p2pkh_utxo();
        let value =
            rpc_getaddresstxids(&chainstate, vec![Value::String(address)], &params).expect("rpc");
        let txids = value.as_array().expect("array");
        let expected = hash256_to_hex(&txid);
        assert_eq!(
            txids.get(0).and_then(Value::as_str),
            Some(expected.as_str())
        );
    }

    #[test]
    fn getaddressmempool_has_cpp_schema() {
        let (chainstate, params, _data_dir, address, _txid, _vout) =
            setup_regtest_chain_with_p2pkh_utxo();
        let mempool = Mutex::new(Mempool::new(0));
        let value =
            rpc_getaddressmempool(&chainstate, &mempool, vec![Value::String(address)], &params)
                .expect("rpc");
        assert!(value.as_array().expect("array").is_empty());
    }

    #[test]
    fn getblocksubsidy_has_cpp_schema_keys() {
        let (chainstate, params, _data_dir) = setup_regtest_chainstate();
        let value = rpc_getblocksubsidy(&chainstate, Vec::new(), &params).expect("rpc");
        let obj = value.as_object().expect("object");
        assert!(obj.contains_key("miner"));
    }

    #[test]
    fn getblockhashes_has_cpp_schema() {
        let (chainstate, _params, _data_dir) = setup_regtest_chainstate();
        let value = rpc_getblockhashes(&chainstate, vec![json!(u32::MAX), json!(0)]).expect("rpc");
        let hashes = value.as_array().expect("array");
        assert!(!hashes.is_empty());
        let first = hashes[0].as_str().expect("string");
        assert!(is_hex_64(first));
    }

    #[test]
    fn getblockdeltas_has_cpp_schema_keys() {
        let (chainstate, params, _data_dir, _address, _txid, _vout) =
            setup_regtest_chain_with_p2pkh_utxo();
        let value = rpc_getblockdeltas(&chainstate, vec![json!(1)], &params).expect("rpc");
        let obj = value.as_object().expect("object");
        for key in [
            "hash",
            "confirmations",
            "size",
            "height",
            "version",
            "merkleroot",
            "deltas",
            "time",
            "mediantime",
            "nonce",
            "bits",
            "difficulty",
            "chainwork",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        assert!(obj.get("deltas").and_then(Value::as_array).is_some());
    }

    #[test]
    fn validateaddress_has_cpp_schema() {
        let (_chainstate, params, _data_dir, address, _txid, _vout) =
            setup_regtest_chain_with_p2pkh_utxo();

        let ok = rpc_validateaddress(vec![Value::String(address.clone())], &params).expect("rpc");
        let obj = ok.as_object().expect("object");
        assert_eq!(obj.get("isvalid").and_then(Value::as_bool), Some(true));
        for key in ["address", "scriptPubKey"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }

        let bad = rpc_validateaddress(vec![Value::String("notanaddress".to_string())], &params)
            .expect("rpc");
        let obj = bad.as_object().expect("object");
        assert_eq!(obj.get("isvalid").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn decodescript_has_cpp_schema_keys() {
        let (_chainstate, params, _data_dir) = setup_regtest_chainstate();
        let script = p2pkh_script([0x55u8; 20]);
        let hex = hex_bytes(&script);
        let value = rpc_decodescript(vec![Value::String(hex)], &params).expect("rpc");
        let obj = value.as_object().expect("object");
        for key in ["asm", "hex", "type", "p2sh"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn createrawtransaction_and_decoderawtransaction_have_cpp_schema() {
        let (chainstate, params, _data_dir) = setup_regtest_chainstate();
        let address =
            script_pubkey_to_address(&p2pkh_script([0x44u8; 20]), params.network).expect("address");

        let mut outputs = serde_json::Map::new();
        outputs.insert(address, json!("1.0"));

        let raw_hex = rpc_createrawtransaction(
            &chainstate,
            vec![Value::Array(Vec::new()), Value::Object(outputs)],
            &params,
        )
        .expect("rpc")
        .as_str()
        .expect("hex string")
        .to_string();
        assert!(bytes_from_hex(&raw_hex).is_some(), "invalid hex");

        let decoded = rpc_decoderawtransaction(vec![Value::String(raw_hex)], &params).expect("rpc");
        let obj = decoded.as_object().expect("object");
        for key in [
            "txid",
            "version",
            "size",
            "overwintered",
            "locktime",
            "vin",
            "vout",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
        let txid = obj.get("txid").and_then(Value::as_str).unwrap_or("");
        assert!(is_hex_64(txid));
    }

    #[test]
    fn getrawtransaction_mempool_verbose_has_cpp_schema() {
        let (chainstate, params, _data_dir) = setup_regtest_chainstate();

        let script_pubkey = p2pkh_script([0x22u8; 20]);
        let tx = Transaction {
            f_overwintered: false,
            version: 1,
            version_group_id: 0,
            vin: vec![TxIn {
                prevout: OutPoint {
                    hash: [0x33u8; 32],
                    index: 0,
                },
                script_sig: Vec::new(),
                sequence: 0,
            }],
            vout: vec![TxOut {
                value: 123,
                script_pubkey,
            }],
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: None,
        };
        let txid = tx.txid().expect("txid");
        let raw = tx.consensus_encode().expect("encode tx");

        let entry = MempoolEntry {
            txid,
            tx,
            raw: raw.clone(),
            time: 0,
            height: 0,
            fee: 0,
            spent_outpoints: Vec::new(),
            parents: Vec::new(),
        };
        let mut inner = Mempool::new(0);
        inner.insert(entry).expect("insert mempool tx");
        let mempool = Mutex::new(inner);

        let raw_value = rpc_getrawtransaction(
            &chainstate,
            &mempool,
            vec![Value::String(hash256_to_hex(&txid))],
            &params,
        )
        .expect("rpc");
        assert_eq!(raw_value.as_str(), Some(hex_bytes(&raw).as_str()));

        let verbose_value = rpc_getrawtransaction(
            &chainstate,
            &mempool,
            vec![Value::String(hash256_to_hex(&txid)), json!(1)],
            &params,
        )
        .expect("rpc");
        let obj = verbose_value.as_object().expect("object");
        for key in [
            "txid",
            "version",
            "size",
            "overwintered",
            "locktime",
            "vin",
            "vout",
            "hex",
        ] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn verifymessage_accepts_valid_signature() {
        let (_chainstate, params, _data_dir) = setup_regtest_chainstate();

        let secret = SecretKey::from_slice(&[1u8; 32]).expect("secret key");
        let pubkey = secret_key_pubkey_bytes(&secret, true);
        let key_hash = hash160(&pubkey);
        let script = p2pkh_script(key_hash);
        let address = script_pubkey_to_address(&script, params.network).expect("address");

        let message = "hello";
        let sig = sign_compact_message(&secret, true, message.as_bytes()).expect("sig");
        let signature = base64::engine::general_purpose::STANDARD.encode(sig);

        let ok = rpc_verifymessage(
            vec![
                Value::String(address.clone()),
                Value::String(signature.clone()),
                Value::String(message.to_string()),
            ],
            &params,
        )
        .expect("rpc");
        assert_eq!(ok, Value::Bool(true));

        let bad = rpc_verifymessage(
            vec![
                Value::String(address),
                Value::String(signature),
                Value::String("not-hello".to_string()),
            ],
            &params,
        )
        .expect("rpc");
        assert_eq!(bad, Value::Bool(false));
    }

    #[test]
    fn createmultisig_has_cpp_schema_keys() {
        let (_chainstate, params, _data_dir) = setup_regtest_chainstate();
        let secret_a = SecretKey::from_slice(&[2u8; 32]).expect("secret key");
        let secret_b = SecretKey::from_slice(&[3u8; 32]).expect("secret key");
        let pub_a = secret_key_pubkey_bytes(&secret_a, true);
        let pub_b = secret_key_pubkey_bytes(&secret_b, true);

        let value = rpc_createmultisig(
            vec![
                json!(1),
                Value::Array(vec![
                    Value::String(hex_bytes(&pub_a)),
                    Value::String(hex_bytes(&pub_b)),
                ]),
            ],
            &params,
        )
        .expect("rpc");
        let obj = value.as_object().expect("object");
        for key in ["address", "redeemScript"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn fluxnode_rpcs_have_cpp_schema_keys() {
        let (chainstate, params, data_dir) = setup_regtest_chainstate();
        extend_regtest_chain_to_height(&chainstate, &params, 69);

        let mut batch = WriteBatch::new();
        let record_cumulus = add_fluxnode_record_to_batch(
            &mut batch,
            OutPoint {
                hash: [0x10u8; 32],
                index: 0,
            },
            1,
            1,
            vec![0x02u8; 33],
            vec![0x03u8; 33],
            100_000,
        );
        add_fluxnode_record_to_batch(
            &mut batch,
            OutPoint {
                hash: [0x11u8; 32],
                index: 1,
            },
            2,
            69,
            vec![0x02u8; 33],
            vec![0x03u8; 33],
            100_000,
        );
        add_fluxnode_record_to_batch(
            &mut batch,
            OutPoint {
                hash: [0x12u8; 32],
                index: 2,
            },
            3,
            69,
            vec![0x02u8; 33],
            vec![0x03u8; 33],
            100_000,
        );
        chainstate.commit_batch(batch).expect("insert fluxnodes");

        let value = rpc_getfluxnodecount(&chainstate, Vec::new()).expect("rpc");
        let obj = value.as_object().expect("object");
        for key in ["total", "stable", "enabled", "inqueue"] {
            assert!(obj.contains_key(key), "missing key {key}");
        }

        let value = rpc_viewdeterministicfluxnodelist(&chainstate, Vec::new()).expect("rpc");
        let list = value.as_array().expect("array");
        assert!(!list.is_empty());
        let first = list[0].as_object().expect("object");
        for key in [
            "collateral",
            "txhash",
            "outidx",
            "added_height",
            "confirmed_height",
            "last_confirmed_height",
            "last_paid_height",
            "tier",
            "pubkey",
            "collateral_pubkey",
        ] {
            assert!(first.contains_key(key), "missing key {key}");
        }

        let value = rpc_fluxnodecurrentwinner(&chainstate, Vec::new()).expect("rpc");
        let winners = value.as_object().expect("object");
        for tier_key in ["CUMULUS Winner", "NIMBUS Winner", "STRATUS Winner"] {
            assert!(winners.contains_key(tier_key), "missing key {tier_key}");
            let winner = winners.get(tier_key).expect("winner key present");
            if winner.is_null() {
                continue;
            }
            let obj = winner.as_object().expect("object");
            for key in [
                "collateral",
                "added_height",
                "confirmed_height",
                "last_confirmed_height",
                "last_paid_height",
                "tier",
            ] {
                assert!(obj.contains_key(key), "missing key {key}");
            }
        }

        let value = rpc_getfluxnodestatus(
            &chainstate,
            vec![Value::String(format_outpoint(&record_cumulus.collateral))],
            &params,
            &data_dir,
        )
        .expect("rpc");
        let status = value.as_object().expect("object");
        for key in [
            "status",
            "collateral",
            "txhash",
            "outidx",
            "ip",
            "network",
            "added_height",
            "confirmed_height",
            "last_confirmed_height",
            "last_paid_height",
            "tier",
            "payment_address",
            "pubkey",
            "activesince",
            "lastpaid",
        ] {
            assert!(status.contains_key(key), "missing key {key}");
        }

        let starts = rpc_getstartlist(&chainstate, Vec::new(), &params).expect("rpc");
        let starts = starts.as_array().expect("array");
        assert_eq!(starts.len(), 2);
        let first = starts[0].as_object().expect("object");
        for key in [
            "collateral",
            "added_height",
            "payment_address",
            "expires_in",
        ] {
            assert!(first.contains_key(key), "missing key {key}");
        }

        let dos = rpc_getdoslist(&chainstate, Vec::new(), &params).expect("rpc");
        let dos = dos.as_array().expect("array");
        assert_eq!(dos.len(), 1);
        let first = dos[0].as_object().expect("object");
        for key in [
            "collateral",
            "added_height",
            "payment_address",
            "eligible_in",
        ] {
            assert!(first.contains_key(key), "missing key {key}");
        }
    }

    #[test]
    fn getblockhash_rejects_wrong_param_count() {
        let (chainstate, _params, _data_dir) = setup_regtest_chainstate();
        let err = rpc_getblockhash(&chainstate, Vec::new()).unwrap_err();
        assert_eq!(err.code, RPC_INVALID_PARAMETER);
    }

    #[test]
    fn getblockhash_rejects_out_of_range_height() {
        let (chainstate, _params, _data_dir) = setup_regtest_chainstate();
        let err = rpc_getblockhash(&chainstate, vec![json!(1)]).unwrap_err();
        assert_eq!(err.code, RPC_INVALID_PARAMETER);
    }

    #[test]
    fn getblockheader_returns_not_found_code() {
        let (chainstate, params, _data_dir) = setup_regtest_chainstate();
        let missing_hash = Value::String("00".repeat(32));
        let err = rpc_getblockheader(&chainstate, vec![missing_hash], &params).unwrap_err();
        assert_eq!(err.code, RPC_INVALID_ADDRESS_OR_KEY);
    }
}

fn system_time_to_unix(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

fn peer_kind_name(kind: PeerKind) -> &'static str {
    match kind {
        PeerKind::Block => "block",
        PeerKind::Header => "header",
        PeerKind::Relay => "relay",
    }
}

fn resolve_block_hash<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    value: &Value,
) -> Result<(Hash256, HeaderEntry), RpcError> {
    let hash = if let Some(height) = parse_height_opt(value)? {
        let best_height = best_block_height(chainstate)?;
        if height < 0 || height > best_height {
            return Err(RpcError::new(
                RPC_INVALID_PARAMETER,
                "block height out of range",
            ));
        }
        chainstate
            .height_hash(height)
            .map_err(map_internal)?
            .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "block not found"))?
    } else {
        parse_hash(value)?
    };
    let entry = chainstate
        .header_entry(&hash)
        .map_err(map_internal)?
        .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "block not found"))?;
    Ok((hash, entry))
}

fn parse_hash(value: &Value) -> Result<Hash256, RpcError> {
    let text = value
        .as_str()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "hash must be a string"))?;
    hash256_from_hex(text).map_err(|_| RpcError::new(RPC_INVALID_PARAMETER, "invalid hash"))
}

fn parse_height(value: &Value) -> Result<i32, RpcError> {
    if let Some(height) = parse_height_opt(value)? {
        return Ok(height);
    }
    Err(RpcError::new(
        RPC_INVALID_PARAMETER,
        "height must be numeric",
    ))
}

fn parse_height_opt(value: &Value) -> Result<Option<i32>, RpcError> {
    if let Some(height) = value.as_i64() {
        return Ok(Some(height as i32));
    }
    let text = match value.as_str() {
        Some(text) => text,
        None => return Ok(None),
    };
    if text.chars().all(|c| c.is_ascii_digit()) {
        let parsed = text
            .parse::<i32>()
            .map_err(|_| RpcError::new(RPC_INVALID_PARAMETER, "invalid height"))?;
        return Ok(Some(parsed));
    }
    Ok(None)
}

fn parse_u32(value: &Value, label: &str) -> Result<u32, RpcError> {
    if let Some(num) = value.as_u64() {
        if num <= u32::MAX as u64 {
            return Ok(num as u32);
        }
    }
    if let Some(num) = value.as_i64() {
        if num >= 0 && num <= u32::MAX as i64 {
            return Ok(num as u32);
        }
    }
    if let Some(text) = value.as_str() {
        if text.chars().all(|c| c.is_ascii_digit()) {
            let parsed = text
                .parse::<u32>()
                .map_err(|_| RpcError::new(RPC_INVALID_PARAMETER, "invalid number"))?;
            return Ok(parsed);
        }
    }
    Err(RpcError::new(
        RPC_INVALID_PARAMETER,
        format!("{label} must be numeric"),
    ))
}

fn parse_bool(value: &Value) -> Result<bool, RpcError> {
    value
        .as_bool()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "boolean expected"))
}

fn parse_verbose_flag(value: &Value) -> Result<bool, RpcError> {
    if let Some(flag) = value.as_bool() {
        return Ok(flag);
    }
    if let Some(flag) = value.as_i64() {
        return Ok(flag != 0);
    }
    Err(RpcError::new(
        RPC_INVALID_PARAMETER,
        "verbose flag must be boolean or numeric",
    ))
}

fn parse_verbosity(value: &Value) -> Result<i32, RpcError> {
    let verbosity = value
        .as_i64()
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "verbosity must be numeric"))?;
    match verbosity {
        0..=2 => Ok(verbosity as i32),
        _ => Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "verbosity must be 0, 1, or 2",
        )),
    }
}

fn parse_addresses_param<'a>(
    value: &'a Value,
) -> Result<(Vec<String>, Option<&'a serde_json::Map<String, Value>>), RpcError> {
    match value {
        Value::String(address) => Ok((vec![address.clone()], None)),
        Value::Object(map) => {
            let addresses = map
                .get("addresses")
                .and_then(|value| value.as_array())
                .ok_or_else(|| {
                    RpcError::new(
                        RPC_INVALID_ADDRESS_OR_KEY,
                        "Addresses is expected to be an array",
                    )
                })?;
            let mut out = Vec::with_capacity(addresses.len());
            for entry in addresses {
                let address = entry
                    .as_str()
                    .ok_or_else(|| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Invalid address"))?;
                out.push(address.to_string());
            }
            Ok((out, Some(map)))
        }
        _ => Err(RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Invalid address")),
    }
}

fn decode_address_scripts(
    addresses: Vec<String>,
    network: Network,
) -> Result<Vec<(String, Vec<u8>)>, RpcError> {
    let mut out = Vec::with_capacity(addresses.len());
    for address in addresses {
        let script = address_to_script_pubkey(&address, network).map_err(|err| match err {
            AddressError::InvalidLength
            | AddressError::InvalidCharacter
            | AddressError::InvalidChecksum
            | AddressError::UnknownPrefix => {
                RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Invalid address")
            }
        })?;
        out.push((address, script));
    }
    Ok(out)
}

fn parse_chain_info_flag(opts: Option<&serde_json::Map<String, Value>>) -> Result<bool, RpcError> {
    let Some(map) = opts else {
        return Ok(false);
    };
    let Some(value) = map.get("chainInfo") else {
        return Ok(false);
    };
    parse_bool(value)
}

fn parse_height_range<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    opts: Option<&serde_json::Map<String, Value>>,
) -> Result<Option<(u32, u32)>, RpcError> {
    let Some(map) = opts else {
        return Ok(None);
    };
    let Some(start_value) = map.get("start") else {
        return Ok(None);
    };
    let Some(end_value) = map.get("end") else {
        return Ok(None);
    };
    if start_value.is_null() || end_value.is_null() {
        return Ok(None);
    }
    let start = parse_u32(start_value, "start")?;
    let end = parse_u32(end_value, "end")?;
    if start == 0 || end == 0 {
        return Err(RpcError::new(
            RPC_INVALID_ADDRESS_OR_KEY,
            "Start and end are expected to be greater than zero",
        ));
    }
    if end < start {
        return Err(RpcError::new(
            RPC_INVALID_ADDRESS_OR_KEY,
            "End value is expected to be greater than start",
        ));
    }
    let best_height = best_block_height(chainstate)?;
    let best_u32 = u32::try_from(best_height).unwrap_or(0);
    if start > best_u32 || end > best_u32 {
        return Err(RpcError::new(
            RPC_INVALID_ADDRESS_OR_KEY,
            "Start or end is outside chain range",
        ));
    }
    Ok(Some((start, end)))
}

fn ensure_no_params(params: &[Value]) -> Result<(), RpcError> {
    if params.is_empty() {
        Ok(())
    } else {
        Err(RpcError::new(
            RPC_INVALID_PARAMETER,
            "method takes no parameters",
        ))
    }
}

fn best_block_height<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
) -> Result<i32, RpcError> {
    Ok(chainstate
        .best_block()
        .map_err(map_internal)?
        .map(|tip| tip.height)
        .unwrap_or(0))
}

fn confirmations_for_height<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    height: i32,
    best_height: i32,
    hash: &Hash256,
) -> Result<i32, RpcError> {
    if height < 0 || height > best_height {
        return Ok(-1);
    }
    let main_hash = chainstate.height_hash(height).map_err(map_internal)?;
    if main_hash.as_ref() != Some(hash) {
        return Ok(-1);
    }
    Ok(best_height - height + 1)
}

fn next_hash_for_height<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    height: i32,
    best_height: i32,
    hash: &Hash256,
) -> Result<Option<Hash256>, RpcError> {
    if height < 0 || height >= best_height {
        return Ok(None);
    }
    let main_hash = chainstate.height_hash(height).map_err(map_internal)?;
    if main_hash.as_ref() != Some(hash) {
        return Ok(None);
    }
    chainstate.height_hash(height + 1).map_err(map_internal)
}

fn median_time_past<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    height: i32,
) -> Result<i64, RpcError> {
    let mut times = Vec::with_capacity(11);
    let mut current = height;
    for _ in 0..11 {
        if current < 0 {
            break;
        }
        let Some(hash) = chainstate.height_hash(current).map_err(map_internal)? else {
            break;
        };
        let entry = chainstate
            .header_entry(&hash)
            .map_err(map_internal)?
            .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "missing header entry"))?;
        times.push(entry.time as i64);
        current -= 1;
    }
    times.sort_unstable();
    Ok(times.get(times.len() / 2).copied().unwrap_or(0))
}

fn spent_details_address(
    address_type: u32,
    address_hash: &[u8; 20],
    network: Network,
) -> Option<String> {
    match address_type {
        1 => {
            let mut script = Vec::with_capacity(25);
            script.extend_from_slice(&[0x76, 0xa9, 0x14]);
            script.extend_from_slice(address_hash);
            script.extend_from_slice(&[0x88, 0xac]);
            script_pubkey_to_address(&script, network)
        }
        2 => {
            let mut script = Vec::with_capacity(23);
            script.extend_from_slice(&[0xa9, 0x14]);
            script.extend_from_slice(address_hash);
            script.push(0x87);
            script_pubkey_to_address(&script, network)
        }
        _ => None,
    }
}

fn resolve_prevout_via_txindex<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    tx_cache: &mut HashMap<Hash256, Transaction>,
    outpoint: &fluxd_primitives::outpoint::OutPoint,
    network: Network,
) -> Result<(i64, Option<String>), RpcError> {
    let prev_txid = outpoint.hash;
    if !tx_cache.contains_key(&prev_txid) {
        let location = chainstate
            .tx_location(&prev_txid)
            .map_err(map_internal)?
            .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "Spent information not available"))?;
        let bytes = chainstate
            .read_block(location.block)
            .map_err(map_internal)?;
        let block =
            fluxd_primitives::block::Block::consensus_decode(&bytes).map_err(map_internal)?;
        let tx_index = location.index as usize;
        let tx = block
            .transactions
            .get(tx_index)
            .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "Spent information not available"))?
            .clone();
        tx_cache.insert(prev_txid, tx);
    }

    let tx = tx_cache
        .get(&prev_txid)
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "Spent information not available"))?;
    let output_index = usize::try_from(outpoint.index)
        .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "Spent information not available"))?;
    let output = tx
        .vout
        .get(output_index)
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "Spent information not available"))?;
    Ok((
        output.value,
        script_pubkey_to_address(&output.script_pubkey, network),
    ))
}

fn build_upgrade_info(params: &ChainParams, height: i32) -> Value {
    let mut map = serde_json::Map::new();
    for idx in ALL_UPGRADES {
        let upgrade = params.consensus.upgrades[idx.as_usize()];
        if upgrade.activation_height
            == fluxd_consensus::upgrades::NetworkUpgrade::NO_ACTIVATION_HEIGHT
        {
            continue;
        }
        let info = NETWORK_UPGRADE_INFO[idx.as_usize()];
        let status = match network_upgrade_state(height, &params.consensus.upgrades, idx) {
            UpgradeState::Active => "active",
            UpgradeState::Pending => "pending",
            UpgradeState::Disabled => "disabled",
        };
        let entry = json!({
            "name": info.name,
            "activationheight": upgrade.activation_height,
            "status": status,
            "info": info.info,
        });
        map.insert(format!("{:08x}", info.branch_id), entry);
    }
    Value::Object(map)
}

fn block_header_version(bytes: &[u8]) -> Result<i32, RpcError> {
    if bytes.len() < 4 {
        return Err(RpcError::new(
            RPC_INTERNAL_ERROR,
            "invalid block header bytes",
        ));
    }
    let mut buf = [0u8; 4];
    buf.copy_from_slice(&bytes[..4]);
    Ok(i32::from_le_bytes(buf))
}

fn build_softfork_info<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    tip: Option<Hash256>,
    consensus: &fluxd_consensus::params::ConsensusParams,
) -> Result<Value, RpcError> {
    let window = consensus.majority_window;
    let mut found_v2 = 0i32;
    let mut found_v3 = 0i32;
    let mut found_v4 = 0i32;

    let mut cursor = tip;
    for _ in 0..window {
        let Some(hash) = cursor else { break };
        let entry = chainstate
            .header_entry(&hash)
            .map_err(map_internal)?
            .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "missing header entry"))?;
        let bytes = chainstate
            .block_header_bytes(&hash)
            .map_err(map_internal)?
            .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "missing block header bytes"))?;
        let version = block_header_version(&bytes)?;
        if version >= 2 {
            found_v2 = found_v2.saturating_add(1);
            if version >= 3 {
                found_v3 = found_v3.saturating_add(1);
                if version >= 4 {
                    found_v4 = found_v4.saturating_add(1);
                }
            }
        }
        cursor = if entry.height > 0 {
            Some(entry.prev_hash)
        } else {
            None
        };
    }

    let enforce_required = consensus.majority_enforce_block_upgrade;
    let reject_required = consensus.majority_reject_block_outdated;

    fn majority_desc(found: i32, required: i32, window: i32) -> Value {
        json!({
            "status": found >= required,
            "found": found,
            "required": required,
            "window": window,
        })
    }

    fn softfork_desc(
        id: &'static str,
        version: i32,
        found: i32,
        enforce_required: i32,
        reject_required: i32,
        window: i32,
    ) -> Value {
        json!({
            "id": id,
            "version": version,
            "enforce": majority_desc(found, enforce_required, window),
            "reject": majority_desc(found, reject_required, window),
        })
    }

    Ok(Value::Array(vec![
        softfork_desc(
            "bip34",
            2,
            found_v2,
            enforce_required,
            reject_required,
            window,
        ),
        softfork_desc(
            "bip66",
            3,
            found_v3,
            enforce_required,
            reject_required,
            window,
        ),
        softfork_desc(
            "bip65",
            4,
            found_v4,
            enforce_required,
            reject_required,
            window,
        ),
    ]))
}

fn network_name(network: Network) -> &'static str {
    match network {
        Network::Mainnet => "main",
        Network::Testnet => "test",
        Network::Regtest => "regtest",
    }
}

fn difficulty_from_bits(bits: u32, params: &ChainParams) -> Result<f64, String> {
    let target = compact_to_u256(bits).map_err(|err| err.to_string())?;
    if target.is_zero() {
        return Ok(0.0);
    }
    let pow_limit = U256::from_little_endian(&params.consensus.pow_limit);
    let pow_limit_f = u256_to_f64(pow_limit);
    let target_f = u256_to_f64(target);
    if target_f == 0.0 {
        return Ok(0.0);
    }
    Ok(pow_limit_f / target_f)
}

fn u256_to_f64(value: U256) -> f64 {
    let bytes = value.to_big_endian();
    let mut acc = 0f64;
    for byte in bytes {
        acc = acc * 256.0 + byte as f64;
    }
    acc
}

fn node_version() -> i64 {
    let version = env!("CARGO_PKG_VERSION");
    let mut parts = version.split('.');
    let major = parts
        .next()
        .and_then(|part| part.parse::<i64>().ok())
        .unwrap_or(0);
    let minor = parts
        .next()
        .and_then(|part| part.parse::<i64>().ok())
        .unwrap_or(0);
    let patch = parts
        .next()
        .and_then(|part| part.parse::<i64>().ok())
        .unwrap_or(0);
    major * 10000 + minor * 100 + patch
}

fn format_outpoint(outpoint: &fluxd_primitives::outpoint::OutPoint) -> String {
    format!("{}:{}", hash256_to_hex(&outpoint.hash), outpoint.index)
}

fn parse_outpoint(input: &str) -> Result<OutPoint, RpcError> {
    let (txid_hex, index_str) = input
        .trim()
        .split_once(':')
        .ok_or_else(|| RpcError::new(RPC_INVALID_PARAMETER, "outpoint must be txid:vout"))?;
    let hash = hash256_from_hex(txid_hex)
        .map_err(|_| RpcError::new(RPC_INVALID_PARAMETER, "invalid txid"))?;
    let index = index_str
        .parse::<u32>()
        .map_err(|_| RpcError::new(RPC_INVALID_PARAMETER, "invalid vout"))?;
    Ok(OutPoint { hash, index })
}

fn fluxnode_network_info(address: &str) -> (String, String) {
    let address = address.trim();
    if address.is_empty() {
        return (String::new(), String::new());
    }

    if let Ok(addr) = address.parse::<SocketAddr>() {
        let ip = addr.ip();
        let network = match ip {
            IpAddr::V4(_) => "ipv4",
            IpAddr::V6(_) => "ipv6",
        };
        return (ip.to_string(), network.to_string());
    }

    let host = if let Some(rest) = address.strip_prefix('[') {
        rest.split_once(']')
            .map(|(host, _)| host)
            .unwrap_or(address)
    } else if let Some((host, port)) = address.rsplit_once(':') {
        if !host.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()) {
            host
        } else {
            address
        }
    } else {
        address
    };

    let network = if host.ends_with(".onion") {
        "onion"
    } else if host.parse::<std::net::Ipv4Addr>().is_ok() {
        "ipv4"
    } else if host.parse::<std::net::Ipv6Addr>().is_ok() {
        "ipv6"
    } else {
        "unknown"
    };

    (host.to_string(), network.to_string())
}

#[derive(Clone, Debug)]
struct FluxnodeConfEntry {
    alias: String,
    address: String,
    privkey: String,
    collateral: OutPoint,
    collateral_privkey: Option<String>,
    redeem_script: Option<String>,
}

fn read_fluxnode_conf(data_dir: &Path) -> Result<Vec<FluxnodeConfEntry>, RpcError> {
    let path = data_dir.join("fluxnode.conf");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = fs::read_to_string(&path).map_err(map_internal)?;
    let mut entries = Vec::new();
    let mut invalid = 0usize;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 {
            invalid = invalid.saturating_add(1);
            continue;
        }
        let alias = parts[0].to_string();
        let address = parts[1].to_string();
        let privkey = parts[2].to_string();
        let txhash = parts[3];
        let outidx = parts[4];
        let Ok(hash) = hash256_from_hex(txhash) else {
            invalid = invalid.saturating_add(1);
            continue;
        };
        let Ok(index) = outidx.parse::<u32>() else {
            invalid = invalid.saturating_add(1);
            continue;
        };
        entries.push(FluxnodeConfEntry {
            alias,
            address,
            privkey,
            collateral: OutPoint { hash, index },
            collateral_privkey: parts.get(5).map(|value| (*value).to_string()),
            redeem_script: parts.get(6).map(|value| (*value).to_string()),
        });
    }
    if entries.is_empty() && invalid > 0 {
        return Err(RpcError::new(
            RPC_INTERNAL_ERROR,
            "fluxnode.conf contains no valid entries",
        ));
    }
    Ok(entries)
}

fn parse_wif_secret_key(wif: &str, network: Network) -> Result<(SecretKey, bool), RpcError> {
    let (secret, compressed) = wif_to_secret_key(wif, network)
        .map_err(|_| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Invalid private key encoding"))?;
    let secret = SecretKey::from_slice(&secret)
        .map_err(|_| RpcError::new(RPC_INVALID_ADDRESS_OR_KEY, "Invalid private key"))?;
    Ok((secret, compressed))
}

fn secret_key_pubkey_bytes(secret: &SecretKey, compressed: bool) -> Vec<u8> {
    let secp = Secp256k1::signing_only();
    let pubkey = PublicKey::from_secret_key(&secp, secret);
    if compressed {
        pubkey.serialize().to_vec()
    } else {
        pubkey.serialize_uncompressed().to_vec()
    }
}

fn sign_compact_message(
    secret: &SecretKey,
    compressed: bool,
    message: &[u8],
) -> Result<[u8; 65], RpcError> {
    let digest = signed_message_hash(message);
    let msg = Message::from_digest_slice(&digest)
        .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "Invalid message digest"))?;
    let secp = Secp256k1::signing_only();
    let sig: RecoverableSignature = secp.sign_ecdsa_recoverable(&msg, secret);
    let (rec_id, sig_bytes) = sig.serialize_compact();
    let header = 27u8
        .saturating_add(rec_id.to_i32().try_into().unwrap_or(0))
        .saturating_add(if compressed { 4 } else { 0 });
    let mut out = [0u8; 65];
    out[0] = header;
    out[1..].copy_from_slice(&sig_bytes);
    Ok(out)
}

fn fluxnode_payment_address<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    record: &FluxnodeRecord,
    network: Network,
) -> Result<Option<String>, RpcError> {
    if let Some(key) = record.p2sh_script {
        let script = chainstate.fluxnode_key(key).map_err(map_internal)?;
        let Some(script) = script else {
            return Ok(None);
        };
        let inner = hash160(&script);
        let mut pubkey = Vec::with_capacity(23);
        pubkey.extend_from_slice(&[0xa9, 0x14]);
        pubkey.extend_from_slice(&inner);
        pubkey.push(0x87);
        return Ok(script_pubkey_to_address(&pubkey, network));
    }
    if let Some(key) = record.collateral_pubkey {
        let pubkey = chainstate.fluxnode_key(key).map_err(map_internal)?;
        let Some(pubkey) = pubkey else {
            return Ok(None);
        };
        let hash = hash160(&pubkey);
        let mut script = Vec::with_capacity(25);
        script.extend_from_slice(&[0x76, 0xa9, 0x14]);
        script.extend_from_slice(&hash);
        script.extend_from_slice(&[0x88, 0xac]);
        return Ok(script_pubkey_to_address(&script, network));
    }
    Ok(None)
}

fn header_time_at_height<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    height: i32,
) -> Option<u32> {
    if height < 0 {
        return None;
    }
    let hash = chainstate.height_hash(height).ok().flatten()?;
    chainstate
        .header_entry(&hash)
        .ok()
        .flatten()
        .map(|entry| entry.time)
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'a' + (value - 10)) as char,
    }
}

fn bytes_from_hex(input: &str) -> Option<Vec<u8>> {
    let mut hex = input.trim();
    if let Some(stripped) = hex.strip_prefix("0x").or_else(|| hex.strip_prefix("0X")) {
        hex = stripped;
    }
    if hex.len() % 2 == 1 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let mut iter = hex.as_bytes().iter().copied();
    while let (Some(high), Some(low)) = (iter.next(), iter.next()) {
        let high = (high as char).to_digit(16)? as u8;
        let low = (low as char).to_digit(16)? as u8;
        bytes.push(high << 4 | low);
    }
    Some(bytes)
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({
        "result": result,
        "error": Value::Null,
        "id": id,
    })
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "result": Value::Null,
        "error": {
            "code": code,
            "message": message.into(),
        },
        "id": id,
    })
}

fn map_internal(err: impl ToString) -> RpcError {
    RpcError::new(RPC_INTERNAL_ERROR, err.to_string())
}

fn write_cookie(path: &Path, user: &str, pass: &str) -> Result<(), String> {
    let mut file = File::create(path).map_err(|err| err.to_string())?;
    let contents = format!("{user}:{pass}");
    file.write_all(contents.as_bytes())
        .map_err(|err| err.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = fs::Permissions::from_mode(0o600);
        fs::set_permissions(path, perms).map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

struct HttpRequest {
    method: String,
    path: String,
    query: Option<String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

async fn read_http_request(stream: &mut tokio::net::TcpStream) -> Result<HttpRequest, String> {
    let mut buffer = Vec::new();
    let mut temp = [0u8; 4096];
    let mut header_end = None;
    while buffer.len() < MAX_REQUEST_BYTES {
        let read = stream
            .read(&mut temp)
            .await
            .map_err(|err| err.to_string())?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&temp[..read]);
        if let Some(pos) = find_header_end(&buffer) {
            header_end = Some(pos);
            break;
        }
    }

    let header_end = header_end.ok_or_else(|| "invalid http request".to_string())?;
    let header_bytes = &buffer[..header_end];
    let mut headers = HashMap::new();
    let mut lines = header_bytes.split(|byte| *byte == b'\n');
    let request_line = lines
        .next()
        .ok_or_else(|| "invalid http request".to_string())?;
    let request_line = String::from_utf8_lossy(request_line);
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let raw_path = parts.next().unwrap_or("/");
    let (path, query) = match raw_path.split_once('?') {
        Some((path, query)) => (path.to_string(), Some(query.to_string())),
        None => (raw_path.to_string(), None),
    };

    for line in lines {
        let line = String::from_utf8_lossy(line).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let mut body = buffer[header_end..].to_vec();
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(body.len());
    if content_length > MAX_REQUEST_BYTES {
        return Err("request too large".to_string());
    }
    while body.len() < content_length {
        let read = stream
            .read(&mut temp)
            .await
            .map_err(|err| err.to_string())?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&temp[..read]);
    }
    body.truncate(content_length);

    Ok(HttpRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

impl RpcAuth {
    fn check(&self, header: Option<&str>) -> bool {
        let Some(header) = header else {
            return false;
        };
        let header = header.trim();
        let Some(encoded) = header.strip_prefix("Basic ") else {
            return false;
        };
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok());
        let Some(decoded) = decoded else {
            return false;
        };
        decoded == format!("{}:{}", self.user, self.pass)
    }
}

fn build_response(status: &str, content_type: &str, body: &str) -> Vec<u8> {
    let mut response = String::new();
    response.push_str("HTTP/1.1 ");
    response.push_str(status);
    response.push_str("\r\nContent-Type: ");
    response.push_str(content_type);
    response.push_str("\r\nContent-Length: ");
    response.push_str(&body.len().to_string());
    response.push_str("\r\nConnection: close\r\n\r\n");
    response.push_str(body);
    response.into_bytes()
}

fn build_unauthorized() -> Vec<u8> {
    let body = "unauthorized";
    let mut response = String::new();
    response.push_str("HTTP/1.1 401 Unauthorized\r\n");
    response.push_str(&format!(
        "WWW-Authenticate: Basic realm=\"{RPC_REALM}\"\r\n"
    ));
    response.push_str("Content-Type: text/plain\r\n");
    response.push_str(&format!("Content-Length: {}\r\n", body.len()));
    response.push_str("Connection: close\r\n\r\n");
    response.push_str(body);
    response.into_bytes()
}
