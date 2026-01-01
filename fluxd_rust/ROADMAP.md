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
- [x] CLI scan tools (`--scan-supply`, `--scan-flatfiles`, `--scan-fluxnodes`, `--db-info`)
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
  - [x] Add detailed connect-stage telemetry in `/stats` (UTXO get/put/delete, undo encode/append, index op counts, fluxnode tx/sig, PoN sig, payout checks)
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
  - [x] Fluxnode confirm expiration parity across upgrades (avoid resurrecting expired nodes after PoN)
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
  - [x] `reindex` RPC + `--reindex` startup flag (wipe `db/` + `blocks/`, then resync)
  - [x] `rescanblockchain` RPC stub (wallet not implemented)
  - [ ] Reindex from existing flatfiles (no network)
  - [ ] Selective index rebuild (txindex/address/spent only)
- [ ] [P2] DB inspection tools (index stats, supply, integrity) (owner: TBD)
  - [x] `getdbinfo` RPC (disk usage breakdown + fjall telemetry + flatfiles meta/fs cross-check)
  - [x] `--db-info` CLI (same JSON as `getdbinfo`, then exit)
  - [ ] Optional: key count sampling per partition (slow)
  - [ ] Optional: integrity summary mode (runs `verifychain` + flatfile/meta sanity)

## Networking state and P2P parity

- [x] [P1] `peers.dat` persistence (owner: TBD)
- [x] [P1] `banlist.dat` persistence (owner: TBD)
- [x] [P1] Header peer probing (probe more candidates before selecting best height peer) (owner: TBD)
- [x] [P1] Sync liveness hardening (timeouts + watchdogs, prevent dead-hangs) (owner: TBD)
  - [x] P2P send timeout (prevents stalled writes / CLOSE-WAIT wedges)
  - [x] Handshake read timeout (prevents stuck initial connects)
  - [x] Block verify/connect pipeline idle timeout (prevents fetch/connect wedging indefinitely)
  - [x] Treat `stalled` errors as transient and reconnect
- [x] [P1] Peer scoring and eviction parity (owner: TBD)
  - [x] Persist peer health stats in `peers.dat` (success/fail, last-seen, last-height, last-version)
  - [x] Prefer recently-good peers and apply exponential backoff after failures
  - [x] Prune stale/unreachable addresses and suppress dial spam
- [x] [P1] Address manager v1 (last-seen timestamps + persistence) (owner: TBD)
- [x] [P2] Address manager parity (bucket-based selection like C++) (owner: TBD)
  - [x] Bucketed sampling by netgroup + tried/new split (reduces peer herding on large address books)
- [ ] [P2] P2P message coverage parity (addr/getaddr/feefilter/mempool, etc.) (owner: TBD)
  - [x] Address discovery: `getaddr` + `addr` ingest (owner: TBD)
  - [x] Tx relay: `inv`/`getdata`/`tx` + `mempool` (owner: TBD)
  - [x] `feefilter` and fee-based relay policies (owner: TBD)
  - [x] `reject`/`notfound` handling parity (owner: TBD)
  - [x] Ignore unsolicited `block` messages during getdata download (prevents sync stalls) (owner: TBD)
- [x] [P2] Service flags and user agent compatibility (owner: TBD)

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
  - [x] Basic mempool tx selection + dependency ordering
  - [x] Template field parity (BIP22-ish keys + Flux deterministic payout fields)
  - [x] Longpoll wait behavior (honor request `longpollid`) (owner: TBD)
  - [x] Proposal mode support (`{"mode":"proposal","data":"..."}`) (owner: TBD)
  - [x] Miner address config (`--miner-address` / `flux.conf` `mineraddress=`) (owner: TBD)
  - [x] Wallet-backed miner script (coinbase script from wallet) (owner: TBD)
- [ ] [P2] `getnetworkhashps`, `getnetworksolps`, `getlocalsolps` (real metrics) (owner: TBD)

## Wallet

