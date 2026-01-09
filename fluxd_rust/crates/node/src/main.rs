use fluxd_log as logging;

macro_rules! log_at {
    ($level:expr, $($arg:tt)*) => {{
        if crate::logging::enabled($level) {
            crate::logging::log($level, module_path!(), file!(), line!(), format_args!($($arg)*));
        }
    }};
}

macro_rules! log_error {
    ($($arg:tt)*) => {{
        log_at!(crate::logging::Level::Error, $($arg)*);
    }};
}

macro_rules! log_warn {
    ($($arg:tt)*) => {{
        log_at!(crate::logging::Level::Warn, $($arg)*);
    }};
}

macro_rules! log_info {
    ($($arg:tt)*) => {{
        log_at!(crate::logging::Level::Info, $($arg)*);
    }};
}

macro_rules! log_debug {
    ($($arg:tt)*) => {{
        log_at!(crate::logging::Level::Debug, $($arg)*);
    }};
}

#[allow(unused_macros)]
macro_rules! log_trace {
    ($($arg:tt)*) => {{
        log_at!(crate::logging::Level::Trace, $($arg)*);
    }};
}

mod dashboard;
mod db_info;
mod fee_estimator;
mod mempool;
mod p2p;
mod p2p_server;
mod peer_book;
mod rpc;
mod stats;
mod tx_relay;
mod verify_chain;
mod wallet;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, unbounded};
use fluxd_chainstate::flatfiles::{FileLocation, FlatFileStore};
use fluxd_chainstate::index::HeaderEntry;
use fluxd_chainstate::metrics::ConnectMetrics;
use fluxd_chainstate::state::{ChainState, HeaderValidationCache};
use fluxd_chainstate::validation::{
    validate_block_with_txids_and_size, ValidationFlags, ValidationMetrics,
};
use fluxd_consensus::money::{money_range, COIN, MAX_MONEY};
use fluxd_consensus::params::{chain_params, hash256_from_hex, ChainParams, Network};
use fluxd_consensus::upgrades::{current_epoch_branch_id, network_upgrade_active, UpgradeIndex};
use fluxd_consensus::Hash256;
use fluxd_consensus::{
    block_subsidy, exchange_fund_amount, foundation_fund_amount, swap_pool_amount,
};
use fluxd_fluxnode::storage::FluxnodeRecord;
use fluxd_pow::validation as pow_validation;
use fluxd_primitives::block::{Block, BlockHeader, CURRENT_VERSION};
use fluxd_primitives::encoding::{Decoder, Encoder};
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::transaction::{Transaction, TxIn, TxOut};
use fluxd_primitives::{address_to_script_pubkey, AddressError};
use fluxd_shielded::{
    default_params_dir, fetch_params, load_params, verify_transaction, ShieldedParams,
};
use fluxd_storage::fjall::{FjallOptions, FjallStore};
use fluxd_storage::memory::MemoryStore;
use fluxd_storage::{KeyValueStore, StoreError, WriteBatch};
use fs2::FileExt;
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinSet;

use crate::p2p::{
    parse_addr, parse_headers, parse_inv, parse_reject, NetTotals, Peer, PeerKind, PeerRegistry,
};
use crate::peer_book::HeaderPeerBook;
use crate::stats::{hash256_to_hex, snapshot_stats, HeaderMetrics, SyncMetrics};

const DEFAULT_DATA_DIR: &str = "data";
const DEFAULT_MAX_FLATFILE_SIZE: u64 = 128 * 1024 * 1024;
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 5;
const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 8;
const DEFAULT_GETDATA_BATCH: usize = 128;
const DEFAULT_BLOCK_PEERS: usize = 3;
const DEFAULT_HEADER_PEERS: usize = 4;
const DEFAULT_HEADER_LEAD: i32 = 20000;
const DEFAULT_INFLIGHT_PER_PEER: usize = 1;
const DEFAULT_TX_PEERS: usize = 2;
const DEFAULT_MAX_CONNECTIONS: usize = 125;
const DEFAULT_MEMPOOL_MAX_MB: u64 = 300;
const DEFAULT_MEMPOOL_PERSIST_INTERVAL_SECS: u64 = 60;
const DEFAULT_TX_CONFIRM_TARGET: u32 = 2;
const DEFAULT_UTXO_CACHE_ENTRIES: usize = 200_000;
const DEFAULT_DB_CACHE_MB: u64 = 256;
const DEFAULT_DB_WRITE_BUFFER_MB: u64 = 2048;
const DEFAULT_DB_JOURNAL_MB: u64 = 2048;
const DEFAULT_DB_MEMTABLE_MB: u64 = 64;
const DEFAULT_DB_FLUSH_WORKERS: usize = 2;
const DEFAULT_DB_COMPACTION_WORKERS: usize = 4;
const READ_TIMEOUT_SECS: u64 = 120;
const READ_TIMEOUT_RETRIES: usize = 3;
const BLOCK_READ_TIMEOUT_SECS: u64 = 30;
const BLOCK_READ_TIMEOUT_RETRIES: usize = 2;
const BLOCK_IDLE_SECS: u64 = 45;
const CONNECT_PIPELINE_IDLE_SECS: u64 = 120;
const HEADERS_TIMEOUT_SECS_PROBE: u64 = 12;
const HEADERS_TIMEOUT_SECS_BEHIND: u64 = 20;
const HEADERS_TIMEOUT_SECS_IDLE: u64 = 8;
const IDLE_SLEEP_SECS: u64 = 2;
const HEADER_TIMEOUT_RETRIES_BEHIND: usize = 2;
const HEADER_TIMEOUT_RETRIES_IDLE: usize = 3;
const HEADER_STALL_SECS_IDLE: u64 = 90;
const HEADER_IDLE_REPROBE_SECS: u64 = 120;
const BLOCK_STALL_SECS: u64 = 90;
const BLOCK_PEER_REFILL_SECS: u64 = 30;
const BLOCK_PEER_BAN_SECS_NOTFOUND: u64 = 300;
const BLOCK_PEER_BAN_SECS_TIMEOUT: u64 = 120;
const BLOCK_PEER_BAN_SECS_PROTOCOL: u64 = 900;
const HEADER_PEER_PROBE_COUNT: usize = 40;
const HEADER_BATCH_QUEUE: usize = 32;
const HEADER_LOCATOR_MAX_WALK: usize = 1024;
const HEADER_BAD_CHAIN_BAN_SECS: u64 = 900;
const HEADER_BEHIND_BAN_SECS: u64 = 300;
const HEADER_BEHIND_BAN_THRESHOLD: i32 = 1000;
const TX_ANNOUNCE_QUEUE: usize = 4096;
const ADDR_BOOK_MAX: usize = 5000;
const ADDR_BOOK_SAMPLE: usize = 128;
const ADDR_DISCOVERY_SAMPLE: usize = 64;
const ADDR_DISCOVERY_PEERS: usize = 4;
const ADDR_DISCOVERY_INTERVAL_SECS: u64 = 120;
const ADDR_DISCOVERY_TIMEOUT_SECS: u64 = 6;
const PEERS_FILE_NAME: &str = "peers.dat";
const BANLIST_FILE_NAME: &str = "banlist.dat";
const MEMPOOL_FILE_NAME: &str = "mempool.dat";
const FEE_ESTIMATES_FILE_NAME: &str = "fee_estimates.dat";
const REINDEX_REQUEST_FILE_NAME: &str = "reindex.flag";
const DATA_DIR_LOCK_FILE_NAME: &str = ".lock";
pub(crate) const DB_SCHEMA_VERSION_KEY: &[u8] = b"db_schema_version";
pub(crate) const DB_SCHEMA_VERSION: u32 = 1;
pub(crate) const TXINDEX_VERSION_KEY: &[u8] = b"txindex_version";
pub(crate) const TXINDEX_VERSION: u32 = 1;
pub(crate) const SPENTINDEX_VERSION_KEY: &[u8] = b"spentindex_version";
pub(crate) const SPENTINDEX_VERSION: u32 = 1;
pub(crate) const ADDRESSINDEX_VERSION_KEY: &[u8] = b"addressindex_version";
pub(crate) const ADDRESSINDEX_VERSION: u32 = 1;
const PEERS_FILE_VERSION: u32 = 2;
const PEERS_FILE_VERSION_V1: u32 = 1;
const MEMPOOL_FILE_VERSION: u32 = 1;
const PEERS_PERSIST_INTERVAL_SECS: u64 = 60;
const BANLIST_PERSIST_INTERVAL_SECS: u64 = 60;
const DEFAULT_FEE_ESTIMATES_PERSIST_INTERVAL_SECS: u64 = 300;
const GENESIS_TIMESTAMP: &str =
    "Zelcash06f40b01ab1f135bd96c5d72f8e37c7906dc216dcaaa36fcd00ebf9b8e109567";
const GENESIS_PUBKEY_HEX: &str =
    "04678afdb0fe5548271967f1a67130b7105cd6a828e03909a67962e0ea1f61deb649f6bc3f4cef38c4f35504e51ec112de5c384df7ba0b8d578a4c702b6bf11d5f";
const GENESIS_MAINNET_NONCE_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000001a8c";
const GENESIS_MAINNET_SOLUTION_HEX: &str = "002794c207f5942df0da515d0f29303a67b87eef343c2df654e3e00a052915289ef3f7842e6da933b2da247cebdee4ea3aabf3bdc33f02c5082633e3bdefc1a9316df787ecaf95a2337c6648e557a73a06fc8dee01479b1b09e350f9c9e2b61bea3736febb24f9f8692552d1a23863f6af2e38926df57e442dbbb69a3719104a70ad2415066ee46355a92a4b980d729e189c1311f4dc99a8cc191f2ae5634f34bfa97a291396d6f001244b9986c92c692986453ea26763767cafbfaeb372aafed3cb5cf5c1ab3f57c4445c85ef68921d568722206b19c1e797d7ce5ba3de50246456ae03fa150b23895e750273ca81cf0754ff4d38546e243bd182f210ae50f627d671b8e46775ed405cb5f2cfa49d5bbc1ed98604c78a5a4b752b72b780434641fbca11cf89183a04a21cc779079ad6f36bae57ca21519672a89e2e335dbc8ce89e85859959f5f4d1bb734abe3aecaa005b0b01020a869d631b01abb168d1b248dfbe3b6d1ad2ffb1fbdc8044e65bf579c3d948c21480dcf3800508ead900065afedae7c072fe5ea5c0a16c7ae78d36ddee0f40b5a6c1c365f66ba0c631ee99e8b9bee301f042f77cd92d6ae3f8937e1a41a38d864fb790121ecaf2d368967a34ca9183f5e7ae193dfb11f11a7931074aefcadac01dcb50b6978dd2cac69df89a656a399bcaade7cfb184b9ca884df3d63a3b8bca1c706602eb8dd2d1432fc79ee7425e35fd8f709d55ef1bab2f2bbe516711cf031ab6f4eee543a67193c81ef2b226d8e6d0d3a222d31811a326954a0a464a2a59ed9751d6f2dcd15da8ffe35fb5b441736c49dd5d75902a067f4c789ecc6e64671da0b67e88cec07f696b1c9828f3859266ca836a76eef5169c351cf1d32d33c918092eeed5f044970171504303629aefc51e63b6d7972b27e7b659e2d7c79f1ff5a6506833e315055f80ed00b42986db8cac0ea48e92ad8d5e3bc555b077f3381bfb53bfa7356195b67baa12cb7f0b0759285f8c9419d98ed33da746c9f6b2d50e0b74ea6311819bc2791bbe3e52ec536b78b80741ec41ae259273b7d3c4050f0bef51330e2ea793210559037ed3a98687ac3c13336f49cdc4a5ea77a40214eb4febbba9fb5e71410715cdb1aa238647a5315d91e97d4bfbc722f69b17332629f7f514cb79369c6132d8aff821e2cad7fd02b002b77eba3fc90f4cf91dd5ef7478acc6f0121966d7139abb672c14313ce69032c897e829417ba8f4c01b0f197144988995fbfb3b63231657798190e57f5a8a0f8643134752c9daf50fd4ab073288817fb1ede7de14007927e61c277b75e2d47294e8e8ae952b9f7a6a3471f4ba859c93852ba3d3e6cb47384d2d613e35641a1ff4d2b916ca8badb0c1c8d8f4629676e23953693b8e9b661b534e2cd34ea832b075c1f21333d1ceda02be8598ac435924d2d2b0d1fd9972f5386d92713e45e00cdc5d321817c7f9d4d966cc1eb5994f7d555107aadbbeb4d2dd24c5965b022dde997f3c5a7f17601b25623dd80836c67c7422e1b2c7a71553fc1d12df0742d986ab085298956c80f75035515922193f5d521db8ae57c05e5b1b801f93e25f5fffad38bb10fd781ab04a6b0a29547db513f3066b55459e061df5279831d9aeaa3b138a03f2a003c92c544e8972969476820419d312028e7c55cfa173fc0bfe414d3cc6ef85dd48c292595920fda320066be5d4eb69e327e37a14d408dddd3d06117abdbcbc36804a3c1fea73a5d4d3dc5c701ed7cf6716179eb94687ec6c73ca0d2c5190a10581566d9d9111152740c46955629a6974de7d0beb05efc3ab91e4fe735081b118cdef4510486e0c370f06ebf6158163d6e1b61280d8f4658618c9e4b9757636c6cc6761a1088f71b57392e9f85e89027a779f6c";
const GENESIS_MAINNET_BITS: u32 = 0x1f07ffff;
const GENESIS_TESTNET_NONCE_HEX: &str =
    "000000000000000000000000000000000000000000000000000000000000021c";
const GENESIS_TESTNET_SOLUTION_HEX: &str = "00069ae382cf568d3f3ba00d15f9d09c8977cd37d90d2c4d612e053e4cdfd4d226db220e51495bfb00d1019180b34c25091fbee2a08e4f08c974a4356760690b00d8da0c8baa3b6902130202e60391a16a5fa1ea08d6d0b63a60ec91dd0790cb432261483fe7fbe9d80d6a07af5599cc6d0b717780184fc0523e5ada8c07134262b9676c0269709501d4403c621cb9a15f55602b400e3fc093034f84ec583f25e6f16a111372ab4f031d6d12270259d7066520f71e63893e8dcedd8db2255272add167e4cd4a0045a6815a16818f9efb075106090b8e47d089dd7d50c838ea4b22caca1fdb866e485f0248c763faada47f8555b8cdb1222e45f2f0a10e3f3dffcb9733090bf2e58eb8f11399ffd7fc58302db98d5d978dac49b88f849ad4af4972b37d3cf2ca1797c28af99b3addc356c460ba6eb161d3304eeef863237c61006b486df070bb29026895172ae79bd9f3018637fe01dbc18d3829a2ed211a7218fcd8f4def308c3a8bd60cb999565f253435f1af115d1d473750c0233fda7aaeff783b8265083a9c0369852a459fe2c093dec6929cfc31cea57a3579fed30add42cfc02260700796ea2d3054dc04020a50c5a8079863b15f68c0e0354bb0aa8f1dd48fec84923d02f1a300d3ec598071a9f0584d918e36ec892450abdfe7d41acfe870624de69f988db06f7790897459c7d9899f290b60f2102a1df05bca84a7f54e746d274625f3322ae5a3eaa02b4565a125c948cf8682396e72494995793bf9379196014fec46c4c3769861808ed6b3fd2b4e57cadb92a7c81fc7fd630c4bae2549aa6efdc02df0f5e1ff20a834d35372301334b214229e872f8ac9415d57d1a325ac539a4a62eb1c685c6478867cf3ea0f999768a0d66fc9e36a35a2f1f768481613da1a99a17739fab0dd2ffe73f58f95ef1a6c2495167b485207dabe48001a200b4a371cf1df817f0b1fb6208d0d77b38d5cf170d83a6b7633a4fb605f44665a314ab5de8dc0ace1091611148705b3fe81e945857291daeb3657c98602cda23a350ed209ba19b6312fdefe765f3de7a16031580eba06145f64bfdf284dca4713335e9735031c71cf36da5f1145c8ed6e69352a7d763be253bd5fc7e1e45660e8d4f2beb98377268bc6303f5de2dd6ce1a35652b7090a8be51d43ef5c779376de1cbabf4758b064259d545524781801e18d005dd2a1ea4d27e1eaf27578c6a5acdf4a27d226293ae7c49d645c07adc8b0dfb0c35e519a6d95e9bb3f4a8bbbc72551e5ab9191d552104620b954523376637077c2e32aa42dec58b07ad0d91e4b93651d74b220070d22171e1b116dda1428082cb54122c27276f283260e1249ed483f97d053a0ad3abc6a032d7b8a5672ad7ec2232010655136e6e1507cbd9fa4a27ec674724a8cf4bdae91e3e34080291899bbb53c209bb3936a9c2d9194de89179396803cdf6bcb5cf9bbaf95b56c613d161c21ff1defe0f056f27e953891fd310aa6760d6f5a5edef8011d8780de5c681261331aed69ee2eccd3bb413cdf55f2e500686b231cbaf451fecdc1327e14bde0973fb4cbd8835b7c31a1a1aaf47eecbb57849ad3eb960cdbd5cdb0e4c53a8d7f10459cbc572faab1ffbd8ca9d0919e0113099339910e897e21fd390f450c3b13b5d198d7306927f258a0297c50daa10a4f29aed6a184df29c0a32a666744bef2c358401350cfb54797a02a35afeba0bfefa865890dded2694d88ad86a5b327662bd7b932c6ce97a7f300bfd8b544316696a4f2e6c197ddf9d0e9b008e3f85427fd6b661970e4177c947fbb6d43324a4f47c26983b55ea90d3bffcbc87c15cab5ba2751314819e7eb21a29b9b915cce6f7cf01ff05936317161b9dae29637d89cb88b55ac74348878b017e7942";
const GENESIS_TESTNET_BITS: u32 = 0x2007ffff;
const GENESIS_REGTEST_NONCE_HEX: &str =
    "0000000000000000000000000000000000000000000000000000000000000016";
const GENESIS_REGTEST_SOLUTION_HEX: &str =
    "02853a9dd062e2356909a0d2b9f0e4873dbf092edd3f00eea317e21222d1f2c414b926ee";
const GENESIS_REGTEST_BITS: u32 = 0x200f0f0f;

fn log_block_requests() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("FLUXD_LOG_BLOCK_REQUESTS").is_some())
}

fn maybe_log_block_request(count: usize) {
    if log_block_requests() {
        log_debug!("Requesting {count} block(s)");
    }
}

#[derive(Clone, Copy, Debug)]
enum Backend {
    Memory,
    Fjall,
}

impl Backend {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "memory" => Some(Self::Memory),
            "fjall" => Some(Self::Fjall),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunProfile {
    Low,
    Default,
    High,
}

impl RunProfile {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "low" => Some(Self::Low),
            "default" => Some(Self::Default),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Default => "default",
            Self::High => "high",
        }
    }
}

struct Config {
    backend: Backend,
    data_dir: PathBuf,
    network: Network,
    params_dir: PathBuf,
    fetch_params: bool,
    reindex: bool,
    resync: bool,
    reindex_txindex: bool,
    reindex_spentindex: bool,
    reindex_addressindex: bool,
    db_info: bool,
    db_info_keys: bool,
    db_integrity: bool,
    miner_address: Option<String>,
    tx_confirm_target: u32,
    scan_flatfiles: bool,
    scan_supply: bool,
    scan_fluxnodes: bool,
    debug_fluxnode_payee_script: Option<Vec<u8>>,
    debug_fluxnode_payout_height: Option<i32>,
    debug_fluxnode_payee_candidates: Option<DebugFluxnodePayeeCandidates>,
    check_script: bool,
    log_level: logging::Level,
    log_format: logging::Format,
    log_timestamps: bool,
    p2p_listen: bool,
    p2p_addr: Option<SocketAddr>,
    rpc_addr: Option<SocketAddr>,
    rpc_user: Option<String>,
    rpc_pass: Option<String>,
    rpc_allow_ips: Vec<String>,
    getdata_batch: usize,
    block_peers: usize,
    header_peers: usize,
    header_lead: i32,
    header_peer_addrs: Vec<String>,
    addnode_addrs: Vec<SocketAddr>,
    max_connections: usize,
    tx_peers: usize,
    inflight_per_peer: usize,
    require_standard: bool,
    min_relay_fee_per_kb: i64,
    limit_free_relay_kb_per_minute: u64,
    mempool_max_bytes: usize,
    mempool_persist_interval_secs: u64,
    fee_estimates_persist_interval_secs: u64,
    status_interval_secs: u64,
    dashboard_addr: Option<SocketAddr>,
    db_cache_bytes: Option<u64>,
    db_write_buffer_bytes: Option<u64>,
    db_journal_bytes: Option<u64>,
    db_memtable_bytes: Option<u32>,
    db_flush_workers: Option<usize>,
    db_compaction_workers: Option<usize>,
    db_fsync_ms: Option<u16>,
    utxo_cache_entries: usize,
    header_verify_workers: usize,
    verify_workers: usize,
    verify_queue: usize,
    shielded_workers: usize,
}

enum CliAction {
    Run(Config),
    PrintHelp,
    PrintVersion,
}

#[derive(Clone, Copy, Debug)]
struct DebugFluxnodePayeeCandidates {
    tier: u8,
    height: i32,
    limit: usize,
}

#[derive(Clone)]
struct PeerContext {
    net_totals: Arc<NetTotals>,
    registry: Arc<PeerRegistry>,
    kind: PeerKind,
}

#[derive(Clone, Copy, Debug)]
struct VerifySettings {
    verify_workers: usize,
    verify_queue: usize,
    shielded_workers: usize,
}

struct HeaderDownloadState {
    tip_hash: Hash256,
    tip_height: i32,
    pending: HashMap<Hash256, HeaderEntry>,
    cache: HeaderValidationCache,
}

impl HeaderDownloadState {
    fn new<S: KeyValueStore>(
        chainstate: &ChainState<S>,
        params: &ChainParams,
    ) -> Result<Self, String> {
        let tip = chainstate.best_header().map_err(|err| err.to_string())?;
        let (tip_hash, tip_height) = if let Some(tip) = tip {
            (tip.hash, tip.height)
        } else {
            (params.consensus.hash_genesis_block, 0)
        };
        Ok(Self {
            tip_hash,
            tip_height,
            pending: HashMap::new(),
            cache: HeaderValidationCache::default(),
        })
    }

