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
- gettxoutsetinfo - Partial
- verifytxoutproof - Implemented
- getspentinfo - Implemented

## Mining

- getblocksubsidy - Implemented
- getblocktemplate - Partial (template fields + longpoll + proposal; miner address config/capabilities still WIP)
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

- reindex - Missing
- stop - Implemented
- createfluxnodekey - Missing
- listfluxnodeconf - Missing
- getfluxnodeoutputs - Missing
- startfluxnode - Missing
- startdeterministicfluxnode - Missing
- verifychain - Missing
- addnode - Implemented (IP/IP:PORT only; no DNS resolution yet)
- clearbanned - Implemented
- disconnectnode - Implemented (address-based; best-effort)
- getaddednodeinfo - Implemented (simplified fields; `dns` param ignored)
- setban - Implemented (SocketAddr bans; `absolute` supported)

## Wallet

- signrawtransaction - Missing
- addmultisigaddress - Missing
- backupwallet - Missing
- dumpprivkey - Missing
- getbalance - Missing
- getnewaddress - Missing
- getrawchangeaddress - Missing
- getreceivedbyaddress - Missing
- getunconfirmedbalance - Missing
- getwalletinfo - Missing
- importaddress - Missing
- importprivkey - Missing
- importwallet - Missing
- keypoolrefill - Missing
- listaddressgroupings - Missing
- listlockunspent - Missing
- listreceivedbyaddress - Missing
- listsinceblock - Missing
- listtransactions - Missing
- listunspent - Missing
- lockunspent - Missing
- rescanblockchain - Missing

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
