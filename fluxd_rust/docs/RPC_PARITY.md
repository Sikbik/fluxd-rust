# RPC parity checklist

This file tracks parity targets with the C++ `fluxd` RPC surface. Statuses:

- Implemented: available and returning structured data.
- Partial: available but fields are placeholders or simplified.
- Stub: method exists but returns "not implemented" or placeholder values.
- Missing: method not registered (returns "method not found").

## General

- help - Implemented
- getinfo - Implemented (includes walletversion/balance/keypool/paytxfee; unlocked_until when encrypted)
- getfluxnodestatus - Implemented (alias: getzelnodestatus; supports optional alias/outpoint lookup; IP fields stored from confirm txs)
- listfluxnodes - Implemented (alias: listzelnodes; via deterministic list)
- viewdeterministicfluxnodelist - Implemented (alias: viewdeterministiczelnodelist)
- getfluxnodecount - Implemented (alias: getzelnodecount)
- getdoslist - Implemented
- getstartlist - Implemented
- fluxnodecurrentwinner - Implemented (alias: zelnodecurrentwinner; deterministic next payee)

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
- getblocktemplate - Implemented (template fields + longpoll + proposal; tx selection uses a C++-style priority window then modified fee-rate; honors `prioritisetransaction` deltas; falls back to `--miner-address` then the wallet if mineraddress is unset)
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
- fundrawtransaction - Partial (wallet funding selects spendable P2PKH and P2SH (multisig) UTXOs; preserves existing `scriptSig` sizes for fee estimation; randomizes change output position by default; supports `options.minconf`, `options.subtractFeeFromOutputs`, `options.changeAddress`, `options.changePosition`, `options.lockUnspents`, `options.includeWatching`; `changePosition` is not allowed with `subtractFeeFromOutputs` unless it keeps change at the final index; fee selection matches `fluxd` wallet: uses `paytxfee` when set, otherwise uses the fee estimator confirm target (`txconfirmtarget`, default 2; with a hard-coded fallback); clamps to a max fee; unsigned P2SH inputs require wallet-known redeem scripts; other non-P2PKH inputs must be pre-signed)
- sendrawtransaction - Implemented (supports spending mempool parents; C++-style reject-code formatting for common invalid/mempool-conflict failures; honors `allowhighfees` absurd-fee guard)
- createmultisig - Implemented (accepts Flux addresses or hex pubkeys; wallet lookup works while locked)
- estimatefee - Implemented
- estimatepriority - Implemented
- validateaddress - Implemented (includes `pubkey`/`iscompressed` for wallet-owned P2PKH; includes `account` label for wallet-known scripts; includes redeem-script details for known P2SH/multisig scripts)
- verifymessage - Implemented

## Extra queries

- gettransaction - Implemented (wallet-only view; amount/fee match `fluxd` semantics; includes `generated`/`expiryheight`/`vJoinSplit`; includes wallet tx metadata from send* (`comment`/`to`) like `fluxd` `mapValue`; change outputs are omitted from `details` on outgoing txs; `details` ordering + coinbase categories match `fluxd`; `walletconflicts` is populated from known spend conflicts (chain spent-index + mempool); confirmed `time`/`timereceived` uses wallet first-seen time when available; wallet tx bytes are persisted for wallet-created txs and for txs discovered via `rescanblockchain`, so `gettransaction` still works when a known wallet tx is not in chain and not in mempool (`confirmations=-1`))
- zvalidateaddress - Partial (validates Sprout/Sapling encoding + returns key components; Sapling `ismine` checks wallet spending keys; `iswatchonly` checks imported Sapling viewing keys)
- getbenchmarks - Implemented (Fluxnode-only; proxies to `fluxbench-cli getbenchmarks` when fluxbenchd reports `status=online`; otherwise returns `"Benchmark not running"`)
- getbenchstatus - Implemented (Fluxnode-only; proxies to `fluxbench-cli getstatus` when fluxbenchd reports `status=online`; otherwise returns `"Benchmark not running"`)
- getblockhashes - Implemented

## Address index (insight)

- getaddresstxids - Implemented
- getaddressbalance - Implemented
- getaddressdeltas - Implemented
- getaddressutxos - Implemented
- getaddressmempool - Implemented

## Node control

- sendfrom - Implemented (fromaccount treated as optional wallet address filter; when non-empty, restricts funding to that address and sends change back to it; minconf supported)
- submitblock - Implemented (BIP22-ish return values including `"duplicate-invalid"` and `"duplicate-inconclusive"`; accepts side-chain/stale blocks by validating + storing them as unconnected block bytes; triggers chain selection by disconnecting to the best-header common ancestor and connecting any now-available unconnected blocks along the best-header chain)
- zcrawjoinsplit - Implemented (Sprout JoinSplit splice + Groth16 proof; requires shielded params)
- zcrawreceive - Implemented (Sprout note decrypt + witness existence check; requires shielded params)
- zcrawkeygen - Implemented (Sprout key/address generator; deprecated but useful for tooling/regtest)
- prioritisetransaction - Implemented (stores fee/priority deltas for mining selection; affects `getblocktemplate` ordering/skip policy)

