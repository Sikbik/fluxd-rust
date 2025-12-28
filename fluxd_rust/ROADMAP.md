# fluxd-rust roadmap

This is a living checklist for parity and modernization work. Mark items as `[x]` when complete.

Legend:
- P0 = safety/consensus blocking
- P1 = parity and user-facing
- P2 = quality of life or ops

Owner format: `owner: <name>` or `owner: TBD`.

## Current sprint (next 3-5)

- [ ] [P1] Spent index + `getspentinfo` (owner: TBD)
- [ ] [P1] Address indexes (deltas/utxos/txids) + `getaddress*` RPCs (owner: TBD)
- [ ] [P1] Header throughput parity with C++ (owner: TBD)

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
- [x] CLI supply scan (`--scan-supply`) and flatfile scan (`--scan-flatfiles`)
- [x] Documentation set (README + docs/)
- [x] Binary name `fluxd`

## Active focus

- [ ] [P1] Block indexing throughput parity (I/O batching, memtable tuning, UTXO cache policy) (owner: TBD)
  - [x] Cache Sprout/Sapling trees in memory and only write tree bytes when the tree root changes
  - [x] Store Sapling anchors as root keys only (empty values) to reduce DB write amplification
  - [x] Run block connect on blocking threads to keep RPC/dashboard responsive under high throughput
  - [x] Reduce per-input allocation overhead (outpoint key handling + per-block UTXO cache)
  - [x] Inline small keys in `WriteBatch` (UTXO/address keys stored without heap allocs)
  - [x] LRU UTXO read cache for sequential block spends (`--utxo-cache-entries`)
  - [ ] Review index write amplification (txindex/address index) and batching opportunities
  - [x] Capture initial throughput stats (~150–180 b/s on 8-core mainnet sync with `--shielded-workers 6` and `--inflight-per-peer 2`)
- [ ] [P1] RPC parity expansion (see detailed checklist below) (owner: TBD)

## Consensus and chainstate parity

- [ ] [P0] Upgrade activation heights and block hashes cross-checked against C++ (owner: TBD)
- [ ] [P0] PoN rules parity (header validation, signature rules, economics) (owner: TBD)
- [ ] [P0] Block reward schedule parity (including canceled halving at 1,968,550) (owner: TBD)
- [ ] [P0] Chainwork parity for PoN/PoW edge cases and existing headers (owner: TBD)
- [ ] [P0] Difficulty/target parity across LWMA/legacy windows and transitions (owner: TBD)
- [ ] [P0] Checkpoint handling parity and tests (owner: TBD)
- [ ] [P0] Expanded block index fields (status/validity/tx counts/undo offsets) (owner: TBD)
- [ ] [P0] Block file metadata (per-file stats, last file tracking, prune flags) (owner: TBD)

## Indexes and data services

- [ ] [P1] Spent index (`getspentinfo`) (owner: TBD)
- [ ] [P1] Address indexes (deltas/utxos/txids) + `getaddress*` RPCs (owner: TBD)
- [ ] [P1] Address unspent index (owner: TBD)
- [x] [P1] UTXO set stats (`gettxoutsetinfo`) (owner: TBD)
- [x] [P1] Shielded value pool totals (Sprout/Sapling) for supply tracking (owner: TBD)
- [ ] [P1] Block deltas index (`getblockdeltas`) (owner: TBD)
- [ ] [P2] Reindex/rescan support for secondary indexes (owner: TBD)
- [ ] [P2] DB inspection tools (index stats, supply, integrity) (owner: TBD)

## Networking state and P2P parity

- [x] [P1] `peers.dat` persistence (owner: TBD)
- [x] [P1] `banlist.dat` persistence (owner: TBD)
- [ ] [P1] Peer scoring and eviction parity (owner: TBD)
- [ ] [P1] Address manager parity (stochastic buckets, last-seen timestamps) (owner: TBD)
- [ ] [P2] P2P message coverage parity (addr/getaddr/feefilter/mempool, etc.) (owner: TBD)
- [ ] [P2] Service flags and user agent compatibility (owner: TBD)

## Mempool and mining