- [x] [P1] Wallet database (keys, keypool, metadata) (owner: TBD)
  - [x] `wallet.dat` key store (P2PKH keys + network tag)
  - [x] Atomic wallet writes + deterministic key order
  - [x] Persist watch-only scripts in `wallet.dat` (v2; `importaddress`)
  - [x] Persist `paytxfee` in `wallet.dat` (v2; `settxfee`)
  - [x] Keypool/change-address management parity
  - [x] Wallet tx history / persistence (txid set in `wallet.dat` v3; updated by `rescanblockchain` + wallet send RPCs)
- [ ] [P1] Transparent wallet RPCs (owner: TBD)
  - [x] `importaddress` (watch-only; rescan is a no-op due to address index)
  - [x] `importwallet` (best-effort WIF import from dump file)
  - [x] `getnewaddress`, `importprivkey`, `dumpprivkey`
  - [x] `getrawchangeaddress`
  - [x] `getbalance`, `getunconfirmedbalance`, `listunspent`, `getwalletinfo` (partial fields)
  - [x] `lockunspent`, `listlockunspent`
  - [x] `listaddressgroupings` (simplified grouping)
  - [x] `getreceivedbyaddress` (P2PKH only)
  - [x] `gettransaction` (partial)
    - [x] `include_watchonly` support
    - [x] `involvesWatchonly` output flag
  - [x] `listtransactions` (partial)
    - [x] `include_watchonly` support
    - [x] `involvesWatchonly` output flag
  - [x] `listsinceblock` (partial)
  - [x] `listreceivedbyaddress` (partial)
    - [x] `include_watchonly` support + `involvesWatchonly`
    - [x] Populate `txids` via address delta index
  - [x] `keypoolrefill`
  - [x] `addmultisigaddress` (partial; watch-only P2SH)
  - [x] `settxfee` (partial)
  - [x] `signmessage`
  - [x] `backupwallet`
  - [x] `fundrawtransaction` (P2PKH only)
  - [x] `signrawtransaction` (P2PKH only)
  - [x] `sendtoaddress` (P2PKH only; no `subtractfeefromamount`)
  - [x] `sendmany` (P2PKH only; no `subtractfeefromamount`)
  - [x] `sendfrom` (fromaccount ignored; minconf supported)
  - [x] `rescanblockchain` (scans address delta index; populates wallet tx history)
- [ ] [P1] Shielded wallet RPCs (owner: TBD)
- [ ] [P2] Rescan, backup, and export/import tooling (owner: TBD)
- [ ] [P2] Wallet encryption and key management parity (owner: TBD)

## UX, ops, and tooling

- [x] [P2] Sync/run profiles (`--profile low|default|high`) for worker + DB presets (owner: TBD)
- [x] [P1] Data-dir lock file (`--data-dir/.lock`) to prevent multi-instance corruption (owner: TBD)
- [ ] [P2] Config file support (`flux.conf` parity) (owner: TBD)
  - [x] Basic `flux.conf` parsing (`rpcuser`, `rpcpassword`, `rpcbind`, `rpcport`, `rpcallowip`, `addnode`, `mineraddress`)
  - [ ] Remaining `flux.conf` keys parity
    - [x] Enforce RPC allowlist via `rpcallowip` (localhost-only default)
    - [x] Support `testnet`/`regtest` toggles and warn on unsupported keys
    - [x] Support `dbcache`, `maxmempool`, `minrelaytxfee`
- [ ] [P2] Structured CLI help output and subcommands (owner: TBD)
- [ ] [P2] DB inspection CLI (index stats, supply, integrity) (owner: TBD)
  - [x] `--db-info`
  - [x] `--scan-flatfiles`
  - [x] `--scan-supply`
- [x] [P2] Metrics export (Prometheus `/metrics`) (owner: TBD)
- [x] [P2] Log levels and structured logging parity (owner: TBD)
  - [x] `--log-level` + `flux.conf` `loglevel`
  - [x] `--log-format text|json` + `flux.conf` `logformat`
  - [x] `--log-timestamps`/`--no-log-timestamps` + `flux.conf` `logtimestamps`
  - [x] Workspace-wide logging via `fluxd-log` (no `println!`/`eprintln!` in hot paths)
- [ ] [P2] Database migrations and upgrade path (owner: TBD)

## Testing and release hardening

