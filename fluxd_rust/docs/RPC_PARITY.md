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
- getblocktemplate - Partial (template fields + longpoll + proposal; falls back to first wallet key if mineraddress is unset; capability negotiation still WIP)
- getlocalsolps - Stub (returns 0.0)
- getmininginfo - Partial (rates and mining fields are placeholders)
- getnetworkhashps - Stub (returns 0.0)
- getnetworksolps - Stub (returns 0.0)

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
- fundrawtransaction - Missing
- sendrawtransaction - Partial (supports spending mempool parents; orphan pool + full policy parity WIP)
- createmultisig - Partial (hex pubkeys only; wallet address lookup not available)
- estimatefee - Implemented
- estimatepriority - Missing
- validateaddress - Implemented
- verifymessage - Implemented

## Extra queries

- gettransaction - Missing
- zvalidateaddress - Missing
- getbenchmarks - Missing
- getbenchstatus - Missing
- getblockhashes - Implemented

## Address index (insight)

- getaddresstxids - Implemented
- getaddressbalance - Implemented
- getaddressdeltas - Implemented
- getaddressutxos - Implemented
- getaddressmempool - Implemented

## Node control

- sendfrom - Missing
- submitblock - Partial (accepts blocks; return codes are simplified)
- zcrawjoinsplit - Missing
- zcrawreceive - Missing
- prioritisetransaction - Missing

- reindex - Partial (requests shutdown + wipes `db/` + `blocks/` on next start; does not rebuild indexes from existing flatfiles)
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

- signrawtransaction - Missing
- addmultisigaddress - Missing
- backupwallet - Missing
- dumpprivkey - Implemented (P2PKH only)
- getbalance - Partial (minconf supported; accounts/watchonly/unconfirmed are placeholders)
- getnewaddress - Implemented (P2PKH only; random keys; label ignored)
- getrawchangeaddress - Missing
- getreceivedbyaddress - Missing
- getunconfirmedbalance - Missing
- getwalletinfo - Partial (balance fields are real; txcount/keypool fields are placeholders)
- importaddress - Missing
- importprivkey - Implemented (rescan param accepted but ignored; address index makes it unnecessary)
- importwallet - Missing
- keypoolrefill - Missing
- listaddressgroupings - Missing
- listlockunspent - Missing
- listreceivedbyaddress - Missing
- listsinceblock - Missing
- listtransactions - Missing
- listunspent - Partial (supports minconf/maxconf/address filter; no locked/watchonly outputs)
- lockunspent - Missing
- rescanblockchain - Stub (wallet rescan not implemented)

- sendmany - Missing
- sendtoaddress - Missing
- settxfee - Missing
- signmessage - Missing

## Shielded

- zexportkey - Missing
- zexportviewingkey - Missing
- zgetbalance - Missing
- zgetmigrationstatus - Missing
- zgetnewaddress - Missing
- zgetoperationresult - Missing
- zgetoperationstatus - Missing
- zgettotalbalance - Missing
- zimportkey - Missing
- zimportviewingkey - Missing
- zimportwallet - Missing
- zlistaddresses - Missing
- zlistoperationids - Missing
- zlistreceivedbyaddress - Missing
- zlistunspent - Missing
- zsendmany - Missing
- zsetmigration - Missing
- zshieldcoinbase - Missing

## Admin and benchmarking

- start - Missing
- restart - Implemented
- ping - Implemented
- zcbenchmark - Missing
- startbenchmark - Missing
- stopbenchmark - Missing

## fluxd-rust extensions

These methods are not part of the legacy C++ `fluxd` RPC surface, but are useful for ops/debugging.

- getdbinfo - Implemented (disk usage breakdown + fjall telemetry)
