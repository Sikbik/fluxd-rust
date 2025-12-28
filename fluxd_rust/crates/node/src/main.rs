mod dashboard;
mod mempool;
mod p2p;
mod peer_book;
mod rpc;
mod stats;
mod tx_relay;

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{bounded, unbounded};
use fluxd_chainstate::flatfiles::FlatFileStore;
use fluxd_chainstate::index::HeaderEntry;
use fluxd_chainstate::metrics::ConnectMetrics;
use fluxd_chainstate::state::{ChainState, HeaderValidationCache};
use fluxd_chainstate::validation::{validate_block_with_txids, ValidationFlags, ValidationMetrics};
use fluxd_consensus::money::{money_range, COIN, MAX_MONEY};
use fluxd_consensus::params::{chain_params, hash256_from_hex, ChainParams, Network};
use fluxd_consensus::upgrades::current_epoch_branch_id;
use fluxd_consensus::Hash256;
use fluxd_consensus::{
    block_subsidy, exchange_fund_amount, foundation_fund_amount, swap_pool_amount,
};
use fluxd_pow::validation as pow_validation;
use fluxd_primitives::block::{Block, BlockHeader, CURRENT_VERSION};
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::transaction::{Transaction, TxIn, TxOut};
use fluxd_shielded::{
    default_params_dir, fetch_params, load_params, verify_transaction, ShieldedParams,
};
use fluxd_storage::fjall::{FjallOptions, FjallStore};
use fluxd_storage::memory::MemoryStore;
use fluxd_storage::{KeyValueStore, StoreError, WriteBatch};
use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;

use crate::p2p::{parse_addr, parse_headers, NetTotals, Peer, PeerKind, PeerRegistry};
use crate::peer_book::HeaderPeerBook;
use crate::stats::{hash256_to_hex, snapshot_stats, HeaderMetrics, SyncMetrics};

const DEFAULT_DATA_DIR: &str = "data";
const DEFAULT_MAX_FLATFILE_SIZE: u64 = 128 * 1024 * 1024;
const DEFAULT_CONNECT_TIMEOUT_SECS: u64 = 5;
const DEFAULT_HANDSHAKE_TIMEOUT_SECS: u64 = 8;
const DEFAULT_GETDATA_BATCH: usize = 128;
const DEFAULT_BLOCK_PEERS: usize = 4;
const DEFAULT_HEADER_PEERS: usize = 4;
const DEFAULT_HEADER_LEAD: i32 = 20000;
const DEFAULT_INFLIGHT_PER_PEER: usize = 2;
const DEFAULT_TX_PEERS: usize = 2;
const DEFAULT_UTXO_CACHE_ENTRIES: usize = 200_000;
const READ_TIMEOUT_SECS: u64 = 120;
const READ_TIMEOUT_RETRIES: usize = 3;
const BLOCK_READ_TIMEOUT_SECS: u64 = 30;
const BLOCK_READ_TIMEOUT_RETRIES: usize = 2;
const BLOCK_IDLE_SECS: u64 = 45;
const HEADERS_TIMEOUT_SECS_PROBE: u64 = 12;
const HEADERS_TIMEOUT_SECS_BEHIND: u64 = 20;
const HEADERS_TIMEOUT_SECS_IDLE: u64 = 8;
const IDLE_SLEEP_SECS: u64 = 2;
const HEADER_TIMEOUT_RETRIES_BEHIND: usize = 2;
const HEADER_TIMEOUT_RETRIES_IDLE: usize = 3;
const HEADER_STALL_SECS_IDLE: u64 = 90;
const HEADER_IDLE_REPROBE_SECS: u64 = 120;
const BLOCK_STALL_SECS: u64 = 90;
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
const PEERS_FILE_VERSION: u32 = 1;
const PEERS_PERSIST_INTERVAL_SECS: u64 = 60;
const BANLIST_PERSIST_INTERVAL_SECS: u64 = 60;
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

struct Config {
    backend: Backend,
    data_dir: PathBuf,
    network: Network,
    params_dir: PathBuf,
    fetch_params: bool,
    scan_flatfiles: bool,
    scan_supply: bool,
    check_script: bool,
    rpc_addr: Option<SocketAddr>,
    rpc_user: Option<String>,
    rpc_pass: Option<String>,
    getdata_batch: usize,
    block_peers: usize,
    header_peers: usize,
    header_lead: i32,
    header_peer_addrs: Vec<String>,
    tx_peers: usize,
    inflight_per_peer: usize,
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

#[derive(Default)]
struct AddrBook {
    addrs: Mutex<HashSet<SocketAddr>>,
    revision: AtomicU64,
}

impl AddrBook {
    fn revision(&self) -> u64 {
        self.revision.load(AtomicOrdering::Relaxed)
    }

    fn insert_many(&self, addrs: Vec<SocketAddr>) -> usize {
        if addrs.is_empty() {
            return 0;
        }
        let mut inserted = 0;
        if let Ok(mut book) = self.addrs.lock() {
            for addr in addrs {
                if book.len() >= ADDR_BOOK_MAX {
                    break;
                }
                if book.insert(addr) {
                    inserted += 1;
                }
            }
        }
        if inserted > 0 {
            self.revision.fetch_add(1, AtomicOrdering::Relaxed);
        }
        inserted
    }

    fn sample(&self, limit: usize) -> Vec<SocketAddr> {
        if limit == 0 {
            return Vec::new();
        }
        let book = match self.addrs.lock() {
            Ok(book) => book,
            Err(_) => return Vec::new(),
        };
        let mut addrs: Vec<SocketAddr> = book.iter().copied().collect();
        if addrs.is_empty() {
            return addrs;
        }
        addrs.shuffle(&mut rand::thread_rng());
        addrs.truncate(limit);
        addrs
    }

