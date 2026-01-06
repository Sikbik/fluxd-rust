# RPC parity checklist

This file tracks parity targets with the C++ `fluxd` RPC surface. Statuses:

- Implemented: available and returning structured data.
- Partial: available but fields are placeholders or simplified.
- Stub: method exists but returns "not implemented" or placeholder values.
- Missing: method not registered (returns "method not found").

## General

- help - Implemented
- getinfo - Implemented
- getfluxnodestatus - Implemented (supports optional alias/outpoint lookup; IP fields stored from confirm txs)
- listfluxnodes - Implemented (via deterministic list)
- viewdeterministicfluxnodelist - Implemented
- getfluxnodecount - Implemented
- getdoslist - Implemented
- getstartlist - Implemented
- fluxnodecurrentwinner - Implemented (deterministic next payee)

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
- getlocalsolps - Implemented (reports local POW header validation throughput; returns 0.0 when idle)
- getmininginfo - Implemented (`currentblock*` fields reflect the last connected block; `localsolps` reports local POW header validation throughput)
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
- fundrawtransaction - Partial (wallet funding selects spendable P2PKH and P2SH (multisig) UTXOs; supports `minconf`; preserves existing `scriptSig` sizes for fee estimation; unsigned P2SH inputs require wallet-known redeem scripts; other non-P2PKH inputs must be pre-signed)
- sendrawtransaction - Implemented (supports spending mempool parents; C++-style reject-code formatting for common invalid/mempool-conflict failures; honors `allowhighfees` absurd-fee guard)
- createmultisig - Implemented (accepts Flux addresses or hex pubkeys; wallet lookup works while locked)
- estimatefee - Implemented
- estimatepriority - Partial (mempool-based estimate; returns -1.0 when insufficient samples are available or `nblocks > 25`)
- validateaddress - Implemented (includes `pubkey`/`iscompressed` for wallet-owned P2PKH; includes `account` label for wallet-known scripts; includes redeem-script details for known P2SH/multisig scripts)
- verifymessage - Implemented

## Extra queries

- gettransaction - Implemented (wallet-only view; amount/fee match `fluxd` semantics; includes `generated`/`expiryheight`/`vJoinSplit`; includes wallet tx metadata from send* (`comment`/`to`) like `fluxd` `mapValue`; change outputs are omitted from `details` on outgoing txs; `details` ordering + coinbase categories match `fluxd`; `walletconflicts` is populated from known spend conflicts (chain spent-index + mempool); confirmed `time`/`timereceived` uses wallet first-seen time when available; wallet tx bytes are persisted for wallet-created txs and for txs discovered via `rescanblockchain`, so `gettransaction` still works when a known wallet tx is not in chain and not in mempool (`confirmations=-1`))
- zvalidateaddress - Partial (validates Sprout/Sapling encoding + returns key components; Sapling `ismine` checks wallet spending keys; `iswatchonly` checks imported Sapling viewing keys)
- getbenchmarks - Partial (Fluxnode-only; proxies to `fluxbench-cli getbenchmarks` when fluxbenchd reports `status=online`; otherwise returns `"Benchmark not running"`)
- getbenchstatus - Partial (Fluxnode-only; proxies to `fluxbench-cli getstatus` when fluxbenchd reports `status=online`; otherwise returns `"Benchmark not running"`)
- getblockhashes - Implemented

## Address index (insight)

- getaddresstxids - Implemented
- getaddressbalance - Implemented
- getaddressdeltas - Implemented
- getaddressutxos - Implemented
- getaddressmempool - Implemented

## Node control

- sendfrom - Implemented (fromaccount ignored; minconf supported)
- submitblock - Partial (BIP22 return values; returns `duplicate` when the header is already known; side-chain acceptance is still simplified vs C++)
- zcrawjoinsplit - Implemented (Sprout JoinSplit splice + Groth16 proof; requires shielded params)
- zcrawreceive - Implemented (Sprout note decrypt + witness existence check; requires shielded params)
- zcrawkeygen - Implemented (Sprout key/address generator; deprecated but useful for tooling/regtest)
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
- verifychain - Partial (checks flatfile decode + header linkage + merkle root + txindex; `checklevel=4` verifies spent-index consistency; `checklevel=5` verifies address index consistency; does not re-apply full UTXO/script validation like C++)
- addnode - Implemented (IP/IP:PORT only; no DNS resolution yet)
- clearbanned - Implemented
- disconnectnode - Implemented (address-based; best-effort)
- getaddednodeinfo - Implemented (simplified fields; `dns` param ignored)
- setban - Implemented (SocketAddr bans; `absolute` supported)