- [x] [P0] Automated consensus vector tests vs C++ (embedded sighash vectors) (owner: TBD)
- [ ] [P1] RPC golden tests against C++ behavior (owner: TBD)
  - [x] Schema parity: `getblockchaininfo` required keys
  - [x] Schema parity: `gettxoutsetinfo` required keys
  - [x] Schema parity: `getinfo` required keys
  - [x] Schema parity: `getnetworkinfo` required keys
  - [x] Schema parity: `getnettotals` required keys
  - [x] Schema parity: `getpeerinfo` required keys
  - [x] Schema parity: `getconnectioncount`/`getdeprecationinfo`/`listbanned` required keys
  - [x] Schema parity: node control RPCs (`addnode`, `getaddednodeinfo`, `disconnectnode`, `setban`, `clearbanned`)
  - [x] Schema parity: `getblockcount`/`getbestblockhash`/`getblockhash` required keys
  - [x] Schema parity: `getdifficulty`/`getchaintips` required keys
  - [x] Schema parity: `getblockheader`/`getblock` required keys
  - [x] Schema parity: `getblocksubsidy`/`getblockhashes`/`getblockdeltas` required keys
  - [x] Schema parity: raw tx/script RPCs (`createrawtransaction`, `decoderawtransaction`, `decodescript`, `validateaddress`, `verifymessage`, `createmultisig`, `getrawtransaction`)
  - [x] Schema parity: `sendrawtransaction` success + common failures
  - [x] Schema parity: `getmempoolinfo`/`getrawmempool` required keys
  - [x] Schema parity: mining/ops RPCs (`getmininginfo`, `estimatefee`, `submitblock`, `getdbinfo`)
  - [x] Schema parity: `gettxout` required keys (chain + mempool)
  - [x] Schema parity: `gettxoutproof`/`verifytxoutproof` basic behavior
  - [x] Schema parity: `getspentinfo` required keys
  - [x] Schema parity: `getaddress*` required keys (`utxos`/`balance`/`deltas`/`txids`/`mempool`)
  - [x] Error-code parity: `getblockhash`/`getblockheader` invalid params + not-found
  - [x] Error-code parity: `getspentinfo` not-found
  - [x] Expand schema checks across remaining RPCs (blocks/mempool/fluxnode/etc)
    - [x] Fluxnode RPC schemas (`getfluxnodecount`, `viewdeterministicfluxnodelist`, `fluxnodecurrentwinner`, `getfluxnodestatus`, `getstartlist`, `getdoslist`)
    - [x] Core daemon/control schemas (`help`, `ping`, `stop`, `restart`, `reindex`, `rescanblockchain`, `verifychain`)
    - [x] Mining metrics/template schemas (`getblocktemplate`, `getnetworkhashps`, `getnetworksolps`, `getlocalsolps`)
    - [x] Fluxnode admin/control schemas (`createfluxnodekey`, `listfluxnodeconf`, `getfluxnodeoutputs`, `startfluxnode`, `startdeterministicfluxnode`)
- [x] [P1] Long-run sync tests with regression gates (owner: TBD)
  - [x] VPS smoke test script (`scripts/remote_smoke_test.sh`) to validate startup + RPC + peer connectivity (supports seeding peers.dat and progress thresholds)
  - [x] VPS progress gate (`scripts/progress_gate.sh`) to detect stalls/regressions during sync
  - [x] VPS watchdog loop (`scripts/longrun_watchdog.sh`) to combine stall detection + fatal log pattern checks
- [x] [P1] Reorg and fork simulation tests (owner: TBD)
- [ ] [P2] Snapshot/fast-sync evaluation (optional) (owner: TBD)
- [ ] [P2] Performance profiling harness (owner: TBD)

## RPC parity (status columns)

This section is a method-level snapshot of parity. See `docs/RPC_PARITY.md` for full notes.

### General / node

| Implemented | Partial | Missing |
| --- | --- | --- |
| `help`<br>`getinfo`<br>`ping`<br>`stop`<br>`restart`<br>`getnetworkinfo`<br>`getpeerinfo`<br>`getnettotals`<br>`getconnectioncount`<br>`listbanned`<br>`clearbanned`<br>`setban`<br>`addnode`<br>`getaddednodeinfo`<br>`disconnectnode`<br>`getdeprecationinfo` | `start` (stub; no-op) | - |