    fn reset<S: KeyValueStore>(
        &mut self,
        chainstate: &ChainState<S>,
        params: &ChainParams,
    ) -> Result<(), String> {
        let next = HeaderDownloadState::new(chainstate, params)?;
        self.tip_hash = next.tip_hash;
        self.tip_height = next.tip_height;
        self.pending.clear();
        self.cache = HeaderValidationCache::default();
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
struct AddrBookEntry {
    last_seen: u64,
    last_success: u64,
    last_failure: u64,
    last_attempt: u64,
    successes: u32,
    failures: u32,
    last_height: i32,
    last_version: i32,
}

#[derive(Default)]
struct AddrBook {
    entries: Mutex<HashMap<SocketAddr, AddrBookEntry>>,
    revision: AtomicU64,
}

impl AddrBook {
    fn revision(&self) -> u64 {
        self.revision.load(AtomicOrdering::Relaxed)
    }

    fn record_attempt(&self, addr: SocketAddr) {
        let now = unix_now_secs();
        if let Ok(mut book) = self.entries.lock() {
            let entry = book.entry(addr).or_default();
            entry.last_attempt = now;
        }
    }

    fn record_success(&self, addr: SocketAddr, peer: &Peer) {
        let now = unix_now_secs();
        if let Ok(mut book) = self.entries.lock() {
            let entry = book.entry(addr).or_default();
            entry.last_seen = now;
            entry.last_success = now;
            entry.successes = entry.successes.saturating_add(1);
            entry.failures = entry.failures.saturating_sub(1);
            entry.last_height = peer.remote_height();
            entry.last_version = peer.remote_version();
            self.revision.fetch_add(1, AtomicOrdering::Relaxed);
        }
    }

    fn record_failure(&self, addr: SocketAddr) {
        let now = unix_now_secs();
        if let Ok(mut book) = self.entries.lock() {
            let entry = book.entry(addr).or_default();
            entry.last_seen = now;
            entry.last_failure = now;
            entry.failures = entry.failures.saturating_add(1);
            self.revision.fetch_add(1, AtomicOrdering::Relaxed);
        }
    }

    fn insert_many(&self, addrs: Vec<SocketAddr>) -> usize {
        if addrs.is_empty() {
            return 0;
        }
        let now = unix_now_secs();
        let mut inserted = 0;
        if let Ok(mut book) = self.entries.lock() {
            for addr in addrs {
                if addr.port() == 0 {
                    continue;
                }
                if !book.contains_key(&addr) && book.len() >= ADDR_BOOK_MAX {
                    prune_addr_book(&mut book, now);
                    if book.len() >= ADDR_BOOK_MAX {
                        break;
                    }
                }
                let entry = book.entry(addr).or_default();
                if entry.last_seen == 0 {
                    inserted += 1;
                }
                entry.last_seen = now;
            }
        }
        if inserted > 0 {
            self.revision.fetch_add(1, AtomicOrdering::Relaxed);
        }
        inserted
    }

    fn load_entries(&self, entries: Vec<(SocketAddr, AddrBookEntry)>) -> usize {
        if entries.is_empty() {
            return 0;
        }
        let now = unix_now_secs();
        let mut inserted = 0;
        if let Ok(mut book) = self.entries.lock() {
            for (addr, mut entry) in entries {
                if addr.port() == 0 {
                    continue;
                }
                entry.last_attempt = 0;
                if !book.contains_key(&addr) && book.len() >= ADDR_BOOK_MAX {
                    prune_addr_book(&mut book, now);
                    if book.len() >= ADDR_BOOK_MAX {
                        break;
                    }
                }
                match book.get_mut(&addr) {
                    Some(existing) => merge_addr_entry(existing, &entry),
                    None => {
                        book.insert(addr, entry);
                        inserted += 1;
                    }
                }
            }
        }
        self.revision.fetch_add(1, AtomicOrdering::Relaxed);
        inserted
    }

    fn sample(&self, limit: usize) -> Vec<SocketAddr> {
        self.sample_for_height(limit, i32::MIN)
    }

    fn sample_for_height(&self, limit: usize, min_height: i32) -> Vec<SocketAddr> {
        if limit == 0 {
            return Vec::new();
        }
        let now = unix_now_secs();
        let book = match self.entries.lock() {
            Ok(book) => book,
            Err(_) => return Vec::new(),
        };
        let mut scored: Vec<(SocketAddr, i64, bool)> = Vec::with_capacity(book.len());
        for (addr, entry) in book.iter() {
            if !addr_is_eligible(addr, entry, now, min_height) {
                continue;
            }
            let score = addr_score(entry, now, min_height);
            let tried = entry.last_success > 0;
            scored.push((*addr, score, tried));
        }

        if scored.is_empty() {
            return Vec::new();
        }

        const BUCKET_KEEP: usize = 8;
        let mut tried_buckets: HashMap<u64, Vec<(SocketAddr, i64)>> = HashMap::new();
        let mut new_buckets: HashMap<u64, Vec<(SocketAddr, i64)>> = HashMap::new();
        for (addr, score, tried) in scored {
            let key = addr_bucket_key(&addr);
            if tried {
                tried_buckets.entry(key).or_default().push((addr, score));
            } else {
                new_buckets.entry(key).or_default().push((addr, score));
            }
        }

        let to_bucket_lists = |mut buckets: HashMap<u64, Vec<(SocketAddr, i64)>>| {
            let mut out = Vec::with_capacity(buckets.len());
            for entries in buckets.values_mut() {
                entries.sort_by(|a, b| a.1.cmp(&b.1));
                if entries.len() > BUCKET_KEEP {
                    let drop = entries.len() - BUCKET_KEEP;
                    entries.drain(0..drop);
                }
                out.push(entries.iter().map(|(addr, _)| *addr).collect::<Vec<_>>());
            }
            out
        };

        let mut tried_lists = to_bucket_lists(tried_buckets);
        let mut new_lists = to_bucket_lists(new_buckets);

        fn take_round_robin(
            buckets: &mut Vec<Vec<SocketAddr>>,
            count: usize,
            out: &mut Vec<SocketAddr>,
        ) {
            if count == 0 {
                return;
            }
            buckets.shuffle(&mut rand::thread_rng());
            let target_len = out.len().saturating_add(count);
            let mut idx = 0usize;
            while out.len() < target_len && !buckets.is_empty() {
                if idx >= buckets.len() {
                    idx = 0;
                }
                if let Some(addr) = buckets[idx].pop() {
                    out.push(addr);
                    idx = idx.saturating_add(1);
                } else {
                    buckets.swap_remove(idx);
                }
            }
        }

        let mut addrs = Vec::with_capacity(limit);
        let tried_target = limit.saturating_mul(3).saturating_div(4);
        take_round_robin(&mut tried_lists, tried_target, &mut addrs);
        let remaining = limit.saturating_sub(addrs.len());
        take_round_robin(&mut new_lists, remaining, &mut addrs);
        let remaining = limit.saturating_sub(addrs.len());
        take_round_robin(&mut tried_lists, remaining, &mut addrs);
        addrs.truncate(limit);
        addrs
    }

    fn len(&self) -> usize {
        match self.entries.lock() {
            Ok(book) => book.len(),
            Err(_) => 0,
        }
    }

    fn snapshot(&self) -> Vec<(SocketAddr, AddrBookEntry)> {
        let now = unix_now_secs();
        match self.entries.lock() {
            Ok(mut book) => {
                prune_addr_book(&mut book, now);
                book.iter().map(|(addr, entry)| (*addr, *entry)).collect()
            }
            Err(_) => Vec::new(),
        }
    }
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn addr_is_eligible(addr: &SocketAddr, entry: &AddrBookEntry, now: u64, min_height: i32) -> bool {
    if addr.port() == 0 {
        return false;
    }

    if entry.last_attempt > 0 && now.saturating_sub(entry.last_attempt) < 5 {
        return false;
    }

    if min_height > 0 && entry.last_height > 0 && entry.last_height + 100 < min_height {
        return false;
    }

    if entry.last_failure > entry.last_success && entry.last_failure > 0 {
        let cooldown = addr_failure_cooldown_secs(entry.failures);
        if now < entry.last_failure.saturating_add(cooldown) {
            return false;
        }
    }

    true
}

fn addr_failure_cooldown_secs(failures: u32) -> u64 {
    let failures = failures.min(10);
    let base = 5u64;
    base.saturating_mul(2u64.saturating_pow(failures)).min(3600)
}

fn addr_score(entry: &AddrBookEntry, now: u64, min_height: i32) -> i64 {
    let mut score: i64 = 0;
    if entry.last_success > 0 {
        let age = now.saturating_sub(entry.last_success);
        if age < 3600 {
            score += 2000;
        } else if age < 86_400 {
            score += 800;
        } else if age < 604_800 {
            score += 200;
        }
    }

    score += i64::from(entry.successes).saturating_mul(15);
    score -= i64::from(entry.failures).saturating_mul(25);

    if entry.last_height > 0 {
        score += i64::from(entry.last_height / 1000).min(2500);
    }

    if min_height > 0 && entry.last_height > 0 {
        let delta = entry.last_height.saturating_sub(min_height);
        if delta >= -10 {
            score += 300;
        } else if delta >= -100 {
            score += 100;
        } else if delta < -1000 {
            score -= 1000;
        }
    }

    if entry.last_failure > entry.last_success && entry.last_failure > 0 {
        let fail_age = now.saturating_sub(entry.last_failure);
        if fail_age < 600 {
            score -= 500;
        }
    }

    score
}

fn addr_bucket_key(addr: &SocketAddr) -> u64 {
    match addr.ip() {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            let group = u64::from(octets[0]) << 8 | u64::from(octets[1]);
            (1u64 << 63) | group
        }
        IpAddr::V6(ip) => {
            let octets = ip.octets();
            let group = u64::from(u32::from_be_bytes([
                octets[0], octets[1], octets[2], octets[3],
            ]));
            (2u64 << 62) | group
        }
    }
}

fn merge_addr_entry(existing: &mut AddrBookEntry, incoming: &AddrBookEntry) {
    existing.last_seen = existing.last_seen.max(incoming.last_seen);
    existing.last_success = existing.last_success.max(incoming.last_success);
    existing.last_failure = existing.last_failure.max(incoming.last_failure);
    existing.successes = existing.successes.max(incoming.successes);
    existing.failures = existing.failures.max(incoming.failures);
    if incoming.last_height > existing.last_height {
        existing.last_height = incoming.last_height;
        existing.last_version = incoming.last_version;
    } else if incoming.last_height == existing.last_height
        && incoming.last_version > existing.last_version
    {
        existing.last_version = incoming.last_version;
    }
}

fn prune_addr_book(book: &mut HashMap<SocketAddr, AddrBookEntry>, now: u64) {
    let stale_cutoff = now.saturating_sub(14 * 86_400);
    book.retain(|_addr, entry| {
        let last_activity = entry
            .last_success
            .max(entry.last_failure)
            .max(entry.last_seen);
        if last_activity == 0 {
            return true;
        }
        if entry.successes == 0 && last_activity < stale_cutoff {
            return false;
        }
        if entry.failures >= 8
            && entry.last_success == 0
            && last_activity < now.saturating_sub(86_400)
        {
            return false;
        }
        true
    });

    if book.len() <= ADDR_BOOK_MAX {
        return;
    }

    let mut scored: Vec<(SocketAddr, i64)> = book
        .iter()
        .map(|(addr, entry)| (*addr, addr_score(entry, now, i32::MIN)))
        .collect();
    scored.sort_by(|a, b| a.1.cmp(&b.1));
    let excess = book.len().saturating_sub(ADDR_BOOK_MAX);
    for (addr, _) in scored.into_iter().take(excess) {
        book.remove(&addr);
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct PeersFileV1 {
    version: u32,
    addrs: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct PeersFileV2 {
    version: u32,
    peers: Vec<PeersFileV2Entry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PeersFileV2Entry {
    addr: String,
    last_seen: u64,
    last_success: u64,
    last_failure: u64,
    successes: u32,
    failures: u32,
    last_height: i32,
    last_version: i32,
}

#[derive(Clone, Copy, Debug, Default)]
struct HeaderCursor {
    tip_hash: Option<fluxd_consensus::Hash256>,
    tip_height: Option<i32>,
    generation: u64,
}

struct VerifyJob {
    hash: fluxd_consensus::Hash256,
    height: i32,
    block: Arc<Block>,
    bytes: Arc<Vec<u8>>,
}

struct ReceivedBlock {
    block: Block,
    bytes: Vec<u8>,
}

struct VerifyResult {
    hash: fluxd_consensus::Hash256,
    height: i32,
    block: Arc<Block>,
    bytes: Arc<Vec<u8>>,
    txids: Vec<fluxd_consensus::Hash256>,
    needs_shielded: bool,
    error: Option<String>,
}

struct ShieldedJob {
    hash: fluxd_consensus::Hash256,
    height: i32,
    block: Arc<Block>,
}

struct ShieldedResult {
    hash: fluxd_consensus::Hash256,
    error: Option<String>,
}

struct VerifiedBlock {
    height: i32,
    block: Arc<Block>,
    bytes: Arc<Vec<u8>>,
    txids: Vec<fluxd_consensus::Hash256>,
}

enum PipelineEvent {
    Verify(VerifyResult),
    Shielded(ShieldedResult),
}

pub(crate) enum Store {
    Memory(MemoryStore),
    Fjall(FjallStore),
}

impl Store {
    pub fn fjall_telemetry_snapshot(&self) -> Option<fluxd_storage::fjall::FjallTelemetrySnapshot> {
        match self {
            Store::Fjall(store) => Some(store.telemetry_snapshot()),
            Store::Memory(_) => None,
        }
    }
}

impl KeyValueStore for Store {
    fn get(
        &self,
        column: fluxd_storage::Column,
        key: &[u8],
    ) -> Result<Option<Vec<u8>>, StoreError> {
        match self {
            Store::Memory(store) => store.get(column, key),
            Store::Fjall(store) => store.get(column, key),
        }
    }

    fn put(
        &self,
        column: fluxd_storage::Column,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), StoreError> {
        match self {
            Store::Memory(store) => store.put(column, key, value),
            Store::Fjall(store) => store.put(column, key, value),
        }
    }

    fn delete(&self, column: fluxd_storage::Column, key: &[u8]) -> Result<(), StoreError> {
        match self {
            Store::Memory(store) => store.delete(column, key),
            Store::Fjall(store) => store.delete(column, key),
        }
    }

    fn scan_prefix(
        &self,
        column: fluxd_storage::Column,
        prefix: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, StoreError> {
        match self {
            Store::Memory(store) => store.scan_prefix(column, prefix),
            Store::Fjall(store) => store.scan_prefix(column, prefix),
        }
    }

    fn for_each_prefix<'a>(
        &self,
        column: fluxd_storage::Column,
        prefix: &[u8],
        visitor: &mut fluxd_storage::PrefixVisitor<'a>,
    ) -> Result<(), StoreError> {
        match self {
            Store::Memory(store) => store.for_each_prefix(column, prefix, visitor),
            Store::Fjall(store) => store.for_each_prefix(column, prefix, visitor),
        }
    }

    fn write_batch(&self, batch: &WriteBatch) -> Result<(), StoreError> {
        match self {
            Store::Memory(store) => store.write_batch(batch),
            Store::Fjall(store) => store.write_batch(batch),
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    if let Err(err) = run().await {
        log_error!("{err}");
        std::process::exit(1);
    }
}

struct DataDirLock {
    _file: File,
}

fn lock_data_dir(data_dir: &Path) -> Result<DataDirLock, String> {
    let lock_path = data_dir.join(DATA_DIR_LOCK_FILE_NAME);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)
        .map_err(|err| format!("failed to open lock file {}: {err}", lock_path.display()))?;

    match file.try_lock_exclusive() {
        Ok(()) => {
            let pid = std::process::id();
            let _ = file.set_len(0);
            let _ = file.seek(SeekFrom::Start(0));
            let _ = writeln!(file, "pid={pid}");
            let _ = file.flush();
            Ok(DataDirLock { _file: file })
        }
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
            let mut holder = String::new();
            let _ =
                File::open(&lock_path).and_then(|mut reader| reader.read_to_string(&mut holder));
            let holder = holder.trim();
            if holder.is_empty() {
                Err(format!(
                    "data dir {} is already locked (another fluxd instance may be running); lock file {}",
                    data_dir.display(),
                    lock_path.display()
                ))
            } else {
                Err(format!(
                    "data dir {} is already locked (another fluxd instance may be running); lock file {} ({holder})",
                    data_dir.display(),
                    lock_path.display()
                ))
            }
        }
        Err(err) => Err(format!(
            "failed to lock data dir {} (lock file {}): {err}",
            data_dir.display(),
            lock_path.display()
        )),
    }
}

async fn run() -> Result<(), String> {
    let cli = parse_args()?;
    match cli {
        CliAction::PrintHelp => {
            println!("{}", usage());
            return Ok(());
        }
        CliAction::PrintVersion => {
            println!("fluxd-rust {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        CliAction::Run(config) => {
            let start_time = Instant::now();
            return run_with_config(start_time, config).await;
        }
    }
}

async fn run_with_config(start_time: Instant, config: Config) -> Result<(), String> {
    logging::init(logging::LogConfig {
        level: config.log_level,
        format: config.log_format,
        timestamps: config.log_timestamps,
    });
    let params = Arc::new(chain_params(config.network));
    let network = config.network;
    let backend = config.backend;
    let status_interval_secs = config.status_interval_secs;
    let dashboard_addr = config.dashboard_addr;
    let getdata_batch = config.getdata_batch;
    let block_peers_target = config.block_peers;
    let header_peers_target = config.header_peers;
    let header_lead = config.header_lead;
    let header_verify_workers = resolve_header_verify_workers(&config);
    let inflight_per_peer = config.inflight_per_peer;
    let data_dir = &config.data_dir;
    let db_path = data_dir.join("db");
    let blocks_path = data_dir.join("blocks");
    let reindex_flag_path = data_dir.join(REINDEX_REQUEST_FILE_NAME);

    fs::create_dir_all(data_dir).map_err(|err| err.to_string())?;
    let _data_dir_lock = lock_data_dir(data_dir)?;

    let mut reindex_from_flatfiles = false;
    if config.resync {
        log_info!(
            "Resync requested; removing {} and {}",
            db_path.display(),
            blocks_path.display()
        );
        if let Err(err) = fs::remove_dir_all(&db_path) {
            if err.kind() != ErrorKind::NotFound {
                return Err(format!(
                    "failed to remove db dir {}: {err}",
                    db_path.display()
                ));
            }
        }
        if let Err(err) = fs::remove_dir_all(&blocks_path) {
            if err.kind() != ErrorKind::NotFound {
                return Err(format!(
                    "failed to remove blocks dir {}: {err}",
                    blocks_path.display()
                ));
            }
        }
        let _ = fs::remove_file(data_dir.join(MEMPOOL_FILE_NAME));
        let _ = fs::remove_file(data_dir.join(FEE_ESTIMATES_FILE_NAME));
        let _ = fs::remove_file(&reindex_flag_path);
    } else if config.reindex || reindex_flag_path.exists() {
        reindex_from_flatfiles = blocks_path.exists();
        log_info!(
            "Reindex requested; removing {} (preserving {})",
            db_path.display(),
            blocks_path.display()
        );
        if let Err(err) = fs::remove_dir_all(&db_path) {
            if err.kind() != ErrorKind::NotFound {
                return Err(format!(
                    "failed to remove db dir {}: {err}",
                    db_path.display()
                ));
            }
        }
        let _ = fs::remove_file(data_dir.join(MEMPOOL_FILE_NAME));
        let _ = fs::remove_file(data_dir.join(FEE_ESTIMATES_FILE_NAME));
        let _ = fs::remove_file(&reindex_flag_path);

        if reindex_from_flatfiles {
            remove_undo_flatfiles(&blocks_path)?;
        }
    }

    if matches!(config.backend, Backend::Fjall)
        && (config.reindex_txindex || config.reindex_spentindex || config.reindex_addressindex)
    {
        let partitions_dir = db_path.join("partitions");
        if config.reindex_txindex {
            let dir = partitions_dir.join(fluxd_storage::Column::TxIndex.as_str());
            log_info!("Selective reindex: removing {}", dir.display());
            if let Err(err) = fs::remove_dir_all(&dir) {
                if err.kind() != ErrorKind::NotFound {
                    return Err(format!("failed to remove {}: {err}", dir.display()));
                }
            }
        }
        if config.reindex_spentindex {
            let dir = partitions_dir.join(fluxd_storage::Column::SpentIndex.as_str());
            log_info!("Selective reindex: removing {}", dir.display());
            if let Err(err) = fs::remove_dir_all(&dir) {
                if err.kind() != ErrorKind::NotFound {
                    return Err(format!("failed to remove {}: {err}", dir.display()));
                }
            }
        }
        if config.reindex_addressindex {
            for column in [
                fluxd_storage::Column::AddressOutpoint,
                fluxd_storage::Column::AddressDelta,
            ] {
                let dir = partitions_dir.join(column.as_str());
                log_info!("Selective reindex: removing {}", dir.display());
                if let Err(err) = fs::remove_dir_all(&dir) {
                    if err.kind() != ErrorKind::NotFound {
                        return Err(format!("failed to remove {}: {err}", dir.display()));
                    }
                }
            }
        }
    } else if matches!(config.backend, Backend::Memory)
        && (config.reindex_txindex || config.reindex_spentindex || config.reindex_addressindex)
    {
        log_warn!("Selective reindex flags are only meaningful for --backend fjall; ignoring for memory backend");
    }

    let store = open_store(config.backend, &db_path, &config)?;
    let store = Arc::new(store);
    let _db_schema_version = ensure_db_schema_version(store.as_ref())?;
    ensure_secondary_index_versions(store.as_ref())?;

    let blocks = FlatFileStore::new(&blocks_path, DEFAULT_MAX_FLATFILE_SIZE)
        .map_err(|err| err.to_string())?;
    let undo = FlatFileStore::new_with_prefix(&blocks_path, "undo", DEFAULT_MAX_FLATFILE_SIZE)
        .map_err(|err| err.to_string())?;
    let chainstate = Arc::new(ChainState::new_with_utxo_cache_capacity(
        Arc::clone(&store),
        blocks,
        undo,
        config.utxo_cache_entries,
    ));

    if config.db_info {
        let info = db_info::collect_db_info(
            chainstate.as_ref(),
            store.as_ref(),
            data_dir,
            backend,
            true,
            config.db_info_keys,
        )?;
        let json = serde_json::to_string_pretty(&info).map_err(|err| err.to_string())?;
        println!("{json}");
        return Ok(());
    }
    let net_totals = Arc::new(NetTotals::default());
    let peer_registry = Arc::new(PeerRegistry::default());
    let header_peer_book = Arc::new(HeaderPeerBook::default());
    let addr_book = Arc::new(AddrBook::default());
    let added_nodes = Arc::new(Mutex::new(HashSet::<SocketAddr>::new()));
    let peers_path = data_dir.join(PEERS_FILE_NAME);
    let banlist_path = data_dir.join(BANLIST_FILE_NAME);
    let mempool_path = data_dir.join(MEMPOOL_FILE_NAME);

    match load_peers_file(&peers_path) {
        Ok(entries) => {
            let loaded = addr_book.load_entries(entries);
            if loaded > 0 {
                log_info!("Loaded {loaded} peers from {}", peers_path.display());
            }
        }
        Err(err) => log_warn!("failed to load peers file: {err}"),
    }

    if !config.addnode_addrs.is_empty() {
        if let Ok(mut guard) = added_nodes.lock() {
            guard.extend(config.addnode_addrs.iter().copied());
        }
        let inserted = addr_book.insert_many(config.addnode_addrs.clone());
        log_info!(
            "Loaded {} addnode(s) from flux.conf (new {})",
            config.addnode_addrs.len(),
            inserted
        );
    }
    match header_peer_book.load_banlist(&banlist_path) {
        Ok(loaded) => {
            if loaded > 0 {
                log_info!("Loaded {loaded} bans from {}", banlist_path.display());
            }
        }
        Err(err) => log_warn!("failed to load banlist: {err}"),
    }

    {
        let addr_book = Arc::clone(&addr_book);
        let peers_path = peers_path.clone();
        thread::spawn(move || persist_peers_loop(addr_book, peers_path));
    }
    {
        let header_peer_book = Arc::clone(&header_peer_book);
        let banlist_path = banlist_path.clone();
        thread::spawn(move || persist_banlist_loop(header_peer_book, banlist_path));
    }

    log_info!(
        "Initialized chainstate on {:?} ({:?})",
        config.network,
        config.backend
    );

    if config.db_integrity {
        const CHECKLEVEL: u32 = 5;
        const NUMBLOCKS: u32 = 288;

        let verify = crate::verify_chain::verify_chain(chainstate.as_ref(), CHECKLEVEL, NUMBLOCKS);
        let verify_json = match verify.as_ref() {
            Ok(()) => serde_json::json!({
                "ok": true,
                "checklevel": CHECKLEVEL,
                "numblocks": NUMBLOCKS,
            }),
            Err(err) => serde_json::json!({
                "ok": false,
                "checklevel": CHECKLEVEL,
                "numblocks": NUMBLOCKS,
                "error": err,
            }),
        };

        let mut info = db_info::collect_db_info(
            chainstate.as_ref(),
            store.as_ref(),
            data_dir,
            backend,
            false,
            false,
        )?;
        if let Some(obj) = info.as_object_mut() {
            obj.insert("verifychain".to_string(), verify_json);
        } else {
            info = serde_json::json!({
                "verifychain": verify_json,
                "db_info": info,
            });
        }
        let output = serde_json::to_string_pretty(&info).map_err(|err| err.to_string())?;
        println!("{output}");

        let flatfiles_ok = info
            .as_object()
            .and_then(|obj| obj.get("integrity"))
            .and_then(|value| value.get("ok"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if verify.is_err() || !flatfiles_ok {
            std::process::exit(1);
        }
        return Ok(());
    }

    if config.scan_flatfiles {
        scan_flatfiles(chainstate.as_ref(), &blocks_path)?;
        return Ok(());
    }

    if config.scan_supply {
        scan_supply(chainstate.as_ref(), params.as_ref())?;
        return Ok(());
    }

    if config.scan_fluxnodes {
        scan_fluxnodes(chainstate.as_ref())?;
        return Ok(());
    }

    if let Some(script) = config.debug_fluxnode_payee_script.as_deref() {
        debug_find_fluxnode_payee_script(chainstate.as_ref(), script)?;
        return Ok(());
    }

    if let Some(height) = config.debug_fluxnode_payout_height {
        debug_print_expected_fluxnode_payouts(chainstate.as_ref(), params.as_ref(), height)?;
        return Ok(());
    }

    if let Some(args) = config.debug_fluxnode_payee_candidates {
        debug_print_fluxnode_payee_candidates(
            chainstate.as_ref(),
            params.as_ref(),
            args.tier,
            args.height,
            args.limit,
        )?;
        return Ok(());
    }

    let rpc_addr = config.rpc_addr.unwrap_or_else(|| default_rpc_addr(network));
    let rpc_auth =
        rpc::load_or_create_auth(config.rpc_user.clone(), config.rpc_pass.clone(), data_dir)?;
    let rpc_allowlist =
        rpc::RpcAllowList::from_allow_ips(&config.rpc_allow_ips).map_err(|err| err.to_string())?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    {
        let shutdown_tx = shutdown_tx.clone();
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sigterm = match signal(SignalKind::terminate()) {
                    Ok(signal) => signal,
                    Err(err) => {
                        log_warn!("failed to install SIGTERM handler: {err}");
                        let _ = tokio::signal::ctrl_c().await;
                        let _ = shutdown_tx.send(true);
                        return;
                    }
                };
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = sigterm.recv() => {},
                }
            }
            #[cfg(not(unix))]
            {
                let _ = tokio::signal::ctrl_c().await;
            }

            log_info!("Signal received; requesting shutdown.");
            let _ = shutdown_tx.send(true);
        });
    }

    if !rpc_addr.ip().is_loopback() && config.rpc_allow_ips.is_empty() {
        log_warn!(
            "Warning: RPC is bound to {} but no rpcallowip is set; only localhost connections will be permitted",
            rpc_addr
        );
    }

    if config.fetch_params {
        fetch_params(&config.params_dir, config.network).map_err(|err| err.to_string())?;
    }
    let shielded_params =
        load_params(&config.params_dir, config.network).map_err(|err| err.to_string())?;
    let validation_metrics = Arc::new(ValidationMetrics::default());
    let connect_metrics = Arc::new(ConnectMetrics::default());
    let write_lock = Arc::new(Mutex::new(()));
    let flags = validation_flags(
        Arc::new(shielded_params),
        config.check_script,
        Some(Arc::clone(&validation_metrics)),
    );

    if reindex_from_flatfiles {
        reindex_blocks_from_flatfiles(
            chainstate.as_ref(),
            &blocks_path,
            params.as_ref(),
            &flags,
            write_lock.as_ref(),
        )?;
    }

    if !reindex_from_flatfiles {
        if config.reindex_txindex {
            rebuild_txindex(chainstate.as_ref(), write_lock.as_ref())?;
        }
        if config.reindex_spentindex {
            rebuild_spentindex(chainstate.as_ref(), write_lock.as_ref())?;
        }
        if config.reindex_addressindex {
            rebuild_addressindex(chainstate.as_ref(), write_lock.as_ref())?;
        }
    }

    let mempool = Arc::new(Mutex::new(mempool::Mempool::new(config.mempool_max_bytes)));
    let mut mempool_policy =
        mempool::MempoolPolicy::standard(config.min_relay_fee_per_kb, config.require_standard);
    mempool_policy.limit_free_relay_kb_per_minute = config.limit_free_relay_kb_per_minute;
    let mempool_policy = Arc::new(mempool_policy);
    let mempool_metrics = Arc::new(stats::MempoolMetrics::default());
    let fee_estimates_path = data_dir.join(FEE_ESTIMATES_FILE_NAME);
    let fee_estimator =
        match fee_estimator::FeeEstimator::load(&fee_estimates_path, config.min_relay_fee_per_kb) {
            Ok(estimator) => estimator,
            Err(err) => {
                log_warn!(
                    "failed to load fee estimates from {}: {err}",
                    fee_estimates_path.display()
                );
                fee_estimator::FeeEstimator::new(config.min_relay_fee_per_kb)
            }
        };
    let fee_estimator = Arc::new(Mutex::new(fee_estimator));
    let (tx_announce, _) = broadcast::channel::<Hash256>(TX_ANNOUNCE_QUEUE);
    let wallet =
        wallet::Wallet::load_or_create(data_dir, config.network).map_err(|err| err.to_string())?;
    let wallet = Arc::new(Mutex::new(wallet));
    let sync_metrics = Arc::new(SyncMetrics::default());
    let header_metrics = Arc::new(HeaderMetrics::default());
    {
        let chainstate = Arc::clone(&chainstate);
        let store = Arc::clone(&store);
        let write_lock = Arc::clone(&write_lock);
        let mempool = Arc::clone(&mempool);
        let mempool_policy = Arc::clone(&mempool_policy);
        let mempool_metrics = Arc::clone(&mempool_metrics);
        let header_metrics = Arc::clone(&header_metrics);
        let fee_estimator = Arc::clone(&fee_estimator);
        let mempool_flags = flags.clone();
        let miner_address = config.miner_address.clone();
        let tx_confirm_target = config.tx_confirm_target;
        let params = params.as_ref().clone();
        let data_dir = data_dir.clone();
        let params_dir = config.params_dir.clone();
        let net_totals = Arc::clone(&net_totals);
        let peer_registry = Arc::clone(&peer_registry);
        let header_peer_book = Arc::clone(&header_peer_book);
        let addr_book = Arc::clone(&addr_book);
        let added_nodes = Arc::clone(&added_nodes);
        let tx_announce = tx_announce.clone();
        let wallet = Arc::clone(&wallet);
        let shutdown_tx = shutdown_tx.clone();
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rpc runtime");
            runtime.block_on(async move {
                if let Err(err) = rpc::serve_rpc(
                    rpc_addr,
                    rpc_auth,
                    rpc_allowlist,
                    chainstate,
                    store,
                    write_lock,
                    mempool,
                    mempool_policy,
                    mempool_metrics,
                    header_metrics,
                    fee_estimator,
                    tx_confirm_target,
                    mempool_flags,
                    miner_address,
                    params,
                    data_dir,
                    params_dir,
                    net_totals,
                    peer_registry,
                    header_peer_book,
                    addr_book,
                    added_nodes,
                    tx_announce,
                    wallet,
                    shutdown_tx,
                )
                .await
                {
                    log_error!("{err}");
                }
            });
        });
    }
    let verify_settings = resolve_verify_settings(
        &config,
        getdata_batch,
        inflight_per_peer,
        block_peers_target,
    );
    log_info!(
        "Worker settings: header_verify_workers={} verify_workers={} shielded_workers={} verify_queue={}",
        header_verify_workers,
        verify_settings.verify_workers,
        verify_settings.shielded_workers,
        verify_settings.verify_queue
    );

    ensure_genesis(
        &chainstate,
        params.as_ref(),
        &flags,
        Some(&connect_metrics),
        &write_lock,
    )?;

    if chainstate
        .utxo_stats()
        .map_err(|err| err.to_string())?
        .is_none()
    {
        log_info!("UTXO stats missing; rebuilding from UTXO set (one-time).");
        let _guard = write_lock
            .lock()
            .map_err(|_| "write lock poisoned".to_string())?;
        chainstate
            .ensure_utxo_stats()
            .map_err(|err| err.to_string())?;
        log_info!("UTXO stats rebuilt.");
    }

    if chainstate
        .value_pools()
        .map_err(|err| err.to_string())?
        .is_none()
    {
        log_info!("Shielded value pools missing; rebuilding from blocks (one-time).");
        let _guard = write_lock
            .lock()
            .map_err(|_| "write lock poisoned".to_string())?;
        chainstate
            .ensure_value_pools()
            .map_err(|err| err.to_string())?;
        log_info!("Shielded value pools rebuilt.");
    }

    if config.mempool_persist_interval_secs > 0 {
        match load_mempool_file(&mempool_path) {
            Ok(raws) => {
                if !raws.is_empty() {
                    log_info!(
                        "Loading {} mempool tx(s) from {}",
                        raws.len(),
                        mempool_path.display()
                    );
                }
                let mut accepted = 0u64;
                let mut rejected = 0u64;
                let mut evicted = 0u64;
                let mut evicted_bytes = 0u64;
                let current_estimate = current_fee_estimate(chainstate.as_ref());
                for raw in raws {
                    let tx = match Transaction::consensus_decode(&raw) {
                        Ok(tx) => tx,
                        Err(_) => {
                            rejected += 1;
                            continue;
                        }
                    };
                    let mempool_prevouts = match mempool.lock() {
                        Ok(guard) => guard.prevouts_for_tx(&tx),
                        Err(_) => {
                            log_warn!("mempool lock poisoned");
                            rejected += 1;
                            continue;
                        }
                    };
                    let entry = match mempool::build_mempool_entry(
                        chainstate.as_ref(),
                        &mempool_prevouts,
                        params.as_ref(),
                        &flags,
                        mempool_policy.as_ref(),
                        tx,
                        raw,
                        false,
                    ) {
                        Ok(entry) => entry,
                        Err(_) => {
                            rejected += 1;
                            continue;
                        }
                    };
                    let tx_info = fee_estimator::MempoolTxInfo {
                        txid: entry.txid,
                        height: u32::try_from(entry.height.max(0)).unwrap_or(0),
                        fee: entry.fee,
                        size: entry.size(),
                        starting_priority: entry.starting_priority(),
                        was_clear_at_entry: entry.was_clear_at_entry,
                    };

                    let insert_outcome = match mempool.lock() {
                        Ok(mut guard) => match guard.insert(entry) {
                            Ok(outcome) => {
                                evicted = evicted.saturating_add(outcome.evicted);
                                evicted_bytes = evicted_bytes.saturating_add(outcome.evicted_bytes);
                                Ok(outcome)
                            }
                            Err(err) => Err(err.kind),
                        },
                        Err(_) => {
                            log_warn!("mempool lock poisoned");
                            rejected += 1;
                            continue;
                        }
                    };

                    let inserted = match insert_outcome {
                        Ok(outcome) => {
                            if let Ok(mut estimator) = fee_estimator.lock() {
                                estimator.process_transaction(tx_info, current_estimate);
                                for txid in outcome.evicted_txids {
                                    estimator.remove_transaction(&txid);
                                }
                            }
                            true
                        }
                        Err(mempool::MempoolErrorKind::AlreadyInMempool) => true,
                        Err(_) => false,
                    };
                    if inserted {
                        accepted += 1;
                    } else {
                        rejected += 1;
                    }
                }
                if accepted > 0 || rejected > 0 || evicted > 0 {
                    log_info!(
                        "Mempool load complete: accepted {} rejected {} evicted {} ({} bytes)",
                        accepted,
                        rejected,
                        evicted,
                        evicted_bytes
                    );
                }
                if accepted > 0 {
                    mempool_metrics.note_loaded(accepted);
                }
                if rejected > 0 {
                    mempool_metrics.note_load_reject(rejected);
                }
                if evicted > 0 {
                    mempool_metrics.note_evicted(evicted, evicted_bytes);
                }

                if rejected > 0 || evicted > 0 {
                    let mut snapshot = match mempool.lock() {
                        Ok(guard) => guard
                            .entries()
                            .map(|entry| (entry.txid, entry.raw.clone()))
                            .collect::<Vec<_>>(),
                        Err(_) => {
                            log_warn!("mempool lock poisoned; skipping mempool rewrite");
                            Vec::new()
                        }
                    };
                    snapshot.sort_by(|a, b| a.0.cmp(&b.0));
                    match save_mempool_file(&mempool_path, &snapshot) {
                        Ok(bytes) => mempool_metrics.note_persisted(bytes as u64),
                        Err(err) => {
                            log_warn!(
                                "failed to rewrite {} after load: {err}",
                                mempool_path.display()
                            );
                        }
                    }
                }
            }
            Err(err) => log_warn!("failed to load mempool file: {err}"),
        }

        let mempool = Arc::clone(&mempool);
        let mempool_metrics = Arc::clone(&mempool_metrics);
        let mempool_path = mempool_path.clone();
        let interval_secs = config.mempool_persist_interval_secs;
        thread::spawn(move || {
            persist_mempool_loop(mempool, mempool_metrics, mempool_path, interval_secs)
        });
    }

    if config.fee_estimates_persist_interval_secs > 0 {
        let fee_estimator = Arc::clone(&fee_estimator);
        let fee_estimates_path = fee_estimates_path.clone();
        let interval_secs = config.fee_estimates_persist_interval_secs;
        thread::spawn(move || {
            persist_fee_estimates_loop(fee_estimator, fee_estimates_path, interval_secs)
        });
    }

    spawn_status_logger(
        Arc::clone(&chainstate),
        Arc::clone(&store),
        Arc::clone(&sync_metrics),
        Arc::clone(&header_metrics),
        Arc::clone(&validation_metrics),
        Arc::clone(&connect_metrics),
        Arc::clone(&mempool),
        Arc::clone(&mempool_metrics),
        network,
        backend,
        start_time,
        status_interval_secs,
    );
    if let Some(addr) = dashboard_addr {
        let chainstate = Arc::clone(&chainstate);
        let store = Arc::clone(&store);
        let sync_metrics = Arc::clone(&sync_metrics);
        let header_metrics = Arc::clone(&header_metrics);
        let validation_metrics = Arc::clone(&validation_metrics);
        let connect_metrics = Arc::clone(&connect_metrics);
        let mempool = Arc::clone(&mempool);
        let mempool_metrics = Arc::clone(&mempool_metrics);
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("dashboard runtime");
            runtime.block_on(async move {
                if let Err(err) = dashboard::serve_dashboard(
                    addr,
                    chainstate,
                    store,
                    sync_metrics,
                    header_metrics,
                    validation_metrics,
                    connect_metrics,
                    mempool,
                    mempool_metrics,
                    network,
                    backend,
                    start_time,
                )
                .await
                {
                    log_error!("{err}");
                }
            });
        });
    }

    if config.p2p_listen {
        let bind_addr = config.p2p_addr.unwrap_or_else(|| {
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), params.default_port)
        });
        let listener = p2p_server::bind_inbound_p2p(bind_addr).await?;
        let chainstate = Arc::clone(&chainstate);
        let params = Arc::clone(&params);
        let addr_book = Arc::clone(&addr_book);
        let peer_registry = Arc::clone(&peer_registry);
        let net_totals = Arc::clone(&net_totals);
        let max_connections = config.max_connections;
        let mempool = Arc::clone(&mempool);
        let mempool_policy = Arc::clone(&mempool_policy);
        let mempool_metrics = Arc::clone(&mempool_metrics);
        let fee_estimator = Arc::clone(&fee_estimator);
        let flags = flags.clone();
        let tx_announce = tx_announce.clone();
        tokio::spawn(async move {
            if let Err(err) = p2p_server::serve_inbound_p2p(
                listener,
                chainstate,
                params,
                addr_book,
                peer_registry,
                net_totals,
                max_connections,
                mempool,
                mempool_policy,
                mempool_metrics,
                fee_estimator,
                flags,
                tx_announce,
            )
            .await
            {
                log_warn!("p2p listener stopped: {err}");
            }
        });
    } else {
        log_info!("P2P listener disabled (--no-p2p-listen)");
    }

    let start_height = start_height(&chainstate)?;
    let min_peer_height = chainstate
        .best_header()
        .map_err(|err| err.to_string())?
        .map(|tip| tip.height)
        .unwrap_or(start_height);
    let block_peer_ctx = PeerContext {
        net_totals: Arc::clone(&net_totals),
        registry: Arc::clone(&peer_registry),
        kind: PeerKind::Block,
    };
    let header_peer_ctx = PeerContext {
        net_totals: Arc::clone(&net_totals),
        registry: Arc::clone(&peer_registry),
        kind: PeerKind::Header,
    };
    let mut retry_delay = Duration::from_secs(1);
    let mut block_peer = loop {
        match connect_to_peer(
            params.as_ref(),
            start_height,
            min_peer_height,
            addr_book.as_ref(),
            &block_peer_ctx,
            Some(header_peer_book.as_ref()),
        )
        .await
        {
            Ok(peer) => break peer,
            Err(err) => {
                if *shutdown_rx.borrow() {
                    return Ok(());
                }
                log_warn!(
                    "block peer connect failed: {err}; retrying in {}s",
                    retry_delay.as_secs()
                );
                tokio::time::sleep(retry_delay).await;
                retry_delay = retry_delay.saturating_mul(2).min(Duration::from_secs(30));
            }
        }
    };
    log_info!("Block peer handshake complete");
    log_info!("Block peer height {}", block_peer.remote_height());
    log_info!(
        "Block peer version {} ua {}",
        block_peer.remote_version(),
        block_peer.remote_user_agent()
    );

    let mut block_peers = if block_peers_target == 0 {
        Vec::new()
    } else {
        match connect_to_peers(
            params.as_ref(),
            block_peers_target,
            start_height,
            min_peer_height,
            Some(addr_book.as_ref()),
            &block_peer_ctx,
            Some(header_peer_book.as_ref()),
        )
        .await
        {
            Ok(peers) => peers,
            Err(err) => {
                log_warn!("failed to connect to additional block peers: {err}");
                Vec::new()
            }
        }
    };
    if block_peers.is_empty() {
        log_warn!("no block peers available, falling back to block peer");
    }

    let header_allow_addr_book = config.header_peer_addrs.is_empty();
    let seed_addrs = if !config.header_peer_addrs.is_empty() {
        match parse_peer_addrs(&config.header_peer_addrs, params.default_port) {
            Ok(addrs) => {
                log_info!("Using {} header peer(s) from --header-peer", addrs.len());
                addrs
            }
            Err(err) => {
                log_warn!("header peer override failed: {err}");
                Vec::new()
            }
        }
    } else {
        match resolve_seed_addresses(&params).await {
            Ok(addrs) => addrs,
            Err(err) => {
                log_warn!("seed resolve failed: {err}");
                Vec::new()
            }
        }
    };
    let seed_addrs = Arc::new(seed_addrs);
    let addr_book_handle = Arc::clone(&addr_book);
    let addr_seeds = Arc::clone(&seed_addrs);
    let addr_params = Arc::clone(&params);
    let addr_peer_ctx = header_peer_ctx.clone();
    tokio::spawn(async move {
        if let Err(err) = addr_discovery_loop(
            addr_params,
            addr_seeds,
            addr_book_handle,
            start_height,
            addr_peer_ctx,
        )
        .await
        {
            log_warn!("addr discovery stopped: {err}");
        }
    });
    let header_cursor = Arc::new(Mutex::new(init_header_cursor(
        chainstate.as_ref(),
        params.as_ref(),
    )?));

    let header_peers = header_peers_target.max(1);
    log_info!(
        "Header sync using 1 active worker (peer probe target {})",
        header_peers
    );
    let (header_tx, header_rx) = mpsc::channel(HEADER_BATCH_QUEUE);
    let header_chainstate = Arc::clone(&chainstate);
    let header_params = Arc::clone(&params);
    let header_seeds = Arc::clone(&seed_addrs);
    let header_addr_book = Arc::clone(&addr_book);
    let header_peer_book_handle = Arc::clone(&header_peer_book);
    let header_commit_chainstate = Arc::clone(&chainstate);
    let header_commit_params = Arc::clone(&params);
    let header_commit_lock = Arc::clone(&write_lock);
    let header_commit_cursor = Arc::clone(&header_cursor);
    let header_commit_metrics = Arc::clone(&header_metrics);
    tokio::spawn(async move {
        if let Err(err) = header_commit_loop(
            header_rx,
            header_commit_chainstate,
            header_commit_params,
            header_commit_lock,
            header_lead,
            header_verify_workers,
            header_commit_cursor,
            header_commit_metrics,
        )
        .await
        {
            log_error!("header commit stopped: {err}");
        }
    });
    let header_sync_metrics = Arc::clone(&header_metrics);
    let header_peer_ctx_task = header_peer_ctx.clone();
    tokio::spawn(async move {
        if let Err(err) = header_sync_loop(
            header_chainstate,
            header_params,
            header_seeds,
            header_addr_book,
            header_allow_addr_book,
            header_peer_book_handle,
            header_tx,
            header_lead,
            header_peers,
            header_sync_metrics,
            header_peer_ctx_task,
        )
        .await
        {
            log_error!("header sync stopped: {err}");
        }
    });

    if config.tx_peers > 0 {
        let relay_peer_ctx = PeerContext {
            net_totals: Arc::clone(&net_totals),
            registry: Arc::clone(&peer_registry),
            kind: PeerKind::Relay,
        };
        let relay_chainstate = Arc::clone(&chainstate);
        let relay_params = Arc::clone(&params);
        let relay_addr_book = Arc::clone(&addr_book);
        let relay_mempool = Arc::clone(&mempool);
        let relay_mempool_policy = Arc::clone(&mempool_policy);
        let relay_mempool_metrics = Arc::clone(&mempool_metrics);
        let relay_fee_estimator = Arc::clone(&fee_estimator);
        let relay_flags = flags.clone();
        let relay_tx_announce = tx_announce.clone();
        let relay_target = config.tx_peers;
        tokio::spawn(async move {
            if let Err(err) = tx_relay::tx_relay_loop(
                relay_chainstate,
                relay_params,
                relay_addr_book,
                relay_peer_ctx,
                relay_mempool,
                relay_mempool_policy,
                relay_mempool_metrics,
                relay_fee_estimator,
                relay_flags,
                relay_tx_announce,
                relay_target,
            )
            .await
            {
                log_warn!("tx relay stopped: {err}");
            }
        });
    }

    sync_chain(
        &mut block_peer,
        &mut block_peers,
        block_peers_target,
        Arc::clone(&chainstate),
        Arc::clone(&mempool),
        Arc::clone(&fee_estimator),
        Arc::clone(&sync_metrics),
        Arc::clone(&params),
        addr_book.as_ref(),
        &block_peer_ctx,
        Some(header_peer_book.as_ref()),
        &flags,
        &verify_settings,
        Arc::clone(&connect_metrics),
        Arc::clone(&write_lock),
        Arc::clone(&header_cursor),
        header_lead,
        getdata_batch,
        inflight_per_peer,
        shutdown_rx.clone(),
    )
    .await?;

    if *shutdown_rx.borrow() {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    Ok(())
}