- [ ] [P1] Mempool persistence + eviction policy parity (owner: TBD)
- [ ] [P1] Fee estimator persistence (owner: TBD)
- [ ] [P1] Standardness policy parity (mempool accept rules) (owner: TBD)
- [ ] [P1] `getmempoolinfo`, `getrawmempool` (owner: TBD)
- [ ] [P1] `getblocktemplate`, `getmininginfo`, `submitblock` (owner: TBD)
- [ ] [P2] `getnetworkhashps`, `getnetworksolps`, `getlocalsolps` (real metrics) (owner: TBD)

## Wallet

- [ ] [P1] Wallet database (keys, keypool, metadata) (owner: TBD)
- [ ] [P1] Transparent wallet RPCs (owner: TBD)
- [ ] [P1] Shielded wallet RPCs (owner: TBD)
- [ ] [P2] Rescan, backup, and export/import tooling (owner: TBD)
- [ ] [P2] Wallet encryption and key management parity (owner: TBD)

## UX, ops, and tooling

- [ ] [P2] Config file support (`flux.conf` parity) (owner: TBD)
- [ ] [P2] Structured CLI help output and subcommands (owner: TBD)
- [ ] [P2] DB inspection CLI (index stats, supply, integrity) (owner: TBD)
- [ ] [P2] Metrics export (Prometheus or similar) (owner: TBD)
- [ ] [P2] Log levels and structured logging parity (owner: TBD)
- [ ] [P2] Database migrations and upgrade path (owner: TBD)

## Testing and release hardening

- [ ] [P0] Automated consensus vector tests vs C++ (`fluxd/`) (owner: TBD)
- [ ] [P1] RPC golden tests against C++ behavior (owner: TBD)
- [ ] [P1] Long-run sync tests with regression gates (owner: TBD)
- [ ] [P1] Reorg and fork simulation tests (owner: TBD)
- [ ] [P2] Snapshot/fast-sync evaluation (optional) (owner: TBD)
- [ ] [P2] Performance profiling harness (owner: TBD)

## RPC parity (status columns)

This section is a method-level snapshot of parity. See `docs/RPC_PARITY.md` for full notes.

### General / node

| Implemented | Partial | Missing |
| --- | --- | --- |
| `help`<br>`getinfo`<br>`getnetworkinfo`<br>`getpeerinfo`<br>`getnettotals`<br>`getconnectioncount`<br>`listbanned` | - | `getdeprecationinfo`<br>`ping`<br>`start`<br>`stop`<br>`restart` |

### Chain and blocks

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getblockcount`<br>`getbestblockhash`<br>`getblockhash`<br>`getblockheader`<br>`getblock`<br>`getblockchaininfo`<br>`getdifficulty`<br>`getchaintips`<br>`getblocksubsidy`<br>`getblockhashes` | `gettxoutsetinfo` | `getblockdeltas` |

### Transactions and scripts

| Implemented | Partial | Missing |
| --- | --- | --- |
| `gettxout` | `getrawtransaction` (chain only) | `createrawtransaction`<br>`decoderawtransaction`<br>`decodescript`<br>`sendrawtransaction`<br>`fundrawtransaction`<br>`signrawtransaction`<br>`createmultisig`<br>`validateaddress`<br>`verifymessage`<br>`signmessage`<br>`estimatefee`<br>`estimatepriority`<br>`gettxoutproof`<br>`verifytxoutproof`<br>`prioritisetransaction` |

### Mempool and relay

| Implemented | Partial | Missing |
| --- | --- | --- |
| - | - | `getmempoolinfo`<br>`getrawmempool` |

### Mining

| Implemented | Partial | Missing |
| --- | --- | --- |
| - | `getnetworkhashps`<br>`getnetworksolps`<br>`getlocalsolps` | `getblocktemplate`<br>`getmininginfo`<br>`submitblock` |

### Fluxnode

| Implemented | Partial | Missing |
| --- | --- | --- |
| `getfluxnodecount`<br>`listfluxnodes`<br>`viewdeterministicfluxnodelist` | `fluxnodecurrentwinner` | `getfluxnodestatus`<br>`getdoslist`<br>`getstartlist`<br>`createfluxnodekey`<br>`listfluxnodeconf`<br>`getfluxnodeoutputs`<br>`startfluxnode`<br>`startdeterministicfluxnode` |

### Address and insight-style indexes

| Implemented | Partial | Missing |
| --- | --- | --- |
| - | - | `getaddressbalance`<br>`getaddressdeltas`<br>`getaddressutxos`<br>`getaddressmempool`<br>`getaddresstxids`<br>`getspentinfo` |

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
