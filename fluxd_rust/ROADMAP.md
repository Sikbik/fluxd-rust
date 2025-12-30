# fluxd-rust roadmap

This is a living checklist for parity and modernization work. Mark items as `[x]` when complete.

Legend:
- P0 = safety/consensus blocking
- P1 = parity and user-facing
- P2 = quality of life or ops

Owner format: `owner: <name>` or `owner: TBD`.

## Current sprint (next 3-5)

- [x] [P1] Spent index + `getspentinfo` (owner: TBD)
- [x] [P1] Address indexes (deltas/utxos/txids) + `getaddress*` RPCs (owner: TBD)
- [x] [P1] Address mempool deltas + `getaddressmempool` (owner: TBD)
- [x] [P1] Remove header-gap bottleneck (fast header ancestry + block queue selection) (owner: TBD)
- [x] [P1] Header-first sync speed parity with C++ (owner: TBD)

## Completed (core)

- [x] Rust workspace + core crate split (node/chainstate/consensus/pow/pon/fluxnode/script/primitives/shielded/storage)
- [x] Fjall backend + flatfile block storage
- [x] Header download pipeline with parallel verification
- [x] Block download/validation pipeline with parallel script + shielded workers
- [x] Header index, height index, block index, block header bytes (written on header accept + block connect; supports `getblockheader` during header-only sync)
- [x] Tx index + UTXO set
- [x] Address outpoint index (script_pubkey -> outpoints)
- [x] Fluxnode index + basic RPCs (partial parity)
- [x] Timestamp index + `getblockhashes`
- [x] Block undo storage + disconnect path
- [x] Reorg handling (disconnect to common ancestor)
- [x] RPC framework (JSON-RPC + `/daemon` endpoints)
- [x] Dashboard + status metrics
- [x] CLI scan tools (`--scan-supply`, `--scan-flatfiles`, `--scan-fluxnodes`)
- [x] Documentation set (README + docs/)
- [x] Binary name `fluxd`

## Active focus

- [ ] [P1] Block indexing throughput parity (I/O batching, memtable tuning, UTXO cache policy) (owner: TBD)
  - [x] Cache Sprout/Sapling trees in memory and only write tree bytes when the tree root changes
  - [x] Store Sapling anchors as root keys only (empty values) to reduce DB write amplification
  - [x] Run block connect on blocking threads to keep RPC/dashboard responsive under high throughput
  - [x] Reduce per-input allocation overhead (outpoint key handling + per-block UTXO cache)
  - [x] Inline small keys in `WriteBatch` (UTXO/address keys stored without heap allocs)
  - [x] Avoid heap allocs for common index keys (txindex/header/block index keys)
  - [x] LRU UTXO read cache for sequential block spends (`--utxo-cache-entries`)
  - [x] Reuse txids across validation/connect/mempool purge (avoid redundant txid hashing)
  - [x] Optimize fluxnode signed-message verification (verify vs recover)
  - [x] Parallelize fluxnode signature checks during connect (multi-core)
  - [x] Fix shielded pipeline ordering bug (prevents rare sync stalls)
  - [x] Expose Fjall flush/compaction worker knobs (`--db-flush-workers`, `--db-compaction-workers`)
  - [x] Warn when `--db-write-buffer-mb` is below `--db-memtable-mb × partitions` (prevents hidden L0 stalls)
  - [x] Raise default Fjall buffers/workers (avoid write stalls during heavy indexing)
  - [x] Log slow Fjall commits (helps diagnose stalls in the field)
  - [x] Surface Fjall health stats in `/stats` (write buffer, compactions, per-partition segments/flushes)
  - [x] Add detailed connect-stage telemetry in `/stats` (UTXO get/put/delete, undo encode/append, index op counts)
  - [x] Review index write amplification (txindex/address index) and batching opportunities
    - [x] Reuse address script hash across indexes (avoid double hashing per I/O)
  - [x] Defer UTXO + address-outpoint writes to end-of-block (eliminate intrablock put+delete pairs)
  - [x] Reserve `WriteBatch` capacity per block (reduce allocator churn)
  - [x] Capture throughput stats (8-core mainnet: default worker auto-split yields ~120–150 b/s typical; shielded proof verification is the primary limiter)