fn scan_supply<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
) -> Result<(), String> {
    let best = chainstate.best_block().map_err(|err| err.to_string())?;
    let Some(best) = best else {
        println!("No blocks found in the local database.");
        return Ok(());
    };
    if best.height < 0 {
        println!("No blocks found in the local database.");
        return Ok(());
    }

    let mut total_coinbase: i128 = 0;
    let mut total_expected: i128 = 0;
    let mut last_progress = Instant::now();

    for height in 0..=best.height {
        let hash = chainstate
            .height_hash(height)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing height index for height {height}"))?;
        let location = chainstate
            .block_location(&hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing block location for height {height}"))?;
        let bytes = chainstate
            .read_block(location)
            .map_err(|err| err.to_string())?;
        let block = Block::consensus_decode(&bytes).map_err(|err| err.to_string())?;
        let coinbase = block
            .transactions
            .first()
            .ok_or_else(|| format!("missing coinbase at height {height}"))?;
        let coinbase_value = tx_value_out_for_supply(coinbase)?;
        total_coinbase += coinbase_value as i128;

        let expected = block_subsidy(height, &params.consensus) as i128
            + exchange_fund_amount(height, &params.funding) as i128
            + foundation_fund_amount(height, &params.funding) as i128
            + swap_pool_amount(height as i64, &params.swap_pool) as i128;
        total_expected += expected;

        if height > 0 && height % 100_000 == 0 {
            println!(
                "Scanned height {} (elapsed {:?})",
                height,
                last_progress.elapsed()
            );
            last_progress = Instant::now();
        }
    }

    let delta = total_coinbase - total_expected;
    println!("Supply scan complete at height {}", best.height);
    println!(
        "Total coinbase out: {} ({})",
        total_coinbase,
        format_amount(total_coinbase)
    );
    println!(
        "Expected subsidy+funds: {} ({})",
        total_expected,
        format_amount(total_expected)
    );
    println!(
        "Coinbase minus expected: {} ({})",
        delta,
        format_amount(delta)
    );
    Ok(())
}

fn scan_fluxnodes<S: KeyValueStore>(chainstate: &ChainState<S>) -> Result<(), String> {
    let records = chainstate
        .fluxnode_records()
        .map_err(|err| err.to_string())?;
    if records.is_empty() {
        println!("No fluxnode records found in the local database.");
        return Ok(());
    }

    let mut total = 0usize;
    let mut confirmed_total = 0usize;
    let mut tier_total = [0usize; 3];
    let mut tier_confirmed = [0usize; 3];
    let mut collateral_value_zero = 0usize;

    let mut min_start: Option<u32> = None;
    let mut max_start: Option<u32> = None;
    let mut min_confirmed: Option<u32> = None;
    let mut max_confirmed: Option<u32> = None;
    let mut min_last_confirmed: Option<u32> = None;
    let mut max_last_confirmed: Option<u32> = None;
    let mut min_last_paid: Option<u32> = None;
    let mut max_last_paid: Option<u32> = None;

    for record in &records {
        total += 1;
        if record.collateral_value == 0 {
            collateral_value_zero += 1;
        }
        if (1..=3).contains(&record.tier) {
            tier_total[(record.tier - 1) as usize] += 1;
        }

        min_start = Some(min_start.map_or(record.start_height, |v| v.min(record.start_height)));
        max_start = Some(max_start.map_or(record.start_height, |v| v.max(record.start_height)));
        min_last_confirmed = Some(
            min_last_confirmed.map_or(record.last_confirmed_height, |v| {
                v.min(record.last_confirmed_height)
            }),
        );
        max_last_confirmed = Some(
            max_last_confirmed.map_or(record.last_confirmed_height, |v| {
                v.max(record.last_confirmed_height)
            }),
        );
        min_last_paid =
            Some(min_last_paid.map_or(record.last_paid_height, |v| v.min(record.last_paid_height)));
        max_last_paid =
            Some(max_last_paid.map_or(record.last_paid_height, |v| v.max(record.last_paid_height)));

        if record.confirmed_height > 0 {
            confirmed_total += 1;
            if (1..=3).contains(&record.tier) {
                tier_confirmed[(record.tier - 1) as usize] += 1;
            }
            min_confirmed = Some(
                min_confirmed.map_or(record.confirmed_height, |v| v.min(record.confirmed_height)),
            );
            max_confirmed = Some(
                max_confirmed.map_or(record.confirmed_height, |v| v.max(record.confirmed_height)),
            );
        }
    }

    println!("Fluxnode DB scan complete");
    println!("Total records: {total}");
    println!("Confirmed records: {confirmed_total}");
    println!(
        "Tier totals: cumulus={} nimbus={} stratus={}",
        tier_total[0], tier_total[1], tier_total[2]
    );
    println!(
        "Tier confirmed: cumulus={} nimbus={} stratus={}",
        tier_confirmed[0], tier_confirmed[1], tier_confirmed[2]
    );
    println!("Records with collateral_value=0: {collateral_value_zero}");
    if let (Some(min), Some(max)) = (min_start, max_start) {
        println!("start_height range: {min}..{max}");
    }
    if let (Some(min), Some(max)) = (min_confirmed, max_confirmed) {
        println!("confirmed_height range: {min}..{max}");
    }
    if let (Some(min), Some(max)) = (min_last_confirmed, max_last_confirmed) {
        println!("last_confirmed_height range: {min}..{max}");
    }
    if let (Some(min), Some(max)) = (min_last_paid, max_last_paid) {
        println!("last_paid_height range: {min}..{max}");
    }
    Ok(())
}

fn debug_find_fluxnode_payee_script<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    target_script: &[u8],
) -> Result<(), String> {
    let records = chainstate
        .fluxnode_records()
        .map_err(|err| err.to_string())?;
    if records.is_empty() {
        println!("No fluxnode records found in the local database.");
        return Ok(());
    }

    let mut matches = 0usize;
    for record in &records {
        let utxo = chainstate
            .utxo_entry(&record.collateral)
            .map_err(|err| err.to_string())?;

        let operator_pubkey = chainstate
            .fluxnode_key(record.operator_pubkey)
            .map_err(|err| err.to_string())?
            .unwrap_or_default();

        let mut candidate_scripts = Vec::new();
        if let Some(key) = record.p2sh_script {
            if let Some(redeem_script) = chainstate
                .fluxnode_key(key)
                .map_err(|err| err.to_string())?
            {
                let script_hash = fluxd_primitives::hash::hash160(&redeem_script);
                let mut script = Vec::with_capacity(23);
                script.extend_from_slice(&[0xa9, 0x14]);
                script.extend_from_slice(&script_hash);
                script.push(0x87);
                candidate_scripts.push(("p2sh(redeem_script)", script));
            }
        }
        if let Some(key) = record.collateral_pubkey {
            if let Some(collateral_pubkey) = chainstate
                .fluxnode_key(key)
                .map_err(|err| err.to_string())?
            {
                let pubkey_hash = fluxd_primitives::hash::hash160(&collateral_pubkey);
                let mut script = Vec::with_capacity(25);
                script.extend_from_slice(&[0x76, 0xa9, 0x14]);
                script.extend_from_slice(&pubkey_hash);
                script.extend_from_slice(&[0x88, 0xac]);
                candidate_scripts.push(("p2pkh(collateral_pubkey)", script));
            }
        }
        if let Some(utxo) = utxo.as_ref() {
            candidate_scripts.push(("collateral_utxo_script", utxo.script_pubkey.clone()));
        }

        let found = candidate_scripts
            .iter()
            .any(|(_, script)| script.as_slice() == target_script);
        if !found {
            continue;
        }

        matches += 1;
        println!(
            "Match {matches}: {}",
            outpoint_to_string(&record.collateral)
        );
        println!(
            "  tier={} confirmed_height={} last_confirmed_height={} last_paid_height={} collateral_value={}",
            record.tier,
            record.confirmed_height,
            record.last_confirmed_height,
            record.last_paid_height,
            record.collateral_value,
        );
        println!(
            "  operator_pubkey_hash160={}",
            hex_encode(&fluxd_primitives::hash::hash160(&operator_pubkey))
        );
        if let Some(utxo) = utxo {
            println!(
                "  utxo: value={} script={}",
                utxo.value,
                hex_encode(&utxo.script_pubkey)
            );
        } else {
            println!("  utxo: missing");
        }
        for (label, script) in candidate_scripts {
            let is_match = script.as_slice() == target_script;
            if is_match {
                println!("  script[{label}]=MATCH {}", hex_encode(&script));
            } else {
                println!("  script[{label}]={}", hex_encode(&script));
            }
        }
    }

    if matches == 0 {
        println!(
            "No fluxnode records matched script {}",
            hex_encode(target_script)
        );
    } else {
        println!("Total matches: {matches}");
    }

    Ok(())
}

fn debug_print_expected_fluxnode_payouts<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
    height: i32,
) -> Result<(), String> {
    let payouts = chainstate
        .deterministic_fluxnode_payouts(height, params)
        .map_err(|err| err.to_string())?;
    if payouts.is_empty() {
        println!("No deterministic fluxnode payouts at height {height}");
        return Ok(());
    }

    let records = chainstate
        .fluxnode_records()
        .map_err(|err| err.to_string())?;
    let mut record_by_outpoint = HashMap::new();
    for record in records {
        record_by_outpoint.insert(outpoint_to_string(&record.collateral), record);
    }

    let block_value = fluxd_consensus::block_subsidy(height, &params.consensus);
    println!("Expected fluxnode payouts at height {height} (block_value={block_value})");
    for (tier, outpoint, script_pubkey, amount) in payouts {
        let key = outpoint_to_string(&outpoint);
        println!(
            "- tier={} outpoint={} amount={} script={}",
            tier,
            key,
            amount,
            hex_encode(&script_pubkey)
        );
        if let Some(record) = record_by_outpoint.get(&key) {
            println!(
                "  record: tier={} confirmed_height={} last_confirmed_height={} last_paid_height={} p2sh_script={} collateral_pubkey={}",
                record.tier,
                record.confirmed_height,
                record.last_confirmed_height,
                record.last_paid_height,
                record.p2sh_script.is_some(),
                record.collateral_pubkey.is_some()
            );
        } else {
            println!("  record: missing");
        }
    }

    Ok(())
}

fn debug_print_fluxnode_payee_candidates<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
    tier: u8,
    height: i32,
    limit: usize,
) -> Result<(), String> {
    if !(1..=3).contains(&tier) {
        return Err(format!("invalid tier {tier} (expected 1..=3)"));
    }
    if height <= 0 {
        return Err(format!("invalid height {height} (expected > 0)"));
    }
    if limit == 0 {
        return Err("limit must be > 0".to_string());
    }

    let pay_height = height.saturating_sub(1);
    let pay_height_u32 =
        u32::try_from(pay_height).map_err(|_| "height out of range".to_string())?;
    let expiration = {
        use fluxd_consensus::constants::{
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V1,
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V2,
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V3,
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V4,
        };

        let upgrades = &params.consensus.upgrades;
        let count = if network_upgrade_active(pay_height, upgrades, UpgradeIndex::Pon) {
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V4
        } else if network_upgrade_active(pay_height, upgrades, UpgradeIndex::Halving) {
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V3
        } else if network_upgrade_active(pay_height, upgrades, UpgradeIndex::Flux) {
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V2
        } else {
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V1
        };
        u32::try_from(count).unwrap_or_default()
    };
    let expire_height_for_last_confirmed = |last_confirmed_height: u32| -> u32 {
        use fluxd_consensus::constants::{
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V1,
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V2,
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V3,
            FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V4,
        };

        let expiration_for_height = |height: u32| -> u32 {
            let height_i32 = i32::try_from(height).unwrap_or(i32::MAX);
            let upgrades = &params.consensus.upgrades;
            let count = if network_upgrade_active(height_i32, upgrades, UpgradeIndex::Pon) {
                FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V4
            } else if network_upgrade_active(height_i32, upgrades, UpgradeIndex::Halving) {
                FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V3
            } else if network_upgrade_active(height_i32, upgrades, UpgradeIndex::Flux) {
                FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V2
            } else {
                FLUXNODE_CONFIRM_UPDATE_EXPIRATION_HEIGHT_V1
            };
            u32::try_from(count).unwrap_or_default()
        };

        let mut expiration = expiration_for_height(last_confirmed_height);
        let mut expire_height = last_confirmed_height
            .saturating_add(expiration)
            .saturating_add(1);
        loop {
            let next_expiration = expiration_for_height(expire_height);
            if next_expiration == expiration {
                break;
            }
            expiration = next_expiration;
            expire_height = last_confirmed_height
                .saturating_add(expiration)
                .saturating_add(1);
        }
        expire_height
    };

    let mut candidates: Vec<FluxnodeRecord> = chainstate
        .fluxnode_records()
        .map_err(|err| err.to_string())?
        .into_iter()
        .filter(|record| record.tier == tier && record.confirmed_height > 0)
        .collect();

    candidates.sort_by(|a, b| {
        let a_has_last_paid = a.last_paid_height > 0;
        let b_has_last_paid = b.last_paid_height > 0;
        let a_comparator_height = if a_has_last_paid {
            a.last_paid_height
        } else {
            a.confirmed_height
        };
        let b_comparator_height = if b_has_last_paid {
            b.last_paid_height
        } else {
            b.confirmed_height
        };
        a_comparator_height
            .cmp(&b_comparator_height)
            .then_with(|| a_has_last_paid.cmp(&b_has_last_paid))
            .then_with(|| a.collateral.hash.cmp(&b.collateral.hash))
            .then_with(|| a.collateral.index.cmp(&b.collateral.index))
    });

    println!(
        "Fluxnode payee candidates tier={tier} height={height} (pay_height={pay_height} expiration={expiration})",
    );
    println!("Candidates scanned: {}", candidates.len());

    #[derive(Clone)]
    struct EligibleCandidate {
        idx: usize,
        outpoint: OutPoint,
        comparator_height: u32,
        has_last_paid: bool,
        confirmed_height: u32,
        last_confirmed_height: u32,
        last_paid_height: u32,
        collateral_value: i64,
        script: Vec<u8>,
        utxo_value: i64,
        utxo_script: Vec<u8>,
        is_p2sh: bool,
    }

    let mut eligible: Vec<EligibleCandidate> = Vec::new();
    for (idx, record) in candidates.iter().enumerate() {
        let expired =
            pay_height_u32 >= expire_height_for_last_confirmed(record.last_confirmed_height);
        if expired {
            continue;
        }
        let Some(utxo) = chainstate
            .utxo_entry(&record.collateral)
            .map_err(|err| err.to_string())?
        else {
            continue;
        };
        if !fluxd_consensus::fluxnode_collateral_matches_tier(
            pay_height,
            utxo.value,
            tier,
            &params.fluxnode,
        ) {
            continue;
        }

        let (script, is_p2sh) = if let Some(key) = record.p2sh_script {
            let redeem_script = chainstate
                .fluxnode_key(key)
                .map_err(|err| err.to_string())?
                .ok_or_else(|| "missing fluxnode redeem script".to_string())?;
            let script_hash = fluxd_primitives::hash::hash160(&redeem_script);
            let mut script = Vec::with_capacity(23);
            script.extend_from_slice(&[0xa9, 0x14]);
            script.extend_from_slice(&script_hash);
            script.push(0x87);
            (script, true)
        } else {
            let collateral_key = record
                .collateral_pubkey
                .ok_or_else(|| "missing fluxnode collateral pubkey key".to_string())?;
            let pubkey_bytes = chainstate
                .fluxnode_key(collateral_key)
                .map_err(|err| err.to_string())?
                .ok_or_else(|| "missing fluxnode collateral pubkey bytes".to_string())?;
            let is_p2sh_signing_key = params.fluxnode.p2sh_public_keys.iter().any(|key| {
                parse_hex_bytes(key.key)
                    .as_ref()
                    .is_some_and(|expected| expected.as_slice() == pubkey_bytes.as_slice())
            });
            if is_p2sh_signing_key {
                (utxo.script_pubkey.clone(), true)
            } else {
                let pubkey_hash = fluxd_primitives::hash::hash160(&pubkey_bytes);
                let mut script = Vec::with_capacity(25);
                script.extend_from_slice(&[0x76, 0xa9, 0x14]);
                script.extend_from_slice(&pubkey_hash);
                script.extend_from_slice(&[0x88, 0xac]);
                (script, false)
            }
        };

        let has_last_paid = record.last_paid_height > 0;
        let comparator_height = if has_last_paid {
            record.last_paid_height
        } else {
            record.confirmed_height
        };
        eligible.push(EligibleCandidate {
            idx,
            outpoint: record.collateral.clone(),
            comparator_height,
            has_last_paid,
            confirmed_height: record.confirmed_height,
            last_confirmed_height: record.last_confirmed_height,
            last_paid_height: record.last_paid_height,
            collateral_value: record.collateral_value,
            script,
            utxo_value: utxo.value,
            utxo_script: utxo.script_pubkey.clone(),
            is_p2sh,
        });
        if eligible.len() >= 10 {
            break;
        }
    }

    for (idx, record) in candidates.iter().enumerate().take(limit) {
        let has_last_paid = record.last_paid_height > 0;
        let comparator_height = if has_last_paid {
            record.last_paid_height
        } else {
            record.confirmed_height
        };
        let outpoint_str = outpoint_to_string(&record.collateral);

        let expired =
            pay_height_u32 >= expire_height_for_last_confirmed(record.last_confirmed_height);
        let utxo = chainstate
            .utxo_entry(&record.collateral)
            .map_err(|err| err.to_string())?;
        let collateral_matches = utxo.as_ref().is_some_and(|utxo| {
            fluxd_consensus::fluxnode_collateral_matches_tier(
                pay_height,
                utxo.value,
                tier,
                &params.fluxnode,
            )
        });

        let mut status = Vec::new();
        if eligible.first().map(|entry| entry.idx) == Some(idx) {
            status.push("WINNER");
        }
        if expired {
            status.push("expired");
        }
        if utxo.is_none() {
            status.push("missing_utxo");
        }
        if !collateral_matches {
            status.push("collateral_mismatch");
        }
        let status = if status.is_empty() {
            "ok".to_string()
        } else {
            status.join(",")
        };

        println!(
            "#{idx:>5} outpoint={outpoint_str} comparator_height={comparator_height} has_last_paid={has_last_paid} confirmed_height={} last_confirmed_height={} last_paid_height={} collateral_value={} [{status}]",
            record.confirmed_height,
            record.last_confirmed_height,
            record.last_paid_height,
            record.collateral_value,
        );

        if let Some(utxo) = utxo {
            println!(
                "      utxo: value={} script={}",
                utxo.value,
                hex_encode(&utxo.script_pubkey)
            );
        } else {
            println!("      utxo: missing");
        }
    }

    if eligible.is_empty() {
        println!("Selected payee: none (no eligible candidates)");
        return Ok(());
    }
    println!("Eligible candidates (first {}):", eligible.len());
    for entry in &eligible {
        println!(
            "- idx={} outpoint={} comparator_height={} has_last_paid={} confirmed_height={} last_confirmed_height={} last_paid_height={} collateral_value={} script={} utxo_value={} utxo_script={} p2sh={}",
            entry.idx,
            outpoint_to_string(&entry.outpoint),
            entry.comparator_height,
            entry.has_last_paid,
            entry.confirmed_height,
            entry.last_confirmed_height,
            entry.last_paid_height,
            entry.collateral_value,
            hex_encode(&entry.script),
            entry.utxo_value,
            hex_encode(&entry.utxo_script),
            entry.is_p2sh,
        );
    }

    Ok(())
}

fn scan_flatfiles<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    blocks_path: &std::path::Path,
) -> Result<(), String> {
    let best = chainstate.best_block().map_err(|err| err.to_string())?;
    let Some(best) = best else {
        println!("No blocks found in the local database.");
        return Ok(());
    };
    if best.height < 0 {
        println!("No blocks found in the local database.");
        return Ok(());
    }

    let mut last_progress = Instant::now();
    for height in 0..=best.height {
        let hash = chainstate
            .height_hash(height)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing height index for height {height}"))?;
        let location = chainstate
            .block_location(&hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing block location for height {height}"))?;
        match read_flatfile_len(blocks_path, location.file_id, location.offset) {
            Ok(stored_len) => {
                if stored_len != location.len {
                    return Err(format!(
                        "flatfile length mismatch at height {height} hash {}: expected {} got {} (file data{:05}.dat offset {})",
                        hash256_to_hex(&hash),
                        location.len,
                        stored_len,
                        location.file_id,
                        location.offset
                    ));
                }
            }
            Err(err) => {
                return Err(format!(
                    "flatfile read failed at height {height} hash {} (file data{:05}.dat offset {}): {err}",
                    hash256_to_hex(&hash),
                    location.file_id,
                    location.offset
                ));
            }
        }

        if height > 0 && height % 100_000 == 0 {
            println!(
                "Scanned height {} (elapsed {:?})",
                height,
                last_progress.elapsed()
            );
            last_progress = Instant::now();
        }
    }

    println!("Flatfile scan complete at height {}", best.height);
    Ok(())
}

fn read_flatfile_len(
    blocks_path: &std::path::Path,
    file_id: u32,
    offset: u64,
) -> Result<u32, String> {
    let path = blocks_path.join(format!("data{file_id:05}.dat"));
    let mut file = File::open(&path).map_err(|err| err.to_string())?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|err| err.to_string())?;
    let mut len_bytes = [0u8; 4];
    file.read_exact(&mut len_bytes)
        .map_err(|err| err.to_string())?;
    Ok(u32::from_le_bytes(len_bytes))
}

fn tx_value_out_for_supply(tx: &Transaction) -> Result<i64, String> {
    let mut total = 0i64;
    for output in &tx.vout {
        if output.value < 0 || output.value > MAX_MONEY {
            return Err("coinbase output value out of range".to_string());
        }
        total = total
            .checked_add(output.value)
            .ok_or_else(|| "coinbase output value out of range".to_string())?;
        if !money_range(total) {
            return Err("coinbase output value out of range".to_string());
        }
    }

    if tx.value_balance <= 0 {
        let balance = -tx.value_balance;
        total = total
            .checked_add(balance)
            .ok_or_else(|| "coinbase output value out of range".to_string())?;
        if !money_range(balance) || !money_range(total) {
            return Err("coinbase output value out of range".to_string());
        }
    }

    for joinsplit in &tx.join_splits {
        total = total
            .checked_add(joinsplit.vpub_old)
            .ok_or_else(|| "coinbase output value out of range".to_string())?;
        if !money_range(joinsplit.vpub_old) || !money_range(total) {
            return Err("coinbase output value out of range".to_string());
        }
    }

    Ok(total)
}

fn format_amount(amount: i128) -> String {
    let sign = if amount < 0 { "-" } else { "" };
    let abs = amount.abs();
    let whole = abs / COIN as i128;
    let frac = abs % COIN as i128;
    format!("{sign}{whole}.{frac:08}")
}

fn open_store(backend: Backend, db_path: &PathBuf, config: &Config) -> Result<Store, String> {
    match backend {
        Backend::Memory => Ok(Store::Memory(MemoryStore::new())),
        Backend::Fjall => {
            let options = FjallOptions {
                cache_bytes: config.db_cache_bytes,
                write_buffer_bytes: config.db_write_buffer_bytes,
                journal_bytes: config.db_journal_bytes,
                memtable_bytes: config.db_memtable_bytes,
                flush_workers: config.db_flush_workers,
                compaction_workers: config.db_compaction_workers,
                fsync_ms: config.db_fsync_ms,
            };
            let partition_count = fluxd_storage::Column::ALL.len() as u64;
            if let (Some(write_buffer), Some(memtable)) =
                (options.write_buffer_bytes, options.memtable_bytes)
            {
                let max_memtables = u64::from(memtable).saturating_mul(partition_count);
                if write_buffer < max_memtables {
                    log_warn!(
                        "Warning: --db-write-buffer-mb ({}) is below partitions ({}) × --db-memtable-mb ({}); expect frequent flushes / L0 stalls",
                        write_buffer / (1024 * 1024),
                        partition_count,
                        u64::from(memtable) / (1024 * 1024),
                    );
                }
            }
            if let (Some(journal), Some(memtable)) = (options.journal_bytes, options.memtable_bytes)
            {
                let min_journal = u64::from(memtable)
                    .saturating_mul(partition_count)
                    .saturating_mul(2);
                if journal < min_journal {
                    log_warn!(
                        "Warning: --db-journal-mb ({}) is below 2 × partitions ({}) × --db-memtable-mb ({}); Fjall may halt writes when journals fill",
                        journal / (1024 * 1024),
                        partition_count,
                        u64::from(memtable) / (1024 * 1024),
                    );
                }
            }
            Ok(Store::Fjall(
                FjallStore::open_with_options(db_path, options).map_err(|err| err.to_string())?,
            ))
        }
    }
}

fn ensure_db_schema_version(store: &Store) -> Result<u32, String> {
    let version = match store
        .get(fluxd_storage::Column::Meta, DB_SCHEMA_VERSION_KEY)
        .map_err(|err| err.to_string())?
    {
        Some(bytes) => {
            let bytes: [u8; 4] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| format!("invalid db schema version length {}", bytes.len()))?;
            u32::from_le_bytes(bytes)
        }
        None => {
            let has_any_data = store_has_any_data(store)?;
            if has_any_data {
                if DB_SCHEMA_VERSION != 1 {
                    return Err(format!(
                        "database schema version missing (expected {DB_SCHEMA_VERSION}); this data dir was created by an older build; run with --reindex to rebuild"
                    ));
                }
                log_warn!(
                    "Database schema version missing; assuming version 1 for a pre-versioned data dir"
                );
                store
                    .put(
                        fluxd_storage::Column::Meta,
                        DB_SCHEMA_VERSION_KEY,
                        &DB_SCHEMA_VERSION.to_le_bytes(),
                    )
                    .map_err(|err| err.to_string())?;
                return Ok(DB_SCHEMA_VERSION);
            }
            store
                .put(
                    fluxd_storage::Column::Meta,
                    DB_SCHEMA_VERSION_KEY,
                    &DB_SCHEMA_VERSION.to_le_bytes(),
                )
                .map_err(|err| err.to_string())?;
            DB_SCHEMA_VERSION
        }
    };

    if version != DB_SCHEMA_VERSION {
        return Err(format!(
            "database schema version mismatch (found {version}, expected {DB_SCHEMA_VERSION}); run with --reindex to rebuild",
        ));
    }

    Ok(version)
}

const SCAN_EARLY_EXIT_SENTINEL: &str = "__fluxd_scan_early_exit__";

fn store_column_has_any(store: &Store, column: fluxd_storage::Column) -> Result<bool, String> {
    let mut found = false;
    let mut visitor = |_: &[u8], _: &[u8]| {
        found = true;
        Err(StoreError::Backend(SCAN_EARLY_EXIT_SENTINEL.to_string()))
    };
    match store.for_each_prefix(column, &[], &mut visitor) {
        Ok(()) => Ok(found),
        Err(StoreError::Backend(message)) if message == SCAN_EARLY_EXIT_SENTINEL => Ok(found),
        Err(err) => Err(err.to_string()),
    }
}

fn store_has_any_data(store: &Store) -> Result<bool, String> {
    for column in fluxd_storage::Column::ALL {
        if store_column_has_any(store, column)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn meta_u32(store: &Store, key: &[u8]) -> Result<Option<u32>, String> {
    match store
        .get(fluxd_storage::Column::Meta, key)
        .map_err(|err| err.to_string())?
    {
        Some(bytes) => {
            let bytes: [u8; 4] = bytes.as_slice().try_into().map_err(|_| {
                format!(
                    "invalid u32 meta key length {} for {}",
                    bytes.len(),
                    String::from_utf8_lossy(key)
                )
            })?;
            Ok(Some(u32::from_le_bytes(bytes)))
        }
        None => Ok(None),
    }
}

fn set_meta_u32(store: &Store, key: &[u8], value: u32) -> Result<(), String> {
    let bytes = value.to_le_bytes();
    store
        .put(fluxd_storage::Column::Meta, key, &bytes)
        .map_err(|err| err.to_string())
}

fn ensure_index_schema_version(
    store: &Store,
    name: &str,
    key: &[u8],
    expected: u32,
    rebuild_hint: &str,
    columns: &[fluxd_storage::Column],
) -> Result<u32, String> {
    match meta_u32(store, key)? {
        Some(found) => {
            if found != expected {
                return Err(format!(
                    "{name} schema version mismatch (found {found}, expected {expected}); rebuild with {rebuild_hint}",
                ));
            }
            Ok(found)
        }
        None => {
            let mut has_data = false;
            for column in columns {
                if *column == fluxd_storage::Column::Meta {
                    continue;
                }
                if store_column_has_any(store, *column)? {
                    has_data = true;
                    break;
                }
            }

            if has_data {
                if expected != 1 {
                    return Err(format!(
                        "{name} schema version missing (expected {expected}); rebuild with {rebuild_hint}",
                    ));
                }
                log_warn!("{name} schema version missing; assuming version 1 for a pre-versioned data dir");
                set_meta_u32(store, key, expected)?;
                return Ok(expected);
            }

            set_meta_u32(store, key, expected)?;
            Ok(expected)
        }
    }
}

fn ensure_secondary_index_versions(store: &Store) -> Result<(), String> {
    let _ = ensure_index_schema_version(
        store,
        "txindex",
        TXINDEX_VERSION_KEY,
        TXINDEX_VERSION,
        "--reindex-txindex",
        &[fluxd_storage::Column::TxIndex],
    )?;
    let _ = ensure_index_schema_version(
        store,
        "spentindex",
        SPENTINDEX_VERSION_KEY,
        SPENTINDEX_VERSION,
        "--reindex-spentindex",
        &[fluxd_storage::Column::SpentIndex],
    )?;
    let _ = ensure_index_schema_version(
        store,
        "addressindex",
        ADDRESSINDEX_VERSION_KEY,
        ADDRESSINDEX_VERSION,
        "--reindex-addressindex",
        &[
            fluxd_storage::Column::AddressOutpoint,
            fluxd_storage::Column::AddressDelta,
        ],
    )?;
    Ok(())
}

fn load_peers_file(path: &Path) -> Result<Vec<(SocketAddr, AddrBookEntry)>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.to_string()),
    };
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|err| format!("invalid peers file: {err}"))?;
    let version = value
        .get("version")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u32;

    match version {
        PEERS_FILE_VERSION_V1 => {
            let file: PeersFileV1 = serde_json::from_value(value)
                .map_err(|err| format!("invalid peers file: {err}"))?;
            let mut out = Vec::new();
            let mut seen = HashSet::new();
            for raw in file.addrs {
                if out.len() >= ADDR_BOOK_MAX {
                    break;
                }
                let Ok(addr) = raw.parse::<SocketAddr>() else {
                    continue;
                };
                if addr.port() == 0 {
                    continue;
                }
                if seen.insert(addr) {
                    out.push((addr, AddrBookEntry::default()));
                }
            }
            Ok(out)
        }
        PEERS_FILE_VERSION => {
            let file: PeersFileV2 = serde_json::from_value(value)
                .map_err(|err| format!("invalid peers file: {err}"))?;
            let mut out = Vec::new();
            let mut seen = HashSet::new();
            for peer in file.peers {
                if out.len() >= ADDR_BOOK_MAX {
                    break;
                }
                let Ok(addr) = peer.addr.parse::<SocketAddr>() else {
                    continue;
                };
                if addr.port() == 0 {
                    continue;
                }
                if seen.insert(addr) {
                    out.push((
                        addr,
                        AddrBookEntry {
                            last_seen: peer.last_seen,
                            last_success: peer.last_success,
                            last_failure: peer.last_failure,
                            last_attempt: 0,
                            successes: peer.successes,
                            failures: peer.failures,
                            last_height: peer.last_height,
                            last_version: peer.last_version,
                        },
                    ));
                }
            }
            Ok(out)
        }
        other => Err(format!(
            "unsupported peers file version {} (expected {} or {})",
            other, PEERS_FILE_VERSION, PEERS_FILE_VERSION_V1
        )),
    }
}

fn save_peers_file(path: &Path, peers: &[(SocketAddr, AddrBookEntry)]) -> Result<(), String> {
    let mut entries = peers
        .iter()
        .map(|(addr, entry)| PeersFileV2Entry {
            addr: addr.to_string(),
            last_seen: entry.last_seen,
            last_success: entry.last_success,
            last_failure: entry.last_failure,
            successes: entry.successes,
            failures: entry.failures,
            last_height: entry.last_height,
            last_version: entry.last_version,
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.addr.cmp(&b.addr));
    entries.dedup_by(|a, b| a.addr == b.addr);
    if entries.len() > ADDR_BOOK_MAX {
        entries.truncate(ADDR_BOOK_MAX);
    }

    let file = PeersFileV2 {
        version: PEERS_FILE_VERSION,
        peers: entries,
    };
    let json = serde_json::to_vec_pretty(&file).map_err(|err| err.to_string())?;
    write_file_atomic(path, &json)
}

fn load_mempool_file(path: &Path) -> Result<Vec<Vec<u8>>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.to_string()),
    };

    let mut decoder = Decoder::new(&bytes);
    let version = decoder
        .read_u32_le()
        .map_err(|err| format!("invalid mempool file: {err}"))?;
    if version != MEMPOOL_FILE_VERSION {
        return Err(format!(
            "unsupported mempool file version {version} (expected {MEMPOOL_FILE_VERSION})"
        ));
    }
    let count = decoder
        .read_varint()
        .map_err(|err| format!("invalid mempool file: {err}"))?;
    let count = usize::try_from(count).map_err(|_| "mempool file count too large".to_string())?;
    let mut out = Vec::with_capacity(count.min(16_384));
    for _ in 0..count {
        let raw = decoder
            .read_var_bytes()
            .map_err(|err| format!("invalid mempool file: {err}"))?;
        out.push(raw);
    }
    if !decoder.is_empty() {
        return Err("invalid mempool file: trailing bytes".to_string());
    }
    Ok(out)
}

fn save_mempool_file(path: &Path, entries: &[(Hash256, Vec<u8>)]) -> Result<usize, String> {
    let mut encoder = Encoder::new();
    encoder.write_u32_le(MEMPOOL_FILE_VERSION);
    encoder.write_varint(entries.len() as u64);
    for (_, raw) in entries {
        encoder.write_var_bytes(raw);
    }
    let bytes = encoder.into_inner();
    let len = bytes.len();
    write_file_atomic(path, &bytes)?;
    Ok(len)
}

