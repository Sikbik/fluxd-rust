# RPC parity checklist

This file tracks parity targets with the C++ `fluxd` RPC surface. Statuses:

- Implemented: available and returning structured data.
- Partial: available but fields are placeholders or simplified.
- Stub: method exists but returns "not implemented" or placeholder values.
- Missing: method not registered (returns "method not found").

## General

- help - Implemented
- getinfo - Implemented
- getfluxnodestatus - Partial (looks up a collateral outpoint; local fluxnode config parsing is basic; IP fields not yet stored)
- listfluxnodes - Implemented (via deterministic list)
- viewdeterministicfluxnodelist - Implemented
- getfluxnodecount - Implemented
- getdoslist - Implemented
- getstartlist - Implemented
- fluxnodecurrentwinner - Partial (best-effort selection)

## Chain and block

- getbestblockhash - Implemented
- getblock - Implemented
- getblockchaininfo - Implemented
- getblockcount - Implemented
- getblockdeltas - Implemented
- getblockhash - Implemented
- getblockheader - Implemented
- getchaintips - Implemented
- getdifficulty - Implemented

## Mempool and UTXO

- getmempoolinfo - Implemented
- getrawmempool - Implemented
- gettxout - Implemented
- gettxoutproof - Implemented
- gettxoutsetinfo - Implemented
- verifytxoutproof - Implemented
- getspentinfo - Implemented

## Mining

- getblocksubsidy - Implemented
- getblocktemplate - Implemented (template fields + longpoll + proposal; falls back to `--miner-address` then the wallet if mineraddress is unset)
- getlocalsolps - Stub (returns 0.0)
- getmininginfo - Partial (rates and mining fields are placeholders)
- getnetworkhashps - Implemented (chainwork/time-based estimate; `blocks<=0` uses Digishield averaging window)
- getnetworksolps - Implemented (chainwork/time-based estimate; `blocks<=0` uses Digishield averaging window)

## Network

- getconnectioncount - Implemented
- getdeprecationinfo - Implemented
- getnettotals - Implemented
- getnetworkinfo - Implemented
- getpeerinfo - Implemented
- listbanned - Implemented

## Raw transactions and scripts

- createrawtransaction - Implemented
- decoderawtransaction - Implemented
- decodescript - Implemented
- getrawtransaction - Implemented (chain + mempool)
- fundrawtransaction - Partial (P2PKH wallet UTXOs only; supports `minconf` option)
- sendrawtransaction - Partial (supports spending mempool parents; orphan pool + full policy parity WIP)
- createmultisig - Partial (hex pubkeys only; wallet address lookup not available)
- estimatefee - Implemented
- estimatepriority - Stub (returns -1.0; priority estimator not implemented)
- validateaddress - Implemented
- verifymessage - Implemented

## Extra queries

- gettransaction - Partial (wallet-only view; amount/fee match `fluxd` semantics; change outputs are omitted from `details` on outgoing txs; `details` ordering + coinbase categories match `fluxd`; remaining field parity WIP)
- zvalidateaddress - Partial (validates Sprout/Sapling encoding + returns key components; Sapling `ismine` checks wallet spending keys; `iswatchonly` checks imported Sapling viewing keys)
- getbenchmarks - Stub (Fluxnode-only; returns `"Benchmark not running"` until fluxbenchd integration exists)
- getbenchstatus - Stub (Fluxnode-only; returns `"Benchmark not running"` until fluxbenchd integration exists)
- getblockhashes - Implemented

## Address index (insight)

- getaddresstxids - Implemented
- getaddressbalance - Implemented
- getaddressdeltas - Implemented
- getaddressutxos - Implemented
- getaddressmempool - Implemented

## Node control

- sendfrom - Implemented (fromaccount ignored; minconf supported)
- submitblock - Partial (accepts blocks; return codes are simplified)
- zcrawjoinsplit - Stub (returns error; joinsplit tooling not implemented)
- zcrawreceive - Stub (returns error; joinsplit tooling not implemented)
- prioritisetransaction - Implemented (stores fee/priority deltas for mining selection)

- reindex - Implemented (requests shutdown; on next start wipes `db/` and rebuilds indexes from existing flatfiles under `blocks/`; use `--resync` to wipe blocks too)
- stop - Implemented
- createfluxnodekey - Implemented (alias: createzelnodekey)
- createzelnodekey - Implemented (alias of createfluxnodekey)
- listfluxnodeconf - Implemented (alias: listzelnodeconf)
- listzelnodeconf - Implemented (alias of listfluxnodeconf)
- getfluxnodeoutputs - Implemented (wallet-less; uses fluxnode.conf + UTXO lookups)
- startfluxnode - Partial (wallet-less; uses fluxnode.conf; requires a collateral WIF key)
- startdeterministicfluxnode - Partial (wallet-less; requires a collateral WIF key; P2SH collateral also requires redeem script)
- verifychain - Partial (checks flatfile decode + header linkage + merkle root + txindex; does not re-apply full UTXO/script validation like C++)
- addnode - Implemented (IP/IP:PORT only; no DNS resolution yet)
- clearbanned - Implemented
- disconnectnode - Implemented (address-based; best-effort)
- getaddednodeinfo - Implemented (simplified fields; `dns` param ignored)
- setban - Implemented (SocketAddr bans; `absolute` supported)

