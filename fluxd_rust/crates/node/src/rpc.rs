use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use rand::RngCore;
use serde_json::{json, Number, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use fluxd_chainstate::index::HeaderEntry;
use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::ValidationFlags;
use fluxd_consensus::block_subsidy;
use fluxd_consensus::constants::PROTOCOL_VERSION;
use fluxd_consensus::money::COIN;
use fluxd_consensus::params::{hash256_from_hex, ChainParams, Network};
use fluxd_consensus::upgrades::{
    current_epoch_branch_id, network_upgrade_state, UpgradeState, ALL_UPGRADES,
    NETWORK_UPGRADE_INFO,
};
use fluxd_consensus::Hash256;
use fluxd_fluxnode::storage::FluxnodeRecord;
use fluxd_pow::difficulty::compact_to_u256;
use fluxd_primitives::transaction::Transaction;
use fluxd_primitives::{address_to_script_pubkey, script_pubkey_to_address, AddressError};
use fluxd_script::standard::{classify_script_pubkey, ScriptType};
use primitive_types::U256;

use crate::mempool::{build_mempool_entry, Mempool, MempoolErrorKind};
use crate::p2p::{NetTotals, PeerKind, PeerRegistry};
use crate::peer_book::HeaderPeerBook;
use crate::stats::{hash256_to_hex, MempoolMetrics};

const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const RPC_REALM: &str = "fluxd";
const RPC_COOKIE_FILE: &str = "rpc.cookie";

const RPC_INVALID_PARAMETER: i64 = -8;
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
    "getblocktemplate",
    "getnetworkhashps",
    "getnetworksolps",
    "getlocalsolps",
    "getconnectioncount",
    "getnettotals",
    "listbanned",
    "getnetworkinfo",
    "getpeerinfo",
    "getfluxnodecount",
    "listfluxnodes",
    "viewdeterministicfluxnodelist",
    "fluxnodecurrentwinner",
    "getfluxnodestatus",
    "getdoslist",
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
    mempool: Arc<Mutex<Mempool>>,
    mempool_metrics: Arc<MempoolMetrics>,
    mempool_flags: ValidationFlags,
    params: ChainParams,
    data_dir: PathBuf,
    net_totals: Arc<NetTotals>,
    peer_registry: Arc<PeerRegistry>,
    header_peer_book: Arc<HeaderPeerBook>,
    tx_announce: broadcast::Sender<Hash256>,
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
        let mempool = Arc::clone(&mempool);
        let mempool_metrics = Arc::clone(&mempool_metrics);
        let mempool_flags = mempool_flags.clone();
        let params = params.clone();
        let data_dir = data_dir.clone();
        let net_totals = Arc::clone(&net_totals);
        let peer_registry = Arc::clone(&peer_registry);
        let header_peer_book = Arc::clone(&header_peer_book);
        let tx_announce = tx_announce.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(
                stream,
                auth,
                chainstate,
                mempool,
                mempool_metrics,
                mempool_flags,
                params,
                data_dir,
                net_totals,
                peer_registry,
                header_peer_book,
                tx_announce,
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
    mempool: Arc<Mutex<Mempool>>,
    mempool_metrics: Arc<MempoolMetrics>,
    mempool_flags: ValidationFlags,
    chain_params: ChainParams,
    data_dir: PathBuf,
    net_totals: Arc<NetTotals>,
    peer_registry: Arc<PeerRegistry>,
    header_peer_book: Arc<HeaderPeerBook>,
    tx_announce: broadcast::Sender<Hash256>,
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
        let rpc_response = match handle_daemon_request(
            method,
            &request,
            chainstate.as_ref(),
            mempool.as_ref(),
            mempool_metrics.as_ref(),
            &mempool_flags,
            &chain_params,
            &data_dir,
            &net_totals,
            &peer_registry,
            &header_peer_book,
            &tx_announce,
        ) {
            Ok(value) => rpc_ok(Value::Null, value),
            Err(err) => rpc_error(Value::Null, err.code, err.message),
        };
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
        mempool.as_ref(),
        mempool_metrics.as_ref(),
        &mempool_flags,
        &chain_params,
        &data_dir,
        &net_totals,
        &peer_registry,
        &header_peer_book,
        &tx_announce,
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

#[allow(clippy::too_many_arguments)]
fn handle_daemon_request<S: fluxd_storage::KeyValueStore>(
    method: &str,
    request: &HttpRequest,
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
    mempool_metrics: &MempoolMetrics,
    mempool_flags: &ValidationFlags,
    chain_params: &ChainParams,
    data_dir: &Path,
    net_totals: &NetTotals,
    peer_registry: &PeerRegistry,
    header_peer_book: &HeaderPeerBook,
    tx_announce: &broadcast::Sender<Hash256>,
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
        mempool,
        mempool_metrics,
        mempool_flags,
        chain_params,
        data_dir,
        net_totals,
        peer_registry,
        header_peer_book,
        tx_announce,
    )
}

fn handle_rpc_request<S: fluxd_storage::KeyValueStore>(
    body: &[u8],
    chainstate: &ChainState<S>,
    mempool: &Mutex<Mempool>,
    mempool_metrics: &MempoolMetrics,
    mempool_flags: &ValidationFlags,
    chain_params: &ChainParams,
    data_dir: &Path,
    net_totals: &NetTotals,
    peer_registry: &PeerRegistry,
    header_peer_book: &HeaderPeerBook,
    tx_announce: &broadcast::Sender<Hash256>,
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
        mempool,
        mempool_metrics,
        mempool_flags,
        chain_params,
        data_dir,
        net_totals,
        peer_registry,
        header_peer_book,
        tx_announce,
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
    mempool: &Mutex<Mempool>,
    mempool_metrics: &MempoolMetrics,
    mempool_flags: &ValidationFlags,
    chain_params: &ChainParams,
    data_dir: &Path,
    net_totals: &NetTotals,
    peer_registry: &PeerRegistry,
    header_peer_book: &HeaderPeerBook,
    tx_announce: &broadcast::Sender<Hash256>,
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
        ),
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
        "getrawtransaction" => rpc_getrawtransaction(chainstate, mempool, params, chain_params),
        "sendrawtransaction" => rpc_sendrawtransaction(
            chainstate,
            mempool,
            mempool_metrics,
            mempool_flags,
            params,
            chain_params,
            tx_announce,
        ),
        "getmempoolinfo" => rpc_getmempoolinfo(params, mempool),
        "getrawmempool" => rpc_getrawmempool(params, mempool),
        "gettxout" => rpc_gettxout(chainstate, mempool, params, chain_params),
        "gettxoutproof" => rpc_gettxoutproof(params),
        "verifytxoutproof" => rpc_verifytxoutproof(params),
        "gettxoutsetinfo" => rpc_gettxoutsetinfo(chainstate, params, data_dir),
        "getblockdeltas" => rpc_getblockdeltas(params),
        "getspentinfo" => rpc_getspentinfo(chainstate, params),
        "getaddressutxos" => rpc_getaddressutxos(chainstate, params, chain_params),
        "getaddressbalance" => rpc_getaddressbalance(chainstate, params, chain_params),
        "getaddressdeltas" => rpc_getaddressdeltas(chainstate, params, chain_params),
        "getaddresstxids" => rpc_getaddresstxids(chainstate, params, chain_params),
        "getaddressmempool" => rpc_getaddressmempool(chainstate, mempool, params, chain_params),
        "getblocktemplate" => rpc_getblocktemplate(params),
        "getnetworkhashps" => rpc_getnetworkhashps(params),
        "getnetworksolps" => rpc_getnetworksolps(params),
        "getlocalsolps" => rpc_getlocalsolps(params),
        "getconnectioncount" => rpc_getconnectioncount(params, peer_registry, net_totals),
        "getnettotals" => rpc_getnettotals(params, net_totals),
        "getnetworkinfo" => rpc_getnetworkinfo(params, peer_registry, net_totals),
        "getpeerinfo" => rpc_getpeerinfo(params, peer_registry),
        "listbanned" => rpc_listbanned(params, header_peer_book),
        "getfluxnodecount" => rpc_getfluxnodecount(chainstate, params),
        "listfluxnodes" => rpc_viewdeterministicfluxnodelist(chainstate, params),
        "viewdeterministicfluxnodelist" => rpc_viewdeterministicfluxnodelist(chainstate, params),
        "fluxnodecurrentwinner" => rpc_fluxnodecurrentwinner(chainstate, params),
        "getfluxnodestatus" => rpc_getfluxnodestatus(params),
        "getdoslist" => rpc_getdoslist(params),
        _ => Err(RpcError::new(RPC_METHOD_NOT_FOUND, "method not found")),
    }
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
        "relayfee": 0.0,
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
    let size_on_disk = dir_size(data_dir).unwrap_or(0);
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
    mempool_metrics: &MempoolMetrics,
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

        {
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
        }

        let entry = build_mempool_entry(chainstate, chain_params, mempool_flags, tx, raw).map_err(
            |err| match err.kind {
                MempoolErrorKind::MissingInput => {
                    RpcError::new(RPC_TRANSACTION_ERROR, "Missing inputs")
                }
                MempoolErrorKind::ConflictingInput => {
                    RpcError::new(RPC_TRANSACTION_REJECTED, err.message)
                }
                MempoolErrorKind::MempoolFull => {
                    RpcError::new(RPC_TRANSACTION_REJECTED, err.message)
                }
                MempoolErrorKind::InvalidTransaction
                | MempoolErrorKind::InvalidScript
                | MempoolErrorKind::InvalidShielded => {
                    RpcError::new(RPC_TRANSACTION_REJECTED, err.message)
                }
                MempoolErrorKind::AlreadyInMempool => {
                    RpcError::new(RPC_INTERNAL_ERROR, "unexpected mempool duplicate")
                }
                MempoolErrorKind::Internal => RpcError::new(RPC_INTERNAL_ERROR, err.message),
            },
        )?;

        let txid = entry.txid;
        let mut guard = mempool
            .lock()
            .map_err(|_| RpcError::new(RPC_INTERNAL_ERROR, "mempool lock poisoned"))?;
        match guard.insert(entry) {
            Ok(outcome) => {
                if outcome.evicted > 0 {
                    mempool_metrics.note_evicted(outcome.evicted, outcome.evicted_bytes);
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
    }
    let entry = match chainstate.utxo_entry(&outpoint).map_err(map_internal)? {
        Some(entry) => entry,
        None => return Ok(Value::Null),
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
        "coinbase": entry.is_coinbase,
    }))
}