fn write_file_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|err| err.to_string())?;
    if fs::rename(&tmp, path).is_err() {
        let _ = fs::remove_file(path);
        fs::rename(&tmp, path).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn persist_peers_loop(addr_book: Arc<AddrBook>, path: PathBuf) {
    let mut last_revision = addr_book.revision().saturating_sub(1);
    loop {
        thread::sleep(Duration::from_secs(PEERS_PERSIST_INTERVAL_SECS));
        let revision = addr_book.revision();
        if revision == last_revision {
            continue;
        }
        let snapshot = addr_book.snapshot();
        if let Err(err) = save_peers_file(&path, &snapshot) {
            log_warn!("failed to persist {}: {err}", path.display());
            continue;
        }
        last_revision = revision;
    }
}

fn persist_banlist_loop(peer_book: Arc<HeaderPeerBook>, path: PathBuf) {
    let mut last_revision = peer_book.banlist_revision();
    loop {
        thread::sleep(Duration::from_secs(BANLIST_PERSIST_INTERVAL_SECS));
        let revision = peer_book.banlist_revision();
        if revision == last_revision {
            continue;
        }
        if let Err(err) = peer_book.save_banlist(&path) {
            log_warn!("failed to persist {}: {err}", path.display());
            continue;
        }
        last_revision = revision;
    }
}

fn persist_mempool_loop(
    mempool: Arc<Mutex<mempool::Mempool>>,
    mempool_metrics: Arc<stats::MempoolMetrics>,
    path: PathBuf,
    interval_secs: u64,
) {
    if interval_secs == 0 {
        return;
    }
    let mut last_revision = mempool.lock().map(|guard| guard.revision()).unwrap_or(0);

    loop {
        thread::sleep(Duration::from_secs(interval_secs));
        let (revision, mut snapshot) = {
            let guard = match mempool.lock() {
                Ok(guard) => guard,
                Err(_) => {
                    log_warn!("mempool lock poisoned");
                    continue;
                }
            };
            let revision = guard.revision();
            if revision == last_revision {
                continue;
            }
            let snapshot: Vec<(Hash256, Vec<u8>)> = guard
                .entries()
                .map(|entry| (entry.txid, entry.raw.clone()))
                .collect();
            (revision, snapshot)
        };

        snapshot.sort_by(|a, b| a.0.cmp(&b.0));
        let persisted = match save_mempool_file(&path, &snapshot) {
            Ok(bytes) => bytes as u64,
            Err(err) => {
                log_warn!("failed to persist {}: {err}", path.display());
                continue;
            }
        };
        mempool_metrics.note_persisted(persisted);
        last_revision = revision;
    }
}

fn persist_fee_estimates_loop(
    fee_estimator: Arc<Mutex<fee_estimator::FeeEstimator>>,
    path: PathBuf,
    interval_secs: u64,
) {
    if interval_secs == 0 {
        return;
    }
    let mut last_revision = fee_estimator
        .lock()
        .map(|guard| guard.revision().saturating_sub(1))
        .unwrap_or(0);

    loop {
        thread::sleep(Duration::from_secs(interval_secs));
        let revision = {
            let guard = match fee_estimator.lock() {
                Ok(guard) => guard,
                Err(_) => {
                    log_warn!("fee estimator lock poisoned");
                    continue;
                }
            };
            let revision = guard.revision();
            if revision == last_revision {
                continue;
            }
            match guard.save(&path) {
                Ok(_) => revision,
                Err(err) => {
                    log_warn!("failed to persist {}: {err}", path.display());
                    continue;
                }
            }
        };
        last_revision = revision;
    }
}

fn start_height<S: KeyValueStore>(chainstate: &ChainState<S>) -> Result<i32, String> {
    if let Some(best) = chainstate.best_block().map_err(|err| err.to_string())? {
        return Ok(best.height);
    }
    if let Some(best) = chainstate.best_header().map_err(|err| err.to_string())? {
        return Ok(best.height);
    }
    Ok(0)
}

fn header_gap<S: KeyValueStore>(chainstate: &ChainState<S>) -> Result<(i32, i32), String> {
    let best_header = chainstate
        .best_header()
        .map_err(|err| err.to_string())?
        .map(|tip| tip.height)
        .unwrap_or(0);
    let best_block = chainstate
        .best_block()
        .map_err(|err| err.to_string())?
        .map(|tip| tip.height)
        .unwrap_or(0);
    Ok((best_header - best_block, best_header))
}

fn current_fee_estimate<S: KeyValueStore>(chainstate: &ChainState<S>) -> bool {
    let best_header = chainstate
        .best_header()
        .ok()
        .flatten()
        .map(|tip| tip.height.max(0))
        .unwrap_or(0);
    let best_block = chainstate
        .best_block()
        .ok()
        .flatten()
        .map(|tip| tip.height.max(0))
        .unwrap_or(0);
    best_header.saturating_sub(best_block) <= 1
}

fn max_fetch_blocks(peer_count: usize, getdata_batch: usize, inflight_per_peer: usize) -> usize {
    let peers = peer_count.max(1);
    let per_peer = getdata_batch.saturating_mul(inflight_per_peer.max(1));
    peers.saturating_mul(per_peer)
}

async fn connect_to_peer(
    params: &ChainParams,
    start_height: i32,
    min_height: i32,
    addr_book: &AddrBook,
    peer_ctx: &PeerContext,
    peer_book: Option<&HeaderPeerBook>,
) -> Result<Peer, String> {
    let probe_target = if min_height > 0 && addr_book.len() > 0 {
        12
    } else {
        HEADER_PEER_PROBE_COUNT
    };
    let peers = connect_to_peers(
        params,
        probe_target,
        start_height,
        min_height,
        Some(addr_book),
        peer_ctx,
        peer_book,
    )
    .await?;
    peers
        .into_iter()
        .max_by_key(|peer| peer.remote_height())
        .ok_or_else(|| "unable to connect to any seed".to_string())
}

async fn connect_to_peers(
    params: &ChainParams,
    count: usize,
    start_height: i32,
    min_height: i32,
    addr_book: Option<&AddrBook>,
    peer_ctx: &PeerContext,
    peer_book: Option<&HeaderPeerBook>,
) -> Result<Vec<Peer>, String> {
    if count == 0 {
        return Ok(Vec::new());
    }

    let is_allowed = |addr: SocketAddr| peer_book.map(|book| !book.is_banned(addr)).unwrap_or(true);
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();

    if let Some(peer_book) = peer_book {
        let mut preferred = peer_book.preferred(HEADER_PEER_PROBE_COUNT);
        preferred.shuffle(&mut rand::thread_rng());
        for addr in preferred {
            if seen.insert(addr) && is_allowed(addr) {
                candidates.push(addr);
            }
        }
    }

    let mut addrs = resolve_seed_addresses(params).await?;
    if let Some(addr_book) = addr_book {
        for addr in addr_book.sample_for_height(ADDR_BOOK_SAMPLE, min_height) {
            addrs.push(addr);
        }
    }
    addrs.shuffle(&mut rand::thread_rng());
    for addr in addrs {
        if seen.insert(addr) && is_allowed(addr) {
            candidates.push(addr);
        }
    }

    if candidates.is_empty() {
        return Err("no peer addresses available".to_string());
    }
    log_debug!("Peer candidates {}", candidates.len());

    let mut peers = Vec::new();
    let mut behind = Vec::new();
    let mut behind_peers = 0usize;
    let mut behind_logged = 0usize;
    const MAX_BEHIND_LOGGED: usize = 8;
    let mut failures = 0usize;
    let mut failures_logged = 0usize;
    const MAX_CONNECT_ERRORS_LOGGED: usize = 8;
    let mut join_set = JoinSet::new();
    let mut next_index = 0usize;
    let max_parallel = candidates.len().min(count.saturating_mul(2).max(4));

    while next_index < candidates.len() && join_set.len() < max_parallel {
        let addr = candidates[next_index];
        let magic = params.message_start;
        let peer_ctx = peer_ctx.clone();
        if let Some(addr_book) = addr_book {
            addr_book.record_attempt(addr);
        }
        join_set.spawn(async move {
            let result = connect_and_handshake(addr, magic, start_height, peer_ctx).await;
            (addr, result.map(|(_addr, peer)| peer))
        });
        next_index += 1;
    }

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok((addr, Ok(peer))) => {
                if let Some(addr_book) = addr_book {
                    addr_book.record_success(addr, &peer);
                }

                let remote_height = peer.remote_height();
                let remote_version = peer.remote_version();
                let remote_agent = peer.remote_user_agent().to_string();
                if min_height > 0 && remote_height >= 0 && remote_height < min_height {
                    behind_peers = behind_peers.saturating_add(1);
                    if behind_logged < MAX_BEHIND_LOGGED {
                        log_debug!(
                            "Peer {addr} behind (height {} < {}), skipping (ver {} ua {})",
                            remote_height,
                            min_height,
                            remote_version,
                            remote_agent
                        );
                        behind_logged += 1;
                    }
                    behind.push(peer);
                } else {
                    log_debug!(
                        "Connected to {addr} (height {} ver {} ua {})",
                        remote_height,
                        remote_version,
                        remote_agent
                    );
                    peers.push(peer);
                }
                if peers.len() >= count {
                    break;
                }
            }
            Ok((addr, Err(err))) => {
                if let Some(addr_book) = addr_book {
                    addr_book.record_failure(addr);
                }
                failures = failures.saturating_add(1);
                if failures_logged < MAX_CONNECT_ERRORS_LOGGED {
                    log_debug!("{err}");
                    failures_logged += 1;
                }
            }
            Err(err) => {
                log_warn!("peer task failed: {err}");
            }
        }

        if next_index < candidates.len() {
            let addr = candidates[next_index];
            let magic = params.message_start;
            let peer_ctx = peer_ctx.clone();
            if let Some(addr_book) = addr_book {
                addr_book.record_attempt(addr);
            }
            join_set.spawn(async move {
                let result = connect_and_handshake(addr, magic, start_height, peer_ctx).await;
                (addr, result.map(|(_addr, peer)| peer))
            });
            next_index += 1;
        }
    }

    if failures > failures_logged {
        log_debug!(
            "peer connect: {} additional failure(s) suppressed",
            failures - failures_logged
        );
    }
    if behind_peers > behind_logged {
        log_debug!(
            "peer connect: {} additional behind peer(s) suppressed",
            behind_peers - behind_logged
        );
    }

    if peers.is_empty() && !behind.is_empty() {
        let fallback = behind
            .into_iter()
            .max_by_key(|peer| peer.remote_height())
            .expect("behind checked to be non-empty");
        log_warn!(
            "All peers behind target height {}; using highest behind peer at {}",
            min_height,
            fallback.remote_height()
        );
        peers.push(fallback);
    }

    if peers.is_empty() {
        Err("unable to connect to any seed".to_string())
    } else {
        Ok(peers)
    }
}

async fn connect_and_handshake(
    addr: SocketAddr,
    magic: [u8; 4],
    start_height: i32,
    peer_ctx: PeerContext,
) -> Result<(SocketAddr, Peer), String> {
    let connect = Peer::connect(
        addr,
        magic,
        peer_ctx.kind,
        Arc::clone(&peer_ctx.registry),
        Arc::clone(&peer_ctx.net_totals),
    );
    let peer = match tokio::time::timeout(
        Duration::from_secs(DEFAULT_CONNECT_TIMEOUT_SECS),
        connect,
    )
    .await
    {
        Ok(Ok(peer)) => peer,
        Ok(Err(err)) => return Err(format!("failed to connect to {addr}: {err}")),
        Err(_) => return Err(format!("connection timed out for {addr}")),
    };

    let mut peer = peer;
    let handshake = tokio::time::timeout(
        Duration::from_secs(DEFAULT_HANDSHAKE_TIMEOUT_SECS),
        peer.handshake(start_height),
    )
    .await;
    match handshake {
        Ok(Ok(())) => Ok((addr, peer)),
        Ok(Err(err)) => Err(format!("handshake failed for {addr}: {err}")),
        Err(_) => Err(format!("handshake timed out for {addr}")),
    }
}

async fn resolve_seed_addresses(params: &ChainParams) -> Result<Vec<SocketAddr>, String> {
    let mut addrs = Vec::new();
    let mut seen = HashSet::new();
    for seed in params.fixed_seeds {
        if let Ok(addr) = seed.parse::<SocketAddr>() {
            if seen.insert(addr) {
                addrs.push(addr);
            }
            continue;
        }
        let host = if seed.contains(':') {
            seed.to_string()
        } else {
            format!("{seed}:{}", params.default_port)
        };
        match tokio::net::lookup_host(host).await {
            Ok(entries) => {
                for addr in entries {
                    if seen.insert(addr) {
                        addrs.push(addr);
                    }
                }
            }
            Err(err) => {
                log_debug!("failed to resolve fixed seed {seed}: {err}");
            }
        }
    }
    for seed in params.dns_seeds {
        let host = format!("{seed}:{}", params.default_port);
        match tokio::net::lookup_host(host).await {
            Ok(entries) => {
                for addr in entries {
                    if seen.insert(addr) {
                        addrs.push(addr);
                    }
                }
            }
            Err(err) => {
                log_debug!("failed to resolve {seed}: {err}");
            }
        }
    }
    addrs.shuffle(&mut rand::thread_rng());
    Ok(addrs)
}

async fn addr_discovery_loop(
    params: Arc<ChainParams>,
    seed_addrs: Arc<Vec<SocketAddr>>,
    addr_book: Arc<AddrBook>,
    start_height: i32,
    peer_ctx: PeerContext,
) -> Result<(), String> {
    let idle_sleep = Duration::from_secs(ADDR_DISCOVERY_INTERVAL_SECS);
    loop {
        let mut candidates = Vec::new();
        let mut seen = HashSet::new();
        for addr in seed_addrs.iter() {
            if seen.insert(*addr) {
                candidates.push(*addr);
            }
        }
        for addr in addr_book.sample(ADDR_DISCOVERY_SAMPLE) {
            if seen.insert(addr) {
                candidates.push(addr);
            }
        }
        if candidates.is_empty() {
            tokio::time::sleep(idle_sleep).await;
            continue;
        }
        candidates.shuffle(&mut rand::thread_rng());
        let probe_count = ADDR_DISCOVERY_PEERS.min(candidates.len());
        let magic = params.message_start;
        let mut join_set = JoinSet::new();
        for addr in candidates.into_iter().take(probe_count) {
            let addr_book = Arc::clone(&addr_book);
            let default_port = params.default_port;
            let peer_ctx = peer_ctx.clone();
            join_set.spawn(async move {
                if let Err(err) = discover_addrs_from_peer(
                    addr,
                    magic,
                    start_height,
                    default_port,
                    addr_book,
                    peer_ctx,
                )
                .await
                {
                    log_debug!("addr discovery failed for {addr}: {err}");
                }
            });
        }
        while join_set.join_next().await.is_some() {}
        tokio::time::sleep(idle_sleep).await;
    }
}

async fn discover_addrs_from_peer(
    addr: SocketAddr,
    magic: [u8; 4],
    start_height: i32,
    default_port: u16,
    addr_book: Arc<AddrBook>,
    peer_ctx: PeerContext,
) -> Result<(), String> {
    addr_book.record_attempt(addr);
    let (_addr, mut peer) = match connect_and_handshake(addr, magic, start_height, peer_ctx).await {
        Ok(value) => value,
        Err(err) => {
            addr_book.record_failure(addr);
            return Err(err);
        }
    };
    addr_book.record_success(addr, &peer);
    peer.send_getaddr().await?;
    let deadline = Instant::now() + Duration::from_secs(ADDR_DISCOVERY_TIMEOUT_SECS);
    let mut new_addrs = Vec::new();
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        let message = tokio::time::timeout(remaining, peer.read_message()).await;
        match message {
            Ok(Ok((command, payload))) => match command.as_str() {
                "addr" => match parse_addr(&payload) {
                    Ok(addrs) => new_addrs
                        .extend(addrs.into_iter().filter(|addr| addr.port() == default_port)),
                    Err(err) => return Err(err),
                },
                "ping" => {
                    peer.send_message("pong", &payload).await?;
                }
                _ => {}
            },
            Ok(Err(err)) => return Err(err),
            Err(_) => break,
        }
        if !new_addrs.is_empty() {
            break;
        }
    }
    let added = addr_book.insert_many(new_addrs);
    if added > 0 {
        log_debug!(
            "Addr discovery: learned {} addrs from {} (book {})",
            added,
            addr,
            addr_book.len()
        );
    }
    Ok(())
}

fn parse_peer_addrs(values: &[String], default_port: u16) -> Result<Vec<SocketAddr>, String> {
    let mut addrs = Vec::new();
    let mut seen = HashSet::new();
    for raw in values {
        let candidate = if raw.contains(':') {
            raw.to_string()
        } else {
            format!("{raw}:{default_port}")
        };
        let addr = candidate
            .parse::<SocketAddr>()
            .map_err(|_| format!("invalid header peer '{raw}'"))?;
        if seen.insert(addr) {
            addrs.push(addr);
        }
    }
    if addrs.is_empty() {
        return Err("no valid header peers provided".to_string());
    }
    Ok(addrs)
}

fn validation_flags(
    shielded_params: Arc<ShieldedParams>,
    check_script: bool,
    metrics: Option<Arc<ValidationMetrics>>,
) -> ValidationFlags {
    ValidationFlags {
        check_pow: true,
        check_pon: true,
        check_script,
        check_shielded: true,
        shielded_params: Some(shielded_params),
        metrics,
    }
}

fn tx_needs_shielded(tx: &Transaction) -> bool {
    !(tx.join_splits.is_empty() && tx.shielded_spends.is_empty() && tx.shielded_outputs.is_empty())
}

fn block_needs_shielded(block: &Block) -> bool {
    block.transactions.iter().any(tx_needs_shielded)
}

fn remove_undo_flatfiles(blocks_path: &Path) -> Result<(), String> {
    let entries = match fs::read_dir(blocks_path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.to_string()),
    };

    let mut removed = 0usize;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        if !entry.file_type().map_err(|err| err.to_string())?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("undo") || !name.ends_with(".dat") {
            continue;
        }
        fs::remove_file(entry.path()).map_err(|err| err.to_string())?;
        removed += 1;
    }

    if removed > 0 {
        log_info!(
            "Removed {removed} undo flatfile(s) under {}",
            blocks_path.display()
        );
    }
    Ok(())
}

fn reindex_blocks_from_flatfiles<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    blocks_path: &Path,
    params: &ChainParams,
    flags: &ValidationFlags,
    write_lock: &Mutex<()>,
) -> Result<(), String> {
    if chainstate
        .best_block()
        .map_err(|err| err.to_string())?
        .is_some()
    {
        return Err("reindex expected empty chainstate (db not wiped?)".to_string());
    }

    log_info!(
        "Reindexing from flatfiles under {} (no network)",
        blocks_path.display()
    );

    let mut connected_blocks: u64 = 0;
    let mut last_progress = Instant::now();
    for file_id in 0u32.. {
        let path = blocks_path.join(format!("data{file_id:05}.dat"));
        if !path.exists() {
            break;
        }
        let mut file = File::open(&path).map_err(|err| err.to_string())?;
        let mut offset: u64 = 0;
        loop {
            let mut len_bytes = [0u8; 4];
            match file.read_exact(&mut len_bytes) {
                Ok(()) => {}
                Err(err) if err.kind() == ErrorKind::UnexpectedEof => break,
                Err(err) => {
                    return Err(format!(
                        "flatfile read failed for {} at offset {}: {err}",
                        path.display(),
                        offset
                    ));
                }
            }
            let len = u32::from_le_bytes(len_bytes);
            if len == 0 {
                return Err(format!(
                    "flatfile record has invalid length 0 ({} offset {})",
                    path.display(),
                    offset
                ));
            }
            if len > fluxd_consensus::constants::MAX_BLOCK_SIZE {
                return Err(format!(
                    "flatfile record length {} exceeds MAX_BLOCK_SIZE ({} offset {})",
                    len,
                    path.display(),
                    offset
                ));
            }

            let mut bytes = vec![0u8; len as usize];
            file.read_exact(&mut bytes).map_err(|err| {
                format!(
                    "flatfile read failed for {} payload (len {}) at offset {}: {err}",
                    path.display(),
                    len,
                    offset
                )
            })?;

            let location = FileLocation {
                file_id,
                offset,
                len,
            };
            offset = offset
                .checked_add(4)
                .and_then(|value| value.checked_add(u64::from(len)))
                .ok_or_else(|| "flatfile offset overflow".to_string())?;

            let block = Block::consensus_decode(&bytes).map_err(|_| {
                format!(
                    "invalid block encoding in {} at offset {}",
                    path.display(),
                    location.offset
                )
            })?;
            let hash = block.header.hash();

            loop {
                match chainstate.best_block().map_err(|err| err.to_string())? {
                    Some(tip) => {
                        if tip.hash == block.header.prev_block {
                            break;
                        }
                        let batch = chainstate
                            .disconnect_block(&tip.hash)
                            .map_err(|err| err.to_string())?;
                        let _guard = write_lock
                            .lock()
                            .map_err(|_| "write lock poisoned".to_string())?;
                        chainstate
                            .commit_batch(batch)
                            .map_err(|err| err.to_string())?;
                    }
                    None => {
                        if block.header.prev_block != [0u8; 32] {
                            return Err(format!(
                                "orphan block {} (prev {}) in {} at offset {}",
                                hash256_to_hex(&hash),
                                hash256_to_hex(&block.header.prev_block),
                                path.display(),
                                location.offset
                            ));
                        }
                        break;
                    }
                }
            }

            let height = chainstate
                .best_block()
                .map_err(|err| err.to_string())?
                .map(|tip| tip.height.saturating_add(1))
                .unwrap_or(0);
            if height == 0 && hash != params.consensus.hash_genesis_block {
                return Err(format!(
                    "genesis hash mismatch in {} at offset {}: got {} expected {}",
                    path.display(),
                    location.offset,
                    hash256_to_hex(&hash),
                    hash256_to_hex(&params.consensus.hash_genesis_block),
                ));
            }

            let batch = chainstate
                .connect_block(
                    &block,
                    height,
                    params,
                    flags,
                    false,
                    None,
                    None,
                    Some(bytes.as_slice()),
                    Some(location),
                )
                .map_err(|err| err.to_string())?;
            let _guard = write_lock
                .lock()
                .map_err(|_| "write lock poisoned".to_string())?;
            chainstate
                .commit_batch(batch)
                .map_err(|err| err.to_string())?;

            connected_blocks = connected_blocks.saturating_add(1);
            if connected_blocks > 0 && connected_blocks % 100_000 == 0 {
                let tip = chainstate
                    .best_block()
                    .map_err(|err| err.to_string())?
                    .map(|tip| tip.height)
                    .unwrap_or(-1);
                log_info!(
                    "Reindex progress: connected {} blocks (tip {}, elapsed {:?})",
                    connected_blocks,
                    tip,
                    last_progress.elapsed()
                );
                last_progress = Instant::now();
            }
        }
    }

    match chainstate.best_block().map_err(|err| err.to_string())? {
        Some(tip) => log_info!(
            "Reindex complete at height {} ({})",
            tip.height,
            hash256_to_hex(&tip.hash)
        ),
        None => log_info!("Reindex complete (no blocks found in flatfiles)"),
    }

    Ok(())
}

fn rebuild_txindex<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    write_lock: &Mutex<()>,
) -> Result<(), String> {
    let Some(best) = chainstate.best_block().map_err(|err| err.to_string())? else {
        log_info!("Tx index rebuild requested but no blocks are present");
        return Ok(());
    };

    log_info!("Rebuilding tx index up to height {}", best.height);
    let mut last_progress = Instant::now();

    for height in 0..=best.height {
        let hash = chainstate
            .height_hash(height)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing height index for height {height}"))?;
        let block_location = chainstate
            .block_location(&hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing block index entry for height {height}"))?;
        let bytes = chainstate
            .read_block(block_location)
            .map_err(|err| err.to_string())?;
        let block =
            Block::consensus_decode(&bytes).map_err(|_| "invalid block encoding".to_string())?;

        let mut batch = WriteBatch::new();
        batch.reserve(block.transactions.len());
        for (tx_index, tx) in block.transactions.iter().enumerate() {
            let txid = tx.txid().map_err(|err| err.to_string())?;
            let location = fluxd_chainstate::txindex::TxLocation {
                block: block_location,
                index: tx_index as u32,
            };
            batch.put(fluxd_storage::Column::TxIndex, txid, location.encode());
        }

        let _guard = write_lock
            .lock()
            .map_err(|_| "write lock poisoned".to_string())?;
        chainstate
            .commit_batch(batch)
            .map_err(|err| err.to_string())?;

        if height > 0 && height % 100_000 == 0 {
            log_info!(
                "Rebuilt tx index at height {} (elapsed {:?})",
                height,
                last_progress.elapsed()
            );
            last_progress = Instant::now();
        }
    }

    let mut version_batch = WriteBatch::new();
    version_batch.put(
        fluxd_storage::Column::Meta,
        TXINDEX_VERSION_KEY,
        TXINDEX_VERSION.to_le_bytes(),
    );
    let _guard = write_lock
        .lock()
        .map_err(|_| "write lock poisoned".to_string())?;
    chainstate
        .commit_batch(version_batch)
        .map_err(|err| err.to_string())?;

    log_info!("Tx index rebuild complete at height {}", best.height);
    Ok(())
}

fn rebuild_spentindex<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    write_lock: &Mutex<()>,
) -> Result<(), String> {
    const TX_CACHE_CAPACITY: usize = 50_000;

    let Some(best) = chainstate.best_block().map_err(|err| err.to_string())? else {
        log_info!("Spent index rebuild requested but no blocks are present");
        return Ok(());
    };

    log_info!("Rebuilding spent index up to height {}", best.height);
    let mut last_progress = Instant::now();
    let mut tx_cache = TxOutCache::new(TX_CACHE_CAPACITY);

    for height in 0..=best.height {
        let hash = chainstate
            .height_hash(height)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing height index for height {height}"))?;
        let block_location = chainstate
            .block_location(&hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing block index entry for height {height}"))?;
        let bytes = chainstate
            .read_block(block_location)
            .map_err(|err| err.to_string())?;
        let block =
            Block::consensus_decode(&bytes).map_err(|_| "invalid block encoding".to_string())?;

        let mut batch = WriteBatch::new();
        let estimated_inputs = block
            .transactions
            .iter()
            .skip(1)
            .map(|tx| tx.vin.len())
            .sum::<usize>();
        let estimated_outputs = block
            .transactions
            .iter()
            .map(|tx| tx.vout.len())
            .sum::<usize>();
        batch.reserve(estimated_inputs.saturating_add(4));

        let mut created: HashMap<fluxd_chainstate::utxo::OutPointKey, CachedTxOut> =
            HashMap::with_capacity(estimated_outputs);
        let mut spent_outpoints: HashSet<fluxd_chainstate::utxo::OutPointKey> =
            HashSet::with_capacity(estimated_inputs);

        for (tx_index, tx) in block.transactions.iter().enumerate() {
            let txid = tx.txid().map_err(|err| err.to_string())?;

            if tx_index != 0 {
                for (input_index, input) in tx.vin.iter().enumerate() {
                    let outpoint_key = fluxd_chainstate::utxo::outpoint_key_bytes(&input.prevout);
                    if !spent_outpoints.insert(outpoint_key) {
                        return Err(format!(
                            "duplicate prevout {}:{} at height {} (tx {})",
                            hash256_to_hex(&input.prevout.hash),
                            input.prevout.index,
                            height,
                            hash256_to_hex(&txid)
                        ));
                    }

                    let prevout = match created.remove(&outpoint_key) {
                        Some(prevout) => prevout,
                        None => resolve_prevout_txout(chainstate, &mut tx_cache, &input.prevout)?,
                    };
                    let details = fluxd_chainstate::spentindex::SpentIndexDetails {
                        satoshis: prevout.value,
                        address_type: prevout.address_type,
                        address_hash: prevout.address_hash,
                    };
                    let value = fluxd_chainstate::spentindex::SpentIndexValue {
                        txid,
                        input_index: input_index as u32,
                        block_height: height as u32,
                        details: Some(details),
                    };
                    batch.put(
                        fluxd_storage::Column::SpentIndex,
                        outpoint_key.as_bytes(),
                        value.encode(),
                    );
                }
            }

            let mut outputs = Vec::with_capacity(tx.vout.len());
            for (out_index, output) in tx.vout.iter().enumerate() {
                let script_hash =
                    fluxd_chainstate::address_index::script_hash(&output.script_pubkey);
                let (address_type, address_hash) = spent_address_info(&output.script_pubkey);
                let out = CachedTxOut {
                    value: output.value,
                    script_hash,
                    address_type,
                    address_hash,
                };
                outputs.push(out);

                let outpoint = OutPoint {
                    hash: txid,
                    index: out_index as u32,
                };
                created.insert(fluxd_chainstate::utxo::outpoint_key_bytes(&outpoint), out);
            }

            tx_cache.insert(txid, outputs);
        }

        let _guard = write_lock
            .lock()
            .map_err(|_| "write lock poisoned".to_string())?;
        chainstate
            .commit_batch(batch)
            .map_err(|err| err.to_string())?;

        if height > 0 && height % 100_000 == 0 {
            log_info!(
                "Rebuilt spent index at height {} (elapsed {:?})",
                height,
                last_progress.elapsed()
            );
            last_progress = Instant::now();
        }
    }

    let mut version_batch = WriteBatch::new();
    version_batch.put(
        fluxd_storage::Column::Meta,
        SPENTINDEX_VERSION_KEY,
        SPENTINDEX_VERSION.to_le_bytes(),
    );
    let _guard = write_lock
        .lock()
        .map_err(|_| "write lock poisoned".to_string())?;
    chainstate
        .commit_batch(version_batch)
        .map_err(|err| err.to_string())?;

    log_info!("Spent index rebuild complete at height {}", best.height);
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct CachedTxOut {
    value: i64,
    script_hash: Option<Hash256>,
    address_type: u32,
    address_hash: [u8; 20],
}

fn spent_address_info(script_pubkey: &[u8]) -> (u32, [u8; 20]) {
    if script_pubkey.len() == 25
        && script_pubkey[0] == 0x76
        && script_pubkey[1] == 0xa9
        && script_pubkey[2] == 0x14
        && script_pubkey[23] == 0x88
        && script_pubkey[24] == 0xac
    {
        let mut hash = [0u8; 20];
        hash.copy_from_slice(&script_pubkey[3..23]);
        return (1, hash);
    }
    if script_pubkey.len() == 23
        && script_pubkey[0] == 0xa9
        && script_pubkey[1] == 0x14
        && script_pubkey[22] == 0x87
    {
        let mut hash = [0u8; 20];
        hash.copy_from_slice(&script_pubkey[2..22]);
        return (2, hash);
    }
    (0, [0u8; 20])
}

struct TxOutCache {
    entries: HashMap<Hash256, Vec<CachedTxOut>>,
    order: VecDeque<Hash256>,
    capacity: usize,
}

impl TxOutCache {
    fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    fn insert(&mut self, txid: Hash256, outputs: Vec<CachedTxOut>) {
        if self.capacity == 0 {
            return;
        }
        if self.entries.contains_key(&txid) {
            self.entries.insert(txid, outputs);
            return;
        }
        self.entries.insert(txid, outputs);
        self.order.push_back(txid);
        while self.entries.len() > self.capacity {
            let Some(evicted) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&evicted);
        }
    }

    fn output(&self, txid: &Hash256, index: u32) -> Option<CachedTxOut> {
        let outputs = self.entries.get(txid)?;
        outputs.get(index as usize).copied()
    }
}

fn address_delta_key(
    script_hash: &Hash256,
    height: u32,
    tx_index: u32,
    txid: &Hash256,
    index: u32,
    spending: bool,
) -> [u8; 77] {
    let mut key = [0u8; 77];
    key[0..32].copy_from_slice(script_hash);
    key[32..36].copy_from_slice(&height.to_be_bytes());
    key[36..40].copy_from_slice(&tx_index.to_be_bytes());
    key[40..72].copy_from_slice(txid);
    key[72..76].copy_from_slice(&index.to_le_bytes());
    key[76] = if spending { 1 } else { 0 };
    key
}

fn rebuild_addressindex<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    write_lock: &Mutex<()>,
) -> Result<(), String> {
    const TX_CACHE_CAPACITY: usize = 50_000;

    let Some(best) = chainstate.best_block().map_err(|err| err.to_string())? else {
        log_info!("Address index rebuild requested but no blocks are present");
        return Ok(());
    };

    log_info!("Rebuilding address indexes up to height {}", best.height);
    let mut last_progress = Instant::now();
    let mut tx_cache = TxOutCache::new(TX_CACHE_CAPACITY);

    #[derive(Clone, Debug)]
    struct CreatedOutput {
        outpoint: OutPoint,
        out: CachedTxOut,
    }

    for height in 0..=best.height {
        let hash = chainstate
            .height_hash(height)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing height index for height {height}"))?;
        let block_location = chainstate
            .block_location(&hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing block index entry for height {height}"))?;
        let bytes = chainstate
            .read_block(block_location)
            .map_err(|err| err.to_string())?;
        let block =
            Block::consensus_decode(&bytes).map_err(|_| "invalid block encoding".to_string())?;

        let estimated_inputs = block
            .transactions
            .iter()
            .skip(1)
            .map(|tx| tx.vin.len())
            .sum::<usize>();
        let estimated_outputs = block
            .transactions
            .iter()
            .map(|tx| tx.vout.len())
            .sum::<usize>();

        let mut batch = WriteBatch::new();
        batch.reserve(
            estimated_inputs
                .saturating_mul(2)
                .saturating_add(estimated_outputs.saturating_mul(2))
                .saturating_add(block.transactions.len()),
        );

        let mut created: HashMap<fluxd_chainstate::utxo::OutPointKey, CreatedOutput> =
            HashMap::with_capacity(estimated_outputs);
        let mut spent_outpoints: HashSet<fluxd_chainstate::utxo::OutPointKey> =
            HashSet::with_capacity(estimated_inputs);

        for (tx_index, tx) in block.transactions.iter().enumerate() {
            let txid = tx.txid().map_err(|err| err.to_string())?;

            if tx_index != 0 {
                for (input_index, input) in tx.vin.iter().enumerate() {
                    let outpoint_key = fluxd_chainstate::utxo::outpoint_key_bytes(&input.prevout);
                    if !spent_outpoints.insert(outpoint_key) {
                        return Err(format!(
                            "duplicate prevout {}:{} at height {} (tx {})",
                            hash256_to_hex(&input.prevout.hash),
                            input.prevout.index,
                            height,
                            hash256_to_hex(&txid)
                        ));
                    }

                    let created_in_block = created.remove(&outpoint_key);
                    let (prev, was_created_in_block) = match created_in_block {
                        Some(created) => (created.out, true),
                        None => (
                            resolve_prevout_txout(chainstate, &mut tx_cache, &input.prevout)?,
                            false,
                        ),
                    };

                    let Some(script_hash) = prev.script_hash else {
                        continue;
                    };
                    if !was_created_in_block {
                        let key =
                            fluxd_chainstate::address_index::address_outpoint_key_with_script_hash(
                                &script_hash,
                                &input.prevout,
                            );
                        batch.delete(fluxd_storage::Column::AddressOutpoint, key);
                    }
                    let satoshis = prev
                        .value
                        .checked_neg()
                        .ok_or_else(|| "prevout value out of range".to_string())?;
                    let delta_key = address_delta_key(
                        &script_hash,
                        height as u32,
                        tx_index as u32,
                        &txid,
                        input_index as u32,
                        true,
                    );
                    batch.put(
                        fluxd_storage::Column::AddressDelta,
                        delta_key,
                        satoshis.to_le_bytes(),
                    );
                }
            }

            let mut outputs = Vec::with_capacity(tx.vout.len());
            for (out_index, output) in tx.vout.iter().enumerate() {
                let script_hash =
                    fluxd_chainstate::address_index::script_hash(&output.script_pubkey);
                let (address_type, address_hash) = spent_address_info(&output.script_pubkey);
                let out = CachedTxOut {
                    value: output.value,
                    script_hash,
                    address_type,
                    address_hash,
                };
                outputs.push(out);

                if let Some(script_hash) = script_hash {
                    let delta_key = address_delta_key(
                        &script_hash,
                        height as u32,
                        tx_index as u32,
                        &txid,
                        out_index as u32,
                        false,
                    );
                    batch.put(
                        fluxd_storage::Column::AddressDelta,
                        delta_key,
                        output.value.to_le_bytes(),
                    );
                }

                let outpoint = OutPoint {
                    hash: txid,
                    index: out_index as u32,
                };
                created.insert(
                    fluxd_chainstate::utxo::outpoint_key_bytes(&outpoint),
                    CreatedOutput { outpoint, out },
                );
            }

            tx_cache.insert(txid, outputs);
        }

        for created_output in created.values() {
            let Some(script_hash) = created_output.out.script_hash else {
                continue;
            };
            let key = fluxd_chainstate::address_index::address_outpoint_key_with_script_hash(
                &script_hash,
                &created_output.outpoint,
            );
            batch.put(fluxd_storage::Column::AddressOutpoint, key, []);
        }

        let _guard = write_lock
            .lock()
            .map_err(|_| "write lock poisoned".to_string())?;
        chainstate
            .commit_batch(batch)
            .map_err(|err| err.to_string())?;

        if height > 0 && height % 100_000 == 0 {
            log_info!(
                "Rebuilt address index at height {} (elapsed {:?})",
                height,
                last_progress.elapsed()
            );
            last_progress = Instant::now();
        }
    }

    let mut version_batch = WriteBatch::new();
    version_batch.put(
        fluxd_storage::Column::Meta,
        ADDRESSINDEX_VERSION_KEY,
        ADDRESSINDEX_VERSION.to_le_bytes(),
    );
    let _guard = write_lock
        .lock()
        .map_err(|_| "write lock poisoned".to_string())?;
    chainstate
        .commit_batch(version_batch)
        .map_err(|err| err.to_string())?;

    log_info!("Address index rebuild complete at height {}", best.height);
    Ok(())
}

fn resolve_prevout_txout<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    tx_cache: &mut TxOutCache,
    prevout: &OutPoint,
) -> Result<CachedTxOut, String> {
    if let Some(cached) = tx_cache.output(&prevout.hash, prevout.index) {
        return Ok(cached);
    }

    let location = chainstate
        .tx_location(&prevout.hash)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| {
            format!(
                "missing tx index entry for prevout {}:{}",
                hash256_to_hex(&prevout.hash),
                prevout.index
            )
        })?;
    let bytes = chainstate
        .read_block(location.block)
        .map_err(|err| err.to_string())?;
    let block =
        Block::consensus_decode(&bytes).map_err(|_| "invalid block encoding".to_string())?;
    let tx = block
        .transactions
        .get(location.index as usize)
        .ok_or_else(|| {
            format!(
                "tx index points beyond block tx list (prevout {})",
                hash256_to_hex(&prevout.hash)
            )
        })?;
    let txid = tx.txid().map_err(|err| err.to_string())?;
    if txid != prevout.hash {
        return Err(format!(
            "tx index mismatch for prevout {}: got txid {}",
            hash256_to_hex(&prevout.hash),
            hash256_to_hex(&txid)
        ));
    }

    let mut outputs = Vec::with_capacity(tx.vout.len());
    for output in &tx.vout {
        let (address_type, address_hash) = spent_address_info(&output.script_pubkey);
        outputs.push(CachedTxOut {
            value: output.value,
            script_hash: fluxd_chainstate::address_index::script_hash(&output.script_pubkey),
            address_type,
            address_hash,
        });
    }

    let out = outputs
        .get(prevout.index as usize)
        .copied()
        .ok_or_else(|| {
            format!(
                "prevout {}:{} refers to missing vout",
                hash256_to_hex(&prevout.hash),
                prevout.index
            )
        })?;
    tx_cache.insert(txid, outputs);
    Ok(out)
}

fn ensure_genesis<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
    flags: &ValidationFlags,
    connect_metrics: Option<&ConnectMetrics>,
    write_lock: &Mutex<()>,
) -> Result<(), String> {
    if chainstate
        .best_block()
        .map_err(|err| err.to_string())?
        .is_some()
    {
        return Ok(());
    }

    let genesis = build_genesis_block(params)?;
    let batch = chainstate
        .connect_block(
            &genesis,
            0,
            params,
            flags,
            false,
            None,
            connect_metrics,
            None,
            None,
        )
        .map_err(|err| err.to_string())?;
    let _guard = write_lock
        .lock()
        .map_err(|_| "write lock poisoned".to_string())?;
    chainstate
        .commit_batch(batch)
        .map_err(|err| err.to_string())?;
    log_info!("Inserted genesis block");
    Ok(())
}