- reindex - Implemented (requests shutdown; on next start wipes `db/` and rebuilds indexes from existing flatfiles under `blocks/`; use `--resync` to wipe blocks too)
- stop - Implemented
- createfluxnodekey - Implemented (alias: createzelnodekey)
- createzelnodekey - Implemented (alias of createfluxnodekey)
- createdelegatekeypair - Implemented (delegate private key + pubkey pair; matches C++ schema)
- createp2shstarttx - Implemented (builds unsigned P2SH fluxnode start tx; validates redeem-script hash vs collateral UTXO; supports delegate pubkeys with the post-PoN feature bit)
- signp2shstarttx - Implemented (signs P2SH fluxnode start tx; supports optional WIF override; otherwise uses wallet key for one of the redeemscript pubkeys and requires an unlocked wallet)
- sendp2shstarttx - Implemented (broadcast wrapper around `sendrawtransaction`)
- startfluxnodewithdelegates - Implemented (builds + broadcasts a deterministic START tx with a delegates UPDATE payload; requires PoN activation)
- startfluxnodeasdelegate - Implemented (builds + broadcasts a delegate-signed deterministic START tx for P2PKH collateral; verifies delegate key is authorized via stored delegates UPDATE on the collateral outpoint)
- startp2shasdelegate - Implemented (builds + broadcasts a delegate-signed deterministic START tx for P2SH collateral; verifies redeemscript hash and delegate authorization via stored delegates UPDATE)
- listfluxnodeconf - Implemented (alias: listzelnodeconf)
- listzelnodeconf - Implemented (alias of listfluxnodeconf)
- getfluxnodeoutputs - Implemented (wallet-less; uses fluxnode.conf + UTXO lookups)
- startfluxnode - Partial (uses wallet collateral key when available; supports wallet-less starts via optional `collateral_privkey_wif` + `redeem_script_hex` columns in `fluxnode.conf`; honors `lockwallet` for encrypted wallets; includes C++-style `transaction_*` detail fields + `reason`/`errorMessage`, plus `txid` on success)
- startdeterministicfluxnode - Partial (uses wallet collateral key when available; supports wallet-less starts via `collateral_privkey_wif` param or `fluxnode.conf` extra columns; honors `lockwallet`; includes C++-style `transaction_*` detail fields + `errorMessage`, plus `txid` on success; still simplified vs C++ behavior)
- verifychain - Partial (checks flatfile decode + header linkage + merkle root + txindex; `checklevel=4` verifies spent-index consistency; `checklevel=5` verifies address index consistency; does not re-apply full UTXO/script validation like C++)
- addnode - Implemented (accepts IPs and hostnames; best-effort DNS resolution used to seed the address book; stores the raw node string in the added-node list like C++)
- clearbanned - Implemented
- disconnectnode - Implemented (address-based; errors if the peer is not connected, like C++)
- getaddednodeinfo - Implemented (honors `dns`; C++-style `connected` + `addresses[]` with `"inbound"|"outbound"|"false"` per-address statuses; `dns` remains optional here for convenience)
- setban - Implemented (SocketAddr bans; `absolute` supported)

## Wallet