fn rpc_gettxoutproof(params: Vec<Value>) -> Result<Value, RpcError> {
    let _ = params;
    Err(RpcError::new(
        RPC_INTERNAL_ERROR,
        "gettxoutproof is not implemented",
    ))
}

fn rpc_verifytxoutproof(params: Vec<Value>) -> Result<Value, RpcError> {
    let _ = params;
    Err(RpcError::new(
        RPC_INTERNAL_ERROR,
        "verifytxoutproof is not implemented",
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
    let stats = chainstate.utxo_stats_or_compute().map_err(map_internal)?;
    let value_pools = chainstate.value_pools_or_compute().map_err(map_internal)?;
    let shielded_total = value_pools
        .sprout
        .checked_add(value_pools.sapling)
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "shielded value pool overflow"))?;
    let total_supply = stats
        .total_amount
        .checked_add(shielded_total)
        .ok_or_else(|| RpcError::new(RPC_INTERNAL_ERROR, "total supply overflow"))?;
    let disk_size = dir_size(&data_dir.join("db")).unwrap_or(0);
    Ok(json!({
        "height": height.max(0),
        "bestblock": hash256_to_hex(&best_hash),
        "transactions": 0,
        "txouts": stats.txouts,
        "bogosize": 0,
        "hash_serialized_2": "0000000000000000000000000000000000000000000000000000000000000000",
        "disk_size": disk_size,
        "total_amount": amount_to_value(stats.total_amount),
        "total_amount_zat": stats.total_amount,
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

fn rpc_getblockdeltas(params: Vec<Value>) -> Result<Value, RpcError> {
    let _ = params;
    Err(RpcError::new(
        RPC_INTERNAL_ERROR,
        "getblockdeltas is not implemented",
    ))
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

fn rpc_getblocktemplate(params: Vec<Value>) -> Result<Value, RpcError> {
    let _ = params;
    Err(RpcError::new(
        RPC_INTERNAL_ERROR,
        "getblocktemplate is not implemented",
    ))
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

fn rpc_getfluxnodestatus(params: Vec<Value>) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    Err(RpcError::new(
        RPC_INTERNAL_ERROR,
        "getfluxnodestatus is not implemented",
    ))
}

fn rpc_getdoslist(params: Vec<Value>) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
    Err(RpcError::new(
        RPC_INTERNAL_ERROR,
        "getdoslist is not implemented",
    ))
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

fn rpc_getnetworkinfo(
    params: Vec<Value>,
    peer_registry: &PeerRegistry,
    net_totals: &NetTotals,
) -> Result<Value, RpcError> {
    ensure_no_params(&params)?;
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
        "localservices": "0000000000000000",
        "timeoffset": 0,
        "connections": connections,
        "networks": networks,
        "relayfee": 0.0,
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
        out.push(json!({
            "addr": peer.addr.to_string(),
            "subver": peer.user_agent,
            "version": peer.version,
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

fn dir_size(path: &Path) -> Result<u64, String> {
    let mut total = 0u64;
    let entries = fs::read_dir(path).map_err(|err| err.to_string())?;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let metadata = entry.metadata().map_err(|err| err.to_string())?;
        if metadata.is_dir() {
            total = total.saturating_add(dir_size(&entry.path())?);
        } else {
            total = total.saturating_add(metadata.len());
        }
    }
    Ok(total)
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