fn build_genesis_block(params: &ChainParams) -> Result<Block, String> {
    let (nonce_hex, solution_hex, bits) = match params.network {
        Network::Mainnet => (
            GENESIS_MAINNET_NONCE_HEX,
            GENESIS_MAINNET_SOLUTION_HEX,
            GENESIS_MAINNET_BITS,
        ),
        Network::Testnet => (
            GENESIS_TESTNET_NONCE_HEX,
            GENESIS_TESTNET_SOLUTION_HEX,
            GENESIS_TESTNET_BITS,
        ),
        Network::Regtest => (
            GENESIS_REGTEST_NONCE_HEX,
            GENESIS_REGTEST_SOLUTION_HEX,
            GENESIS_REGTEST_BITS,
        ),
    };

    let nonce = hash256_from_hex(nonce_hex).map_err(|_| "invalid genesis nonce".to_string())?;
    let solution = decode_hex(solution_hex)?;
    let script_sig = genesis_script_sig();
    let script_pubkey = genesis_script_pubkey()?;

    let tx = Transaction {
        f_overwintered: false,
        version: 1,
        version_group_id: 0,
        vin: vec![TxIn {
            prevout: OutPoint::null(),
            script_sig,
            sequence: u32::MAX,
        }],
        vout: vec![TxOut {
            value: 0,
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

    let txid = tx.txid().map_err(|err| err.to_string())?;
    let txid_hex = hash256_to_hex(&txid);
    let header = BlockHeader {
        version: CURRENT_VERSION,
        prev_block: [0u8; 32],
        merkle_root: txid,
        final_sapling_root: [0u8; 32],
        time: params.consensus.genesis_time,
        bits,
        nonce,
        solution,
        nodes_collateral: OutPoint::null(),
        block_sig: Vec::new(),
    };
    let block = Block {
        header,
        transactions: vec![tx],
    };

    let actual_hash = block.header.hash();
    if actual_hash != params.consensus.hash_genesis_block {
        return Err(format!(
            "genesis hash mismatch (expected {}, got {}, txid {})",
            hash256_to_hex(&params.consensus.hash_genesis_block),
            hash256_to_hex(&actual_hash),
            txid_hex
        ));
    }

    Ok(block)
}

fn genesis_script_sig() -> Vec<u8> {
    let mut script = Vec::new();
    push_data(&mut script, &script_num_to_vec(520617983));
    push_data(&mut script, &script_num_to_vec(4));
    push_data(&mut script, GENESIS_TIMESTAMP.as_bytes());
    script
}

fn genesis_script_pubkey() -> Result<Vec<u8>, String> {
    let pubkey = decode_hex(GENESIS_PUBKEY_HEX)?;
    let mut script = Vec::with_capacity(pubkey.len() + 2);
    push_data(&mut script, &pubkey);
    script.push(0xac);
    Ok(script)
}

fn push_data(script: &mut Vec<u8>, data: &[u8]) {
    match data.len() {
        0..=75 => script.push(data.len() as u8),
        76..=0xff => {
            script.push(0x4c);
            script.push(data.len() as u8);
        }
        0x100..=0xffff => {
            script.push(0x4d);
            script.extend_from_slice(&(data.len() as u16).to_le_bytes());
        }
        _ => {
            script.push(0x4e);
            script.extend_from_slice(&(data.len() as u32).to_le_bytes());
        }
    }
    script.extend_from_slice(data);
}

fn script_num_to_vec(value: i64) -> Vec<u8> {
    if value == 0 {
        return Vec::new();
    }
    let mut abs = value.unsigned_abs();
    let mut result = Vec::new();
    while abs > 0 {
        result.push((abs & 0xff) as u8);
        abs >>= 8;
    }
    let sign_bit = 0x80u8;
    if let Some(last) = result.last_mut() {
        if (*last & sign_bit) != 0 {
            result.push(if value < 0 { sign_bit } else { 0 });
        } else if value < 0 {
            *last |= sign_bit;
        }
    }
    result
}

fn decode_hex(input: &str) -> Result<Vec<u8>, String> {
    let mut hex = input.trim();
    if let Some(stripped) = hex.strip_prefix("0x").or_else(|| hex.strip_prefix("0X")) {
        hex = stripped;
    }

    if hex.is_empty() {
        return Err("empty hex string".to_string());
    }

    let mut owned = String::new();
    if hex.len() % 2 == 1 {
        owned.push('0');
        owned.push_str(hex);
        hex = owned.as_str();
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for i in (0..hex.len()).step_by(2) {
        let byte =
            u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| "invalid hex string".to_string())?;
        bytes.push(byte);
    }
    Ok(bytes)
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{:02x}", byte);
    }
    out
}

#[allow(clippy::too_many_arguments)]
async fn header_sync_loop<S: KeyValueStore + Send + Sync + 'static>(
    chainstate: Arc<ChainState<S>>,
    params: Arc<ChainParams>,
    seed_addrs: Arc<Vec<SocketAddr>>,
    addr_book: Arc<AddrBook>,
    allow_addr_book: bool,
    peer_book: Arc<HeaderPeerBook>,
    header_tx: mpsc::Sender<Vec<BlockHeader>>,
    header_lead: i32,
    header_peers: usize,
    header_metrics: Arc<HeaderMetrics>,
    peer_ctx: PeerContext,
) -> Result<(), String> {
    let idle_sleep = Duration::from_secs(IDLE_SLEEP_SECS);
    let mut download_state = HeaderDownloadState::new(chainstate.as_ref(), params.as_ref())?;
    loop {
        if let Err(err) = header_peer_loop(
            Arc::clone(&chainstate),
            params.clone(),
            Arc::clone(&seed_addrs),
            Arc::clone(&addr_book),
            allow_addr_book,
            Arc::clone(&peer_book),
            header_tx.clone(),
            header_lead,
            header_peers,
            &mut download_state,
            Arc::clone(&header_metrics),
            peer_ctx.clone(),
        )
        .await
        {
            log_warn!("header worker stopped: {err}");
        }
        tokio::time::sleep(idle_sleep).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn header_peer_loop<S: KeyValueStore + Send + Sync + 'static>(
    chainstate: Arc<ChainState<S>>,
    params: Arc<ChainParams>,
    seed_addrs: Arc<Vec<SocketAddr>>,
    addr_book: Arc<AddrBook>,
    allow_addr_book: bool,
    peer_book: Arc<HeaderPeerBook>,
    header_tx: mpsc::Sender<Vec<BlockHeader>>,
    header_lead: i32,
    header_peers: usize,
    download_state: &mut HeaderDownloadState,
    header_metrics: Arc<HeaderMetrics>,
    peer_ctx: PeerContext,
) -> Result<(), String> {
    let idle_sleep = Duration::from_secs(IDLE_SLEEP_SECS);
    loop {
        let height = match start_height(&chainstate) {
            Ok(height) => height,
            Err(err) => {
                log_warn!("header sync start height failed: {err}");
                tokio::time::sleep(idle_sleep).await;
                continue;
            }
        };
        let preferred = peer_book.preferred(HEADER_PEER_PROBE_COUNT);
        let addr_book_opt = if allow_addr_book {
            Some(addr_book.as_ref())
        } else {
            None
        };
        let mut peer = match connect_to_cached_seed(
            &params,
            &seed_addrs,
            &preferred,
            header_peers,
            height,
            addr_book_opt,
            Some(peer_book.as_ref()),
            &peer_ctx,
        )
        .await
        {
            Ok(peer) => peer,
            Err(err) => {
                log_warn!("header peer connect failed: {err}");
                tokio::time::sleep(idle_sleep).await;
                continue;
            }
        };
        log_debug!("Header peer handshake complete");
        log_debug!("Header peer height {}", peer.remote_height());
        log_debug!(
            "Header peer version {} ua {}",
            peer.remote_version(),
            peer.remote_user_agent()
        );
        let peer_addr = peer.addr();
        let mut last_headers_at = Instant::now();
        let mut timeout_failures = 0usize;
        let mut probing = true;

        loop {
            if peer_ctx.registry.take_disconnect_request(peer_addr) {
                log_debug!("Disconnect requested for header peer {peer_addr}; reconnecting");
                break;
            }
            let remote_height = peer.remote_height();
            if let Some(best_header) = chainstate.best_header().map_err(|err| err.to_string())? {
                if best_header.height > download_state.tip_height {
                    download_state.reset(chainstate.as_ref(), &params)?;
                }
            }
            let best_block_height = chainstate
                .best_block()
                .map_err(|err| err.to_string())?
                .map(|tip| tip.height)
                .unwrap_or(-1);
            let fetch_gap = download_state.tip_height.saturating_sub(best_block_height);
            let behind = if remote_height > 0 {
                remote_height > download_state.tip_height
            } else {
                fetch_gap > 0
            };
            if remote_height > 0 && remote_height < download_state.tip_height {
                let lag = download_state.tip_height.saturating_sub(remote_height);
                if lag > HEADER_BEHIND_BAN_THRESHOLD {
                    peer_book.ban_for(peer_addr, HEADER_BEHIND_BAN_SECS);
                }
                peer_book.record_failure(peer_addr);
                addr_book.record_failure(peer_addr);
                log_debug!(
                    "header peer behind (remote {} < tip {}), reconnecting",
                    remote_height,
                    download_state.tip_height
                );
                break;
            }
            let should_fetch_headers = header_lead == 0 || fetch_gap < header_lead;
            if !should_fetch_headers {
                tokio::time::sleep(idle_sleep).await;
                continue;
            }

            let locator = match build_download_locator(&chainstate, &params, download_state) {
                Ok(value) => value,
                Err(err) => {
                    log_warn!("header locator failed: {err}");
                    tokio::time::sleep(idle_sleep).await;
                    continue;
                }
            };

            let headers_timeout = if probing {
                HEADERS_TIMEOUT_SECS_PROBE
            } else if behind {
                HEADERS_TIMEOUT_SECS_BEHIND
            } else {
                HEADERS_TIMEOUT_SECS_IDLE
            };
            let request_start = Instant::now();
            let headers_result = tokio::time::timeout(
                Duration::from_secs(headers_timeout),
                request_headers(&mut peer, &locator),
            )
            .await;
            match headers_result {
                Ok(Ok(headers)) => {
                    header_metrics.record_request(1, request_start.elapsed());
                    timeout_failures = 0;
                    if headers.is_empty() {
                        if behind {
                            log_warn!("header peer returned no headers while behind");
                            peer_book.record_failure(peer_addr);
                            addr_book.record_failure(peer_addr);
                        } else if last_headers_at.elapsed()
                            > Duration::from_secs(HEADER_IDLE_REPROBE_SECS)
                        {
                            log_warn!(
                                "header peer idle at height {} for {:?}; reconnecting",
                                download_state.tip_height,
                                last_headers_at.elapsed()
                            );
                            peer_book.record_failure(peer_addr);
                            addr_book.record_failure(peer_addr);
                            break;
                        }
                        tokio::time::sleep(idle_sleep).await;
                        continue;
                    }
                    peer_book.record_success(peer_addr);
                    if !headers_are_contiguous(&headers) {
                        log_warn!("non-continuous headers sequence from peer");
                        peer_book.record_bad_chain(peer_addr, HEADER_BAD_CHAIN_BAN_SECS);
                        addr_book.record_failure(peer_addr);
                        break;
                    }
                    if headers[0].prev_block != download_state.tip_hash {
                        let prev = headers[0].prev_block;
                        let prev_entry = download_state
                            .pending
                            .get(&prev)
                            .cloned()
                            .or_else(|| chainstate.header_entry(&prev).ok().flatten());
                        if let Some(entry) = prev_entry {
                            log_debug!(
                            "header batch forks from tip {}; switching to ancestor {} at height {}",
                            hash256_to_hex(&download_state.tip_hash),
                            hash256_to_hex(&prev),
                            entry.height
                        );
                            download_state.tip_hash = prev;
                            download_state.tip_height = entry.height;
                            download_state.pending.clear();
                            download_state.cache = HeaderValidationCache::default();
                        } else {
                            log_warn!(
                                "header batch does not connect to known header {}; resetting",
                                hash256_to_hex(&prev)
                            );
                            download_state.reset(chainstate.as_ref(), &params)?;
                            peer_book.record_bad_chain(peer_addr, HEADER_BAD_CHAIN_BAN_SECS);
                            addr_book.record_failure(peer_addr);
                            break;
                        }
                    }
                    let validate_start = Instant::now();
                    if let Err(err) = chainstate.validate_headers_batch_with_cache(
                        &headers,
                        &params.consensus,
                        &mut download_state.pending,
                        false, // skip PoW here; commit loop validates in parallel
                        &mut download_state.cache,
                    ) {
                        log_warn!("header validation failed: {err}");
                        download_state.reset(chainstate.as_ref(), &params)?;
                        peer_book.record_failure(peer_addr);
                        addr_book.record_failure(peer_addr);
                        break;
                    }
                    header_metrics.record_validate(headers.len() as u64, validate_start.elapsed());
                    if let Some(last) = headers.last() {
                        let hash = last.hash();
                        if let Some(entry) = download_state.pending.get(&hash) {
                            download_state.tip_hash = hash;
                            download_state.tip_height = entry.height;
                        }
                    }
                    peer.bump_remote_height(download_state.tip_height);
                    addr_book.record_success(peer_addr, &peer);
                    probing = false;
                    last_headers_at = Instant::now();
                    log_debug!("Received {} headers", headers.len());
                    if header_tx.send(headers).await.is_err() {
                        return Ok(());
                    }
                }
                Ok(Err(err)) => {
                    if behind {
                        log_debug!("header request failed: {err}");
                    }
                    peer_book.record_failure(peer_addr);
                    addr_book.record_failure(peer_addr);
                    break;
                }
                Err(_) => {
                    if behind {
                        log_debug!("header request timed out");
                    }
                    peer_book.record_failure(peer_addr);
                    addr_book.record_failure(peer_addr);
                    timeout_failures = timeout_failures.saturating_add(1);
                    if behind && timeout_failures >= HEADER_TIMEOUT_RETRIES_BEHIND {
                        log_warn!("header peer timed out while behind; reconnecting");
                        break;
                    }
                    let stall_limit = HEADER_STALL_SECS_IDLE;
                    if timeout_failures >= HEADER_TIMEOUT_RETRIES_IDLE
                        && last_headers_at.elapsed() > Duration::from_secs(stall_limit)
                    {
                        log_warn!(
                            "header peer stalled ({} timeouts, last headers {:?} ago), reconnecting",
                            timeout_failures,
                            last_headers_at.elapsed()
                        );
                        break;
                    }
                    tokio::time::sleep(idle_sleep).await;
                    continue;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn connect_to_cached_seed(
    params: &ChainParams,
    seed_addrs: &Arc<Vec<SocketAddr>>,
    preferred_addrs: &[SocketAddr],
    target_peers: usize,
    start_height: i32,
    addr_book: Option<&AddrBook>,
    peer_book: Option<&HeaderPeerBook>,
    peer_ctx: &PeerContext,
) -> Result<Peer, String> {
    let is_allowed = |addr: SocketAddr| peer_book.map(|book| !book.is_banned(addr)).unwrap_or(true);
    let mut preferred_candidates = Vec::new();
    let mut seen = HashSet::new();
    for addr in preferred_addrs {
        if seen.insert(*addr) && is_allowed(*addr) {
            preferred_candidates.push(*addr);
        }
    }
    if !preferred_candidates.is_empty() {
        preferred_candidates.shuffle(&mut rand::thread_rng());
        let peers = connect_to_candidates(
            &preferred_candidates,
            params.message_start,
            start_height,
            1,
            addr_book,
            peer_ctx,
        )
        .await;
        if let Some(peer) = pick_best_height_peer(peers) {
            return Ok(peer);
        }
    }

    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    for addr in seed_addrs.iter() {
        if seen.insert(*addr) && is_allowed(*addr) {
            candidates.push(*addr);
        }
    }
    if let Some(addr_book) = addr_book {
        for addr in addr_book.sample_for_height(ADDR_BOOK_SAMPLE, start_height) {
            if seen.insert(addr) && is_allowed(addr) {
                candidates.push(addr);
            }
        }
    }
    if candidates.is_empty() {
        return Err("no cached peer addresses available".to_string());
    }
    candidates.shuffle(&mut rand::thread_rng());
    let peers = connect_to_candidates(
        &candidates,
        params.message_start,
        start_height,
        target_peers,
        addr_book,
        peer_ctx,
    )
    .await;
    pick_best_height_peer(peers).ok_or_else(|| "unable to connect to any cached seed".to_string())
}

async fn connect_to_candidates(
    candidates: &[SocketAddr],
    magic: [u8; 4],
    start_height: i32,
    target_peers: usize,
    addr_book: Option<&AddrBook>,
    peer_ctx: &PeerContext,
) -> Vec<Peer> {
    if candidates.is_empty() {
        return Vec::new();
    }
    let probe_count = candidates.len();
    let target_peers = target_peers.max(1).min(probe_count);
    let attempt_target = probe_count.min(target_peers.saturating_mul(8).max(target_peers));
    let max_parallel = attempt_target.clamp(1, 8);
    let mut join_set = JoinSet::new();
    let mut next_index = 0usize;
    let mut peers = Vec::new();
    let mut failures = 0usize;
    let mut failures_logged = 0usize;
    const MAX_CONNECT_ERRORS_LOGGED: usize = 4;

    while next_index < attempt_target && join_set.len() < max_parallel {
        let addr = candidates[next_index];
        let peer_ctx = peer_ctx.clone();
        if let Some(addr_book) = addr_book {
            addr_book.record_attempt(addr);
        }
        join_set.spawn(async move {
            let result = connect_and_handshake(addr, magic, start_height, peer_ctx).await;
            (addr, result.map(|(_addr, peer)| peer))
        });
        next_index += 1;
    }

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok((addr, Ok(peer))) => {
                if let Some(addr_book) = addr_book {
                    addr_book.record_success(addr, &peer);
                }
                peers.push(peer);
            }
            Ok((addr, Err(err))) => {
                if let Some(addr_book) = addr_book {
                    addr_book.record_failure(addr);
                }
                failures = failures.saturating_add(1);
                if failures_logged < MAX_CONNECT_ERRORS_LOGGED {
                    log_debug!("{err}");
                    failures_logged += 1;
                }
            }
            Err(err) => log_warn!("peer task failed: {err}"),
        }

        if next_index < attempt_target {
            let addr = candidates[next_index];
            let peer_ctx = peer_ctx.clone();
            if let Some(addr_book) = addr_book {
                addr_book.record_attempt(addr);
            }
            join_set.spawn(async move {
                let result = connect_and_handshake(addr, magic, start_height, peer_ctx).await;
                (addr, result.map(|(_addr, peer)| peer))
            });
            next_index += 1;
        }
    }

    if failures > failures_logged {
        log_debug!(
            "peer connect: {} additional failure(s) suppressed",
            failures - failures_logged
        );
    }

    peers
}

fn pick_best_height_peer(mut peers: Vec<Peer>) -> Option<Peer> {
    if peers.is_empty() {
        return None;
    }
    let max_height = peers
        .iter()
        .map(|peer| peer.remote_height())
        .max()
        .unwrap_or(-1);
    let mut top_peers: Vec<Peer> = peers
        .drain(..)
        .filter(|peer| peer.remote_height() == max_height)
        .collect();
    top_peers.shuffle(&mut rand::thread_rng());
    top_peers.pop()
}
fn init_header_cursor<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
) -> Result<HeaderCursor, String> {
    let best = chainstate.best_header().map_err(|err| err.to_string())?;
    let (tip_hash, tip_height) = if let Some(tip) = best {
        (Some(tip.hash), Some(tip.height))
    } else {
        (Some(params.consensus.hash_genesis_block), Some(0))
    };
    Ok(HeaderCursor {
        tip_hash,
        tip_height,
        generation: 0,
    })
}

fn header_entry_from_pending_or_db<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    pending: &HashMap<Hash256, HeaderEntry>,
    hash: &Hash256,
) -> Result<HeaderEntry, String> {
    if let Some(entry) = pending.get(hash) {
        return Ok(entry.clone());
    }
    chainstate
        .header_entry(hash)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "missing header entry while building locator".to_string())
}

fn build_download_locator<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
    state: &HeaderDownloadState,
) -> Result<Vec<Hash256>, String> {
    if state.tip_hash != params.consensus.hash_genesis_block
        && !state.pending.contains_key(&state.tip_hash)
        && chainstate
            .header_entry(&state.tip_hash)
            .map_err(|err| err.to_string())?
            .is_none()
    {
        return build_locator(chainstate, &params.consensus.hash_genesis_block);
    }

    let mut locator = Vec::new();
    let mut hash = state.tip_hash;
    let mut height = state.tip_height;
    let mut step: i32 = 1;
    let mut walked: usize = 0;

    loop {
        locator.push(hash);
        if height == 0 {
            break;
        }
        let mut back = step;
        while back > 0 && height > 0 {
            if walked >= HEADER_LOCATOR_MAX_WALK {
                break;
            }
            let entry = header_entry_from_pending_or_db(chainstate, &state.pending, &hash)?;
            hash = entry.prev_hash;
            height -= 1;
            back -= 1;
            walked = walked.saturating_add(1);
        }
        if walked >= HEADER_LOCATOR_MAX_WALK {
            break;
        }
        if locator.len() > 10 {
            step = step.saturating_mul(2);
        }
    }

    if locator.last() != Some(&params.consensus.hash_genesis_block) {
        locator.push(params.consensus.hash_genesis_block);
    }

    Ok(locator)
}

fn cap_header_gap<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    header_lead: i32,
    write_lock: &Mutex<()>,
    cursor: &Arc<Mutex<HeaderCursor>>,
) -> Result<(), String> {
    if header_lead <= 0 {
        return Ok(());
    }

    let best_header = match chainstate.best_header().map_err(|err| err.to_string())? {
        Some(tip) => tip,
        None => return Ok(()),
    };
    let best_block_height = chainstate
        .best_block()
        .map_err(|err| err.to_string())?
        .map(|tip| tip.height)
        .unwrap_or(-1);
    let gap = best_header.height.saturating_sub(best_block_height);
    if gap <= header_lead {
        return Ok(());
    }

    let target_height = best_block_height.saturating_add(header_lead);
    let mut hash = best_header.hash;
    loop {
        let entry = chainstate
            .header_entry(&hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing header entry while capping header lead".to_string())?;
        if entry.height <= target_height {
            let _guard = write_lock
                .lock()
                .map_err(|_| "write lock poisoned".to_string())?;
            chainstate
                .set_best_header(&hash)
                .map_err(|err| err.to_string())?;
            if let Ok(mut cursor) = cursor.lock() {
                cursor.tip_hash = Some(hash);
                cursor.tip_height = Some(entry.height);
                cursor.generation = cursor.generation.saturating_add(1);
            }
            log_info!(
                "Capped header lead at height {} (gap {})",
                entry.height,
                header_lead
            );
            break;
        }
        if entry.height == 0 {
            break;
        }
        hash = entry.prev_hash;
    }

    Ok(())
}

fn headers_are_contiguous(headers: &[BlockHeader]) -> bool {
    headers
        .windows(2)
        .all(|pair| pair[1].prev_block == pair[0].hash())
}

fn commit_headers_batch<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
    headers: &[BlockHeader],
    header_verify_workers: usize,
    header_metrics: &HeaderMetrics,
) -> Result<(), String> {
    if headers.is_empty() {
        return Ok(());
    }
    if header_verify_workers > 1 {
        let pow_start = Instant::now();
        prevalidate_pow_headers(chainstate, params, headers, header_verify_workers)?;
        header_metrics.record_pow(headers.len() as u64, pow_start.elapsed());
    }
    let commit_start = Instant::now();
    let mut batch = WriteBatch::new();
    let entries = if header_verify_workers > 1 {
        chainstate
            .insert_headers_batch_with_pow(headers, &params.consensus, &mut batch, false)
            .map_err(|err| err.to_string())?
    } else {
        chainstate
            .insert_headers_batch(headers, &params.consensus, &mut batch)
            .map_err(|err| err.to_string())?
    };
    chainstate
        .commit_batch(batch)
        .map_err(|err| err.to_string())?;
    header_metrics.record_commit(headers.len() as u64, commit_start.elapsed());
    log_debug!("Committed {} headers", headers.len());
    if let Some((_, last_entry)) = entries.last() {
        if let Ok(Some(best)) = chainstate.best_header() {
            if last_entry.height > best.height {
                let last_hash = headers.last().map(|header| header.hash());
                let work_cmp = match last_entry.chainwork.cmp(&best.chainwork) {
                    Ordering::Greater => "gt",
                    Ordering::Equal => "eq",
                    Ordering::Less => "lt",
                };
                log_warn!(
                    "header chainwork behind: last {} {} bits {:#x} work {} ({}) best {} {} work {}",
                    last_entry.height,
                    last_hash
                        .as_ref()
                        .map(hash256_to_hex)
                        .unwrap_or_else(|| "-".to_string()),
                    last_entry.bits,
                    bytes_to_hex(&last_entry.chainwork),
                    work_cmp,
                    best.height,
                    hash256_to_hex(&best.hash),
                    bytes_to_hex(&best.chainwork)
                );
            }
        }
    }
    Ok(())
}

fn prevalidate_pow_headers<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
    headers: &[BlockHeader],
    workers: usize,
) -> Result<(), String> {
    if headers.is_empty() {
        return Ok(());
    }

    let mut pending_heights: HashMap<Hash256, i32> = HashMap::new();
    let mut jobs: Vec<(&BlockHeader, i32)> = Vec::new();

    for header in headers {
        let hash = header.hash();
        if let Some(entry) = chainstate
            .header_entry(&hash)
            .map_err(|err| err.to_string())?
        {
            pending_heights.insert(hash, entry.height);
            continue;
        }

        let height =
            if header.prev_block == [0u8; 32] && hash == params.consensus.hash_genesis_block {
                0
            } else {
                let prev_height = if let Some(height) = pending_heights.get(&header.prev_block) {
                    *height
                } else if let Some(entry) = chainstate
                    .header_entry(&header.prev_block)
                    .map_err(|err| err.to_string())?
                {
                    entry.height
                } else {
                    return Err("missing header entry while prevalidating pow".to_string());
                };
                prev_height + 1
            };

        pending_heights.insert(hash, height);
        if !header.is_pon() {
            jobs.push((header, height));
        }
    }

    if jobs.is_empty() {
        return Ok(());
    }

    if workers <= 1 {
        for (header, height) in jobs {
            pow_validation::validate_pow_header(header, height, &params.consensus)
                .map_err(|err| err.to_string())?;
        }
        return Ok(());
    }

    let threads = workers.min(jobs.len());
    let next = AtomicUsize::new(0);
    let error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    thread::scope(|scope| {
        for _ in 0..threads {
            let error = Arc::clone(&error);
            let next = &next;
            let jobs = &jobs;
            scope.spawn(move || loop {
                if let Ok(guard) = error.lock() {
                    if guard.is_some() {
                        break;
                    }
                }
                let index = next.fetch_add(1, AtomicOrdering::SeqCst);
                if index >= jobs.len() {
                    break;
                }
                let (header, height) = jobs[index];
                if let Err(err) =
                    pow_validation::validate_pow_header(header, height, &params.consensus)
                {
                    if let Ok(mut guard) = error.lock() {
                        if guard.is_none() {
                            *guard = Some(err.to_string());
                        }
                    }
                    break;
                }
            });
        }
    });

    if let Ok(guard) = error.lock() {
        if let Some(err) = guard.clone() {
            return Err(format!("pow prevalidation failed: {err}"));
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn header_commit_loop<S: KeyValueStore + Send + Sync + 'static>(
    mut header_rx: mpsc::Receiver<Vec<BlockHeader>>,
    chainstate: Arc<ChainState<S>>,
    params: Arc<ChainParams>,
    write_lock: Arc<Mutex<()>>,
    header_lead: i32,
    header_verify_workers: usize,
    header_cursor: Arc<Mutex<HeaderCursor>>,
    header_metrics: Arc<HeaderMetrics>,
) -> Result<(), String> {
    let mut pending: HashMap<Hash256, Vec<BlockHeader>> = HashMap::new();
    while let Some(headers) = header_rx.recv().await {
        if headers.is_empty() {
            continue;
        }
        queue_or_commit_headers(
            chainstate.as_ref(),
            params.as_ref(),
            &write_lock,
            &mut pending,
            header_lead,
            header_verify_workers,
            &header_metrics,
            headers,
        )?;
        drain_ready_header_batches(
            chainstate.as_ref(),
            params.as_ref(),
            &write_lock,
            &mut pending,
            header_lead,
            header_verify_workers,
            &header_metrics,
        )?;
        refresh_header_cursor(chainstate.as_ref(), &header_cursor)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn queue_or_commit_headers<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
    write_lock: &Mutex<()>,
    pending: &mut HashMap<Hash256, Vec<BlockHeader>>,
    header_lead: i32,
    header_verify_workers: usize,
    header_metrics: &HeaderMetrics,
    headers: Vec<BlockHeader>,
) -> Result<(), String> {
    if headers.is_empty() {
        return Ok(());
    }
    let prev = headers[0].prev_block;
    let prev_exists = if prev == params.consensus.hash_genesis_block {
        true
    } else {
        chainstate
            .header_entry(&prev)
            .map_err(|err| err.to_string())?
            .is_some()
    };
    if !prev_exists {
        if pending.is_empty() {
            log_debug!("header batch queued (prev missing)");
        }
        pending.entry(prev).or_insert(headers);
        return Ok(());
    }

    let (commit_headers, remainder) =
        split_headers_by_lead(chainstate, params, header_lead, prev, headers)?;
    if commit_headers.is_empty() {
        log_debug!("header lead clamp: nothing committed");
        if let Some(queued) = remainder {
            pending.entry(prev).or_insert(queued);
        }
        return Ok(());
    }

    let _guard = write_lock
        .lock()
        .map_err(|_| "write lock poisoned".to_string())?;
    commit_headers_batch(
        chainstate,
        params,
        &commit_headers,
        header_verify_workers,
        header_metrics,
    )?;
    if let Some(queued) = remainder {
        if let Some(last) = commit_headers.last() {
            pending.entry(last.hash()).or_insert(queued);
        }
    }
    Ok(())
}

fn drain_ready_header_batches<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
    write_lock: &Mutex<()>,
    pending: &mut HashMap<Hash256, Vec<BlockHeader>>,
    header_lead: i32,
    header_verify_workers: usize,
    header_metrics: &HeaderMetrics,
) -> Result<(), String> {
    loop {
        let ready: Vec<Hash256> = pending
            .iter()
            .filter_map(|(prev, _)| {
                let exists = if *prev == params.consensus.hash_genesis_block {
                    true
                } else {
                    chainstate.header_entry(prev).ok().flatten().is_some()
                };
                if exists {
                    Some(*prev)
                } else {
                    None
                }
            })
            .collect();
        if ready.is_empty() {
            break;
        }
        let mut did_commit = false;
        for prev in ready {
            if let Some(headers) = pending.remove(&prev) {
                let (commit_headers, remainder) =
                    split_headers_by_lead(chainstate, params, header_lead, prev, headers)?;
                if commit_headers.is_empty() {
                    if let Some(queued) = remainder {
                        pending.entry(prev).or_insert(queued);
                    }
                    continue;
                }
                let _guard = write_lock
                    .lock()
                    .map_err(|_| "write lock poisoned".to_string())?;
                commit_headers_batch(
                    chainstate,
                    params,
                    &commit_headers,
                    header_verify_workers,
                    header_metrics,
                )?;
                if let Some(queued) = remainder {
                    if let Some(last) = commit_headers.last() {
                        pending.entry(last.hash()).or_insert(queued);
                    }
                }
                did_commit = true;
            }
        }
        if !did_commit {
            break;
        }
    }
    Ok(())
}

fn split_headers_by_lead<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    params: &ChainParams,
    header_lead: i32,
    prev_hash: Hash256,
    mut headers: Vec<BlockHeader>,
) -> Result<(Vec<BlockHeader>, Option<Vec<BlockHeader>>), String> {
    if headers.is_empty() {
        return Ok((headers, None));
    }
    if header_lead <= 0 {
        return Ok((headers, None));
    }

    let best_block_height = chainstate
        .best_block()
        .map_err(|err| err.to_string())?
        .map(|tip| tip.height)
        .unwrap_or(-1);
    let max_height = best_block_height.saturating_add(header_lead);
    let prev_height = if prev_hash == params.consensus.hash_genesis_block {
        -1
    } else {
        chainstate
            .header_entry(&prev_hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing header entry while trimming headers".to_string())?
            .height
    };
    let allowed = max_height.saturating_sub(prev_height);
    if allowed <= 0 {
        return Ok((Vec::new(), Some(headers)));
    }
    let allowed = allowed as usize;
    if allowed >= headers.len() {
        return Ok((headers, None));
    }
    let remainder = headers.split_off(allowed);
    Ok((headers, Some(remainder)))
}

fn refresh_header_cursor<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    cursor: &Arc<Mutex<HeaderCursor>>,
) -> Result<(), String> {
    let best = match chainstate.best_header().map_err(|err| err.to_string())? {
        Some(tip) => tip,
        None => return Ok(()),
    };
    let mut cursor = match cursor.lock() {
        Ok(cursor) => cursor,
        Err(_) => return Ok(()),
    };
    if cursor.tip_hash == Some(best.hash) && cursor.tip_height == Some(best.height) {
        return Ok(());
    }
    let cursor_height = cursor.tip_height.unwrap_or(i32::MIN);
    if cursor_height > best.height {
        return Ok(());
    }
    cursor.tip_hash = Some(best.hash);
    cursor.tip_height = Some(best.height);
    cursor.generation = cursor.generation.saturating_add(1);
    log_debug!("Header tip advanced to {}", best.height);
    Ok(())
}

#[allow(unreachable_code)]
#[allow(clippy::too_many_arguments)]
async fn sync_chain<S: KeyValueStore + 'static>(
    block_peer: &mut Peer,
    block_peers: &mut Vec<Peer>,
    block_peers_target: usize,
    chainstate: Arc<ChainState<S>>,
    mempool: Arc<Mutex<mempool::Mempool>>,
    fee_estimator: Arc<Mutex<fee_estimator::FeeEstimator>>,
    metrics: Arc<SyncMetrics>,
    params: Arc<ChainParams>,
    addr_book: &AddrBook,
    peer_ctx: &PeerContext,
    peer_book: Option<&HeaderPeerBook>,
    flags: &ValidationFlags,
    verify_settings: &VerifySettings,
    connect_metrics: Arc<ConnectMetrics>,
    write_lock: Arc<Mutex<()>>,
    header_cursor: Arc<Mutex<HeaderCursor>>,
    header_lead: i32,
    getdata_batch: usize,
    inflight_per_peer: usize,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    let idle_sleep = Duration::from_secs(IDLE_SLEEP_SECS);
    let mut last_progress_height = chainstate
        .best_block()
        .map_err(|err| err.to_string())?
        .map(|tip| tip.height)
        .unwrap_or(-1);
    let mut last_progress_at = Instant::now();
    let mut last_peer_refill_at = Instant::now() - Duration::from_secs(BLOCK_PEER_REFILL_SECS);
    loop {
        if *shutdown.borrow() {
            log_info!("Shutdown requested; stopping sync loop.");
            break;
        }
        cap_header_gap(
            chainstate.as_ref(),
            header_lead,
            write_lock.as_ref(),
            &header_cursor,
        )?;
        reorg_to_best_header(chainstate.as_ref(), write_lock.as_ref())?;
        let best_block_height = chainstate
            .best_block()
            .map_err(|err| err.to_string())?
            .map(|tip| tip.height)
            .unwrap_or(-1);
        if best_block_height > last_progress_height {
            last_progress_height = best_block_height;
            last_progress_at = Instant::now();
        }
        let (gap, best_header_height) = header_gap(chainstate.as_ref())?;

        if peer_ctx.registry.take_disconnect_request(block_peer.addr()) {
            let addr = block_peer.addr();
            log_info!("Disconnect requested for block peer {addr}; reconnecting");
            match connect_to_peer(
                params.as_ref(),
                best_block_height,
                best_header_height,
                addr_book,
                peer_ctx,
                peer_book,
            )
            .await
            {
                Ok(new_peer) => {
                    *block_peer = new_peer;
                }
                Err(err) => {
                    log_warn!("disconnect reconnect failed for block peer {addr}: {err}");
                }
            }
        }

        let mut disconnected_block_peers: Vec<SocketAddr> = Vec::new();
        block_peers.retain(|peer| {
            let addr = peer.addr();
            if peer_ctx.registry.take_disconnect_request(addr) {
                disconnected_block_peers.push(addr);
                false
            } else {
                true
            }
        });
        if !disconnected_block_peers.is_empty() {
            log_info!(
                "Disconnect requested for {} additional block peer(s)",
                disconnected_block_peers.len()
            );
            last_peer_refill_at = Instant::now() - Duration::from_secs(BLOCK_PEER_REFILL_SECS);
        }

        if block_peers_target > 0
            && block_peers.len() < block_peers_target
            && last_peer_refill_at.elapsed() > Duration::from_secs(BLOCK_PEER_REFILL_SECS)
        {
            let needed = block_peers_target.saturating_sub(block_peers.len());
            let start_height = best_block_height;
            let mut existing: HashSet<SocketAddr> =
                block_peers.iter().map(|peer| peer.addr()).collect();
            existing.insert(block_peer.addr());
            match connect_to_peers(
                params.as_ref(),
                needed,
                start_height,
                best_header_height,
                Some(addr_book),
                peer_ctx,
                peer_book,
            )
            .await
            {
                Ok(mut new_peers) => {
                    new_peers.retain(|peer| existing.insert(peer.addr()));
                    if !new_peers.is_empty() {
                        log_info!("Connected {} additional block peer(s)", new_peers.len());
                        block_peers.extend(new_peers);
                    }
                }
                Err(err) => {
                    log_warn!("refill block peers failed: {err}");
                }
            }
            last_peer_refill_at = Instant::now();
        }
        let max_fetch = max_fetch_blocks(
            block_peers.len().saturating_add(1),
            getdata_batch,
            inflight_per_peer,
        );
        let missing = collect_missing_blocks(chainstate.as_ref(), max_fetch)?;
        if !missing.is_empty() {
            if gap > 0 && last_progress_at.elapsed() > Duration::from_secs(BLOCK_STALL_SECS) {
                log_warn!(
                    "no block progress for {}s; reconnecting block peers",
                    last_progress_at.elapsed().as_secs()
                );
                let best_header_height = chainstate
                    .best_header()
                    .map_err(|err| err.to_string())?
                    .map(|tip| tip.height)
                    .unwrap_or(best_block_height);
                let start_height = best_block_height;
                match connect_to_peer(
                    params.as_ref(),
                    start_height,
                    best_header_height,
                    addr_book,
                    peer_ctx,
                    peer_book,
                )
                .await
                {
                    Ok(new_peer) => {
                        *block_peer = new_peer;
                    }
                    Err(err) => {
                        log_warn!("reconnect block peer failed: {err}");
                    }
                }
                if block_peers_target > 0 {
                    match connect_to_peers(
                        params.as_ref(),
                        block_peers_target,
                        start_height,
                        best_header_height,
                        Some(addr_book),
                        peer_ctx,
                        peer_book,
                    )
                    .await
                    {
                        Ok(new_peers) => {
                            *block_peers = new_peers;
                        }
                        Err(err) => {
                            log_warn!("reconnect block peers failed: {err}");
                        }
                    }
                }
                last_progress_at = Instant::now();
            }
            let fetch_result = fetch_blocks(
                block_peer,
                block_peers,
                peer_book,
                Arc::clone(&chainstate),
                Arc::clone(&mempool),
                Arc::clone(&fee_estimator),
                Arc::clone(&metrics),
                Arc::clone(&params),
                &missing,
                flags,
                verify_settings,
                Arc::clone(&connect_metrics),
                Arc::clone(&write_lock),
                Arc::clone(&header_cursor),
                getdata_batch,
                inflight_per_peer,
            );
            let fetch_result = tokio::select! {
                _ = shutdown.changed() => {
                    log_info!("Shutdown requested; aborting block fetch.");
                    break;
                }
                result = fetch_result => result,
            };
            if let Err(err) = fetch_result {
                if is_transient_block_error(&err) {
                    log_debug!("block fetch failed: {err}");
                    let best_header_height = chainstate
                        .best_header()
                        .map_err(|err| err.to_string())?
                        .map(|tip| tip.height)
                        .unwrap_or(best_block_height);
                    let start_height = best_block_height;
                    match connect_to_peer(
                        params.as_ref(),
                        start_height,
                        best_header_height,
                        addr_book,
                        peer_ctx,
                        peer_book,
                    )
                    .await
                    {
                        Ok(new_peer) => {
                            *block_peer = new_peer;
                        }
                        Err(err) => {
                            log_warn!("reconnect block peer failed: {err}");
                        }
                    }
                    if block_peers_target > 0 {
                        match connect_to_peers(
                            params.as_ref(),
                            block_peers_target,
                            start_height,
                            best_header_height,
                            Some(addr_book),
                            peer_ctx,
                            peer_book,
                        )
                        .await
                        {
                            Ok(new_peers) => {
                                *block_peers = new_peers;
                            }
                            Err(err) => {
                                log_warn!("reconnect block peers failed: {err}");
                            }
                        }
                    }
                    last_progress_at = Instant::now();
                    tokio::select! {
                        _ = shutdown.changed() => break,
                        _ = tokio::time::sleep(idle_sleep) => {}
                    }
                    continue;
                }
                return Err(err);
            }
        } else {
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = tokio::time::sleep(idle_sleep) => {}
            }
        }
    }

    Ok(())
}