## Wallet

- signrawtransaction - Partial (P2PKH only; supports wallet keys and optional WIF override list)
- addmultisigaddress - Partial (adds a watch-only P2SH script; spending multisig outputs is not yet supported)
- backupwallet - Implemented
- dumpprivkey - Implemented (P2PKH only)
- getbalance - Partial (minconf supported; `minconf=0` includes spendable mempool outputs; `include_watchonly` supported; accounts ignored)
- getnewaddress - Implemented (P2PKH only; label ignored; keypool-backed)
- getrawchangeaddress - Partial (P2PKH only; address_type param ignored; reserves change addresses tracked in wallet.dat)
- getreceivedbyaddress - Partial (P2PKH only; uses address deltas for confirmed receives, plus mempool outputs when `minconf=0`)
- getunconfirmedbalance - Partial (derived from spendable mempool outputs paying to the wallet)
- getwalletinfo - Partial (balances derived from the address index; `txcount` and keypool fields are persisted)
- importaddress - Implemented (watch-only; rescan ignored due to address index)
- importprivkey - Implemented (rescan param accepted but ignored; address index makes it unnecessary)
- importwallet - Partial (best-effort WIF import from dump file)
- keypoolrefill - Implemented (fills persisted keypool; does not create addresses)
- listaddressgroupings - Partial (simplified grouping)
- listlockunspent - Implemented
- listreceivedbyaddress - Partial (transparent only; `include_watchonly` supported; `txids` populated; labels are WIP)
- listsinceblock - Partial (transparent only; confirmed via address deltas; mempool included; `include_watchonly` supported; returns one entry per wallet-relevant output; coinbase categories match `fluxd`)
- listtransactions - Partial (transparent only; confirmed via address deltas; mempool included; `include_watchonly` supported; ordered oldest → newest; returns one entry per wallet-relevant output; coinbase categories match `fluxd`)
- listunspent - Partial (supports minconf/maxconf/address filter; `minconf=0` includes mempool outputs; returns spendable flag and locked state)
- lockunspent - Implemented
- rescanblockchain - Implemented (scans address delta index; populates wallet tx history)

- sendmany - Partial (P2PKH only)
- sendtoaddress - Partial (P2PKH only)
- settxfee - Partial (sets in-process wallet fee-rate override used by fundrawtransaction/send*)
- signmessage - Implemented (P2PKH only; compatible with verifymessage)

## Shielded

- zexportkey - Partial (Sapling only; exports Sapling extended spending key)
- zexportviewingkey - Partial (Sapling only; exports Sapling full viewing key)
- zgetbalance - Partial (Sapling only; scans chain for Sapling notes; excludes notes spent by chain nullifiers or mempool nullifiers; supports watch-only via includeWatchonly)
- zgetmigrationstatus - Implemented (returns disabled migration status; amount fields are strings for C++ parity; migration not supported)
- zgetnewaddress - Partial (Sapling only; persists a Sapling key in wallet.dat)
- zgetoperationresult - Implemented (async op manager; returns completed ops and removes them)
- zgetoperationstatus - Implemented (async op manager; returns op status entries)
- zgettotalbalance - Partial (Sapling only for private balance; scans chain for Sapling notes; excludes notes spent by mempool nullifiers; supports watch-only via includeWatchonly)
- zimportkey - Partial (Sapling only; resets Sapling scan cursor so historical notes can be discovered on next shielded balance query)
- zimportviewingkey - Partial (Sapling only; stores watch-only viewing keys; resets Sapling scan cursor so historical notes can be discovered on next shielded balance query)
- zimportwallet - Partial (imports Sapling spending keys and WIFs from file; resets Sapling scan cursor so historical notes can be discovered on next shielded balance query)
- zlistaddresses - Partial (Sapling only; `includeWatchonly=true` includes watch-only addresses)
- zlistoperationids - Implemented (async op manager; optional filter)
- zlistreceivedbyaddress - Partial (Sapling only; lists received Sapling notes for a zaddr; supports watch-only via includeWatchonly; memo is placeholder)
- zlistunspent - Partial (Sapling only; lists unspent Sapling notes; excludes notes spent by mempool nullifiers; supports watch-only via includeWatchonly)
- zsendmany - Implemented (Sapling only; async op; tx construction + mempool submission; uses cached Sapling note rseed when available; has RPC smoke + ignored end-to-end spend harness)
- zsetmigration - Implemented (deprecated on Flux fork; returns misc error)
- zshieldcoinbase - Implemented (deprecated on Flux fork; returns misc error)

## Admin and benchmarking

- start - Stub (no-op; returns `"fluxd already running"`)
- restart - Implemented
- ping - Implemented
- zcbenchmark - Stub (returns error; zcash benchmarks not implemented)
- startbenchmark - Stub (alias: `startfluxbenchd`/`startzelbenchd`; control not implemented)
- stopbenchmark - Stub (alias: `stopfluxbenchd`/`stopzelbenchd`; control not implemented)

## fluxd-rust extensions

These methods are not part of the legacy C++ `fluxd` RPC surface, but are useful for ops/debugging.

- getdbinfo - Implemented (disk usage breakdown + fjall telemetry)