## Wallet

- signrawtransaction - Partial (supports P2PKH and P2SH (multisig and P2PKH redeem scripts); uses wallet redeem scripts when available; supports `prevtxs[].redeemScript` and optional WIF override list)
- addmultisigaddress - Partial (adds P2SH redeem script + watch script to the wallet; stores optional `account` label; P2SH outputs are marked spendable when enough keys are present)
- backupwallet - Implemented
- dumpwallet - Implemented (exports transparent keys; includes `label=` with C++-style percent encoding; refuses to overwrite an existing file)
- encryptwallet - Implemented (encrypts wallet private keys in wallet.dat; wallet starts locked)
- walletpassphrase - Implemented (temporarily unlocks an encrypted wallet)
- walletpassphrasechange - Implemented
- walletlock - Implemented
- dumpprivkey - Implemented (P2PKH only)
- getbalance - Partial (minconf supported; `minconf=0` includes spendable mempool outputs; `include_watchonly` supported; account param validated like `fluxd` (`""`/`"*"` only))
- getnewaddress - Implemented (P2PKH only; label ignored; keypool-backed)
- getrawchangeaddress - Partial (P2PKH only; address_type param ignored; reserves change addresses tracked in wallet.dat)
- getreceivedbyaddress - Implemented (wallet addresses only; uses address deltas for confirmed receives, plus mempool outputs when `minconf=0`)
- getunconfirmedbalance - Partial (derived from spendable mempool outputs paying to the wallet)
- getwalletinfo - Implemented (C++ key set + conditional `unlocked_until`; balances derived from the address index; also returns `*_zat` fields for exact amounts)
- importaddress - Implemented (watch-only; rescan ignored due to address index)
- importprivkey - Implemented (rescan param accepted but ignored; address index makes it unnecessary)
- importwallet - Partial (imports WIFs from a wallet dump; also imports `label=` fields)
- keypoolrefill - Implemented (fills persisted keypool; does not create addresses)
- listaddressgroupings - Partial (clusters co-spent inputs + wallet-owned outputs; heuristic is index-driven vs C++ wallet internals)
- listlockunspent - Implemented
- listreceivedbyaddress - Implemented (transparent only; `include_watchonly` supported; `txids` populated; `account`/`label` populated from wallet address labels)
- listsinceblock - Partial (transparent only; confirmed via address deltas; mempool included; wallet store included for wallet-known txs not in chain/mempool (`confirmations=-1`); includes WalletTxToJSON fields like `walletconflicts`/`generated`/`expiryheight`/`vJoinSplit`/`comment`/`to`; `include_watchonly` supported; returns one entry per wallet-relevant output; coinbase categories match `fluxd`)
- listtransactions - Partial (transparent only; confirmed via address deltas; mempool included; wallet store included for wallet-known txs not in chain/mempool (`confirmations=-1`); includes WalletTxToJSON fields like `walletconflicts`/`generated`/`expiryheight`/`vJoinSplit`/`comment`/`to`; `account="*"` returns all and other values filter entries by wallet label/account; `include_watchonly` supported; ordered oldest → newest; returns one entry per wallet-relevant output; coinbase categories match `fluxd`)
- listunspent - Partial (supports minconf/maxconf/address filter; `minconf=0` includes mempool outputs; includes `redeemScript` for known P2SH; excludes locked coins like C++)
- lockunspent - Implemented
- rescanblockchain - Implemented (scans address delta index; populates wallet tx history)

- sendmany - Partial (supports P2PKH + P2SH destinations; `subtractfeefrom` supported)
- sendtoaddress - Partial (supports P2PKH + P2SH destinations; `subtractfeefromamount` supported)
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
- z_exportwallet - Implemented (exports transparent keys + Sapling spending keys; refuses to overwrite an existing file)
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
- zcbenchmark - Partial (supports `sleep` and returns running times; other benchmark types not implemented yet)
- startbenchmark - Partial (alias: `startfluxbenchd`/`startzelbenchd`; starts `fluxbenchd`/`zelbenchd` if present next to `fluxd`)
- stopbenchmark - Partial (alias: `stopfluxbenchd`/`stopzelbenchd`; calls `fluxbench-cli stop` when online)

## fluxd-rust extensions

These methods are not part of the legacy C++ `fluxd` RPC surface, but are useful for ops/debugging.

- getdbinfo - Implemented (disk usage breakdown + fjall telemetry)