fn is_transient_block_error(err: &str) -> bool {
    let err = err.to_lowercase();
    [
        "peer",
        "stalled",
        "timeout",
        "timed out",
        "notfound",
        "reject",
        "connection",
        "broken pipe",
        "reset by peer",
        "invalid magic",
        "payload",
        "eof",
    ]
    .iter()
    .any(|marker| err.contains(marker))
}

fn block_peer_ban_secs(err: &str) -> Option<u64> {
    let err = err.to_lowercase();
    if err.contains("notfound") {
        return Some(BLOCK_PEER_BAN_SECS_NOTFOUND);
    }
    if err.contains("reject") {
        return Some(BLOCK_PEER_BAN_SECS_PROTOCOL);
    }
    if err.contains("stalled") || err.contains("timeout") || err.contains("timed out") {
        return Some(BLOCK_PEER_BAN_SECS_TIMEOUT);
    }
    if err.contains("invalid magic") || err.contains("payload") {
        return Some(BLOCK_PEER_BAN_SECS_PROTOCOL);
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn spawn_status_logger<S: KeyValueStore + Send + Sync + 'static>(
    chainstate: Arc<ChainState<S>>,
    store: Arc<Store>,
    sync_metrics: Arc<SyncMetrics>,
    header_metrics: Arc<HeaderMetrics>,
    validation_metrics: Arc<ValidationMetrics>,
    connect_metrics: Arc<ConnectMetrics>,
    mempool: Arc<Mutex<mempool::Mempool>>,
    mempool_metrics: Arc<stats::MempoolMetrics>,
    network: Network,
    backend: Backend,
    start_time: Instant,
    interval_secs: u64,
) {
    if interval_secs == 0 {
        return;
    }

    let interval = Duration::from_secs(interval_secs);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        let mut last: Option<stats::StatsSnapshot> = None;
        loop {
            ticker.tick().await;
            match snapshot_stats(
                &chainstate,
                Some(store.as_ref()),
                network,
                backend,
                start_time,
                Some(&sync_metrics),
                Some(&header_metrics),
                Some(&validation_metrics),
                Some(&connect_metrics),
                Some(mempool.as_ref()),
                Some(mempool_metrics.as_ref()),
            ) {
                Ok(stats) => {
                    let header_hash = short_hash(stats.best_header_hash.as_ref());
                    let block_hash = short_hash(stats.best_block_hash.as_ref());
                    let (headers_per_sec, blocks_per_sec) = rates_from_last(&stats, last.as_ref());
                    let (download_ms, verify_ms, commit_ms) =
                        stage_ms_from_last(&stats, last.as_ref());
                    let (header_req_ms, header_val_ms, header_commit_ms, header_pow_ms) =
                        header_ms_from_last(&stats, last.as_ref());
                    let (validate_ms, script_ms, shield_ms) =
                        validation_ms_from_last(&stats, last.as_ref());
                    let (utxo_ms, index_ms, anchor_ms, flat_ms) =
                        connect_ms_from_last(&stats, last.as_ref());
                    log_info!(
                        "Status: headers {} blocks {} gap {} h/s {} b/s {} dl_ms {} ver_ms {} db_ms {} hdr_req_ms {} hdr_val_ms {} hdr_commit_ms {} hdr_pow_ms {} val_ms {} script_ms {} shield_ms {} utxo_ms {} idx_ms {} anchor_ms {} flat_ms {} header {} block {} uptime {}s",
                        stats.best_header_height,
                        stats.best_block_height,
                        stats.header_gap,
                        headers_per_sec,
                        blocks_per_sec,
                        download_ms,
                        verify_ms,
                        commit_ms,
                        header_req_ms,
                        header_val_ms,
                        header_commit_ms,
                        header_pow_ms,
                        validate_ms,
                        script_ms,
                        shield_ms,
                        utxo_ms,
                        index_ms,
                        anchor_ms,
                        flat_ms,
                        header_hash,
                        block_hash,
                        stats.uptime_secs
                    );
                    last = Some(stats);
                }
                Err(err) => {
                    log_warn!("status snapshot failed: {err}");
                }
            }
        }
    });
}

fn short_hash(value: Option<&String>) -> &str {
    match value {
        Some(hash) => {
            let end = hash.len().min(12);
            &hash[..end]
        }
        None => "-",
    }
}

fn rates_from_last(
    current: &stats::StatsSnapshot,
    last: Option<&stats::StatsSnapshot>,
) -> (String, String) {
    let Some(prev) = last else {
        return ("-".to_string(), "-".to_string());
    };
    let delta_time = current.unix_time_secs.saturating_sub(prev.unix_time_secs);
    if delta_time == 0 {
        return ("-".to_string(), "-".to_string());
    }
    let headers_delta = current.header_count.saturating_sub(prev.header_count);
    let blocks_delta = current.block_count.saturating_sub(prev.block_count);
    let headers_per_sec = headers_delta as f64 / delta_time as f64;
    let blocks_per_sec = blocks_delta as f64 / delta_time as f64;
    (
        format!("{headers_per_sec:.2}"),
        format!("{blocks_per_sec:.2}"),
    )
}

fn stage_ms_from_last(
    current: &stats::StatsSnapshot,
    last: Option<&stats::StatsSnapshot>,
) -> (String, String, String) {
    let Some(prev) = last else {
        return ("-".to_string(), "-".to_string(), "-".to_string());
    };

    let download_blocks = current.download_blocks.saturating_sub(prev.download_blocks);
    let verify_blocks = current.verify_blocks.saturating_sub(prev.verify_blocks);
    let commit_blocks = current.commit_blocks.saturating_sub(prev.commit_blocks);

    let download_ms = if download_blocks == 0 {
        "-".to_string()
    } else {
        let delta_us = current.download_us.saturating_sub(prev.download_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / download_blocks as f64)
    };
    let verify_ms = if verify_blocks == 0 {
        "-".to_string()
    } else {
        let delta_us = current.verify_us.saturating_sub(prev.verify_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / verify_blocks as f64)
    };
    let commit_ms = if commit_blocks == 0 {
        "-".to_string()
    } else {
        let delta_us = current.commit_us.saturating_sub(prev.commit_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / commit_blocks as f64)
    };

    (download_ms, verify_ms, commit_ms)
}

fn header_ms_from_last(
    current: &stats::StatsSnapshot,
    last: Option<&stats::StatsSnapshot>,
) -> (String, String, String, String) {
    let Some(prev) = last else {
        return (
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
        );
    };

    let request_batches = current
        .header_request_batches
        .saturating_sub(prev.header_request_batches);
    let validate_headers = current
        .header_validate_headers
        .saturating_sub(prev.header_validate_headers);
    let commit_headers = current
        .header_commit_headers
        .saturating_sub(prev.header_commit_headers);
    let pow_headers = current
        .header_pow_headers
        .saturating_sub(prev.header_pow_headers);

    let request_ms = if request_batches == 0 {
        "-".to_string()
    } else {
        let delta_us = current
            .header_request_us
            .saturating_sub(prev.header_request_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / request_batches as f64)
    };
    let commit_ms = if commit_headers == 0 {
        "-".to_string()
    } else {
        let delta_us = current
            .header_commit_us
            .saturating_sub(prev.header_commit_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / commit_headers as f64)
    };
    let validate_ms = if validate_headers == 0 {
        "-".to_string()
    } else {
        let delta_us = current
            .header_validate_us
            .saturating_sub(prev.header_validate_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / validate_headers as f64)
    };
    let pow_ms = if pow_headers == 0 {
        "-".to_string()
    } else {
        let delta_us = current.header_pow_us.saturating_sub(prev.header_pow_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / pow_headers as f64)
    };

    (request_ms, validate_ms, commit_ms, pow_ms)
}

fn validation_ms_from_last(
    current: &stats::StatsSnapshot,
    last: Option<&stats::StatsSnapshot>,
) -> (String, String, String) {
    let Some(prev) = last else {
        return ("-".to_string(), "-".to_string(), "-".to_string());
    };

    let validate_blocks = current.validate_blocks.saturating_sub(prev.validate_blocks);
    let script_blocks = current.script_blocks.saturating_sub(prev.script_blocks);
    let shielded_txs = current.shielded_txs.saturating_sub(prev.shielded_txs);

    let validate_ms = if validate_blocks == 0 {
        "-".to_string()
    } else {
        let delta_us = current.validate_us.saturating_sub(prev.validate_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / validate_blocks as f64)
    };
    let script_ms = if script_blocks == 0 {
        "-".to_string()
    } else {
        let delta_us = current.script_us.saturating_sub(prev.script_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / script_blocks as f64)
    };
    let shield_ms = if shielded_txs == 0 {
        "-".to_string()
    } else {
        let delta_us = current.shielded_us.saturating_sub(prev.shielded_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / shielded_txs as f64)
    };

    (validate_ms, script_ms, shield_ms)
}

fn connect_ms_from_last(
    current: &stats::StatsSnapshot,
    last: Option<&stats::StatsSnapshot>,
) -> (String, String, String, String) {
    let Some(prev) = last else {
        return (
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
        );
    };

    let utxo_blocks = current.utxo_blocks.saturating_sub(prev.utxo_blocks);
    let index_blocks = current.index_blocks.saturating_sub(prev.index_blocks);
    let anchor_blocks = current.anchor_blocks.saturating_sub(prev.anchor_blocks);
    let flat_blocks = current.flatfile_blocks.saturating_sub(prev.flatfile_blocks);

    let utxo_ms = if utxo_blocks == 0 {
        "-".to_string()
    } else {
        let delta_us = current.utxo_us.saturating_sub(prev.utxo_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / utxo_blocks as f64)
    };
    let index_ms = if index_blocks == 0 {
        "-".to_string()
    } else {
        let delta_us = current.index_us.saturating_sub(prev.index_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / index_blocks as f64)
    };
    let anchor_ms = if anchor_blocks == 0 {
        "-".to_string()
    } else {
        let delta_us = current.anchor_us.saturating_sub(prev.anchor_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / anchor_blocks as f64)
    };
    let flat_ms = if flat_blocks == 0 {
        "-".to_string()
    } else {
        let delta_us = current.flatfile_us.saturating_sub(prev.flatfile_us) as f64;
        format!("{:.2}", delta_us / 1000.0 / flat_blocks as f64)
    };

    (utxo_ms, index_ms, anchor_ms, flat_ms)
}

fn verify_shielded_block(
    block: &Block,
    height: i32,
    consensus: &fluxd_consensus::params::ConsensusParams,
    shielded_params: &ShieldedParams,
    metrics: Option<&ValidationMetrics>,
) -> Result<(), String> {
    let branch_id = current_epoch_branch_id(height, &consensus.upgrades);
    for tx in &block.transactions {
        if !tx_needs_shielded(tx) {
            continue;
        }
        let start = Instant::now();
        verify_transaction(tx, branch_id, shielded_params).map_err(|err| err.to_string())?;
        if let Some(metrics) = metrics {
            metrics.record_shielded(start.elapsed());
        }
    }
    Ok(())
}

fn collect_missing_blocks<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    max_count: usize,
) -> Result<Vec<fluxd_consensus::Hash256>, String> {
    if max_count == 0 {
        return Ok(Vec::new());
    }

    let best_header = match chainstate.best_header().map_err(|err| err.to_string())? {
        Some(tip) => tip,
        None => return Ok(Vec::new()),
    };

    let best_block = match chainstate.best_block().map_err(|err| err.to_string())? {
        Some(tip) => tip,
        None => return Ok(Vec::new()),
    };

    if best_header.hash == best_block.hash {
        return Ok(Vec::new());
    }

    let (anchor_hash, anchor_height) = if header_descends_from(
        chainstate,
        best_header.hash,
        best_block.hash,
        best_block.height,
    )? {
        (best_block.hash, best_block.height)
    } else {
        find_common_ancestor(
            chainstate,
            best_header.hash,
            best_header.height,
            best_block.hash,
            best_block.height,
        )?
    };

    let start_height = anchor_height.saturating_add(1);
    if start_height > best_header.height {
        return Ok(Vec::new());
    }

    let end_height = start_height
        .saturating_add(max_count as i32)
        .saturating_sub(1)
        .min(best_header.height);
    let end_hash = chainstate
        .header_ancestor_hash(&best_header.hash, end_height)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "missing header entry while scanning for blocks".to_string())?;

    let mut missing = Vec::with_capacity(max_count.min(256));
    let mut hash = end_hash;
    loop {
        if hash == anchor_hash {
            break;
        }
        missing.push(hash);
        let entry = chainstate
            .header_entry(&hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing header entry while scanning for blocks".to_string())?;
        if entry.height <= 0 {
            break;
        }
        hash = entry.prev_hash;
    }
    missing.reverse();
    Ok(missing)
}

fn build_locator<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    genesis_hash: &fluxd_consensus::Hash256,
) -> Result<Vec<fluxd_consensus::Hash256>, String> {
    let tip = match chainstate.best_header().map_err(|err| err.to_string())? {
        Some(tip) => tip,
        None => return Ok(vec![*genesis_hash]),
    };

    let mut locator = Vec::new();
    let mut hash = tip.hash;
    let mut height = tip.height;
    let mut step: i32 = 1;

    loop {
        locator.push(hash);
        if height == 0 {
            break;
        }
        let mut back = step;
        while back > 0 && height > 0 {
            let entry = chainstate
                .header_entry(&hash)
                .map_err(|err| err.to_string())?
                .ok_or_else(|| "missing header entry while building locator".to_string())?;
            hash = entry.prev_hash;
            height -= 1;
            back -= 1;
        }
        if locator.len() > 10 {
            step = step.saturating_mul(2);
        }
    }

    if locator.last() != Some(genesis_hash) {
        locator.push(*genesis_hash);
    }

    Ok(locator)
}

fn header_descends_from<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    hash: fluxd_consensus::Hash256,
    ancestor_hash: fluxd_consensus::Hash256,
    ancestor_height: i32,
) -> Result<bool, String> {
    if hash == ancestor_hash {
        return Ok(true);
    }

    let entry = chainstate
        .header_entry(&hash)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "missing header entry while checking ancestry".to_string())?;
    if entry.height < ancestor_height {
        return Ok(false);
    }
    let ancestor = chainstate
        .header_ancestor_hash(&hash, ancestor_height)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "missing header entry while checking ancestry".to_string())?;
    Ok(ancestor == ancestor_hash)
}

fn find_common_ancestor<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    a_hash: Hash256,
    a_height: i32,
    b_hash: Hash256,
    b_height: i32,
) -> Result<(Hash256, i32), String> {
    let max_height = a_height.min(b_height);
    if max_height < 0 {
        return Err("missing header entry while finding ancestor".to_string());
    }

    let mut low: i32 = 0;
    let mut high: i32 = max_height;
    while low < high {
        let mid = low + (high - low + 1) / 2;
        let mid_a = chainstate
            .header_ancestor_hash(&a_hash, mid)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing header entry while finding ancestor".to_string())?;
        let mid_b = chainstate
            .header_ancestor_hash(&b_hash, mid)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing header entry while finding ancestor".to_string())?;
        if mid_a == mid_b {
            low = mid;
        } else {
            high = mid.saturating_sub(1);
        }
    }

    let ancestor = chainstate
        .header_ancestor_hash(&a_hash, low)
        .map_err(|err| err.to_string())?
        .ok_or_else(|| "missing header entry while finding ancestor".to_string())?;
    Ok((ancestor, low))
}

fn reorg_to_best_header<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    write_lock: &Mutex<()>,
) -> Result<(), String> {
    let best_block = match chainstate.best_block().map_err(|err| err.to_string())? {
        Some(tip) => tip,
        None => return Ok(()),
    };
    let best_header = match chainstate.best_header().map_err(|err| err.to_string())? {
        Some(tip) => tip,
        None => return Ok(()),
    };

    if best_header.hash == best_block.hash {
        return Ok(());
    }
    if header_descends_from(
        chainstate,
        best_header.hash,
        best_block.hash,
        best_block.height,
    )? {
        return Ok(());
    }

    let (ancestor_hash, ancestor_height) = find_common_ancestor(
        chainstate,
        best_header.hash,
        best_header.height,
        best_block.hash,
        best_block.height,
    )?;

    let mut disconnected: usize = 0;
    loop {
        let tip = chainstate
            .best_block()
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing best block during reorg".to_string())?;
        if tip.hash == ancestor_hash {
            break;
        }
        let batch = chainstate
            .disconnect_block(&tip.hash)
            .map_err(|err| err.to_string())?;
        let _guard = write_lock
            .lock()
            .map_err(|_| "write lock poisoned".to_string())?;
        chainstate
            .commit_batch(batch)
            .map_err(|err| err.to_string())?;
        disconnected += 1;
    }

    if disconnected > 0 {
        log_info!(
            "Reorg: disconnected {} block(s) to height {} ({})",
            disconnected,
            ancestor_height,
            hash256_to_hex(&ancestor_hash)
        );
    }
    Ok(())
}