- [ ] [P1] RPC parity expansion (see detailed checklist below) (owner: TBD)

## Consensus and chainstate parity

- [x] [P0] Upgrade activation heights and block hashes cross-checked against C++ (owner: TBD)
  - [x] Mainnet upgrade schedule regression tests (protocol version, height, activation hash)
- [x] [P0] PoN rules parity (header validation, signature rules, economics) (owner: TBD)
  - [x] PoN slot number and hash determinism regression tests
  - [x] PoN signature validation regression test (secp256k1)
  - [x] Emergency block multisig encoding/verification regression test
  - [x] Proof-of-node target bound regression tests
  - [x] Fluxnode tx signed-message verification (start/confirm + benchmark)
  - [x] Fluxnode collateral ownership checks (P2PKH + P2SH redeem script hash)
- [x] [P0] Block reward schedule parity (including canceled halving at 1,968,550) (owner: TBD)
  - [x] Regression tests for 1,968,550 canceled halving and PoN activation subsidy/dev-fund
  - [x] Regression tests for PoN annual subsidy reductions and cap
  - [x] Regression tests for PoN tier distribution + dev-fund remainder
- [x] [P0] Coinbase rules parity (funding outputs + maturity) (owner: TBD)
  - [x] Regression tests for exchange/foundation/swap-pool coinbase enforcement (mainnet)
  - [x] Deterministic fluxnode payout ordering parity (never-paid tie-break on equal height)
  - [x] Regression test for coinbase maturity (premature spend rejection)
- [x] [P0] Chainwork parity for PoN/PoW edge cases and existing headers (owner: TBD)
  - [x] Regression tests for PoW chainwork accumulation and PoN fixed-work increment (2^40)
- [x] [P0] Difficulty/target parity across LWMA/legacy windows and transitions (owner: TBD)
  - [x] Digishield vector tests (Flux mainnet params)
  - [x] LWMA vector test (Flux mainnet params)
  - [x] PoN difficulty adjustment regression tests (start-limit, fast/slow, stability)
  - [x] LWMA3 stability regression test (perfect timing)
  - [x] LWMA/LWMA3 selection regression tests (transition heights)
  - [x] Equihash epoch-fade reset regression test (pow-limit at epoch end + 60)
  - [x] ZelHash ramp regression tests (toward LWMA3)
- [x] [P0] Checkpoint handling parity and tests (owner: TBD)
  - [x] Mainnet checkpoint list regression test (values + ordering)
- [x] [P0] Expanded block index fields (status/validity/tx counts/undo offsets) (owner: TBD)
- [x] [P0] Block file metadata (per-file stats, last file tracking, prune flags) (owner: TBD)

## Indexes and data services

- [x] [P1] Spent index (`getspentinfo`) (owner: TBD)
- [x] [P1] Address indexes (deltas/utxos/txids) + `getaddress*` RPCs (owner: TBD)
- [x] [P1] Address unspent index (`getaddressutxos`) (owner: TBD)
- [x] [P1] UTXO set stats (`gettxoutsetinfo`) (owner: TBD)
- [x] [P1] Shielded value pool totals (Sprout/Sapling) for supply tracking (owner: TBD)
- [x] [P1] Block deltas index (`getblockdeltas`) (owner: TBD)
- [ ] [P2] Reindex/rescan support for secondary indexes (owner: TBD)
- [ ] [P2] DB inspection tools (index stats, supply, integrity) (owner: TBD)

## Networking state and P2P parity