### Chain and blocks

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getblockcount`<br>`getbestblockhash`<br>`getblockhash`<br>`getblockheader`<br>`getblock`<br>`getblockchaininfo`<br>`getdifficulty`<br>`getchaintips`<br>`getblocksubsidy`<br>`getblockhashes`<br>`getblockdeltas`<br>`gettxoutsetinfo` | - | - |

### Transactions and scripts

| Implemented | Partial | Missing |
| --- | --- | --- |
| `createrawtransaction`<br>`decoderawtransaction`<br>`decodescript`<br>`createmultisig`<br>`gettxout`<br>`gettxoutproof`<br>`verifytxoutproof`<br>`getrawtransaction`<br>`estimatefee`<br>`validateaddress`<br>`verifymessage`<br>`signmessage` | `sendrawtransaction` (relays; confirmed inputs only)<br>`fundrawtransaction` (P2PKH only)<br>`signrawtransaction` (P2PKH only)<br>`estimatepriority` (returns -1.0) | - |

### Mempool and relay

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getmempoolinfo`<br>`getrawmempool` | - | - |

### Mining

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getmininginfo`<br>`submitblock`<br>`prioritisetransaction` | `getblocktemplate`<br>`getnetworkhashps`<br>`getnetworksolps`<br>`getlocalsolps` | - |

### Fluxnode

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getfluxnodecount`<br>`listfluxnodes`<br>`viewdeterministicfluxnodelist`<br>`getdoslist`<br>`getstartlist`<br>`createfluxnodekey`<br>`listfluxnodeconf`<br>`getfluxnodeoutputs` | `fluxnodecurrentwinner`<br>`getfluxnodestatus`<br>`startfluxnode`<br>`startdeterministicfluxnode` | - |

### Benchmarking

| Implemented | Partial | Missing |
| --- | --- | --- |
| - | `getbenchmarks` (stub)<br>`getbenchstatus` (stub)<br>`startbenchmark` (stub; alias: `startfluxbenchd`/`startzelbenchd`)<br>`stopbenchmark` (stub; alias: `stopfluxbenchd`/`stopzelbenchd`)<br>`zcbenchmark` (stub; returns error) | - |

### Address and insight-style indexes

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getspentinfo`<br>`getaddressbalance`<br>`getaddressdeltas`<br>`getaddressutxos`<br>`getaddressmempool`<br>`getaddresstxids` | - | - |

### Network admin

| Implemented | Partial | Missing |
| --- | --- | --- |
| `addnode`<br>`disconnectnode`<br>`getaddednodeinfo`<br>`setban`<br>`clearbanned`<br>`verifychain` | `reindex`<br>`rescanblockchain` | - |

### Wallet (transparent)

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getnewaddress`<br>`importprivkey`<br>`dumpprivkey`<br>`signmessage`<br>`backupwallet`<br>`importaddress`<br>`lockunspent`<br>`listlockunspent`<br>`keypoolrefill` | `getrawchangeaddress`<br>`getbalance`<br>`getunconfirmedbalance`<br>`getreceivedbyaddress`<br>`gettransaction`<br>`listtransactions`<br>`listsinceblock`<br>`listreceivedbyaddress`<br>`addmultisigaddress`<br>`settxfee`<br>`getwalletinfo`<br>`listunspent`<br>`fundrawtransaction`<br>`signrawtransaction`<br>`sendtoaddress`<br>`sendmany`<br>`importwallet`<br>`listaddressgroupings`<br>`sendfrom` | - |

### Wallet (shielded)

| Implemented | Partial | Missing |
| --- | --- | --- |
| - | `zvalidateaddress` (partial; `ismine` is always false) | `zgetbalance`<br>`zgettotalbalance`<br>`zgetnewaddress`<br>`zlistaddresses`<br>`zlistunspent`<br>`zsendmany`<br>`zshieldcoinbase`<br>`zexportkey`<br>`zexportviewingkey`<br>`zimportkey`<br>`zimportviewingkey`<br>`zimportwallet`<br>`zgetoperationstatus`<br>`zgetoperationresult`<br>`zlistoperationids`<br>`zgetmigrationstatus`<br>`zsetmigration`<br>`zcrawjoinsplit`<br>`zcrawreceive` |