async fn request_headers(
    peer: &mut Peer,
    locator: &[fluxd_consensus::Hash256],
) -> Result<Vec<fluxd_primitives::block::BlockHeader>, String> {
    peer.send_getheaders(locator).await?;
    loop {
        let (command, payload) = read_message_with_timeout(peer).await?;
        match command.as_str() {
            "headers" => return parse_headers(&payload),
            _ => handle_aux_message(peer, &command, &payload).await?,
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn fetch_blocks<S: KeyValueStore + 'static>(
    peer: &mut Peer,
    block_peers: &mut Vec<Peer>,
    peer_book: Option<&HeaderPeerBook>,
    chainstate: Arc<ChainState<S>>,
    mempool: Arc<Mutex<mempool::Mempool>>,
    fee_estimator: Arc<Mutex<fee_estimator::FeeEstimator>>,
    metrics: Arc<SyncMetrics>,
    params: Arc<ChainParams>,
    hashes: &[fluxd_consensus::Hash256],
    flags: &ValidationFlags,
    verify_settings: &VerifySettings,
    connect_metrics: Arc<ConnectMetrics>,
    write_lock: Arc<Mutex<()>>,
    header_cursor: Arc<Mutex<HeaderCursor>>,
    getdata_batch: usize,
    inflight_per_peer: usize,
) -> Result<(), String> {
    if hashes.is_empty() {
        return Ok(());
    }

    if block_peers.is_empty() {
        let addr = peer.addr();
        let result = fetch_blocks_single(
            peer,
            chainstate,
            mempool,
            fee_estimator,
            Arc::clone(&metrics),
            params,
            hashes,
            flags,
            verify_settings,
            connect_metrics,
            write_lock,
            header_cursor,
            getdata_batch,
            inflight_per_peer,
        )
        .await;
        if let Some(peer_book) = peer_book {
            match &result {
                Ok(()) => peer_book.record_success(addr),
                Err(err) => {
                    peer_book.record_failure(addr);
                    if let Some(secs) = block_peer_ban_secs(err) {
                        peer_book.ban_for(addr, secs);
                    }
                }
            }
        }
        return result;
    }

    fetch_blocks_multi(
        peer,
        block_peers,
        peer_book,
        chainstate,
        mempool,
        fee_estimator,
        metrics,
        params,
        hashes,
        flags,
        verify_settings,
        connect_metrics,
        write_lock,
        header_cursor,
        getdata_batch,
        inflight_per_peer,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn fetch_blocks_single<S: KeyValueStore + 'static>(
    peer: &mut Peer,
    chainstate: Arc<ChainState<S>>,
    mempool: Arc<Mutex<mempool::Mempool>>,
    fee_estimator: Arc<Mutex<fee_estimator::FeeEstimator>>,
    metrics: Arc<SyncMetrics>,
    params: Arc<ChainParams>,
    hashes: &[fluxd_consensus::Hash256],
    flags: &ValidationFlags,
    verify_settings: &VerifySettings,
    connect_metrics: Arc<ConnectMetrics>,
    write_lock: Arc<Mutex<()>>,
    header_cursor: Arc<Mutex<HeaderCursor>>,
    getdata_batch: usize,
    inflight_per_peer: usize,
) -> Result<(), String> {
    let mut pending: VecDeque<fluxd_consensus::Hash256> = hashes.iter().copied().collect();
    let chunks: Vec<Vec<fluxd_consensus::Hash256>> = hashes
        .chunks(getdata_batch)
        .map(|chunk| chunk.to_vec())
        .collect();
    let download_start = Instant::now();
    let mut received = fetch_blocks_on_peer_inner(peer, chunks, inflight_per_peer).await?;
    metrics.record_download(received.len() as u64, download_start.elapsed());

    let chainstate = Arc::clone(&chainstate);
    let params = Arc::clone(&params);
    let mempool = Arc::clone(&mempool);
    let fee_estimator = Arc::clone(&fee_estimator);
    let metrics = Arc::clone(&metrics);
    let flags = flags.clone();
    let verify_settings = *verify_settings;
    let connect_metrics = Arc::clone(&connect_metrics);
    let write_lock = Arc::clone(&write_lock);
    let header_cursor = Arc::clone(&header_cursor);
    let join = tokio::task::spawn_blocking(move || {
        connect_pending(
            chainstate.as_ref(),
            mempool.as_ref(),
            fee_estimator.as_ref(),
            params.as_ref(),
            &flags,
            metrics.as_ref(),
            &verify_settings,
            connect_metrics.as_ref(),
            write_lock.as_ref(),
            &header_cursor,
            &mut pending,
            &mut received,
        )
    });
    match join.await {
        Ok(result) => result?,
        Err(err) => return Err(format!("block connect task failed: {err}")),
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn fetch_blocks_multi<S: KeyValueStore + 'static>(
    block_peer: &mut Peer,
    block_peers: &mut Vec<Peer>,
    peer_book: Option<&HeaderPeerBook>,
    chainstate: Arc<ChainState<S>>,
    mempool: Arc<Mutex<mempool::Mempool>>,
    fee_estimator: Arc<Mutex<fee_estimator::FeeEstimator>>,
    metrics: Arc<SyncMetrics>,
    params: Arc<ChainParams>,
    hashes: &[fluxd_consensus::Hash256],
    flags: &ValidationFlags,
    verify_settings: &VerifySettings,
    connect_metrics: Arc<ConnectMetrics>,
    write_lock: Arc<Mutex<()>>,
    header_cursor: Arc<Mutex<HeaderCursor>>,
    getdata_batch: usize,
    inflight_per_peer: usize,
) -> Result<(), String> {
    if hashes.is_empty() {
        return Ok(());
    }

    let mut pending: VecDeque<fluxd_consensus::Hash256> = hashes.iter().copied().collect();
    let mut received: HashMap<fluxd_consensus::Hash256, ReceivedBlock> = HashMap::new();

    let mut peers = std::mem::take(block_peers);
    let mut rounds = 0usize;
    let max_rounds = 3usize;

    while rounds < max_rounds {
        let remaining: Vec<fluxd_consensus::Hash256> = hashes
            .iter()
            .copied()
            .filter(|hash| !received.contains_key(hash))
            .collect();
        if remaining.is_empty() {
            break;
        }
        rounds = rounds.saturating_add(1);

        let chunks: Vec<Vec<fluxd_consensus::Hash256>> = remaining
            .chunks(getdata_batch)
            .map(|chunk| chunk.to_vec())
            .collect();
        let queue = Arc::new(Mutex::new(VecDeque::from(chunks)));

        let before = received.len();
        let download_start = Instant::now();
        let mut join_set = JoinSet::new();
        for peer in peers.drain(..) {
            let queue = Arc::clone(&queue);
            join_set.spawn(async move {
                let mut peer = peer;
                let outcome =
                    fetch_blocks_on_peer_queue_inner(&mut peer, &queue, inflight_per_peer).await;
                (peer, outcome)
            });
        }

        let queue_for_block_peer = Arc::clone(&queue);
        let block_peer_task =
            fetch_blocks_on_peer_queue_inner(block_peer, &queue_for_block_peer, inflight_per_peer);
        let peer_tasks = async move {
            let mut out = Vec::new();
            while let Some(result) = join_set.join_next().await {
                match result {
                    Ok(entry) => out.push(entry),
                    Err(err) => log_warn!("block peer join failed: {err}"),
                }
            }
            out
        };

        let (block_peer_outcome, peer_outcomes) = tokio::join!(block_peer_task, peer_tasks);

        let block_peer_addr = block_peer.addr();
        if !block_peer_outcome.received.is_empty() {
            received.extend(block_peer_outcome.received);
            if let Some(peer_book) = peer_book {
                peer_book.record_success(block_peer_addr);
            }
        }
        if let Some(err) = block_peer_outcome.error {
            log_debug!("block peer fetch failed: {err}");
            if let Some(peer_book) = peer_book {
                peer_book.record_failure(block_peer_addr);
                if let Some(secs) = block_peer_ban_secs(&err) {
                    peer_book.ban_for(block_peer_addr, secs);
                }
            }
        }

        let mut next_peers = Vec::new();
        for (peer, outcome) in peer_outcomes {
            let addr = peer.addr();
            if !outcome.received.is_empty() {
                received.extend(outcome.received);
                if let Some(peer_book) = peer_book {
                    peer_book.record_success(addr);
                }
            }
            if let Some(err) = outcome.error {
                log_debug!("block peer fetch failed: {err}");
                if let Some(peer_book) = peer_book {
                    peer_book.record_failure(addr);
                    if let Some(secs) = block_peer_ban_secs(&err) {
                        peer_book.ban_for(addr, secs);
                    }
                }
                continue;
            }
            next_peers.push(peer);
        }
        peers = next_peers;

        let downloaded = received.len().saturating_sub(before);
        if downloaded > 0 {
            metrics.record_download(downloaded as u64, download_start.elapsed());
        }

        if downloaded == 0 {
            break;
        }
    }

    *block_peers = peers;

    if received.is_empty() {
        return Err("no blocks received from any peer".to_string());
    }
    if let Some(next) = pending.front().copied() {
        if !received.contains_key(&next) {
            return Err("peer download missing next tip block".to_string());
        }
    }
    let chainstate = Arc::clone(&chainstate);
    let params = Arc::clone(&params);
    let mempool = Arc::clone(&mempool);
    let fee_estimator = Arc::clone(&fee_estimator);
    let metrics = Arc::clone(&metrics);
    let flags = flags.clone();
    let verify_settings = *verify_settings;
    let connect_metrics = Arc::clone(&connect_metrics);
    let write_lock = Arc::clone(&write_lock);
    let header_cursor = Arc::clone(&header_cursor);
    let join = tokio::task::spawn_blocking(move || {
        connect_pending(
            chainstate.as_ref(),
            mempool.as_ref(),
            fee_estimator.as_ref(),
            params.as_ref(),
            &flags,
            metrics.as_ref(),
            &verify_settings,
            connect_metrics.as_ref(),
            write_lock.as_ref(),
            &header_cursor,
            &mut pending,
            &mut received,
        )
    });
    match join.await {
        Ok(result) => result?,
        Err(err) => return Err(format!("block connect task failed: {err}")),
    }

    Ok(())
}

struct BlockPeerFetchOutcome {
    received: HashMap<fluxd_consensus::Hash256, ReceivedBlock>,
    error: Option<String>,
}

async fn fetch_blocks_on_peer_queue_inner(
    peer: &mut Peer,
    queue: &Arc<Mutex<VecDeque<Vec<fluxd_consensus::Hash256>>>>,
    inflight_per_peer: usize,
) -> BlockPeerFetchOutcome {
    let mut received: HashMap<fluxd_consensus::Hash256, ReceivedBlock> = HashMap::new();
    let mut inflight: Vec<HashSet<fluxd_consensus::Hash256>> = Vec::new();

    let pop_chunk = || -> Option<Vec<fluxd_consensus::Hash256>> {
        let Ok(mut guard) = queue.lock() else {
            return None;
        };
        guard.pop_front()
    };

    let inflight_target = inflight_per_peer.max(1);
    while inflight.len() < inflight_target {
        let Some(chunk) = pop_chunk() else {
            break;
        };
        if chunk.is_empty() {
            continue;
        }
        maybe_log_block_request(chunk.len());
        if let Err(err) = peer.send_getdata_blocks(&chunk).await {
            return BlockPeerFetchOutcome {
                received,
                error: Some(err),
            };
        }
        inflight.push(chunk.into_iter().collect());
    }

    let mut last_block_at = Instant::now();
    while !inflight.is_empty() {
        if last_block_at.elapsed() > Duration::from_secs(BLOCK_IDLE_SECS) {
            return BlockPeerFetchOutcome {
                received,
                error: Some("block peer stalled (no blocks received)".to_string()),
            };
        }
        let (command, payload) = match read_message_with_timeout_opts(
            peer,
            BLOCK_READ_TIMEOUT_SECS,
            BLOCK_READ_TIMEOUT_RETRIES,
        )
        .await
        {
            Ok(message) => message,
            Err(err) => {
                return BlockPeerFetchOutcome {
                    received,
                    error: Some(err),
                };
            }
        };
        match command.as_str() {
            "block" => {
                let bytes = payload;
                let block = match Block::consensus_decode(&bytes) {
                    Ok(block) => block,
                    Err(err) => {
                        return BlockPeerFetchOutcome {
                            received,
                            error: Some(err.to_string()),
                        };
                    }
                };
                let hash = block.header.hash();
                let mut matched = false;
                if let Some(pos) = inflight.iter_mut().position(|set| set.contains(&hash)) {
                    matched = true;
                    let set = &mut inflight[pos];
                    set.remove(&hash);
                    if set.is_empty() {
                        inflight.remove(pos);
                        while inflight.len() < inflight_target {
                            let Some(chunk) = pop_chunk() else {
                                break;
                            };
                            if chunk.is_empty() {
                                continue;
                            }
                            maybe_log_block_request(chunk.len());
                            if let Err(err) = peer.send_getdata_blocks(&chunk).await {
                                return BlockPeerFetchOutcome {
                                    received,
                                    error: Some(err),
                                };
                            }
                            inflight.push(chunk.into_iter().collect());
                        }
                    }
                }
                if matched {
                    received.insert(hash, ReceivedBlock { block, bytes });
                    last_block_at = Instant::now();
                }
            }
            "notfound" => {
                let message = match parse_inv(&payload) {
                    Ok(items) if !items.is_empty() => {
                        let first = hash256_to_hex(&items[0].hash);
                        format!(
                            "peer returned notfound for {} item(s) (first {})",
                            items.len(),
                            first
                        )
                    }
                    _ => "peer returned notfound for block request".to_string(),
                };
                return BlockPeerFetchOutcome {
                    received,
                    error: Some(message),
                };
            }
            "reject" => {
                let message = match parse_reject(&payload) {
                    Ok(reject) => {
                        let suffix = reject
                            .data
                            .map(|hash| format!(" {}", hash256_to_hex(&hash)))
                            .unwrap_or_default();
                        format!(
                            "peer sent reject for {} (code {} reason {}){}",
                            reject.message, reject.code, reject.reason, suffix
                        )
                    }
                    Err(err) => format!("peer sent reject (unparseable): {err}"),
                };
                return BlockPeerFetchOutcome {
                    received,
                    error: Some(message),
                };
            }
            _ => {
                if let Err(err) = handle_aux_message(peer, &command, &payload).await {
                    return BlockPeerFetchOutcome {
                        received,
                        error: Some(err),
                    };
                }
            }
        }
    }

    BlockPeerFetchOutcome {
        received,
        error: None,
    }
}

async fn fetch_blocks_on_peer_inner(
    peer: &mut Peer,
    chunks: Vec<Vec<fluxd_consensus::Hash256>>,
    inflight_per_peer: usize,
) -> Result<HashMap<fluxd_consensus::Hash256, ReceivedBlock>, String> {
    let mut received: HashMap<fluxd_consensus::Hash256, ReceivedBlock> = HashMap::new();
    if chunks.is_empty() {
        return Ok(received);
    }

    let mut inflight: Vec<HashSet<fluxd_consensus::Hash256>> = Vec::new();
    let mut next_index = 0usize;

    let inflight_target = inflight_per_peer.max(1);
    while next_index < chunks.len() && inflight.len() < inflight_target {
        let chunk = &chunks[next_index];
        maybe_log_block_request(chunk.len());
        peer.send_getdata_blocks(chunk).await?;
        inflight.push(chunk.iter().copied().collect());
        next_index += 1;
    }

    let mut last_block_at = Instant::now();
    while !inflight.is_empty() {
        if last_block_at.elapsed() > Duration::from_secs(BLOCK_IDLE_SECS) {
            return Err("block peer stalled (no blocks received)".to_string());
        }
        let (command, payload) = read_message_with_timeout_opts(
            peer,
            BLOCK_READ_TIMEOUT_SECS,
            BLOCK_READ_TIMEOUT_RETRIES,
        )
        .await?;
        match command.as_str() {
            "block" => {
                let bytes = payload;
                let block = Block::consensus_decode(&bytes).map_err(|err| err.to_string())?;
                let hash = block.header.hash();
                let mut matched = false;
                if let Some(pos) = inflight.iter_mut().position(|set| set.contains(&hash)) {
                    matched = true;
                    let set = &mut inflight[pos];
                    set.remove(&hash);
                    if set.is_empty() {
                        inflight.remove(pos);
                        if next_index < chunks.len() {
                            let chunk = &chunks[next_index];
                            maybe_log_block_request(chunk.len());
                            peer.send_getdata_blocks(chunk).await?;
                            inflight.push(chunk.iter().copied().collect());
                            next_index += 1;
                        }
                    }
                }
                if matched {
                    received.insert(hash, ReceivedBlock { block, bytes });
                    last_block_at = Instant::now();
                }
            }
            "notfound" => {
                let message = match parse_inv(&payload) {
                    Ok(items) if !items.is_empty() => {
                        let first = hash256_to_hex(&items[0].hash);
                        format!(
                            "peer returned notfound for {} item(s) (first {})",
                            items.len(),
                            first
                        )
                    }
                    _ => "peer returned notfound for block request".to_string(),
                };
                return Err(message);
            }
            "reject" => {
                let message = match parse_reject(&payload) {
                    Ok(reject) => {
                        let suffix = reject
                            .data
                            .map(|hash| format!(" {}", hash256_to_hex(&hash)))
                            .unwrap_or_default();
                        format!(
                            "peer sent reject for {} (code {} reason {}){}",
                            reject.message, reject.code, reject.reason, suffix
                        )
                    }
                    Err(err) => format!("peer sent reject (unparseable): {err}"),
                };
                return Err(message);
            }
            _ => handle_aux_message(peer, &command, &payload).await?,
        }
    }

    Ok(received)
}

#[allow(clippy::too_many_arguments)]
fn connect_pending<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    mempool: &Mutex<mempool::Mempool>,
    fee_estimator: &Mutex<fee_estimator::FeeEstimator>,
    params: &ChainParams,
    flags: &ValidationFlags,
    metrics: &SyncMetrics,
    verify_settings: &VerifySettings,
    connect_metrics: &ConnectMetrics,
    write_lock: &Mutex<()>,
    _header_cursor: &Arc<Mutex<HeaderCursor>>,
    pending: &mut VecDeque<fluxd_consensus::Hash256>,
    received: &mut HashMap<fluxd_consensus::Hash256, ReceivedBlock>,
) -> Result<(), String> {
    if received.is_empty() {
        return Ok(());
    }

    let verify_queue = verify_settings.verify_queue.max(1);
    let (verify_tx, verify_rx) = bounded::<VerifyJob>(verify_queue);
    let (shielded_tx, shielded_rx) = unbounded::<ShieldedJob>();
    let (event_tx, event_rx) = unbounded::<PipelineEvent>();

    let mut pre_flags = flags.clone();
    pre_flags.check_pow = false;
    pre_flags.check_pon = false;
    pre_flags.check_shielded = false;
    pre_flags.metrics = flags.metrics.clone();
    let pre_flags = Arc::new(pre_flags);
    let shielded_enabled = flags.check_shielded && flags.shielded_params.is_some();
    let shielded_params = flags.shielded_params.clone();
    let validation_metrics = flags.metrics.clone();
    let consensus = Arc::new(params.consensus.clone());

    let mut verify_handles = Vec::new();
    for _ in 0..verify_settings.verify_workers.max(1) {
        let verify_rx = verify_rx.clone();
        let event_tx = event_tx.clone();
        let shielded_tx = shielded_tx.clone();
        let pre_flags = Arc::clone(&pre_flags);
        let consensus = Arc::clone(&consensus);
        verify_handles.push(thread::spawn(move || {
            while let Ok(job) = verify_rx.recv() {
                let block_size = u32::try_from(job.bytes.len()).unwrap_or(u32::MAX);
                let (txids, error) = match validate_block_with_txids_and_size(
                    job.block.as_ref(),
                    job.height,
                    &consensus,
                    &pre_flags,
                    Some(block_size),
                ) {
                    Ok(txids) => (txids, None),
                    Err(err) => (Vec::new(), Some(err.to_string())),
                };
                let mut needs_shielded = false;
                if error.is_none() && shielded_enabled && block_needs_shielded(job.block.as_ref()) {
                    needs_shielded = true;
                    let shielded_job = ShieldedJob {
                        hash: job.hash,
                        height: job.height,
                        block: Arc::clone(&job.block),
                    };
                    if shielded_tx.send(shielded_job).is_err() {
                        let _ = event_tx.send(PipelineEvent::Verify(VerifyResult {
                            hash: job.hash,
                            height: job.height,
                            block: job.block,
                            bytes: job.bytes,
                            txids,
                            needs_shielded,
                            error: Some("shielded queue closed".to_string()),
                        }));
                        continue;
                    }
                }
                let _ = event_tx.send(PipelineEvent::Verify(VerifyResult {
                    hash: job.hash,
                    height: job.height,
                    block: job.block,
                    bytes: job.bytes,
                    txids,
                    needs_shielded,
                    error,
                }));
            }
        }));
    }

    let mut shielded_handles = Vec::new();
    if shielded_enabled {
        if let Some(params) = shielded_params {
            for _ in 0..verify_settings.shielded_workers.max(1) {
                let shielded_rx = shielded_rx.clone();
                let event_tx = event_tx.clone();
                let consensus = Arc::clone(&consensus);
                let params = Arc::clone(&params);
                let validation_metrics = validation_metrics.clone();
                shielded_handles.push(thread::spawn(move || {
                    while let Ok(job) = shielded_rx.recv() {
                        let result = verify_shielded_block(
                            job.block.as_ref(),
                            job.height,
                            &consensus,
                            &params,
                            validation_metrics.as_deref(),
                        );
                        let _ = event_tx.send(PipelineEvent::Shielded(ShieldedResult {
                            hash: job.hash,
                            error: result.err(),
                        }));
                    }
                }));
            }
        }
    }

    let mut received_heights = HashMap::new();
    for hash in pending.iter().copied() {
        let Some(received_block) = received.remove(&hash) else {
            break;
        };
        let height = match chainstate
            .header_entry(&hash)
            .map_err(|err| err.to_string())?
        {
            Some(entry) => entry.height,
            None => {
                if hash == params.consensus.hash_genesis_block {
                    0
                } else {
                    return Err("missing header entry for block".to_string());
                }
            }
        };
        let bytes = Arc::new(received_block.bytes);
        let job = VerifyJob {
            hash,
            height,
            block: Arc::new(received_block.block),
            bytes,
        };
        verify_tx
            .send(job)
            .map_err(|_| "verify queue closed".to_string())?;
        received_heights.insert(hash, height);
    }
    received.clear();

    drop(verify_tx);
    drop(shielded_tx);
    drop(event_tx);

    let mut pending_verify = received_heights.len();
    let mut pending_shielded = 0usize;
    let mut verified: HashMap<fluxd_consensus::Hash256, VerifiedBlock> = HashMap::new();
    let mut waiting_shielded: HashMap<fluxd_consensus::Hash256, VerifiedBlock> = HashMap::new();
    let mut shielded_ready: HashSet<fluxd_consensus::Hash256> = HashSet::new();
    let mut connect_flags = flags.clone();
    if shielded_enabled {
        connect_flags.check_shielded = false;
    }

    while let Some(hash) = pending.front().copied() {
        if let Some(verified_block) = verified.remove(&hash) {
            let verify_start = Instant::now();
            let batch = match chainstate.connect_block(
                verified_block.block.as_ref(),
                verified_block.height,
                params,
                &connect_flags,
                true,
                Some(verified_block.txids.as_slice()),
                Some(connect_metrics),
                Some(verified_block.bytes.as_slice()),
                None,
            ) {
                Ok(batch) => batch,
                Err(fluxd_chainstate::state::ChainStateError::InvalidHeader(
                    "block does not extend best block tip",
                ))
                | Err(fluxd_chainstate::state::ChainStateError::InvalidHeader(
                    "block height does not match header index",
                )) => {
                    log_warn!(
                        "block connect mismatch at height {} ({}); attempting reorg",
                        verified_block.height,
                        hash256_to_hex(&hash)
                    );
                    reorg_to_best_header(chainstate, write_lock)?;
                    return Ok(());
                }
                Err(err) => return Err(err.to_string()),
            };
            metrics.record_verify(1, verify_start.elapsed());

            let commit_start = Instant::now();
            let should_reorg = {
                let _guard = write_lock
                    .lock()
                    .map_err(|_| "write lock poisoned".to_string())?;
                let tip = chainstate.best_block().map_err(|err| err.to_string())?;
                if let Some(tip) = tip {
                    if tip.hash == hash {
                        false
                    } else if tip.hash != verified_block.block.header.prev_block {
                        true
                    } else {
                        chainstate
                            .commit_batch(batch)
                            .map_err(|err| err.to_string())?;
                        false
                    }
                } else {
                    true
                }
            };
            if should_reorg {
                log_warn!(
                    "block commit tip moved at height {} ({}); attempting reorg",
                    verified_block.height,
                    hash256_to_hex(&hash)
                );
                reorg_to_best_header(chainstate, write_lock)?;
                return Ok(());
            }
            metrics.record_commit(1, commit_start.elapsed());
            let purge = purge_mempool_for_connected_block(
                mempool,
                verified_block.height,
                verified_block.block.as_ref(),
                verified_block.txids.as_slice(),
            )?;
            if !purge.removed_txids.is_empty() || !purge.mined_entries.is_empty() {
                let current_estimate = current_fee_estimate(chainstate);
                if let Ok(mut estimator) = fee_estimator.lock() {
                    for txid in &purge.removed_txids {
                        estimator.remove_transaction(txid);
                    }
                    estimator.process_block(
                        u32::try_from(verified_block.height.max(0)).unwrap_or(0),
                        purge.mined_entries.as_slice(),
                        current_estimate,
                    );
                }
            }

            pending.pop_front();
            continue;
        }

        if !received_heights.contains_key(&hash) {
            break;
        }

        if pending_verify == 0 && pending_shielded == 0 {
            break;
        }

        let event = match event_rx.recv_timeout(Duration::from_secs(CONNECT_PIPELINE_IDLE_SECS)) {
            Ok(event) => event,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                let height = received_heights.get(&hash).copied().unwrap_or(-1);
                return Err(format!(
                        "block verify pipeline stalled for {}s (height {} hash {} pending_verify {} pending_shielded {} verified {} waiting_shielded {} shielded_ready {})",
                        CONNECT_PIPELINE_IDLE_SECS,
                        height,
                        hash256_to_hex(&hash),
                        pending_verify,
                        pending_shielded,
                        verified.len(),
                        waiting_shielded.len(),
                        shielded_ready.len(),
                    ));
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        };
        match event {
            PipelineEvent::Verify(result) => {
                pending_verify = pending_verify.saturating_sub(1);
                if let Some(error) = result.error {
                    return Err(format!(
                        "pre-validation failed at height {} ({}) : {}",
                        result.height,
                        hash256_to_hex(&result.hash),
                        error
                    ));
                }
                let entry = VerifiedBlock {
                    height: result.height,
                    block: result.block,
                    bytes: result.bytes,
                    txids: result.txids,
                };
                if result.needs_shielded {
                    if shielded_ready.remove(&result.hash) {
                        verified.insert(result.hash, entry);
                    } else {
                        pending_shielded = pending_shielded.saturating_add(1);
                        waiting_shielded.insert(result.hash, entry);
                    }
                } else {
                    verified.insert(result.hash, entry);
                }
            }
            PipelineEvent::Shielded(result) => {
                if let Some(error) = result.error {
                    return Err(format!(
                        "shielded verification failed for {}: {}",
                        hash256_to_hex(&result.hash),
                        error
                    ));
                }
                if let Some(entry) = waiting_shielded.remove(&result.hash) {
                    pending_shielded = pending_shielded.saturating_sub(1);
                    verified.insert(result.hash, entry);
                } else {
                    shielded_ready.insert(result.hash);
                }
            }
        }
    }

    for handle in verify_handles {
        let _ = handle.join();
    }
    for handle in shielded_handles {
        let _ = handle.join();
    }

    Ok(())
}

#[derive(Default)]
struct MempoolPurgeOutcome {
    removed_txids: Vec<Hash256>,
    mined_entries: Vec<fee_estimator::BlockTxInfo>,
}

fn purge_mempool_for_connected_block(
    mempool: &Mutex<mempool::Mempool>,
    block_height: i32,
    block: &Block,
    txids: &[Hash256],
) -> Result<MempoolPurgeOutcome, String> {
    if block.transactions.len() <= 1 {
        return Ok(MempoolPurgeOutcome::default());
    }
    if txids.len() != block.transactions.len() {
        return Err("transaction id cache mismatch".to_string());
    }

    let mut guard = mempool
        .lock()
        .map_err(|_| "mempool lock poisoned".to_string())?;
    let mut mined: HashSet<Hash256> = HashSet::new();
    let mut conflicts: HashSet<Hash256> = HashSet::new();
    for (txid, tx) in txids
        .iter()
        .copied()
        .skip(1)
        .zip(block.transactions.iter().skip(1))
    {
        mined.insert(txid);
        for input in &tx.vin {
            if let Some(conflict) = guard.spender(&input.prevout) {
                if !mined.contains(&conflict) {
                    conflicts.insert(conflict);
                }
            }
        }
    }

    let mut outcome = MempoolPurgeOutcome::default();
    for txid in mined {
        let Some(entry) = guard.remove(&txid) else {
            continue;
        };
        outcome.removed_txids.push(txid);
        outcome.mined_entries.push(fee_estimator::BlockTxInfo {
            fee: entry.fee,
            size: entry.size(),
            height: u32::try_from(entry.height.max(0)).unwrap_or(0),
            priority: entry.current_priority(block_height),
            was_clear_at_entry: entry.was_clear_at_entry,
        });
    }
    for txid in conflicts {
        let removed = guard.remove_with_descendants(&txid);
        if removed.is_empty() {
            continue;
        }
        outcome
            .removed_txids
            .extend(removed.iter().map(|entry| entry.txid));
    }

    Ok(outcome)
}

async fn read_message_with_timeout(peer: &mut Peer) -> Result<(String, Vec<u8>), String> {
    read_message_with_timeout_opts(peer, READ_TIMEOUT_SECS, READ_TIMEOUT_RETRIES).await
}

async fn read_message_with_timeout_opts(
    peer: &mut Peer,
    timeout_secs: u64,
    retries: usize,
) -> Result<(String, Vec<u8>), String> {
    let retries = retries.max(1);
    for attempt in 0..retries {
        let read =
            tokio::time::timeout(Duration::from_secs(timeout_secs), peer.read_message()).await;
        match read {
            Ok(result) => return result,
            Err(_) if attempt + 1 == retries => {
                return Err("peer read timed out".to_string());
            }
            Err(_) => {
                log_debug!("peer read timed out (attempt {}/{})", attempt + 1, retries);
            }
        }
    }

    Err("peer read timed out".to_string())
}

async fn handle_aux_message(peer: &mut Peer, command: &str, payload: &[u8]) -> Result<(), String> {
    match command {
        "ping" => peer.send_message("pong", payload).await?,
        "version" => peer.send_message("verack", &[]).await?,
        _ => {}
    }
    Ok(())
}

fn parse_args() -> Result<CliAction, String> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from<I>(raw_args: I) -> Result<CliAction, String>
where
    I: IntoIterator<Item = String>,
{
    let mut backend = Backend::Fjall;
    let mut profile: Option<RunProfile> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut conf_path: Option<PathBuf> = None;
    let mut params_dir: Option<PathBuf> = None;
    let mut fetch_params = false;
    let mut reindex = false;
    let mut resync = false;
    let mut reindex_txindex = false;
    let mut reindex_spentindex = false;
    let mut reindex_addressindex = false;
    let mut db_info = false;
    let mut db_info_keys = false;
    let mut db_integrity = false;
    let mut scan_flatfiles = false;
    let mut scan_supply = false;
    let mut scan_fluxnodes = false;
    let mut debug_fluxnode_payee_script: Option<Vec<u8>> = None;
    let mut debug_fluxnode_payout_height: Option<i32> = None;
    let mut debug_fluxnode_payee_candidates: Option<DebugFluxnodePayeeCandidates> = None;
    let mut check_script = true;
    let mut log_level = logging::Level::Info;
    let mut log_level_set = false;
    let mut log_format = logging::Format::Text;
    let mut log_format_set = false;
    let mut log_timestamps = true;
    let mut log_timestamps_set = false;
    let mut p2p_listen = true;
    let mut p2p_listen_set = false;
    let mut p2p_addr: Option<SocketAddr> = None;
    let mut p2p_addr_set = false;
    let mut rpc_addr: Option<SocketAddr> = None;
    let mut rpc_addr_set = false;
    let mut rpc_user: Option<String> = None;
    let mut rpc_user_set = false;
    let mut rpc_pass: Option<String> = None;
    let mut rpc_pass_set = false;
    let mut rpc_allow_ips: Vec<String> = Vec::new();
    let mut network = Network::Mainnet;
    let mut network_set = false;
    let mut getdata_batch: usize = DEFAULT_GETDATA_BATCH;
    let mut getdata_batch_set = false;
    let mut block_peers: usize = DEFAULT_BLOCK_PEERS;
    let mut block_peers_set = false;
    let mut header_peers: usize = DEFAULT_HEADER_PEERS;
    let mut header_peers_set = false;
    let mut header_lead: i32 = DEFAULT_HEADER_LEAD;
    let mut header_lead_set = false;
    let mut header_peer_addrs: Vec<String> = Vec::new();
    let mut addnode_addrs: Vec<SocketAddr> = Vec::new();
    let mut max_connections: usize = DEFAULT_MAX_CONNECTIONS;
    let mut max_connections_set = false;
    let mut tx_peers: usize = DEFAULT_TX_PEERS;
    let mut tx_peers_set = false;
    let mut inflight_per_peer: usize = DEFAULT_INFLIGHT_PER_PEER;
    let mut inflight_per_peer_set = false;
    let mut require_standard: Option<bool> = None;
    let mut min_relay_fee_per_kb: i64 = 100;
    let mut min_relay_fee_per_kb_set = false;
    let mut limit_free_relay_kb_per_minute: u64 = 500;
    let mut limit_free_relay_kb_per_minute_set = false;
    let mut miner_address: Option<String> = None;
    let mut miner_address_set = false;
    let mut tx_confirm_target: u32 = DEFAULT_TX_CONFIRM_TARGET;
    let mut tx_confirm_target_set = false;
    let mut mempool_max_mb: u64 = DEFAULT_MEMPOOL_MAX_MB;
    let mut mempool_max_mb_set = false;
    let mut mempool_persist_interval_secs: u64 = DEFAULT_MEMPOOL_PERSIST_INTERVAL_SECS;
    let mut mempool_persist_interval_set = false;
    let mut fee_estimates_persist_interval_secs: u64 = DEFAULT_FEE_ESTIMATES_PERSIST_INTERVAL_SECS;
    let mut fee_estimates_persist_interval_set = false;
    let mut status_interval_secs: u64 = 15;
    let mut status_interval_set = false;
    let mut dashboard_addr: Option<SocketAddr> = None;
    let mut db_cache_mb: u64 = DEFAULT_DB_CACHE_MB;
    let mut db_cache_set = false;
    let mut db_write_buffer_mb: u64 = DEFAULT_DB_WRITE_BUFFER_MB;
    let mut db_journal_mb: u64 = DEFAULT_DB_JOURNAL_MB;
    let mut db_memtable_mb: u64 = DEFAULT_DB_MEMTABLE_MB;
    let mut db_flush_workers: usize = DEFAULT_DB_FLUSH_WORKERS;
    let mut db_compaction_workers: usize = DEFAULT_DB_COMPACTION_WORKERS;
    let mut db_write_buffer_set = false;
    let mut db_journal_set = false;
    let mut db_memtable_set = false;
    let mut db_flush_workers_set = false;
    let mut db_compaction_workers_set = false;
    let mut db_fsync_ms_set = false;
    let mut db_fsync_ms: Option<u16> = None;
    let mut utxo_cache_entries: usize = DEFAULT_UTXO_CACHE_ENTRIES;
    let mut utxo_cache_entries_set = false;
    let mut header_verify_workers: usize = 0;
    let mut header_verify_workers_set = false;
    let mut verify_workers: usize = 0;
    let mut verify_workers_set = false;
    let mut verify_queue: usize = 0;
    let mut verify_queue_set = false;
    let mut shielded_workers: usize = 0;
    let mut shielded_workers_set = false;
    let mut args = raw_args.into_iter().peekable();

    if let Some(first) = args.peek().map(|value| value.as_str()) {
        match first {
            "help" => return Ok(CliAction::PrintHelp),
            "version" => return Ok(CliAction::PrintVersion),
            "db-info" => {
                db_info = true;
                let _ = args.next();
            }
            "db-info-keys" => {
                db_info = true;
                db_info_keys = true;
                let _ = args.next();
            }
            "db-integrity" => {
                db_integrity = true;
                let _ = args.next();
            }
            "scan-flatfiles" => {
                scan_flatfiles = true;
                let _ = args.next();
            }
            "scan-supply" => {
                scan_supply = true;
                let _ = args.next();
            }
            "scan-fluxnodes" => {
                scan_fluxnodes = true;
                let _ = args.next();
            }
            _ => {}
        }
    }
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--backend" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --backend\n{}", usage()))?;
                backend = Backend::parse(&value)
                    .ok_or_else(|| format!("invalid backend '{value}'\n{}", usage()))?;
            }
            "--data-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --data-dir\n{}", usage()))?;
                data_dir = Some(PathBuf::from(value));
            }
            "--conf" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --conf\n{}", usage()))?;
                conf_path = Some(PathBuf::from(value));
            }
            "--params-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --params-dir\n{}", usage()))?;
                params_dir = Some(PathBuf::from(value));
            }
            "--profile" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --profile\n{}", usage()))?;
                profile = Some(RunProfile::parse(&value).ok_or_else(|| {
                    format!(
                        "invalid profile '{value}' (expected low|default|high)\n{}",
                        usage()
                    )
                })?);
            }
            "--fetch-params" => {
                fetch_params = true;
            }
            "--reindex" => {
                reindex = true;
            }
            "--resync" => {
                resync = true;
            }
            "--reindex-txindex" => {
                reindex_txindex = true;
            }
            "--reindex-spentindex" => {
                reindex_spentindex = true;
            }
            "--reindex-addressindex" => {
                reindex_addressindex = true;
            }
            "--db-info" => {
                db_info = true;
            }
            "--db-info-keys" => {
                db_info = true;
                db_info_keys = true;
            }
            "--db-integrity" => {
                db_integrity = true;
            }
            "--scan-flatfiles" => {
                scan_flatfiles = true;
            }
            "--scan-supply" => {
                scan_supply = true;
            }
            "--scan-fluxnodes" => {
                scan_fluxnodes = true;
            }
            "--debug-fluxnode-payee-script" => {
                let value = args.next().ok_or_else(|| {
                    format!(
                        "missing value for --debug-fluxnode-payee-script\n{}",
                        usage()
                    )
                })?;
                debug_fluxnode_payee_script = Some(parse_hex_bytes(&value).ok_or_else(|| {
                    format!(
                        "invalid script hex for --debug-fluxnode-payee-script\n{}",
                        usage()
                    )
                })?);
            }
            "--debug-fluxnode-payouts" => {
                let value = args.next().ok_or_else(|| {
                    format!("missing value for --debug-fluxnode-payouts\n{}", usage())
                })?;
                debug_fluxnode_payout_height = Some(value.parse::<i32>().map_err(|_| {
                    format!("invalid height for --debug-fluxnode-payouts\n{}", usage())
                })?);
            }
            "--debug-fluxnode-payee-candidates" => {
                let tier = args.next().ok_or_else(|| {
                    format!(
                        "missing tier for --debug-fluxnode-payee-candidates\n{}",
                        usage()
                    )
                })?;
                let height = args.next().ok_or_else(|| {
                    format!(
                        "missing height for --debug-fluxnode-payee-candidates\n{}",
                        usage()
                    )
                })?;
                let tier = tier.parse::<u8>().map_err(|_| {
                    format!(
                        "invalid tier for --debug-fluxnode-payee-candidates\n{}",
                        usage()
                    )
                })?;
                let height = height.parse::<i32>().map_err(|_| {
                    format!(
                        "invalid height for --debug-fluxnode-payee-candidates\n{}",
                        usage()
                    )
                })?;
                debug_fluxnode_payee_candidates = Some(DebugFluxnodePayeeCandidates {
                    tier,
                    height,
                    limit: 50,
                });
            }
            "--log-level" | "--loglevel" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --log-level\n{}", usage()))?;
                log_level = logging::Level::parse(&value)
                    .ok_or_else(|| format!("invalid log level '{value}'\n{}", usage()))?;
                log_level_set = true;
            }
            "--log-format" | "--logformat" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --log-format\n{}", usage()))?;
                log_format = logging::Format::parse(&value)
                    .ok_or_else(|| format!("invalid log format '{value}'\n{}", usage()))?;
                log_format_set = true;
            }
            "--log-timestamps" | "--logtimestamps" => {
                log_timestamps = true;
                log_timestamps_set = true;
            }
            "--no-log-timestamps" | "--no-logtimestamps" => {
                log_timestamps = false;
                log_timestamps_set = true;
            }
            "--skip-script" => {
                check_script = false;
            }
            "--miner-address" | "--mineraddress" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --miner-address\n{}", usage()))?;
                miner_address = Some(value);
                miner_address_set = true;
            }
            "--txconfirmtarget" | "--tx-confirm-target" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --txconfirmtarget\n{}", usage()))?;
                tx_confirm_target = value
                    .parse::<u32>()
                    .map_err(|_| format!("invalid txconfirmtarget '{value}'\n{}", usage()))?;
                if tx_confirm_target == 0 {
                    return Err(format!("txconfirmtarget must be > 0\n{}", usage()));
                }
                tx_confirm_target_set = true;
            }
            "--p2p-addr" | "--p2paddr" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --p2p-addr\n{}", usage()))?;
                p2p_addr = Some(
                    parse_socket_addr_with_default_port(&value, default_p2p_addr(network).port())
                        .ok_or_else(|| format!("invalid p2p addr '{value}'\n{}", usage()))?,
                );
                p2p_addr_set = true;
            }
            "--no-p2p-listen" | "--no-listen" => {
                p2p_listen = false;
                p2p_listen_set = true;
            }
            "--rpc-addr" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --rpc-addr\n{}", usage()))?;
                rpc_addr = Some(
                    value
                        .parse::<SocketAddr>()
                        .map_err(|_| format!("invalid rpc addr '{value}'\n{}", usage()))?,
                );
                rpc_addr_set = true;
            }
            "--rpc-user" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --rpc-user\n{}", usage()))?;
                rpc_user = Some(value);
                rpc_user_set = true;
            }
            "--rpc-pass" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --rpc-pass\n{}", usage()))?;
                rpc_pass = Some(value);
                rpc_pass_set = true;
            }
            "--rpc-allow-ip" | "--rpcallowip" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --rpc-allow-ip\n{}", usage()))?;
                rpc_allow_ips.push(value);
            }
            "--network" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --network\n{}", usage()))?;
                network = match value.as_str() {
                    "mainnet" => Network::Mainnet,
                    "testnet" => Network::Testnet,
                    "regtest" => Network::Regtest,
                    _ => return Err(format!("invalid network '{value}'\n{}", usage())),
                };
                network_set = true;
            }
            "--getdata-batch" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --getdata-batch\n{}", usage()))?;
                getdata_batch = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid getdata batch '{value}'\n{}", usage()))?;
                getdata_batch_set = true;
                if getdata_batch == 0 {
                    return Err(format!("getdata batch must be > 0\n{}", usage()));
                }
            }
            "--block-peers" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --block-peers\n{}", usage()))?;
                block_peers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid block peers '{value}'\n{}", usage()))?;
                block_peers_set = true;
            }
            "--maxconnections" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --maxconnections\n{}", usage()))?;
                max_connections = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid maxconnections '{value}'\n{}", usage()))?;
                max_connections_set = true;
                if max_connections == 0 {
                    return Err(format!("maxconnections must be > 0\n{}", usage()));
                }
            }
            "--header-peers" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --header-peers\n{}", usage()))?;
                header_peers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid header peers '{value}'\n{}", usage()))?;
                header_peers_set = true;
                if header_peers == 0 {
                    return Err(format!("header peers must be > 0\n{}", usage()));
                }
            }
            "--header-peer" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --header-peer\n{}", usage()))?;
                header_peer_addrs.push(value);
            }
            "--header-lead" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --header-lead\n{}", usage()))?;
                header_lead = value
                    .parse::<i32>()
                    .map_err(|_| format!("invalid header lead '{value}'\n{}", usage()))?;
                header_lead_set = true;
                if header_lead < 0 {
                    return Err(format!("header lead must be >= 0\n{}", usage()));
                }
            }
            "--tx-peers" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --tx-peers\n{}", usage()))?;
                tx_peers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid tx peers '{value}'\n{}", usage()))?;
                tx_peers_set = true;
            }
            "--inflight-per-peer" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --inflight-per-peer\n{}", usage()))?;
                inflight_per_peer = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid inflight per peer '{value}'\n{}", usage()))?;
                inflight_per_peer_set = true;
                if inflight_per_peer == 0 {
                    return Err(format!("inflight per peer must be > 0\n{}", usage()));
                }
            }
            "--minrelaytxfee" | "--min-relay-tx-fee" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --minrelaytxfee\n{}", usage()))?;
                min_relay_fee_per_kb =
                    parse_fee_rate_per_kb(&value).map_err(|err| format!("{err}\n{}", usage()))?;
                min_relay_fee_per_kb_set = true;
            }
            "--limitfreerelay" | "--limit-free-relay" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --limitfreerelay\n{}", usage()))?;
                limit_free_relay_kb_per_minute = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid limitfreerelay '{value}'\n{}", usage()))?;
                limit_free_relay_kb_per_minute_set = true;
            }
            "--accept-non-standard" => {
                require_standard = Some(false);
            }
            "--require-standard" => {
                require_standard = Some(true);
            }
            "--mempool-max-mb" | "--maxmempool" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --mempool-max-mb\n{}", usage()))?;
                mempool_max_mb = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid mempool max mb '{value}'\n{}", usage()))?;
                mempool_max_mb_set = true;
            }
            "--mempool-persist-interval" => {
                let value = args.next().ok_or_else(|| {
                    format!("missing value for --mempool-persist-interval\n{}", usage())
                })?;
                mempool_persist_interval_secs = value.parse::<u64>().map_err(|_| {
                    format!("invalid mempool persist interval '{value}'\n{}", usage())
                })?;
                mempool_persist_interval_set = true;
            }
            "--fee-estimates-persist-interval" => {
                let value = args.next().ok_or_else(|| {
                    format!(
                        "missing value for --fee-estimates-persist-interval\n{}",
                        usage()
                    )
                })?;
                fee_estimates_persist_interval_secs = value.parse::<u64>().map_err(|_| {
                    format!(
                        "invalid fee estimates persist interval '{value}'\n{}",
                        usage()
                    )
                })?;
                fee_estimates_persist_interval_set = true;
            }
            "--status-interval" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --status-interval\n{}", usage()))?;
                status_interval_secs = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid status interval '{value}'\n{}", usage()))?;
                status_interval_set = true;
            }
            "--db-cache-mb" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --db-cache-mb\n{}", usage()))?;
                let mb = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid db cache '{value}'\n{}", usage()))?;
                db_cache_mb = mb;
                db_cache_set = true;
            }
            "--db-write-buffer-mb" => {
                let value = args.next().ok_or_else(|| {
                    format!("missing value for --db-write-buffer-mb\n{}", usage())
                })?;
                let mb = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid db write buffer '{value}'\n{}", usage()))?;
                db_write_buffer_mb = mb;
                db_write_buffer_set = true;
            }
            "--db-journal-mb" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --db-journal-mb\n{}", usage()))?;
                let mb = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid db journal '{value}'\n{}", usage()))?;
                db_journal_mb = mb;
                db_journal_set = true;
            }
            "--db-memtable-mb" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --db-memtable-mb\n{}", usage()))?;
                let mb = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid db memtable '{value}'\n{}", usage()))?;
                let bytes = mb_to_bytes(mb);
                if bytes > u64::from(u32::MAX) {
                    return Err(format!("db memtable too large '{value}'\n{}", usage()));
                }
                db_memtable_mb = mb;
                db_memtable_set = true;
            }
            "--db-flush-workers" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --db-flush-workers\n{}", usage()))?;
                let workers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid db flush workers '{value}'\n{}", usage()))?;
                if workers == 0 {
                    return Err(format!("db flush workers must be > 0\n{}", usage()));
                }
                db_flush_workers = workers;
                db_flush_workers_set = true;
            }
            "--db-compaction-workers" => {
                let value = args.next().ok_or_else(|| {
                    format!("missing value for --db-compaction-workers\n{}", usage())
                })?;
                let workers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid db compaction workers '{value}'\n{}", usage()))?;
                if workers == 0 {
                    return Err(format!("db compaction workers must be > 0\n{}", usage()));
                }
                db_compaction_workers = workers;
                db_compaction_workers_set = true;
            }
            "--db-fsync-ms" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --db-fsync-ms\n{}", usage()))?;
                let ms = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid db fsync ms '{value}'\n{}", usage()))?;
                if ms > u64::from(u16::MAX) {
                    return Err(format!("db fsync ms too large '{value}'\n{}", usage()));
                }
                let ms = ms as u16;
                db_fsync_ms = if ms == 0 { None } else { Some(ms) };
                db_fsync_ms_set = true;
            }
            "--utxo-cache-entries" => {
                let value = args.next().ok_or_else(|| {
                    format!("missing value for --utxo-cache-entries\n{}", usage())
                })?;
                utxo_cache_entries = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid utxo cache entries '{value}'\n{}", usage()))?;
                utxo_cache_entries_set = true;
            }
            "--header-verify-workers" => {
                let value = args.next().ok_or_else(|| {
                    format!("missing value for --header-verify-workers\n{}", usage())
                })?;
                header_verify_workers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid header verify workers '{value}'\n{}", usage()))?;
                header_verify_workers_set = true;
            }
            "--verify-workers" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --verify-workers\n{}", usage()))?;
                verify_workers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid verify workers '{value}'\n{}", usage()))?;
                verify_workers_set = true;
            }
            "--verify-queue" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --verify-queue\n{}", usage()))?;
                verify_queue = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid verify queue '{value}'\n{}", usage()))?;
                verify_queue_set = true;
            }
            "--shielded-workers" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --shielded-workers\n{}", usage()))?;
                shielded_workers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid shielded workers '{value}'\n{}", usage()))?;
                shielded_workers_set = true;
            }
            "--dashboard-addr" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --dashboard-addr\n{}", usage()))?;
                dashboard_addr = Some(
                    value
                        .parse::<SocketAddr>()
                        .map_err(|_| format!("invalid dashboard addr '{value}'\n{}", usage()))?,
                );
            }
            "--help" | "-h" => return Ok(CliAction::PrintHelp),
            "--version" | "-V" => return Ok(CliAction::PrintVersion),
            other => {
                return Err(format!("unknown argument '{other}'\n{}", usage()));
            }
        }
    }

    let data_dir = data_dir.unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));
    let conf_file = conf_path.unwrap_or_else(|| data_dir.join("flux.conf"));
    let mut unsupported_conf_keys: Vec<String> = Vec::new();
    if let Some(conf) = load_flux_conf(&conf_file)? {
        if !network_set {
            if let Some(values) = conf.get("regtest") {
                if let Some(raw) = values.last() {
                    match parse_conf_bool(raw) {
                        Some(true) => network = Network::Regtest,
                        Some(false) => {}
                        None => {
                            return Err(format!(
                                "invalid regtest value '{raw}' in {}",
                                conf_file.display()
                            ));
                        }
                    }
                }
            }
            if network != Network::Regtest {
                if let Some(values) = conf.get("testnet") {
                    if let Some(raw) = values.last() {
                        match parse_conf_bool(raw) {
                            Some(true) => network = Network::Testnet,
                            Some(false) => {}
                            None => {
                                return Err(format!(
                                    "invalid testnet value '{raw}' in {}",
                                    conf_file.display()
                                ));
                            }
                        }
                    }
                }
            }
        }

        let params = chain_params(network);
        let default_port = params.default_port;

        if !rpc_user_set {
            if let Some(values) = conf.get("rpcuser") {
                if let Some(value) = values.last() {
                    rpc_user = Some(value.clone());
                }
            }
        }
        if !rpc_pass_set {
            if let Some(values) = conf.get("rpcpassword") {
                if let Some(value) = values.last() {
                    rpc_pass = Some(value.clone());
                }
            }
        }
        if let Some(values) = conf.get("rpcallowip") {
            rpc_allow_ips.extend(values.iter().cloned());
        }

        if !rpc_addr_set {
            let mut bind_socket: Option<SocketAddr> = None;
            let mut bind_ip: Option<IpAddr> = None;
            if let Some(values) = conf.get("rpcbind") {
                if let Some(raw) = values.last() {
                    if let Ok(addr) = raw.parse::<SocketAddr>() {
                        bind_socket = Some(addr);
                    } else if let Ok(ip) = raw.parse::<IpAddr>() {
                        bind_ip = Some(ip);
                    } else {
                        return Err(format!(
                            "invalid rpcbind '{raw}' in {}",
                            conf_file.display()
                        ));
                    }
                }
            }

            let mut port: Option<u16> = None;
            if let Some(values) = conf.get("rpcport") {
                if let Some(raw) = values.last() {
                    port = Some(raw.parse::<u16>().map_err(|_| {
                        format!("invalid rpcport '{raw}' in {}", conf_file.display())
                    })?);
                }
            }

            if let Some(addr) = bind_socket {
                rpc_addr = Some(addr);
            } else if bind_ip.is_some() || port.is_some() {
                let ip = bind_ip.unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
                let default_port = default_rpc_addr(network).port();
                rpc_addr = Some(SocketAddr::new(ip, port.unwrap_or(default_port)));
            }
        }

        if !p2p_listen_set {
            if let Some(values) = conf.get("listen") {
                if let Some(raw) = values.last() {
                    match parse_conf_bool(raw) {
                        Some(value) => p2p_listen = value,
                        None => {
                            return Err(format!(
                                "invalid listen value '{raw}' in {}",
                                conf_file.display()
                            ));
                        }
                    }
                }
            }
        }

        if !p2p_addr_set {
            if let Some(values) = conf.get("bind") {
                if let Some(raw) = values.last() {
                    p2p_addr = Some(
                        parse_socket_addr_with_default_port(raw, default_port).ok_or_else(
                            || format!("invalid bind '{raw}' in {}", conf_file.display()),
                        )?,
                    );
                }
            }
        }

        if !miner_address_set {
            if let Some(values) = conf.get("mineraddress") {
                if let Some(value) = values.last() {
                    miner_address = Some(value.clone());
                }
            }
        }

        if !tx_confirm_target_set {
            if let Some(values) = conf.get("txconfirmtarget") {
                if let Some(raw) = values.last() {
                    tx_confirm_target = raw.parse::<u32>().map_err(|_| {
                        format!("invalid txconfirmtarget '{raw}' in {}", conf_file.display())
                    })?;
                    if tx_confirm_target == 0 {
                        return Err(format!(
                            "invalid txconfirmtarget '{raw}' in {}",
                            conf_file.display()
                        ));
                    }
                }
            }
        }

        if !db_cache_set {
            if let Some(values) = conf.get("dbcache") {
                if let Some(raw) = values.last() {
                    db_cache_mb = raw.parse::<u64>().map_err(|_| {
                        format!("invalid dbcache '{raw}' in {}", conf_file.display())
                    })?;
                    db_cache_set = true;
                }
            }
        }

        if !mempool_max_mb_set {
            if let Some(values) = conf.get("maxmempool") {
                if let Some(raw) = values.last() {
                    mempool_max_mb = raw.parse::<u64>().map_err(|_| {
                        format!("invalid maxmempool '{raw}' in {}", conf_file.display())
                    })?;
                    mempool_max_mb_set = true;
                }
            }
        }

        if !max_connections_set {
            if let Some(values) = conf.get("maxconnections") {
                if let Some(raw) = values.last() {
                    max_connections = raw.parse::<usize>().map_err(|_| {
                        format!("invalid maxconnections '{raw}' in {}", conf_file.display())
                    })?;
                    if max_connections == 0 {
                        return Err(format!(
                            "invalid maxconnections '{raw}' in {}",
                            conf_file.display()
                        ));
                    }
                }
            }
        }

        if !min_relay_fee_per_kb_set {
            if let Some(values) = conf.get("minrelaytxfee") {
                if let Some(raw) = values.last() {
                    min_relay_fee_per_kb = parse_fee_rate_per_kb(raw).map_err(|err| {
                        format!(
                            "invalid minrelaytxfee '{raw}' in {}: {err}",
                            conf_file.display()
                        )
                    })?;
                }
            }
        }

        if !limit_free_relay_kb_per_minute_set {
            if let Some(values) = conf.get("limitfreerelay") {
                if let Some(raw) = values.last() {
                    limit_free_relay_kb_per_minute = raw.parse::<u64>().map_err(|_| {
                        format!("invalid limitfreerelay '{raw}' in {}", conf_file.display())
                    })?;
                }
            }
        }

        if !log_level_set {
            if let Some(values) = conf.get("loglevel") {
                if let Some(raw) = values.last() {
                    log_level = logging::Level::parse(raw).ok_or_else(|| {
                        format!("invalid loglevel '{raw}' in {}", conf_file.display())
                    })?;
                }
            }
        }

        if !log_format_set {
            if let Some(values) = conf.get("logformat") {
                if let Some(raw) = values.last() {
                    log_format = logging::Format::parse(raw).ok_or_else(|| {
                        format!("invalid logformat '{raw}' in {}", conf_file.display())
                    })?;
                }
            }
        }

        if !log_timestamps_set {
            if let Some(values) = conf.get("logtimestamps") {
                if let Some(raw) = values.last() {
                    match parse_conf_bool(raw) {
                        Some(value) => log_timestamps = value,
                        None => {
                            return Err(format!(
                                "invalid logtimestamps value '{raw}' in {}",
                                conf_file.display()
                            ));
                        }
                    }
                }
            }
        }

        if let Some(values) = conf.get("addnode") {
            let mut seen = HashSet::new();
            for raw in values {
                let addr = parse_socket_addr_with_default_port(raw, default_port)
                    .ok_or_else(|| format!("invalid addnode '{raw}' in {}", conf_file.display()))?;
                if seen.insert(addr) {
                    addnode_addrs.push(addr);
                }
            }
        }

        let supported_keys = [
            "addnode",
            "bind",
            "dbcache",
            "limitfreerelay",
            "logformat",
            "loglevel",
            "logtimestamps",
            "listen",
            "maxconnections",
            "maxmempool",
            "mineraddress",
            "minrelaytxfee",
            "rpcallowip",
            "rpcbind",
            "rpcpassword",
            "rpcport",
            "rpcuser",
            "regtest",
            "testnet",
            "txconfirmtarget",
        ];
        let mut unsupported: Vec<String> = conf
            .keys()
            .filter(|key| !supported_keys.contains(&key.as_str()))
            .cloned()
            .collect();
        unsupported.sort();
        unsupported_conf_keys = unsupported;
    }

    logging::init(logging::LogConfig {
        level: log_level,
        format: log_format,
        timestamps: log_timestamps,
    });
    if !unsupported_conf_keys.is_empty() {
        log_warn!(
            "Warning: ignoring unsupported keys in {}: {}",
            conf_file.display(),
            unsupported_conf_keys.join(", ")
        );
    }

    if let Some(profile) = profile {
        macro_rules! set_default {
            ($var:ident, $set:ident, $value:expr) => {
                if !$set {
                    $var = $value;
                }
            };
        }

        match profile {
            RunProfile::Low => {
                set_default!(getdata_batch, getdata_batch_set, 64);
                set_default!(block_peers, block_peers_set, 1);
                set_default!(header_peers, header_peers_set, 2);
                set_default!(tx_peers, tx_peers_set, 0);
                set_default!(inflight_per_peer, inflight_per_peer_set, 1);
                set_default!(header_lead, header_lead_set, DEFAULT_HEADER_LEAD);

                set_default!(mempool_max_mb, mempool_max_mb_set, 100);
                set_default!(
                    mempool_persist_interval_secs,
                    mempool_persist_interval_set,
                    0
                );
                set_default!(
                    fee_estimates_persist_interval_secs,
                    fee_estimates_persist_interval_set,
                    0
                );
                set_default!(status_interval_secs, status_interval_set, 30);

                set_default!(db_cache_mb, db_cache_set, 128);
                set_default!(db_write_buffer_mb, db_write_buffer_set, 512);
                set_default!(db_journal_mb, db_journal_set, 1024);
                set_default!(db_memtable_mb, db_memtable_set, 16);
                set_default!(db_flush_workers, db_flush_workers_set, 1);
                set_default!(db_compaction_workers, db_compaction_workers_set, 2);
                set_default!(db_fsync_ms, db_fsync_ms_set, None);

                set_default!(utxo_cache_entries, utxo_cache_entries_set, 50_000);
                set_default!(header_verify_workers, header_verify_workers_set, 1);
                set_default!(verify_workers, verify_workers_set, 0);
                set_default!(verify_queue, verify_queue_set, 0);
                set_default!(shielded_workers, shielded_workers_set, 1);
            }
            RunProfile::Default => {}
            RunProfile::High => {
                set_default!(getdata_batch, getdata_batch_set, 256);
                set_default!(block_peers, block_peers_set, 6);
                set_default!(header_peers, header_peers_set, 8);
                set_default!(tx_peers, tx_peers_set, 4);
                set_default!(inflight_per_peer, inflight_per_peer_set, 2);
                set_default!(header_lead, header_lead_set, DEFAULT_HEADER_LEAD);

                set_default!(mempool_max_mb, mempool_max_mb_set, 1000);

                set_default!(db_cache_mb, db_cache_set, DEFAULT_DB_CACHE_MB);
                set_default!(db_write_buffer_mb, db_write_buffer_set, 4096);
                set_default!(db_journal_mb, db_journal_set, 16384);
                set_default!(db_memtable_mb, db_memtable_set, 128);
                set_default!(db_flush_workers, db_flush_workers_set, 4);
                set_default!(db_compaction_workers, db_compaction_workers_set, 6);
                set_default!(db_fsync_ms, db_fsync_ms_set, None);

                set_default!(utxo_cache_entries, utxo_cache_entries_set, 1_000_000);
            }
        }

        if profile != RunProfile::Default {
            log_info!("Using profile {}", profile.as_str());
        }
    }

    if rpc_user.is_some() ^ rpc_pass.is_some() {
        return Err(format!(
            "rpcuser and rpcpassword must both be set (via CLI or flux.conf)\n{}",
            usage()
        ));
    }

    if let Some(address) = miner_address.as_deref() {
        address_to_script_pubkey(address, network).map_err(|err| {
            let message = match err {
                AddressError::UnknownPrefix => "miner address has invalid prefix",
                _ => "invalid miner address",
            };
            format!("{message} '{address}'\n{}", usage())
        })?;
    }

    let require_standard = require_standard.unwrap_or(network != Network::Regtest);
    let partition_count = fluxd_storage::Column::ALL.len() as u64;
    if !db_memtable_set && db_memtable_mb == 0 {
        db_memtable_mb = DEFAULT_DB_MEMTABLE_MB;
    }
    let memtable_bytes = mb_to_bytes(db_memtable_mb);
    if memtable_bytes > u64::from(u32::MAX) {
        return Err(format!(
            "db memtable too large '{db_memtable_mb}'\n{}",
            usage()
        ));
    }
    let memtable_bytes = memtable_bytes as u32;

    let min_write_buffer_mb = partition_count.saturating_mul(db_memtable_mb).max(1);
    let min_journal_mb = min_write_buffer_mb.saturating_mul(2);
    if db_write_buffer_mb < min_write_buffer_mb {
        if db_write_buffer_set {
            log_warn!(
                "Warning: --db-write-buffer-mb ({}) is below partitions ({}) × --db-memtable-mb ({}); clamping to {}",
                db_write_buffer_mb,
                partition_count,
                db_memtable_mb,
                min_write_buffer_mb
            );
        }
        db_write_buffer_mb = min_write_buffer_mb;
    } else if !db_write_buffer_set {
        db_write_buffer_mb = db_write_buffer_mb.max(min_write_buffer_mb);
    }
    if db_journal_mb < min_journal_mb {
        if db_journal_set {
            log_warn!(
                "Warning: --db-journal-mb ({}) is below 2 × partitions ({}) × --db-memtable-mb ({}); clamping to {}",
                db_journal_mb,
                partition_count,
                db_memtable_mb,
                min_journal_mb
            );
        }
        db_journal_mb = min_journal_mb;
    } else if !db_journal_set {
        db_journal_mb = db_journal_mb.max(min_journal_mb);
    }

    let db_cache_bytes = Some(mb_to_bytes(db_cache_mb));
    let db_write_buffer_bytes = Some(mb_to_bytes(db_write_buffer_mb));
    let db_journal_bytes = Some(mb_to_bytes(db_journal_mb));
    let db_memtable_bytes = Some(memtable_bytes);
    let db_flush_workers = Some(db_flush_workers);
    let db_compaction_workers = Some(db_compaction_workers);

    Ok(CliAction::Run(Config {
        backend,
        data_dir,
        network,
        params_dir: params_dir.unwrap_or_else(default_params_dir),
        fetch_params,
        reindex,
        resync,
        reindex_txindex,
        reindex_spentindex,
        reindex_addressindex,
        db_info,
        db_info_keys,
        db_integrity,
        miner_address,
        tx_confirm_target,
        scan_flatfiles,
        scan_supply,
        scan_fluxnodes,
        debug_fluxnode_payee_script,
        debug_fluxnode_payout_height,
        debug_fluxnode_payee_candidates,
        check_script,
        log_level,
        log_format,
        log_timestamps,
        p2p_listen,
        p2p_addr,
        rpc_addr,
        rpc_user,
        rpc_pass,
        rpc_allow_ips,
        getdata_batch,
        block_peers,
        header_peers,
        header_lead,
        header_peer_addrs,
        addnode_addrs,
        max_connections,
        tx_peers,
        inflight_per_peer,
        require_standard,
        min_relay_fee_per_kb,
        limit_free_relay_kb_per_minute,
        mempool_max_bytes: mb_to_bytes(mempool_max_mb).try_into().unwrap_or(usize::MAX),
        mempool_persist_interval_secs,
        fee_estimates_persist_interval_secs,
        status_interval_secs,
        dashboard_addr,
        db_cache_bytes,
        db_write_buffer_bytes,
        db_journal_bytes,
        db_memtable_bytes,
        db_flush_workers,
        db_compaction_workers,
        db_fsync_ms,
        utxo_cache_entries,
        header_verify_workers,
        verify_workers,
        verify_queue,
        shielded_workers,
    }))
}