    fn len(&self) -> usize {
        match self.addrs.lock() {
            Ok(book) => book.len(),
            Err(_) => 0,
        }
    }

    fn snapshot(&self) -> Vec<SocketAddr> {
        match self.addrs.lock() {
            Ok(book) => book.iter().copied().collect(),
            Err(_) => Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct PeersFile {
    version: u32,
    addrs: Vec<String>,
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
}

struct VerifyResult {
    hash: fluxd_consensus::Hash256,
    height: i32,
    block: Arc<Block>,
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
        eprintln!("{err}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let start_time = Instant::now();
    let config = parse_args()?;
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

    fs::create_dir_all(data_dir).map_err(|err| err.to_string())?;

    let store = open_store(config.backend, &db_path, &config)?;
    let store = Arc::new(store);

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
    let net_totals = Arc::new(NetTotals::default());
    let peer_registry = Arc::new(PeerRegistry::default());
    let header_peer_book = Arc::new(HeaderPeerBook::default());
    let addr_book = Arc::new(AddrBook::default());
    let peers_path = data_dir.join(PEERS_FILE_NAME);
    let banlist_path = data_dir.join(BANLIST_FILE_NAME);

    match load_peers_file(&peers_path) {
        Ok(addrs) => {
            let loaded = addr_book.insert_many(addrs);
            if loaded > 0 {
                println!("Loaded {loaded} peers from {}", peers_path.display());
            }
        }
        Err(err) => eprintln!("failed to load peers file: {err}"),
    }
    match header_peer_book.load_banlist(&banlist_path) {
        Ok(loaded) => {
            if loaded > 0 {
                println!("Loaded {loaded} bans from {}", banlist_path.display());
            }
        }
        Err(err) => eprintln!("failed to load banlist: {err}"),
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

    println!(
        "Initialized chainstate on {:?} ({:?})",
        config.network, config.backend
    );

    if config.scan_flatfiles {
        scan_flatfiles(chainstate.as_ref(), &blocks_path)?;
        return Ok(());
    }

    if config.scan_supply {
        scan_supply(chainstate.as_ref(), params.as_ref())?;
        return Ok(());
    }

    let rpc_addr = config.rpc_addr.unwrap_or_else(|| default_rpc_addr(network));
    let rpc_auth =
        rpc::load_or_create_auth(config.rpc_user.clone(), config.rpc_pass.clone(), data_dir)?;

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
    let mempool = Arc::new(Mutex::new(mempool::Mempool::default()));
    let (tx_announce, _) = broadcast::channel::<Hash256>(TX_ANNOUNCE_QUEUE);
    {
        let chainstate = Arc::clone(&chainstate);
        let mempool = Arc::clone(&mempool);
        let mempool_flags = flags.clone();
        let params = params.as_ref().clone();
        let data_dir = data_dir.clone();
        let net_totals = Arc::clone(&net_totals);
        let peer_registry = Arc::clone(&peer_registry);
        let header_peer_book = Arc::clone(&header_peer_book);
        let tx_announce = tx_announce.clone();
        thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("rpc runtime");
            runtime.block_on(async move {
                if let Err(err) = rpc::serve_rpc(
                    rpc_addr,
                    rpc_auth,
                    chainstate,
                    mempool,
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
                    eprintln!("{err}");
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
        println!("UTXO stats missing; rebuilding from UTXO set (one-time).");
        let _guard = write_lock
            .lock()
            .map_err(|_| "write lock poisoned".to_string())?;
        chainstate
            .ensure_utxo_stats()
            .map_err(|err| err.to_string())?;
        println!("UTXO stats rebuilt.");
    }

    if chainstate
        .value_pools()
        .map_err(|err| err.to_string())?
        .is_none()
    {
        println!("Shielded value pools missing; rebuilding from blocks (one-time).");
        let _guard = write_lock
            .lock()
            .map_err(|_| "write lock poisoned".to_string())?;
        chainstate
            .ensure_value_pools()
            .map_err(|err| err.to_string())?;
        println!("Shielded value pools rebuilt.");
    }

    let sync_metrics = Arc::new(SyncMetrics::default());
    let header_metrics = Arc::new(HeaderMetrics::default());
    spawn_status_logger(
        Arc::clone(&chainstate),
        Arc::clone(&store),
        Arc::clone(&sync_metrics),
        Arc::clone(&header_metrics),
        Arc::clone(&validation_metrics),
        Arc::clone(&connect_metrics),
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
                    network,
                    backend,
                    start_time,
                )
                .await
                {
                    eprintln!("{err}");
                }
            });
        });
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
    let mut block_peer = connect_to_peer(
        params.as_ref(),
        start_height,
        min_peer_height,
        addr_book.as_ref(),
        &block_peer_ctx,
    )
    .await?;
    println!("Block peer handshake complete");
    println!("Block peer height {}", block_peer.remote_height());
    println!(
        "Block peer version {} ua {}",
        block_peer.remote_version(),
        block_peer.remote_user_agent()
    );

    let mut block_peers = if block_peers_target == 0 {
        Vec::new()
    } else {
        connect_to_peers(
            params.as_ref(),
            block_peers_target,
            start_height,
            min_peer_height,
            Some(addr_book.as_ref()),
            &block_peer_ctx,
        )
        .await?
    };
    if block_peers.is_empty() {
        eprintln!("no block peers available, falling back to block peer");
    }

    let header_allow_addr_book = config.header_peer_addrs.is_empty();
    let seed_addrs = if !config.header_peer_addrs.is_empty() {
        match parse_peer_addrs(&config.header_peer_addrs, params.default_port) {
            Ok(addrs) => {
                println!("Using {} header peer(s) from --header-peer", addrs.len());
                addrs
            }
            Err(err) => {
                eprintln!("header peer override failed: {err}");
                Vec::new()
            }
        }
    } else {
        match resolve_seed_addresses(&params).await {
            Ok(addrs) => addrs,
            Err(err) => {
                eprintln!("seed resolve failed: {err}");
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
            eprintln!("addr discovery stopped: {err}");
        }
    });
    let header_cursor = Arc::new(Mutex::new(init_header_cursor(
        chainstate.as_ref(),
        params.as_ref(),
    )?));

    let header_peers = header_peers_target.max(1);
    println!(
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
            eprintln!("header commit stopped: {err}");
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
            eprintln!("header sync stopped: {err}");
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
                relay_flags,
                relay_tx_announce,
                relay_target,
            )
            .await
            {
                eprintln!("tx relay stopped: {err}");
            }
        });
    }

    sync_chain(
        &mut block_peer,
        &mut block_peers,
        block_peers_target,
        Arc::clone(&chainstate),
        Arc::clone(&mempool),
        Arc::clone(&sync_metrics),
        Arc::clone(&params),
        addr_book.as_ref(),
        &block_peer_ctx,
        &flags,
        &verify_settings,
        Arc::clone(&connect_metrics),
        Arc::clone(&write_lock),
        Arc::clone(&header_cursor),
        header_lead,
        getdata_batch,
        inflight_per_peer,
    )
    .await?;

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
            if let (Some(write_buffer), Some(memtable)) =
                (options.write_buffer_bytes, options.memtable_bytes)
            {
                let partition_count = fluxd_storage::Column::ALL.len() as u64;
                let max_memtables = u64::from(memtable).saturating_mul(partition_count);
                if write_buffer < max_memtables {
                    eprintln!(
                        "Warning: --db-write-buffer-mb ({}) is below partitions ({}) × --db-memtable-mb ({}); expect frequent flushes / L0 stalls",
                        write_buffer / (1024 * 1024),
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

fn load_peers_file(path: &Path) -> Result<Vec<SocketAddr>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.to_string()),
    };
    let file: PeersFile =
        serde_json::from_slice(&bytes).map_err(|err| format!("invalid peers file: {err}"))?;
    if file.version != PEERS_FILE_VERSION {
        return Err(format!(
            "unsupported peers file version {} (expected {})",
            file.version, PEERS_FILE_VERSION
        ));
    }
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
            out.push(addr);
        }
    }
    Ok(out)
}

fn save_peers_file(path: &Path, addrs: &[SocketAddr]) -> Result<(), String> {
    let mut entries: Vec<String> = addrs.iter().map(ToString::to_string).collect();
    entries.sort();
    entries.dedup();
    if entries.len() > ADDR_BOOK_MAX {
        entries.truncate(ADDR_BOOK_MAX);
    }
    let file = PeersFile {
        version: PEERS_FILE_VERSION,
        addrs: entries,
    };
    let json = serde_json::to_vec_pretty(&file).map_err(|err| err.to_string())?;
    write_file_atomic(path, &json)
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
    let mut last_revision = addr_book.revision();
    loop {
        thread::sleep(Duration::from_secs(PEERS_PERSIST_INTERVAL_SECS));
        let revision = addr_book.revision();
        if revision == last_revision {
            continue;
        }
        let snapshot = addr_book.snapshot();
        if let Err(err) = save_peers_file(&path, &snapshot) {
            eprintln!("failed to persist {}: {err}", path.display());
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
            eprintln!("failed to persist {}: {err}", path.display());
            continue;
        }
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
) -> Result<Peer, String> {
    let peers = connect_to_peers(
        params,
        HEADER_PEER_PROBE_COUNT,
        start_height,
        min_height,
        Some(addr_book),
        peer_ctx,
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
) -> Result<Vec<Peer>, String> {
    if count == 0 {
        return Ok(Vec::new());
    }

    let mut addrs = resolve_seed_addresses(params).await?;
    if let Some(addr_book) = addr_book {
        let mut seen: HashSet<SocketAddr> = addrs.iter().copied().collect();
        for addr in addr_book.sample(ADDR_BOOK_SAMPLE) {
            if seen.insert(addr) {
                addrs.push(addr);
            }
        }
    }
    if addrs.is_empty() {
        return Err("no seed addresses resolved".to_string());
    }
    println!("Resolved {} seed addresses", addrs.len());
    addrs.shuffle(&mut rand::thread_rng());

    let mut peers = Vec::new();
    let mut behind = Vec::new();
    let mut join_set = JoinSet::new();
    let mut next_index = 0usize;
    let max_parallel = addrs.len().min(count.saturating_mul(2).max(4));

    while next_index < addrs.len() && join_set.len() < max_parallel {
        let addr = addrs[next_index];
        let magic = params.message_start;
        let peer_ctx = peer_ctx.clone();
        join_set
            .spawn(async move { connect_and_handshake(addr, magic, start_height, peer_ctx).await });
        next_index += 1;
    }

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok((addr, peer))) => {
                let remote_height = peer.remote_height();
                let remote_version = peer.remote_version();
                let remote_agent = peer.remote_user_agent();
                if min_height > 0 && remote_height >= 0 && remote_height < min_height {
                    eprintln!(
                        "Peer {addr} behind (height {} < {}), skipping (ver {} ua {})",
                        remote_height, min_height, remote_version, remote_agent
                    );
                    behind.push(peer);
                } else {
                    println!(
                        "Connected to {addr} (height {} ver {} ua {})",
                        remote_height, remote_version, remote_agent
                    );
                    peers.push(peer);
                }
                if peers.len() >= count {
                    break;
                }
            }
            Ok(Err(err)) => {
                eprintln!("{err}");
            }
            Err(err) => {
                eprintln!("peer task failed: {err}");
            }
        }

        if next_index < addrs.len() {
            let addr = addrs[next_index];
            let magic = params.message_start;
            let peer_ctx = peer_ctx.clone();
            join_set.spawn(async move {
                connect_and_handshake(addr, magic, start_height, peer_ctx).await
            });
            next_index += 1;
        }
    }