- [x] [P1] `peers.dat` persistence (owner: TBD)
- [x] [P1] `banlist.dat` persistence (owner: TBD)
- [x] [P1] Header peer probing (probe more candidates before selecting best height peer) (owner: TBD)
- [x] [P1] Peer scoring and eviction parity (owner: TBD)
  - [x] Persist peer health stats in `peers.dat` (success/fail, last-seen, last-height, last-version)
  - [x] Prefer recently-good peers and apply exponential backoff after failures
  - [x] Prune stale/unreachable addresses and suppress dial spam
- [x] [P1] Address manager v1 (last-seen timestamps + persistence) (owner: TBD)
- [ ] [P2] Address manager parity (bucket-based selection like C++) (owner: TBD)
- [ ] [P2] P2P message coverage parity (addr/getaddr/feefilter/mempool, etc.) (owner: TBD)
  - [x] Address discovery: `getaddr` + `addr` ingest (owner: TBD)
  - [x] Tx relay: `inv`/`getdata`/`tx` + `mempool` (owner: TBD)
  - [x] `feefilter` and fee-based relay policies (owner: TBD)
  - [x] `reject`/`notfound` handling parity (owner: TBD)
  - [x] Ignore unsolicited `block` messages during getdata download (prevents sync stalls) (owner: TBD)
- [ ] [P2] Service flags and user agent compatibility (owner: TBD)

## Mempool and mining

- [x] [P1] Mempool persistence + eviction policy parity (owner: TBD)
  - [x] `mempool.dat` load/save (`--mempool-persist-interval`)
  - [x] Size cap + fee-rate eviction (`--mempool-max-mb` / `--maxmempool`)
- [x] [P1] Fee estimator persistence (owner: TBD)
- [x] [P1] Standardness policy parity (mempool accept rules) (owner: TBD)
- [x] [P1] `getmempoolinfo`, `getrawmempool` (owner: TBD)
- [x] [P1] Basic P2P tx relay (inv/getdata/tx + `mempool`) (owner: TBD)
- [x] [P1] `getmininginfo`, `submitblock` (owner: TBD)
- [ ] [P1] `getblocktemplate` (coinbase + deterministic payouts; tx selection/assembly WIP) (owner: TBD)
- [ ] [P2] `getnetworkhashps`, `getnetworksolps`, `getlocalsolps` (real metrics) (owner: TBD)

## Wallet

- [ ] [P1] Wallet database (keys, keypool, metadata) (owner: TBD)
- [ ] [P1] Transparent wallet RPCs (owner: TBD)
- [ ] [P1] Shielded wallet RPCs (owner: TBD)
- [ ] [P2] Rescan, backup, and export/import tooling (owner: TBD)
- [ ] [P2] Wallet encryption and key management parity (owner: TBD)

## UX, ops, and tooling

- [ ] [P2] Sync/run profiles (`--profile low|default|high`) for worker + DB presets (owner: TBD)
- [ ] [P2] Config file support (`flux.conf` parity) (owner: TBD)
- [ ] [P2] Structured CLI help output and subcommands (owner: TBD)
- [ ] [P2] DB inspection CLI (index stats, supply, integrity) (owner: TBD)
- [ ] [P2] Metrics export (Prometheus or similar) (owner: TBD)
- [ ] [P2] Log levels and structured logging parity (owner: TBD)
- [ ] [P2] Database migrations and upgrade path (owner: TBD)

## Testing and release hardening

- [x] [P0] Automated consensus vector tests vs C++ (embedded sighash vectors) (owner: TBD)
- [ ] [P1] RPC golden tests against C++ behavior (owner: TBD)
- [ ] [P1] Long-run sync tests with regression gates (owner: TBD)
- [x] [P1] Reorg and fork simulation tests (owner: TBD)
- [ ] [P2] Snapshot/fast-sync evaluation (optional) (owner: TBD)
- [ ] [P2] Performance profiling harness (owner: TBD)

## RPC parity (status columns)

This section is a method-level snapshot of parity. See `docs/RPC_PARITY.md` for full notes.

### General / node