- signrawtransaction - Implemented (supports P2PKH and P2SH (multisig and P2PKH redeem scripts); supports optional `prevtxs[].redeemScript`, optional `branchid`, and optional WIF override list; wallet fallback still applies when `privkeys` is provided)
- addmultisigaddress - Implemented (adds P2SH redeem script + watch script to the wallet; `account` must be empty string; P2SH outputs are marked spendable when enough keys are present)
- backupwallet - Implemented
- dumpwallet - Implemented (exports transparent keys; includes `label=` with C++-style percent encoding; refuses to overwrite an existing file)
- encryptwallet - Implemented (encrypts wallet private keys in wallet.dat; wallet starts locked)
- walletpassphrase - Implemented (temporarily unlocks an encrypted wallet)
- walletpassphrasechange - Implemented
- walletlock - Implemented
- dumpprivkey - Implemented (P2PKH only)
- getbalance - Partial (minconf supported; `minconf=0` includes spendable mempool outputs; `include_watchonly` supported; account param validated like `fluxd` (`""`/`"*"` only))
- getnewaddress - Implemented (P2PKH only; label ignored; keypool-backed)
- getrawchangeaddress - Implemented (returns a new wallet-owned P2PKH change address; optional arg ignored for `fluxd` compatibility)
- getreceivedbyaddress - Implemented (wallet addresses only; uses address deltas for confirmed receives, plus mempool outputs when `minconf=0`)
- getunconfirmedbalance - Implemented (derived from spendable mempool outputs paying to the wallet)
- getwalletinfo - Implemented (C++ key set + conditional `unlocked_until`; balances derived from the address index; also returns `*_zat` fields for exact amounts)
- importaddress - Implemented (watch-only; `rescan=true` triggers `rescanblockchain` to populate wallet tx history)
- importprivkey - Implemented (`rescan=true` triggers `rescanblockchain` to populate wallet tx history)
- importwallet - Implemented (imports WIFs from a wallet dump; also imports `label=` fields; triggers `rescanblockchain`)
- keypoolrefill - Implemented (fills persisted keypool; does not create addresses)
- listaddressgroupings - Implemented (clusters co-spent inputs + wallet-owned outputs; index-driven heuristic; includes wallet label in the third tuple field)
- listlockunspent - Implemented
- listreceivedbyaddress - Implemented (transparent only; `include_watchonly` supported; `txids` populated; `account`/`label` populated from wallet address labels)
- listsinceblock - Partial (transparent only; confirmed via address deltas; mempool included; wallet store included for wallet-known txs not in chain/mempool (`confirmations=-1`); includes WalletTxToJSON fields like `walletconflicts`/`generated`/`expiryheight`/`vJoinSplit`/`comment`/`to`; `include_watchonly` supported; `blockhash` parsing matches `fluxd` (`SetHex`-style leniency: invalid/unknown treated as omitted, trailing junk ignored); returns one entry per wallet-relevant output; coinbase categories match `fluxd`)
- listtransactions - Partial (transparent only; confirmed via address deltas; mempool included; wallet store included for wallet-known txs not in chain/mempool (`confirmations=-1`); includes WalletTxToJSON fields like `walletconflicts`/`generated`/`expiryheight`/`vJoinSplit`/`comment`/`to`; `account="*"` returns all and other values filter entries by wallet label/account; `include_watchonly` supported; `count`/`from` slicing matches `fluxd` (including negative parameter errors); ordered oldest → newest; returns one entry per wallet-relevant output; coinbase categories match `fluxd`)
- listunspent - Implemented (supports minconf/maxconf/address filter; rejects duplicated address filters; `minconf=0` includes mempool outputs; includes `redeemScript` for known P2SH; excludes locked coins like C++; includes `account` label when available)
- lockunspent - Implemented
- rescanblockchain - Implemented (scans address delta index; populates wallet tx history)

- sendmany - Implemented (supports P2PKH + P2SH destinations; optional fromaccount wallet address filter + change destination; `subtractfeefrom` supported; ignores unknown `subtractfeefrom` entries like `fluxd`)
- sendtoaddress - Implemented (supports P2PKH + P2SH destinations; `subtractfeefromamount` supported; dust checks when standardness is enabled)
- settxfee - Implemented (persists wallet fee-rate override in `wallet.dat`; used by fundrawtransaction/send*)
- signmessage - Implemented (P2PKH only; compatible with verifymessage)

## Shielded

- zexportkey - Partial (Sapling only; exports Sapling extended spending key)
- zexportviewingkey - Partial (Sapling only; exports Sapling full viewing key)
- zgetbalance - Partial (taddrs supported via wallet UTXO scan; zaddrs Sapling only; excludes notes spent by chain/mempool nullifiers; includes watch-only by default like C++ (override via `includeWatchonly`))
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
- zlistreceivedbyaddress - Partial (Sapling only; lists received Sapling notes for a zaddr; watch-only is allowed by default like C++; memo is always hex; `change` is reported for spendable notes only like C++)
- zlistunspent - Partial (Sapling only; lists unspent Sapling notes; excludes notes spent by mempool nullifiers; memo is always hex; `change` is reported for spendable notes only like C++)
- zsendmany - Implemented (Sapling only; async op; tx construction + mempool submission; uses cached Sapling note rseed when available; has RPC smoke + ignored end-to-end spend harness)
- zsetmigration - Implemented (deprecated on Flux fork; returns misc error)
- zshieldcoinbase - Implemented (deprecated on Flux fork; returns misc error)

## Admin and benchmarking

- start - Stub (no-op; returns `"fluxd already running"`)
- restart - Implemented
- ping - Implemented
- zcbenchmark - Partial (supports `sleep` and returns running times; other benchmark types not implemented yet)
- startbenchmark - Implemented (alias: `startfluxbenchd`/`startzelbenchd`; starts `fluxbenchd`/`zelbenchd` if present next to `fluxd`)
- stopbenchmark - Implemented (alias: `stopfluxbenchd`/`stopzelbenchd`; calls `fluxbench-cli stop` when online)

## fluxd-rust extensions

These methods are not part of the legacy C++ `fluxd` RPC surface, but are useful for ops/debugging.

- getdbinfo - Implemented (disk usage breakdown + fjall telemetry)