fn load_flux_conf(path: &Path) -> Result<Option<HashMap<String, Vec<String>>>, String> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.to_string()),
    };

    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for raw_line in contents.lines() {
        let mut line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(idx) = line.find('#') {
            line = &line[..idx];
        }
        if let Some(idx) = line.find(';') {
            line = &line[..idx];
        }
        line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some((key, value)) => (key.trim(), value.trim()),
            None => (line, "1"),
        };
        if key.is_empty() {
            continue;
        }
        let key = key.to_ascii_lowercase();
        out.entry(key)
            .or_insert_with(Vec::new)
            .push(value.to_string());
    }
    Ok(Some(out))
}

fn parse_conf_bool(value: &str) -> Option<bool> {
    let value = value.trim();
    if value.is_empty() {
        return Some(true);
    }
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Some(true),
        "0" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn parse_socket_addr_with_default_port(value: &str, default_port: u16) -> Option<SocketAddr> {
    if let Ok(addr) = value.parse::<SocketAddr>() {
        return Some(addr);
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Some(SocketAddr::new(ip, default_port));
    }
    None
}

fn mb_to_bytes(mb: u64) -> u64 {
    mb.saturating_mul(1024 * 1024)
}

fn parse_hex_bytes(value: &str) -> Option<Vec<u8>> {
    let mut hex = value.trim();
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

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        let _ = write!(out, "{:02x}", byte);
    }
    out
}

fn outpoint_to_string(outpoint: &OutPoint) -> String {
    format!(
        "{}:{}",
        stats::hash256_to_hex(&outpoint.hash),
        outpoint.index
    )
}

fn parse_fee_rate_per_kb(value: &str) -> Result<i64, String> {
    if value.contains('.') {
        return parse_amount_zat(value);
    }
    value
        .parse::<i64>()
        .map_err(|_| format!("invalid fee rate '{value}'"))
        .and_then(|amount| {
            if amount < 0 {
                return Err("fee rate must be >= 0".to_string());
            }
            Ok(amount)
        })
}

fn parse_amount_zat(value: &str) -> Result<i64, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("amount is empty".to_string());
    }
    if value.starts_with('-') {
        return Err("amount must be >= 0".to_string());
    }

    let (whole, frac) = match value.split_once('.') {
        Some((whole, frac)) => (whole, Some(frac)),
        None => (value, None),
    };
    if whole.is_empty() && frac.is_none() {
        return Err(format!("invalid amount '{value}'"));
    }

    let whole = if whole.is_empty() {
        0i64
    } else {
        whole
            .parse::<i64>()
            .map_err(|_| format!("invalid amount '{value}'"))?
    };
    if whole < 0 {
        return Err("amount must be >= 0".to_string());
    }

    let mut frac_value = 0i64;
    if let Some(frac) = frac {
        if frac.len() > 8 {
            return Err(format!("amount has too many decimal places '{value}'"));
        }
        if !frac.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(format!("invalid amount '{value}'"));
        }
        let mut frac_str = frac.to_string();
        while frac_str.len() < 8 {
            frac_str.push('0');
        }
        frac_value = frac_str
            .parse::<i64>()
            .map_err(|_| format!("invalid amount '{value}'"))?;
    }

    whole
        .checked_mul(COIN)
        .and_then(|whole_zat| whole_zat.checked_add(frac_value))
        .ok_or_else(|| format!("amount out of range '{value}'"))
}

fn default_rpc_addr(network: Network) -> SocketAddr {
    let port = match network {
        Network::Mainnet => 16_124,
        Network::Testnet | Network::Regtest => 26_124,
    };
    SocketAddr::from(([127, 0, 0, 1], port))
}

fn default_p2p_addr(network: Network) -> SocketAddr {
    let port = match network {
        Network::Mainnet => 16_125,
        Network::Testnet | Network::Regtest => 26_125,
    };
    SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port)
}

fn resolve_verify_settings(
    config: &Config,
    getdata_batch: usize,
    inflight_per_peer: usize,
    block_peers: usize,
) -> VerifySettings {
    let cores = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4);
    let reserved = if cores >= 3 { 1 } else { 0 };
    let available = cores.saturating_sub(reserved).max(1);
    let shielded_workers = if config.shielded_workers > 0 {
        config.shielded_workers
    } else {
        // Shielded proof verification becomes the dominant cost on mainnet, so default to roughly
        // half of available cores, leaving the remainder for block validation/connect + async IO.
        ((available + 1) / 2).max(1)
    };
    let verify_workers = if config.verify_workers > 0 {
        config.verify_workers
    } else {
        available.saturating_sub(shielded_workers).max(1)
    };
    let inflight = getdata_batch
        .saturating_mul(inflight_per_peer.max(1))
        .saturating_mul(block_peers.max(1));
    let verify_queue = if config.verify_queue > 0 {
        config.verify_queue
    } else {
        inflight.max(64)
    };

    VerifySettings {
        verify_workers,
        verify_queue,
        shielded_workers,
    }
}

fn resolve_header_verify_workers(config: &Config) -> usize {
    if config.header_verify_workers > 0 {
        return config.header_verify_workers;
    }
    // Header POW verification can saturate CPU if allowed to use all cores; reserve a little headroom
    // for block connect/verification.
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .saturating_sub(2)
        .max(1)
}

fn usage() -> String {
    [
        "Usage:",
        "  fluxd [options]",
        "  fluxd <command> [options]",
        "",
        "Commands:",
        "  help            Print this help and exit",
        "  version         Print version and exit",
        "  db-info         Alias for --db-info",
        "  db-info-keys    Alias for --db-info-keys",
        "  db-integrity    Alias for --db-integrity",
        "  scan-flatfiles  Alias for --scan-flatfiles",
        "  scan-supply     Alias for --scan-supply",
        "  scan-fluxnodes  Alias for --scan-fluxnodes",
        "",
        "Options:",
        "  --help, -h  Print this help and exit",
        "  --version, -V  Print version and exit",
        "  --backend   Storage backend to use (default: fjall)",
        "  --data-dir  Base data directory (default: ./data)",
        "  --conf  Config file path (default: <data-dir>/flux.conf)",
        "  --params-dir    Shielded params directory (default: ~/.zcash-params)",
        "  --profile  Apply a preset for sync/DB/worker tuning (low|default|high)",
        "  --log-level  Log verbosity (error|warn|info|debug|trace) (default: info)",
        "  --log-format  Log output format (text|json) (default: text)",
        "  --log-timestamps  Enable timestamps in text logs (default: on)",
        "  --no-log-timestamps  Disable timestamps in text logs",
        "  --fetch-params  Download shielded params into --params-dir",
        "  --reindex  Rebuild db/ indexes from existing flatfiles under --data-dir/blocks (no network)",
        "  --resync  Wipe db/ and blocks/ under --data-dir and restart from genesis",
        "  --reindex-txindex  Rebuild txindex from blocks under --data-dir/blocks",
        "  --reindex-spentindex  Rebuild spent index from blocks under --data-dir/blocks",
        "  --reindex-addressindex  Rebuild address index (outpoints + deltas) from blocks under --data-dir/blocks",
        "  --db-info  Print DB/flatfile size breakdown and fjall telemetry, then exit",
        "  --db-info-keys  Like --db-info, but also counts keys/bytes in each DB partition (slow)",
        "  --db-integrity  Print DB/flatfile sanity + verify last 288 blocks (checklevel 5), then exit nonzero on failure",
        "  --scan-flatfiles  Scan flatfiles for block index mismatches, then exit",
        "  --scan-supply  Scan blocks in the local DB and print coinbase totals, then exit",
        "  --scan-fluxnodes  Scan fluxnode records in the local DB and print summary stats, then exit",
        "  --debug-fluxnode-payee-script  Scan fluxnode records for a matching payee script, then exit",
        "  --debug-fluxnode-payouts  Print expected deterministic fluxnode payouts at a height, then exit",
        "  --debug-fluxnode-payee-candidates  Print ordered deterministic payee candidates for a tier+height, then exit",
        "  --skip-script  Disable script validation (testing only)",
        "  --network   Network selection (default: mainnet)",
        "  --miner-address  Default miner address for getblocktemplate when wallet is not available",
        "  --txconfirmtarget  Fee estimation target in blocks when paytxfee is unset (default: 2)",
        "  --p2p-addr  Bind P2P listener (default: 0.0.0.0:16125 mainnet, 26125 testnet)",
        "  --no-p2p-listen  Disable inbound P2P listener",
        "  --rpc-addr  Bind JSON-RPC server (default: 127.0.0.1:16124 mainnet, 26124 testnet)",
        "  --rpc-user  JSON-RPC basic auth username (required unless cookie exists)",
        "  --rpc-pass  JSON-RPC basic auth password (required unless cookie exists)",
        "  --rpc-allow-ip  Allow JSON-RPC requests from IP/CIDR (repeatable; default: localhost only)",
        "  --getdata-batch  Max blocks per getdata request (default: 128)",
        "  --block-peers  Number of parallel peers for block download (default: 3)",
        "  --maxconnections  Maintain at most N total peer connections (default: 125)",
        "  --header-peers  Number of peers to probe for header sync (default: 4)",
        "  --header-peer  Header peer IP:PORT to pin for header sync (repeatable)",
        "  --header-lead  Target header lead over blocks (default: 20000, 0 disables cap)",
        "  --tx-peers  Number of relay peers for tx inventory/tx relay (0 disables, default: 2)",
        "  --inflight-per-peer  Concurrent getdata requests per peer (default: 1)",
        "  --minrelaytxfee  Minimum relay fee-rate in zatoshis/kB (default: 100)",
        "  --limitfreerelay  Rate-limit free transactions to N*1000 bytes/min (default: 500)",
        "  --accept-non-standard  Disable standardness checks (default: off on mainnet/testnet)",
        "  --require-standard  Force standardness checks on regtest (default: off)",
        "  --mempool-max-mb  Mempool max size in MiB (0 disables cap, default: 300)",
        "  --mempool-persist-interval  Persist mempool to disk every N seconds (0 disables, default: 60)",
        "  --fee-estimates-persist-interval  Persist fee estimates every N seconds (0 disables, default: 300)",
        "  --status-interval  Status log interval in seconds (default: 15, 0 disables)",
        "  --db-cache-mb  Fjall block cache size in MiB (default: 256)",
        "  --db-write-buffer-mb  Fjall max write buffer in MiB (default: 2048)",
        "  --db-journal-mb  Fjall max journaling size in MiB (default: 2048)",
        "  --db-memtable-mb  Fjall partition memtable size in MiB (default: 64)",
        "  --db-flush-workers  Fjall flush worker threads (default: 2)",
        "  --db-compaction-workers  Fjall compaction worker threads (default: 4)",
        "  --db-fsync-ms  Fjall async fsync interval in ms (0 disables, optional)",
        "  --utxo-cache-entries  In-memory UTXO entry cache size (0 disables, default: 200000)",
        "  --header-verify-workers  POW header verification threads (0 = auto)",
        "  --verify-workers  Pre-validation worker threads (0 = auto)",
        "  --verify-queue  Pre-validation queue depth (0 = auto)",
        "  --shielded-workers  Shielded verification threads (0 = auto)",
        "  --dashboard-addr  Bind dashboard HTTP server (disabled by default)",
    ]
    .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxd_consensus::constants::COINBASE_MATURITY;
    use fluxd_primitives::hash::sha256d;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn genesis_pow_validation_is_allowed() {
        for network in [Network::Mainnet, Network::Testnet, Network::Regtest] {
            let params = chain_params(network);
            let block = build_genesis_block(&params).expect("genesis block");
            pow_validation::validate_pow_header(&block.header, 0, &params.consensus)
                .expect("genesis pow validation");
        }
    }

    fn merkle_hash_pair(left: &Hash256, right: &Hash256) -> Hash256 {
        let mut buf = [0u8; 64];
        buf[0..32].copy_from_slice(left);
        buf[32..64].copy_from_slice(right);
        sha256d(&buf)
    }

    fn merkle_root(txids: &[Hash256]) -> Hash256 {
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

    fn p2pkh_script(pubkey_hash: [u8; 20]) -> Vec<u8> {
        let mut script = Vec::with_capacity(25);
        script.extend_from_slice(&[0x76, 0xa9, 0x14]);
        script.extend_from_slice(&pubkey_hash);
        script.extend_from_slice(&[0x88, 0xac]);
        script
    }

    fn build_coinbase_tx(
        height: i32,
        params: &ChainParams,
        miner_script_pubkey: Vec<u8>,
    ) -> Transaction {
        let miner_value = block_subsidy(height, &params.consensus);
        let exchange_amount = exchange_fund_amount(height, &params.funding);
        let foundation_amount = foundation_fund_amount(height, &params.funding);
        let swap_amount = swap_pool_amount(height as i64, &params.swap_pool);

        let mut vout = Vec::new();
        vout.push(TxOut {
            value: miner_value,
            script_pubkey: miner_script_pubkey,
        });
        if exchange_amount > 0 {
            let script = address_to_script_pubkey(params.funding.exchange_address, params.network)
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

        Transaction {
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
        }
    }

    fn connect_regtest_block(
        chainstate: &ChainState<MemoryStore>,
        params: &ChainParams,
        height: i32,
        transactions: Vec<Transaction>,
    ) -> Hash256 {
        let tip = chainstate
            .best_block()
            .expect("best block")
            .expect("best block present");
        let tip_entry = chainstate
            .header_entry(&tip.hash)
            .expect("header entry")
            .expect("header entry present");
        let spacing = params.consensus.pow_target_spacing.max(1) as u32;
        let time = tip_entry.time.saturating_add(spacing);
        let bits = chainstate
            .next_work_required_bits(&tip.hash, height, time as i64, &params.consensus)
            .expect("next bits");

        let txids: Vec<Hash256> = transactions
            .iter()
            .map(|tx| tx.txid().expect("txid"))
            .collect();
        let merkle_root = merkle_root(&txids);
        let final_sapling_root = chainstate.sapling_root().expect("sapling root");
        let header = BlockHeader {
            version: CURRENT_VERSION,
            prev_block: tip.hash,
            merkle_root,
            final_sapling_root,
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
            transactions,
        };
        let block_bytes = block.consensus_encode().expect("encode block");
        let flags = ValidationFlags::default();
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
                None,
            )
            .expect("connect block");
        chainstate.commit_batch(batch).expect("commit block");
        block.header.hash()
    }

    fn extend_regtest_chain_to_height(
        chainstate: &ChainState<MemoryStore>,
        params: &ChainParams,
        target_height: i32,
    ) {
        loop {
            let tip = chainstate
                .best_block()
                .expect("best block")
                .expect("best block present");
            if tip.height >= target_height {
                break;
            }
            let height = tip.height + 1;
            let coinbase = build_coinbase_tx(height, params, Vec::new());
            connect_regtest_block(chainstate, params, height, vec![coinbase]);
        }
    }

    #[test]
    fn db_schema_version_missing_on_empty_db_is_initialized() {
        let store = Store::Memory(MemoryStore::new());
        let version = ensure_db_schema_version(&store).expect("db schema version");
        assert_eq!(version, DB_SCHEMA_VERSION);
        assert_eq!(
            meta_u32(&store, DB_SCHEMA_VERSION_KEY).expect("meta"),
            Some(DB_SCHEMA_VERSION)
        );
    }

    #[test]
    fn db_schema_version_missing_on_nonempty_db_is_initialized() {
        let store = Store::Memory(MemoryStore::new());
        store
            .put(fluxd_storage::Column::HeaderIndex, b"header", b"entry")
            .expect("put header");
        let version = ensure_db_schema_version(&store).expect("db schema version");
        assert_eq!(version, DB_SCHEMA_VERSION);
        assert_eq!(
            meta_u32(&store, DB_SCHEMA_VERSION_KEY).expect("meta"),
            Some(DB_SCHEMA_VERSION)
        );
    }

    #[test]
    fn index_schema_version_missing_on_empty_column_is_initialized() {
        let store = Store::Memory(MemoryStore::new());
        let key = b"test_index_schema_version";
        let expected = 42u32;
        let version = ensure_index_schema_version(
            &store,
            "txindex",
            key,
            expected,
            "--reindex-txindex",
            &[fluxd_storage::Column::TxIndex],
        )
        .expect("index version");
        assert_eq!(version, expected);
        assert_eq!(meta_u32(&store, key).expect("meta"), Some(expected));
    }

    #[test]
    fn index_schema_version_missing_on_nonempty_column_is_initialized_for_v1() {
        let store = Store::Memory(MemoryStore::new());
        store
            .put(fluxd_storage::Column::TxIndex, b"txid", b"entry")
            .expect("put txindex entry");
        let key = b"test_index_schema_version_nonempty";
        let version = ensure_index_schema_version(
            &store,
            "txindex",
            key,
            1,
            "--reindex-txindex",
            &[fluxd_storage::Column::TxIndex],
        )
        .expect("index version");
        assert_eq!(version, 1);
        assert_eq!(meta_u32(&store, key).expect("meta"), Some(1));
    }

    #[test]
    fn index_schema_version_missing_on_nonempty_column_errors_for_newer_versions() {
        let store = Store::Memory(MemoryStore::new());
        store
            .put(fluxd_storage::Column::TxIndex, b"txid", b"entry")
            .expect("put txindex entry");
        let key = b"test_index_schema_version_nonempty_new";
        let err = ensure_index_schema_version(
            &store,
            "txindex",
            key,
            2,
            "--reindex-txindex",
            &[fluxd_storage::Column::TxIndex],
        )
        .unwrap_err();
        assert!(err.contains("schema version missing"), "{err}");
        assert_eq!(meta_u32(&store, key).expect("meta"), None);
    }

    #[test]
    fn reindex_spentindex_rebuilds_details_from_txindex() {
        struct TempDirGuard {
            path: PathBuf,
        }

        impl Drop for TempDirGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.path);
            }
        }

        fn temp_data_dir(prefix: &str) -> PathBuf {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
        }

        let data_dir = temp_data_dir("fluxd-spentindex-test");
        let _guard = TempDirGuard {
            path: data_dir.clone(),
        };
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        let blocks_dir = data_dir.join("blocks");
        std::fs::create_dir_all(&blocks_dir).expect("create blocks dir");
        let blocks = FlatFileStore::new(&blocks_dir, 10_000_000).expect("flatfiles");
        let undo =
            FlatFileStore::new_with_prefix(&blocks_dir, "undo", 10_000_000).expect("flatfiles");
        let store = Arc::new(MemoryStore::new());
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let params = chain_params(Network::Regtest);
        let flags = ValidationFlags::default();
        let write_lock = Mutex::new(());
        ensure_genesis(&chainstate, &params, &flags, None, &write_lock).expect("genesis");

        let pubkey_hash = [0x11u8; 20];
        let miner_script_pubkey = p2pkh_script(pubkey_hash);
        let height = 1;
        let miner_value = block_subsidy(height, &params.consensus);
        let coinbase = build_coinbase_tx(height, &params, miner_script_pubkey);
        let coinbase_txid = coinbase.txid().expect("coinbase txid");
        connect_regtest_block(&chainstate, &params, height, vec![coinbase]);

        extend_regtest_chain_to_height(&chainstate, &params, COINBASE_MATURITY);

        let spend_height = COINBASE_MATURITY + 1;
        let spend_value = miner_value.saturating_sub(1000);
        let spend_tx = Transaction {
            f_overwintered: false,
            version: 1,
            version_group_id: 0,
            vin: vec![TxIn {
                prevout: OutPoint {
                    hash: coinbase_txid,
                    index: 0,
                },
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![TxOut {
                value: spend_value,
                script_pubkey: Vec::new(),
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
        let coinbase = build_coinbase_tx(spend_height, &params, Vec::new());
        connect_regtest_block(&chainstate, &params, spend_height, vec![coinbase, spend_tx]);

        let outpoint = OutPoint {
            hash: coinbase_txid,
            index: 0,
        };
        let spent_before = chainstate
            .spent_info(&outpoint)
            .expect("spent info")
            .expect("spent entry");
        let details_before = spent_before.details.expect("details");
        assert_eq!(details_before.satoshis, miner_value);
        assert_eq!(details_before.address_type, 1);
        assert_eq!(details_before.address_hash, pubkey_hash);

        rebuild_spentindex(&chainstate, &write_lock).expect("rebuild spent index");

        let spent_after = chainstate
            .spent_info(&outpoint)
            .expect("spent info")
            .expect("spent entry");
        let details_after = spent_after.details.expect("details");
        assert_eq!(details_after.satoshis, details_before.satoshis);
        assert_eq!(details_after.address_type, details_before.address_type);
        assert_eq!(details_after.address_hash, details_before.address_hash);
    }
}