| Implemented | Partial | Missing |
| --- | --- | --- |
| `help`<br>`getinfo`<br>`ping`<br>`stop`<br>`restart`<br>`getnetworkinfo`<br>`getpeerinfo`<br>`getnettotals`<br>`getconnectioncount`<br>`listbanned`<br>`getdeprecationinfo` | - | `start` |

### Chain and blocks

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getblockcount`<br>`getbestblockhash`<br>`getblockhash`<br>`getblockheader`<br>`getblock`<br>`getblockchaininfo`<br>`getdifficulty`<br>`getchaintips`<br>`getblocksubsidy`<br>`getblockhashes`<br>`getblockdeltas` | `gettxoutsetinfo` | - |

### Transactions and scripts

| Implemented | Partial | Missing |
| --- | --- | --- |
| `gettxout`<br>`getrawtransaction`<br>`estimatefee` | `sendrawtransaction` (relays; confirmed inputs only) | `createrawtransaction`<br>`decoderawtransaction`<br>`decodescript`<br>`fundrawtransaction`<br>`signrawtransaction`<br>`createmultisig`<br>`validateaddress`<br>`verifymessage`<br>`signmessage`<br>`estimatepriority`<br>`gettxoutproof`<br>`verifytxoutproof`<br>`prioritisetransaction` |

### Mempool and relay

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getmempoolinfo`<br>`getrawmempool` | - | - |

### Mining

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getmininginfo`<br>`submitblock` | `getblocktemplate`<br>`getnetworkhashps`<br>`getnetworksolps`<br>`getlocalsolps` | - |

### Fluxnode

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getfluxnodecount`<br>`listfluxnodes`<br>`viewdeterministicfluxnodelist`<br>`getdoslist`<br>`getstartlist` | `fluxnodecurrentwinner`<br>`getfluxnodestatus` | `createfluxnodekey`<br>`listfluxnodeconf`<br>`getfluxnodeoutputs`<br>`startfluxnode`<br>`startdeterministicfluxnode` |

### Address and insight-style indexes

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getspentinfo`<br>`getaddressbalance`<br>`getaddressdeltas`<br>`getaddressutxos`<br>`getaddressmempool`<br>`getaddresstxids` | - | - |

### Network admin

| Implemented | Partial | Missing |
| --- | --- | --- |
| - | - | `addnode`<br>`disconnectnode`<br>`getaddednodeinfo`<br>`setban`<br>`clearbanned`<br>`verifychain`<br>`reindex`<br>`rescanblockchain` |

### Wallet (transparent)

| Implemented | Partial | Missing |
| --- | --- | --- |
| - | - | `getnewaddress`<br>`getbalance`<br>`getwalletinfo`<br>`listtransactions`<br>`listunspent`<br>`sendtoaddress`<br>`sendmany`<br>`settxfee`<br>`importaddress`<br>`importprivkey`<br>`importwallet`<br>`backupwallet`<br>`dumpprivkey`<br>`keypoolrefill`<br>`listreceivedbyaddress`<br>`listaddressgroupings`<br>`getreceivedbyaddress`<br>`getunconfirmedbalance`<br>`getrawchangeaddress`<br>`lockunspent`<br>`listlockunspent`<br>`sendfrom` |

### Wallet (shielded)

| Implemented | Partial | Missing |
| --- | --- | --- |
| - | - | `zgetbalance`<br>`zgettotalbalance`<br>`zgetnewaddress`<br>`zlistaddresses`<br>`zlistunspent`<br>`zsendmany`<br>`zshieldcoinbase`<br>`zexportkey`<br>`zexportviewingkey`<br>`zimportkey`<br>`zimportviewingkey`<br>`zimportwallet`<br>`zgetoperationstatus`<br>`zgetoperationresult`<br>`zlistoperationids`<br>`zgetmigrationstatus`<br>`zsetmigration`<br>`zvalidateaddress`<br>`zcrawjoinsplit`<br>`zcrawreceive`<br>`zcbenchmark` |
