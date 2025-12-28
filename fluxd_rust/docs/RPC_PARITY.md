# RPC parity checklist

This file tracks parity targets with the C++ `fluxd` RPC surface. Statuses:

- Implemented: available and returning structured data.
- Partial: available but fields are placeholders or simplified.
- Stub: method exists but returns "not implemented" or placeholder values.
- Missing: method not registered (returns "method not found").

## General

- help - Implemented
- getinfo - Implemented
- getfluxnodestatus - Stub
- listfluxnodes - Implemented (via deterministic list)
- viewdeterministicfluxnodelist - Implemented
- getfluxnodecount - Implemented
- getdoslist - Stub
- getstartlist - Missing
- fluxnodecurrentwinner - Partial (best-effort selection)

## Chain and block

- getbestblockhash - Implemented
- getblock - Implemented
- getblockchaininfo - Implemented
- getblockcount - Implemented
- getblockdeltas - Stub
- getblockhash - Implemented
- getblockheader - Implemented
- getchaintips - Implemented
- getdifficulty - Implemented

## Mempool and UTXO

- getmempoolinfo - Implemented
- getrawmempool - Implemented
- gettxout - Implemented
- gettxoutproof - Stub
- gettxoutsetinfo - Partial
- verifytxoutproof - Stub
- getspentinfo - Implemented

## Mining

- getblocksubsidy - Implemented
- getblocktemplate - Stub
- getlocalsolps - Stub (returns 0.0)
- getmininginfo - Missing
- getnetworkhashps - Stub (returns 0.0)
- getnetworksolps - Stub (returns 0.0)

## Network

- getconnectioncount - Implemented
- getdeprecationinfo - Missing
- getnettotals - Implemented
- getnetworkinfo - Implemented
- getpeerinfo - Implemented
- listbanned - Implemented

## Raw transactions and scripts

- createrawtransaction - Missing
- decoderawtransaction - Missing
- decodescript - Missing
- getrawtransaction - Implemented (chain + mempool)
- fundrawtransaction - Missing
- sendrawtransaction - Partial (relays; confirmed inputs only)
- createmultisig - Missing
- estimatefee - Missing
- estimatepriority - Missing
- validateaddress - Missing
- verifymessage - Missing

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
- submitblock - Missing
- zcrawjoinsplit - Missing
- zcrawreceive - Missing
- prioritisetransaction - Missing

- reindex - Missing
- stop - Missing
- createfluxnodekey - Missing
- listfluxnodeconf - Missing
- getfluxnodeoutputs - Missing
- startfluxnode - Missing
- startdeterministicfluxnode - Missing
- verifychain - Missing
- addnode - Missing
- clearbanned - Missing
- disconnectnode - Missing
- getaddednodeinfo - Missing
- setban - Missing

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
- restart - Missing
- ping - Missing
- zcbenchmark - Missing
- startbenchmark - Missing
- stopbenchmark - Missing