    if peers.is_empty() && !behind.is_empty() {
        let fallback = behind
            .into_iter()
            .max_by_key(|peer| peer.remote_height())
            .expect("behind checked to be non-empty");
        eprintln!(
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
                eprintln!("failed to resolve fixed seed {seed}: {err}");
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
                eprintln!("failed to resolve {seed}: {err}");
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
                    eprintln!("addr discovery failed for {addr}: {err}");
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
    let (_addr, mut peer) = connect_and_handshake(addr, magic, start_height, peer_ctx).await?;
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
        println!(
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
        .connect_block(&genesis, 0, params, flags, false, None, connect_metrics)
        .map_err(|err| err.to_string())?;
    let _guard = write_lock
        .lock()
        .map_err(|_| "write lock poisoned".to_string())?;
    chainstate
        .commit_batch(batch)
        .map_err(|err| err.to_string())?;
    println!("Inserted genesis block");
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
            eprintln!("header worker stopped: {err}");
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
                eprintln!("header sync start height failed: {err}");
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
                eprintln!("header peer connect failed: {err}");
                tokio::time::sleep(idle_sleep).await;
                continue;
            }
        };
        println!("Header peer handshake complete");
        println!("Header peer height {}", peer.remote_height());
        println!(
            "Header peer version {} ua {}",
            peer.remote_version(),
            peer.remote_user_agent()
        );
        let peer_addr = peer.addr();
        let mut last_headers_at = Instant::now();
        let mut timeout_failures = 0usize;
        let mut probing = true;

        loop {
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
                eprintln!(
                    "header peer behind (remote {} < tip {}), reconnecting",
                    remote_height, download_state.tip_height
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
                    eprintln!("header locator failed: {err}");
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
                            eprintln!("header peer returned no headers while behind");
                            peer_book.record_failure(peer_addr);
                        } else if last_headers_at.elapsed()
                            > Duration::from_secs(HEADER_IDLE_REPROBE_SECS)
                        {
                            eprintln!(
                                "header peer idle at height {} for {:?}; reconnecting",
                                download_state.tip_height,
                                last_headers_at.elapsed()
                            );
                            peer_book.record_failure(peer_addr);
                            break;
                        }
                        tokio::time::sleep(idle_sleep).await;
                        continue;
                    }
                    peer_book.record_success(peer_addr);
                    if !headers_are_contiguous(&headers) {
                        eprintln!("non-continuous headers sequence from peer");
                        peer_book.record_bad_chain(peer_addr, HEADER_BAD_CHAIN_BAN_SECS);
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
                            eprintln!(
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
                            eprintln!(
                                "header batch does not connect to known header {}; resetting",
                                hash256_to_hex(&prev)
                            );
                            download_state.reset(chainstate.as_ref(), &params)?;
                            peer_book.record_bad_chain(peer_addr, HEADER_BAD_CHAIN_BAN_SECS);
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
                        eprintln!("header validation failed: {err}");
                        download_state.reset(chainstate.as_ref(), &params)?;
                        peer_book.record_failure(peer_addr);
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
                    probing = false;
                    last_headers_at = Instant::now();
                    println!("Received {} headers", headers.len());
                    if header_tx.send(headers).await.is_err() {
                        return Ok(());
                    }
                }
                Ok(Err(err)) => {
                    if behind {
                        eprintln!("header request failed: {err}");
                    }
                    peer_book.record_failure(peer_addr);
                    break;
                }
                Err(_) => {
                    if behind {
                        eprintln!("header request timed out");
                    }
                    peer_book.record_failure(peer_addr);
                    timeout_failures = timeout_failures.saturating_add(1);
                    if behind && timeout_failures >= HEADER_TIMEOUT_RETRIES_BEHIND {
                        eprintln!("header peer timed out while behind; reconnecting");
                        break;
                    }
                    let stall_limit = HEADER_STALL_SECS_IDLE;
                    if timeout_failures >= HEADER_TIMEOUT_RETRIES_IDLE
                        && last_headers_at.elapsed() > Duration::from_secs(stall_limit)
                    {
                        eprintln!(
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
        for addr in addr_book.sample(ADDR_BOOK_SAMPLE) {
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

    while next_index < attempt_target && join_set.len() < max_parallel {
        let addr = candidates[next_index];
        let peer_ctx = peer_ctx.clone();
        join_set
            .spawn(async move { connect_and_handshake(addr, magic, start_height, peer_ctx).await });
        next_index += 1;
    }

    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(Ok((_addr, peer))) => peers.push(peer),
            Ok(Err(err)) => eprintln!("{err}"),
            Err(err) => eprintln!("peer task failed: {err}"),
        }

        if next_index < attempt_target {
            let addr = candidates[next_index];
            let peer_ctx = peer_ctx.clone();
            join_set.spawn(async move {
                connect_and_handshake(addr, magic, start_height, peer_ctx).await
            });
            next_index += 1;
        }
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
            println!(
                "Capped header lead at height {} (gap {})",
                entry.height, header_lead
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
    println!("Committed {} headers", headers.len());
    if let Some((_, last_entry)) = entries.last() {
        if let Ok(Some(best)) = chainstate.best_header() {
            if last_entry.height > best.height {
                let last_hash = headers.last().map(|header| header.hash());
                let work_cmp = match last_entry.chainwork.cmp(&best.chainwork) {
                    Ordering::Greater => "gt",
                    Ordering::Equal => "eq",
                    Ordering::Less => "lt",
                };
                eprintln!(
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
            eprintln!("header batch queued (prev missing)");
        }
        pending.entry(prev).or_insert(headers);
        return Ok(());
    }

    let (commit_headers, remainder) =
        split_headers_by_lead(chainstate, params, header_lead, prev, headers)?;
    if commit_headers.is_empty() {
        eprintln!("header lead clamp: nothing committed");
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
    println!("Header tip advanced to {}", best.height);
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
    metrics: Arc<SyncMetrics>,
    params: Arc<ChainParams>,
    addr_book: &AddrBook,
    peer_ctx: &PeerContext,
    flags: &ValidationFlags,
    verify_settings: &VerifySettings,
    connect_metrics: Arc<ConnectMetrics>,
    write_lock: Arc<Mutex<()>>,
    header_cursor: Arc<Mutex<HeaderCursor>>,
    header_lead: i32,
    getdata_batch: usize,
    inflight_per_peer: usize,
) -> Result<(), String> {
    let idle_sleep = Duration::from_secs(IDLE_SLEEP_SECS);
    let mut last_progress_height = chainstate
        .best_block()
        .map_err(|err| err.to_string())?
        .map(|tip| tip.height)
        .unwrap_or(-1);
    let mut last_progress_at = Instant::now();
    loop {
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
        let (gap, _) = header_gap(chainstate.as_ref())?;
        let max_fetch = max_fetch_blocks(block_peers.len(), getdata_batch, inflight_per_peer);
        let missing = collect_missing_blocks(chainstate.as_ref(), max_fetch)?;
        if !missing.is_empty() {
            if gap > 0 && last_progress_at.elapsed() > Duration::from_secs(BLOCK_STALL_SECS) {
                eprintln!(
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
                )
                .await
                {
                    Ok(new_peer) => {
                        *block_peer = new_peer;
                    }
                    Err(err) => {
                        eprintln!("reconnect block peer failed: {err}");
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
                    )
                    .await
                    {
                        Ok(new_peers) => {
                            *block_peers = new_peers;
                        }
                        Err(err) => {
                            eprintln!("reconnect block peers failed: {err}");
                        }
                    }
                }
                last_progress_at = Instant::now();
            }
            let fetch_result = fetch_blocks(
                block_peer,
                block_peers,
                Arc::clone(&chainstate),
                Arc::clone(&mempool),
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
            )
            .await;
            if let Err(err) = fetch_result {
                if is_transient_block_error(&err) {
                    eprintln!("block fetch failed: {err}");
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
                    )
                    .await
                    {
                        Ok(new_peer) => {
                            *block_peer = new_peer;
                        }
                        Err(err) => {
                            eprintln!("reconnect block peer failed: {err}");
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
                        )
                        .await
                        {
                            Ok(new_peers) => {
                                *block_peers = new_peers;
                            }
                            Err(err) => {
                                eprintln!("reconnect block peers failed: {err}");
                            }
                        }
                    }
                    last_progress_at = Instant::now();
                    tokio::time::sleep(idle_sleep).await;
                    continue;
                }
                return Err(err);
            }
        } else {
            tokio::time::sleep(idle_sleep).await;
        }
    }

    Ok(())
}

fn is_transient_block_error(err: &str) -> bool {
    let err = err.to_lowercase();
    [
        "peer",
        "timeout",
        "timed out",
        "notfound",
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

#[allow(clippy::too_many_arguments)]
fn spawn_status_logger<S: KeyValueStore + Send + Sync + 'static>(
    chainstate: Arc<ChainState<S>>,
    store: Arc<Store>,
    sync_metrics: Arc<SyncMetrics>,
    header_metrics: Arc<HeaderMetrics>,
    validation_metrics: Arc<ValidationMetrics>,
    connect_metrics: Arc<ConnectMetrics>,
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
                    println!(
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
                    eprintln!("status snapshot failed: {err}");
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

    let (anchor_hash, _anchor_height) = if header_descends_from(
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

    let mut missing: VecDeque<fluxd_consensus::Hash256> = VecDeque::new();
    let mut hash = best_header.hash;
    loop {
        let entry = chainstate
            .header_entry(&hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing header entry while scanning for blocks".to_string())?;
        if hash == anchor_hash {
            break;
        }
        missing.push_front(hash);
        if entry.height == 0 {
            break;
        }
        hash = entry.prev_hash;
    }

    let mut missing: Vec<fluxd_consensus::Hash256> = missing.into_iter().collect();
    if max_count > 0 && missing.len() > max_count {
        missing.truncate(max_count);
    }
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
    mut hash: fluxd_consensus::Hash256,
    ancestor_hash: fluxd_consensus::Hash256,
    ancestor_height: i32,
) -> Result<bool, String> {
    if hash == ancestor_hash {
        return Ok(true);
    }
    loop {
        let entry = chainstate
            .header_entry(&hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing header entry while checking ancestry".to_string())?;
        if entry.height <= ancestor_height {
            return Ok(false);
        }
        hash = entry.prev_hash;
        if hash == ancestor_hash {
            return Ok(true);
        }
    }
}

fn find_common_ancestor<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    mut a_hash: Hash256,
    mut a_height: i32,
    mut b_hash: Hash256,
    mut b_height: i32,
) -> Result<(Hash256, i32), String> {
    while a_height > b_height {
        let entry = chainstate
            .header_entry(&a_hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing header entry while finding ancestor".to_string())?;
        a_hash = entry.prev_hash;
        a_height = entry.height.saturating_sub(1);
    }
    while b_height > a_height {
        let entry = chainstate
            .header_entry(&b_hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing header entry while finding ancestor".to_string())?;
        b_hash = entry.prev_hash;
        b_height = entry.height.saturating_sub(1);
    }
    while a_hash != b_hash {
        let entry_a = chainstate
            .header_entry(&a_hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing header entry while finding ancestor".to_string())?;
        let entry_b = chainstate
            .header_entry(&b_hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "missing header entry while finding ancestor".to_string())?;
        a_hash = entry_a.prev_hash;
        b_hash = entry_b.prev_hash;
        a_height = entry_a.height.saturating_sub(1);
    }
    Ok((a_hash, a_height))
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
        println!(
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
    chainstate: Arc<ChainState<S>>,
    mempool: Arc<Mutex<mempool::Mempool>>,
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
        return fetch_blocks_single(
            peer,
            chainstate,
            mempool,
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
    }

    fetch_blocks_multi(
        block_peers,
        chainstate,
        mempool,
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
    block_peers: &mut Vec<Peer>,
    chainstate: Arc<ChainState<S>>,
    mempool: Arc<Mutex<mempool::Mempool>>,
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
    let mut received: HashMap<fluxd_consensus::Hash256, Block> = HashMap::new();

    let peers = std::mem::take(block_peers);
    let mut assignments: Vec<Vec<Vec<fluxd_consensus::Hash256>>> = vec![Vec::new(); peers.len()];
    for (index, chunk) in hashes.chunks(getdata_batch).enumerate() {
        assignments[index % peers.len()].push(chunk.to_vec());
    }

    let mut tasks = Vec::new();
    let mut idle_peers = Vec::new();
    for (peer, chunks) in peers.into_iter().zip(assignments) {
        if chunks.is_empty() {
            idle_peers.push(peer);
            continue;
        }
        let metrics = Arc::clone(&metrics);
        tasks.push(tokio::spawn(async move {
            fetch_blocks_on_peer(peer, chunks, inflight_per_peer, metrics).await
        }));
    }

    let mut returned_peers = idle_peers;
    for task in tasks {
        match task.await {
            Ok(Ok((peer, blocks))) => {
                returned_peers.push(peer);
                received.extend(blocks);
            }
            Ok(Err(err)) => {
                eprintln!("block peer fetch failed: {err}");
            }
            Err(err) => {
                eprintln!("block peer join failed: {err}");
            }
        }
    }

    *block_peers = returned_peers;
    let chainstate = Arc::clone(&chainstate);
    let params = Arc::clone(&params);
    let mempool = Arc::clone(&mempool);
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

async fn fetch_blocks_on_peer(
    mut peer: Peer,
    chunks: Vec<Vec<fluxd_consensus::Hash256>>,
    inflight_per_peer: usize,
    metrics: Arc<SyncMetrics>,
) -> Result<(Peer, HashMap<fluxd_consensus::Hash256, Block>), String> {
    let download_start = Instant::now();
    let received = fetch_blocks_on_peer_inner(&mut peer, chunks, inflight_per_peer).await?;
    metrics.record_download(received.len() as u64, download_start.elapsed());
    Ok((peer, received))
}

async fn fetch_blocks_on_peer_inner(
    peer: &mut Peer,
    chunks: Vec<Vec<fluxd_consensus::Hash256>>,
    inflight_per_peer: usize,
) -> Result<HashMap<fluxd_consensus::Hash256, Block>, String> {
    let mut received: HashMap<fluxd_consensus::Hash256, Block> = HashMap::new();
    if chunks.is_empty() {
        return Ok(received);
    }

    let mut inflight: Vec<HashSet<fluxd_consensus::Hash256>> = Vec::new();
    let mut next_index = 0usize;

    let inflight_target = inflight_per_peer.max(1);
    while next_index < chunks.len() && inflight.len() < inflight_target {
        let chunk = &chunks[next_index];
        println!("Requesting {} block(s)", chunk.len());
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
                let block = Block::consensus_decode(&payload).map_err(|err| err.to_string())?;
                let hash = block.header.hash();
                if let Some(pos) = inflight.iter_mut().position(|set| set.contains(&hash)) {
                    let set = &mut inflight[pos];
                    set.remove(&hash);
                    if set.is_empty() {
                        inflight.remove(pos);
                        if next_index < chunks.len() {
                            let chunk = &chunks[next_index];
                            println!("Requesting {} block(s)", chunk.len());
                            peer.send_getdata_blocks(chunk).await?;
                            inflight.push(chunk.iter().copied().collect());
                            next_index += 1;
                        }
                    }
                }
                received.insert(hash, block);
                last_block_at = Instant::now();
            }
            "notfound" => {
                return Err("peer returned notfound for block request".to_string());
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
    params: &ChainParams,
    flags: &ValidationFlags,
    metrics: &SyncMetrics,
    verify_settings: &VerifySettings,
    connect_metrics: &ConnectMetrics,
    write_lock: &Mutex<()>,
    _header_cursor: &Arc<Mutex<HeaderCursor>>,
    pending: &mut VecDeque<fluxd_consensus::Hash256>,
    received: &mut HashMap<fluxd_consensus::Hash256, Block>,
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
                let (txids, error) = match validate_block_with_txids(
                    job.block.as_ref(),
                    job.height,
                    &consensus,
                    &pre_flags,
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
    for (hash, block) in received.drain() {
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
        let job = VerifyJob {
            hash,
            height,
            block: Arc::new(block),
        };
        verify_tx
            .send(job)
            .map_err(|_| "verify queue closed".to_string())?;
        received_heights.insert(hash, height);
    }

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
            ) {
                Ok(batch) => batch,
                Err(fluxd_chainstate::state::ChainStateError::InvalidHeader(
                    "block does not extend best block tip",
                ))
                | Err(fluxd_chainstate::state::ChainStateError::InvalidHeader(
                    "block height does not match header index",
                )) => {
                    eprintln!(
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
            let _guard = write_lock
                .lock()
                .map_err(|_| "write lock poisoned".to_string())?;
            chainstate
                .commit_batch(batch)
                .map_err(|err| err.to_string())?;
            metrics.record_commit(1, commit_start.elapsed());
            purge_mempool_for_connected_block(
                mempool,
                verified_block.block.as_ref(),
                verified_block.txids.as_slice(),
            )?;

            pending.pop_front();
            continue;
        }

        if !received_heights.contains_key(&hash) {
            break;
        }

        if pending_verify == 0 && pending_shielded == 0 {
            break;
        }

        let event = match event_rx.recv() {
            Ok(event) => event,
            Err(_) => break,
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

fn purge_mempool_for_connected_block(
    mempool: &Mutex<mempool::Mempool>,
    block: &Block,
    txids: &[Hash256],
) -> Result<(), String> {
    if block.transactions.len() <= 1 {
        return Ok(());
    }
    if txids.len() != block.transactions.len() {
        return Err("transaction id cache mismatch".to_string());
    }

    let mut guard = mempool
        .lock()
        .map_err(|_| "mempool lock poisoned".to_string())?;
    let mut to_remove: HashSet<Hash256> = HashSet::new();
    for (txid, tx) in txids
        .iter()
        .copied()
        .skip(1)
        .zip(block.transactions.iter().skip(1))
    {
        to_remove.insert(txid);
        for input in &tx.vin {
            if let Some(conflict) = guard.spender(&input.prevout) {
                to_remove.insert(conflict);
            }
        }
    }

    for txid in to_remove {
        guard.remove(&txid);
    }
    Ok(())
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
                eprintln!("peer read timed out (attempt {}/{})", attempt + 1, retries);
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

fn parse_args() -> Result<Config, String> {
    let mut backend = Backend::Fjall;
    let mut data_dir: Option<PathBuf> = None;
    let mut params_dir: Option<PathBuf> = None;
    let mut fetch_params = false;
    let mut scan_flatfiles = false;
    let mut scan_supply = false;
    let mut check_script = true;
    let mut rpc_addr: Option<SocketAddr> = None;
    let mut rpc_user: Option<String> = None;
    let mut rpc_pass: Option<String> = None;
    let mut network = Network::Mainnet;
    let mut getdata_batch: usize = DEFAULT_GETDATA_BATCH;
    let mut block_peers: usize = DEFAULT_BLOCK_PEERS;
    let mut header_peers: usize = DEFAULT_HEADER_PEERS;
    let mut header_lead: i32 = DEFAULT_HEADER_LEAD;
    let mut header_peer_addrs: Vec<String> = Vec::new();
    let mut tx_peers: usize = DEFAULT_TX_PEERS;
    let mut inflight_per_peer: usize = DEFAULT_INFLIGHT_PER_PEER;
    let mut status_interval_secs: u64 = 15;
    let mut dashboard_addr: Option<SocketAddr> = None;
    let mut db_cache_bytes: Option<u64> = None;
    let mut db_write_buffer_bytes: Option<u64> = None;
    let mut db_journal_bytes: Option<u64> = None;
    let mut db_memtable_bytes: Option<u32> = None;
    let mut db_flush_workers: Option<usize> = None;
    let mut db_compaction_workers: Option<usize> = None;
    let mut db_fsync_ms: Option<u16> = None;
    let mut utxo_cache_entries: usize = DEFAULT_UTXO_CACHE_ENTRIES;
    let mut header_verify_workers: usize = 0;
    let mut verify_workers: usize = 0;
    let mut verify_queue: usize = 0;
    let mut shielded_workers: usize = 0;
    let mut args = std::env::args().skip(1);
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
            "--params-dir" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --params-dir\n{}", usage()))?;
                params_dir = Some(PathBuf::from(value));
            }
            "--fetch-params" => {
                fetch_params = true;
            }
            "--scan-flatfiles" => {
                scan_flatfiles = true;
            }
            "--scan-supply" => {
                scan_supply = true;
            }
            "--skip-script" => {
                check_script = false;
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
            }
            "--rpc-user" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --rpc-user\n{}", usage()))?;
                rpc_user = Some(value);
            }
            "--rpc-pass" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --rpc-pass\n{}", usage()))?;
                rpc_pass = Some(value);
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
            }
            "--getdata-batch" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --getdata-batch\n{}", usage()))?;
                getdata_batch = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid getdata batch '{value}'\n{}", usage()))?;
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
            }
            "--header-peers" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --header-peers\n{}", usage()))?;
                header_peers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid header peers '{value}'\n{}", usage()))?;
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
            }
            "--inflight-per-peer" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --inflight-per-peer\n{}", usage()))?;
                inflight_per_peer = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid inflight per peer '{value}'\n{}", usage()))?;
                if inflight_per_peer == 0 {
                    return Err(format!("inflight per peer must be > 0\n{}", usage()));
                }
            }
            "--status-interval" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --status-interval\n{}", usage()))?;
                status_interval_secs = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid status interval '{value}'\n{}", usage()))?;
            }
            "--db-cache-mb" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --db-cache-mb\n{}", usage()))?;
                let mb = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid db cache '{value}'\n{}", usage()))?;
                db_cache_bytes = Some(mb_to_bytes(mb));
            }
            "--db-write-buffer-mb" => {
                let value = args.next().ok_or_else(|| {
                    format!("missing value for --db-write-buffer-mb\n{}", usage())
                })?;
                let mb = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid db write buffer '{value}'\n{}", usage()))?;
                db_write_buffer_bytes = Some(mb_to_bytes(mb));
            }
            "--db-journal-mb" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --db-journal-mb\n{}", usage()))?;
                let mb = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid db journal '{value}'\n{}", usage()))?;
                db_journal_bytes = Some(mb_to_bytes(mb));
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
                db_memtable_bytes = Some(bytes as u32);
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
                db_flush_workers = Some(workers);
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
                db_compaction_workers = Some(workers);
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
            }
            "--utxo-cache-entries" => {
                let value = args.next().ok_or_else(|| {
                    format!("missing value for --utxo-cache-entries\n{}", usage())
                })?;
                utxo_cache_entries = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid utxo cache entries '{value}'\n{}", usage()))?;
            }
            "--header-verify-workers" => {
                let value = args.next().ok_or_else(|| {
                    format!("missing value for --header-verify-workers\n{}", usage())
                })?;
                header_verify_workers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid header verify workers '{value}'\n{}", usage()))?;
            }
            "--verify-workers" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --verify-workers\n{}", usage()))?;
                verify_workers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid verify workers '{value}'\n{}", usage()))?;
            }
            "--verify-queue" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --verify-queue\n{}", usage()))?;
                verify_queue = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid verify queue '{value}'\n{}", usage()))?;
            }
            "--shielded-workers" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("missing value for --shielded-workers\n{}", usage()))?;
                shielded_workers = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid shielded workers '{value}'\n{}", usage()))?;
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
            "--help" | "-h" => {
                return Err(usage());
            }
            other => {
                return Err(format!("unknown argument '{other}'\n{}", usage()));
            }
        }
    }

    Ok(Config {
        backend,
        data_dir: data_dir.unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR)),
        network,
        params_dir: params_dir.unwrap_or_else(default_params_dir),
        fetch_params,
        scan_flatfiles,
        scan_supply,
        check_script,
        rpc_addr,
        rpc_user,
        rpc_pass,
        getdata_batch,
        block_peers,
        header_peers,
        header_lead,
        header_peer_addrs,
        tx_peers,
        inflight_per_peer,
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
    })
}

fn mb_to_bytes(mb: u64) -> u64 {
    mb.saturating_mul(1024 * 1024)
}

fn default_rpc_addr(network: Network) -> SocketAddr {
    let port = match network {
        Network::Mainnet => 16_124,
        Network::Testnet | Network::Regtest => 26_124,
    };
    SocketAddr::from(([127, 0, 0, 1], port))
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
    let shielded_workers = if config.shielded_workers > 0 {
        config.shielded_workers
    } else {
        (cores / 4).max(1)
    };
    let verify_workers = if config.verify_workers > 0 {
        config.verify_workers
    } else {
        cores.saturating_sub(shielded_workers + 1).max(1)
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
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
}

fn usage() -> String {
    [
        "Usage: fluxd [--backend fjall|memory] [--data-dir PATH] [--params-dir PATH] [--fetch-params] [--scan-flatfiles] [--scan-supply] [--skip-script] [--network mainnet|testnet|regtest] [--rpc-addr IP:PORT] [--rpc-user USER] [--rpc-pass PASS] [--getdata-batch N] [--block-peers N] [--header-peers N] [--header-peer IP:PORT] [--header-lead N] [--tx-peers N] [--inflight-per-peer N] [--status-interval SECS] [--db-cache-mb N] [--db-write-buffer-mb N] [--db-journal-mb N] [--db-memtable-mb N] [--db-flush-workers N] [--db-compaction-workers N] [--db-fsync-ms N] [--utxo-cache-entries N] [--header-verify-workers N] [--verify-workers N] [--verify-queue N] [--shielded-workers N] [--dashboard-addr IP:PORT]",
        "",
        "Options:",
        "  --backend   Storage backend to use (default: fjall)",
        "  --data-dir  Base data directory (default: ./data)",
        "  --params-dir    Shielded params directory (default: ~/.zcash-params)",
        "  --fetch-params  Download shielded params into --params-dir",
        "  --scan-flatfiles  Scan flatfiles for block index mismatches, then exit",
        "  --scan-supply  Scan blocks in the local DB and print coinbase totals, then exit",
        "  --skip-script  Disable script validation (testing only)",
        "  --network   Network selection (default: mainnet)",
        "  --rpc-addr  Bind JSON-RPC server (default: 127.0.0.1:16124 mainnet, 26124 testnet)",
        "  --rpc-user  JSON-RPC basic auth username (required unless cookie exists)",
        "  --rpc-pass  JSON-RPC basic auth password (required unless cookie exists)",
        "  --getdata-batch  Max blocks per getdata request (default: 128)",
        "  --block-peers  Number of parallel peers for block download (default: 4)",
        "  --header-peers  Number of peers to probe for header sync (default: 4)",
        "  --header-peer  Header peer IP:PORT to pin for header sync (repeatable)",
        "  --header-lead  Target header lead over blocks (default: 20000, 0 disables cap)",
        "  --tx-peers  Number of relay peers for tx inventory/tx relay (0 disables, default: 2)",
        "  --inflight-per-peer  Concurrent getdata requests per peer (default: 2)",
        "  --status-interval  Status log interval in seconds (default: 15, 0 disables)",
        "  --db-cache-mb  Fjall block cache size in MiB (optional)",
        "  --db-write-buffer-mb  Fjall max write buffer in MiB (optional)",
        "  --db-journal-mb  Fjall max journaling size in MiB (optional)",
        "  --db-memtable-mb  Fjall partition memtable size in MiB (optional)",
        "  --db-flush-workers  Fjall flush worker threads (optional)",
        "  --db-compaction-workers  Fjall compaction worker threads (optional)",
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
