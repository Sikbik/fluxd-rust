# RPC reference

The fluxd-rust daemon (binary `fluxd`) exposes both JSON-RPC and REST-like
`/daemon` endpoints over HTTP with Basic Auth.

## Authentication

RPC uses HTTP Basic Auth.

- If `--rpc-user` and `--rpc-pass` are provided, those credentials are required.
- Otherwise the daemon creates a cookie at `--data-dir/rpc.cookie` with the form `__cookie__:password`.

Example:

```bash
curl -u "$(cat ./data/rpc.cookie)" http://127.0.0.1:16124/daemon/getinfo
```

## JSON-RPC

- Endpoint: `POST /`
- Body: JSON object with `method` and `params` (array).
- Batch requests are not supported.

Example:

```bash
curl -u "$(cat ./data/rpc.cookie)" \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"1.0","id":"curl","method":"getblockcount","params":[]}' \
  http://127.0.0.1:16124/
```

## /daemon endpoints

- Endpoint: `GET /daemon/<method>` or `POST /daemon/<method>`.
- Parameters are parsed from the query or body.

Rules:
- If query contains `params=...`, it is parsed as JSON. Use this for arrays and objects.
- Otherwise, each `key=value` becomes a positional parameter, in order of appearance. The key name is ignored.
- POST body can be a JSON array, an object with `params`, or a single object (treated as one parameter).

Examples:

```bash
# Positional query params
curl -u "$(cat ./data/rpc.cookie)" \
  "http://127.0.0.1:16124/daemon/getblockhash?height=1000"

# Explicit params array
curl -g -u "$(cat ./data/rpc.cookie)" \
  "http://127.0.0.1:16124/daemon/getblockhashes?params=[1700000000,1699990000,{\"noOrphans\":true}]"
```

Type notes:
- Some methods require strict booleans (for example `getblockheader` verbose). Use `true`/`false`, not `1`/`0`.
- `verbosity` for `getblock` must be numeric (0, 1, or 2).
- If you pass `params=[...]` in the URL, use `curl -g` (`--globoff`) so `[`/`]` are not treated as URL globs.

## Error codes

- `-32600` invalid request
- `-32601` method not found
- `-32603` internal error
- `-32700` parse error
- `-8` invalid parameter
- `-4` wallet error
- `-5` invalid address or key

## Supported methods

### General

- `help [method]`
- `getinfo`
- `ping`
- `start` (stub; no-op)
- `stop`
- `restart`
- `reindex`
- `rescanblockchain [start_height] [stop_height]` (populates wallet tx history via the address delta index)
- `getdbinfo`
- `getnetworkinfo`
- `getpeerinfo`
- `getnettotals`
- `getconnectioncount`
- `listbanned`
- `clearbanned`
- `setban <ip|ip:port> <add|remove> [bantime] [absolute]`
- `addnode <node> <add|remove|onetry>`
- `getaddednodeinfo [dns] [node]`
- `disconnectnode <node>`
- `getdeprecationinfo`

### Chain and blocks

- `getblockcount`
- `getbestblockhash`
- `getblockhash <height>`
- `getblockheader <hash> [verbose]`
- `getblock <hash|height> [verbosity]`
- `getblockchaininfo`
- `getdifficulty`
- `getchaintips`
- `getblocksubsidy [height]`
- `getblockhashes <high> <low> [options]`
- `verifychain [checklevel] [numblocks]`

### Transactions and UTXO

- `createrawtransaction <transactions> <addresses> [locktime] [expiryheight]`
- `decoderawtransaction <hexstring>`
- `decodescript <hex>`
- `createmultisig <nrequired> <keys>`
- `getrawtransaction <txid> [verbose]`
- `fundrawtransaction <hexstring> [options]` (partial)
- `signrawtransaction <hexstring> [prevtxs] [privkeys] [sighashtype]` (partial)
- `sendrawtransaction <hexstring> [allowhighfees]`
- `gettxout <txid> <vout> [include_mempool]`
- `gettxoutsetinfo`
- `validateaddress <fluxaddress>`
- `zvalidateaddress <zaddr>` (partial; validates Sprout/Sapling encoding; does not check ownership)
- `verifymessage <fluxaddress> <signature> <message>`

### Wallet (transparent)

Wallet state is stored at `--data-dir/wallet.dat`.

- `getwalletinfo` (partial)
- `gettransaction <txid> [include_watchonly]` (partial)
- `listtransactions [account] [count] [skip] [include_watchonly]` (partial)
- `listsinceblock [blockhash] [target_confirmations] [include_watchonly]` (partial)
- `addmultisigaddress <nrequired> <keys> [account]` (partial; adds a watch-only P2SH script)
- `listreceivedbyaddress [minconf] [include_empty] [include_watchonly] [address_filter]` (partial)
- `keypoolrefill [newsize]` (partial)
- `settxfee <amount>` (partial)
- `getnewaddress [label]` (label ignored)
- `getrawchangeaddress [address_type]` (partial; address_type ignored)
- `importprivkey <wif> [label] [rescan]` (label accepted; rescan ignored)
- `dumpprivkey <address>`
- `backupwallet <destination>`
- `signmessage <address> <message>`
- `getbalance [account] [minconf] [include_watchonly]` (partial)
- `getunconfirmedbalance`
- `getreceivedbyaddress <address> [minconf]` (partial)
- `listunspent [minconf] [maxconf] [addresses]` (partial)
- `sendtoaddress <address> <amount> [comment] [comment_to] [subtractfeefromamount] ...` (partial)
- `sendmany <fromaccount> <amounts> [minconf] [comment] [subtractfeefrom]` (partial)

### Wallet (shielded) (WIP)

Shielded wallet RPCs are registered for parity, but currently return a wallet error (`-4`)
because shielded wallet support is not implemented yet.

Consensus note (mainnet): after the Flux rebrand upgrade, transactions with transparent inputs and
Sapling outputs / JoinSplits are rejected (t→z shielding disabled). Existing shielded funds can
still be spent out of the pool (z→t) and moved within the pool (z→z).

- `zgetbalance` / `z_getbalance`
- `zgettotalbalance` / `z_gettotalbalance`
- `zgetnewaddress` / `z_getnewaddress`
- `zlistaddresses` / `z_listaddresses`
- `zlistunspent` / `z_listunspent`
- `zsendmany` / `z_sendmany`
- `zshieldcoinbase` / `z_shieldcoinbase`
- `zexportkey` / `z_exportkey`
- `zexportviewingkey` / `z_exportviewingkey`
- `zimportkey` / `z_importkey`
- `zimportviewingkey` / `z_importviewingkey`
- `zimportwallet` / `z_importwallet`
- `zgetoperationstatus` / `z_getoperationstatus`
- `zgetoperationresult` / `z_getoperationresult`
- `zlistoperationids` / `z_listoperationids`
- `zgetmigrationstatus` / `z_getmigrationstatus`
- `zsetmigration` / `z_setmigration`
- `zlistreceivedbyaddress` / `z_listreceivedbyaddress`

Joinsplit helper RPCs are also stubbed:
- `zcrawjoinsplit` (returns error)
- `zcrawreceive` (returns error)

### Mining and mempool

- `getmempoolinfo`
- `getrawmempool [verbose]`
- `getmininginfo` (partial; rate fields return 0.0)
- `getblocktemplate` (partial; includes deterministic fluxnode payouts + basic mempool tx selection)
- `submitblock <hexdata>` (partial)
- `getnetworkhashps` (returns 0.0)
- `getnetworksolps` (returns 0.0)
- `getlocalsolps` (returns 0.0)
- `prioritisetransaction <txid> <priority_delta> <fee_delta_sat>` (mining selection hint)
- `estimatefee <nblocks>`
- `estimatepriority <nblocks>` (returns -1.0; priority estimator not implemented)

### Fluxnode

- `createfluxnodekey` / `createzelnodekey`
- `listfluxnodeconf [filter]` / `listzelnodeconf [filter]`
- `getfluxnodeoutputs` / `getzelnodeoutputs`
- `startfluxnode <all|alias> <lockwallet> [alias]` / `startzelnode ...` (partial; wallet-less)
- `startdeterministicfluxnode <alias> <lockwallet> [collateral_privkey_wif] [redeem_script_hex]` / `startdeterministiczelnode ...` (partial; wallet-less)
- `getfluxnodecount`
- `listfluxnodes`
- `viewdeterministicfluxnodelist [filter]`
- `fluxnodecurrentwinner`
- `getfluxnodestatus [alias|txid:vout]` (partial; uses `--data-dir/fluxnode.conf` when called with no params)
- `getdoslist`
- `getstartlist`
- `getbenchmarks` (stub; Fluxnode-only)
- `getbenchstatus` (stub; Fluxnode-only)
- `startbenchmark` / `startfluxbenchd` / `startzelbenchd` (stub; Fluxnode-only)
- `stopbenchmark` / `stopfluxbenchd` / `stopzelbenchd` (stub; Fluxnode-only)
- `zcbenchmark` (stub; returns error)

### Indexer endpoints (insight-style)

- `getblockdeltas`
- `getspentinfo`
- `getaddressutxos`
- `getaddressbalance`
- `getaddressdeltas`
- `getaddresstxids`
- `getaddressmempool`
- `gettxoutproof ["txid", ...] (blockhash)`
- `verifytxoutproof <proof>`

## Method details

### help

- Params: optional `method` string.
- Result: list of supported methods or confirmation string.

### getinfo

Returns a summary similar to the C++ daemon:

Fields:
- `version` - numeric version from crate version.
- `protocolversion`
- `blocks` - best block height.
- `timeoffset` - currently 0.
- `connections`
- `proxy` - empty string.
- `difficulty`
- `testnet` - boolean.
- `relayfee` - min relay fee-rate in FLUX/kB (from `--minrelaytxfee`).
- `errors` - empty string.

### ping

- Result: `null`

### start

Not a standard `fluxd` RPC. Provided for compatibility with existing Flux infra.

- Params: none
- Result: string (`"fluxd already running"`)

### stop

- Result: string (`"fluxd stopping"`)

### restart

- Result: string (`"fluxd restarting ..."`).
- Note: this requests process exit; actual restart depends on your supervisor (systemd, docker, etc).

### reindex

- Result: string (`"fluxd reindex requested ..."`).
- Note: writes `--data-dir/reindex.flag` and requests process exit; on next start the daemon wipes `db/` and `blocks/` and re-syncs from genesis.

### rescanblockchain

- Params: optional `start_height`, `stop_height`.
- Result: object `{ "start_height": number, "stop_height": number }`.

Notes:
- This scans the address delta index for wallet scripts and stores discovered txids in `wallet.dat` (used by `getwalletinfo.txcount`).

### getwalletinfo

- Result: basic wallet summary (balances are computed from the address index; keypool fields reflect the persisted keypool).

Notes:
- `unconfirmed_balance` is derived from spendable mempool outputs paying to the wallet.
- `txcount` is backed by persisted wallet txids (populated by `rescanblockchain` and wallet send RPCs).

### getnewaddress

- Result: new transparent P2PKH address (persisted to `wallet.dat`).

### getrawchangeaddress

- Params: optional `address_type` (string; currently ignored).
- Result: new transparent P2PKH address (persisted to `wallet.dat`).

### importprivkey

- Params: `<wif> [label] [rescan]` (`label` accepted; `rescan` ignored).
- Result: `null`

### dumpprivkey

- Params: `<address>` (P2PKH).
- Result: WIF private key if present; error `-4` if the address is not in the wallet.

### backupwallet

- Params: `<destination>` (string path).
- Result: `null`

Notes:
- Writes a copy of `wallet.dat` to the destination path.

### signmessage

- Params: `<address> <message>` (P2PKH).
- Result: base64 signature string (compatible with `verifymessage`).

### getbalance

- Params: `[account] [minconf] [include_watchonly]` (partial; `minconf` is enforced).
- Result: wallet balance (mature + not spent by mempool).

Notes:
- If `minconf=0`, includes spendable mempool outputs paying to the wallet.

### getunconfirmedbalance

- Result: sum of spendable mempool outputs paying to the wallet.

### getreceivedbyaddress

- Params: `<address> [minconf]` (partial; P2PKH wallet addresses only).
- Result: total amount received by the address with at least `minconf` confirmations.

Notes:
- If `minconf=0`, includes mempool outputs paying to the address.

### gettransaction

- Params: `<txid> [include_watchonly]` (partial; `include_watchonly=true` includes watch-only scripts imported via `importaddress`).
- Result: best-effort wallet view of a transaction (confirmed txs via address deltas + tx index; mempool txs via script matching).

Notes:
- `involvesWatchonly` is set when the transaction touches watch-only scripts.

### listtransactions

- Params: `[account] [count] [skip] [include_watchonly]` (partial; `account` is ignored, `include_watchonly` is honored).
- Result: array of wallet transactions (ordered oldest → newest; mempool entries appear last).

Notes:
- `involvesWatchonly` is set when the transaction touches watch-only scripts.

### listsinceblock

- Params: `[blockhash] [target_confirmations] [include_watchonly]` (partial; `include_watchonly` is honored).
- Result: object `{ "transactions": array, "lastblock": string }`.

Notes:
- If `blockhash` is omitted (or unknown), returns all wallet transactions.
- `lastblock` is the best block at depth `target_confirmations` (1 = chain tip).

### addmultisigaddress

- Params: `<nrequired> <keys> [account]` (partial; `account` must be `""` if provided).
- Result: string (P2SH address).

Notes:
- `keys` entries may be hex public keys or P2PKH wallet addresses (the address must be present in the wallet).
- Adds the resulting P2SH `scriptPubKey` to the wallet as watch-only (spending multisig outputs is not yet supported).

### listreceivedbyaddress

- Params: `[minconf] [include_empty] [include_watchonly] [address_filter]` (partial; `include_watchonly` is honored; labels are WIP).
- Result: array of wallet addresses with received totals.

Notes:
- `txids` is derived from the address delta index (and includes mempool txids when `minconf=0`).

### keypoolrefill

- Params: `[newsize]` (partial).
- Result: `null`

Notes:
- Fills the wallet keypool to at least `newsize` keys (persisted in `wallet.dat`).
- Does not create new addresses; addresses are reserved from the keypool by `getnewaddress` / `getrawchangeaddress`.

### settxfee

- Params: `<amount>` (fee rate in FLUX/kB).
- Result: boolean.

### listunspent

- Params: `[minconf] [maxconf] [addresses]` (partial).
- Result: array of unspent outputs owned by the wallet.

Notes:
- If `minconf=0`, includes spendable mempool outputs paying to the wallet (with `confirmations=0`).

### sendtoaddress

- Params: `<address> <amount> [comment] [comment_to] [subtractfeefromamount] ...` (partial)
- Result: transaction id hex string.

Notes:
- Builds a transparent transaction, funds it from wallet UTXOs, signs it, and submits it to the local mempool.
- Currently only supports P2PKH wallet UTXOs and does not support `subtractfeefromamount`.

### sendmany

- Params: `<fromaccount> <amounts> [minconf] [comment] [subtractfeefrom]` (partial)
  - `fromaccount` is accepted for legacy compatibility but ignored.
  - `amounts` is a JSON object mapping `"taddr": amount`.
- Result: transaction id hex string.

Notes:
- Builds a transparent transaction with multiple outputs, funds it from wallet UTXOs, signs it, and submits it to the local mempool.
- Currently only supports P2PKH wallet UTXOs and does not support `subtractfeefromamount`.

### getdbinfo

- Result: object containing a disk usage breakdown (`db/`, `blocks/`, per-partition sizes), flatfile meta vs filesystem cross-check, and (when using Fjall) current Fjall telemetry.

### getdeprecationinfo

- Result: object with `deprecated`, `version`, `subversion`, and `warnings`.

### getblockcount

- Result: best block height as integer.

### getbestblockhash

- Result: hex hash of the best block.

### getblockhash

- Params: `height` (number).
- Result: block hash at height on the best chain.

### getblockheader

- Params:
  - `hash` (hex string)
  - `verbose` (boolean, default true)
- Result:
  - If `verbose=false`, hex-encoded header bytes.
  - If `verbose=true`, object with:
    - `hash`, `confirmations`, `height`, `version`
    - `merkleroot`, `finalsaplingroot`
    - `time`, `bits` (hex string), `difficulty`, `chainwork`
    - `type` ("POW" or "PON")
    - PoW: `nonce`, `solution`
    - PoN: `collateral`, `blocksig`
    - `previousblockhash`, `nextblockhash` (if known)

### getblock

- Params:
  - `hash` (hex) or `height` (number)
  - `verbosity` (0, 1, or 2; default 1)
- Result:
  - `verbosity=0`: hex-encoded block bytes.
  - `verbosity=1`: block object with `tx` as array of txids.
  - `verbosity=2`: block object with `tx` as full transaction objects.

Block fields include `hash`, `confirmations`, `size`, `height`, `version`, `merkleroot`,
`finalsaplingroot`, `time`, `bits`, `difficulty`, `chainwork`, and type-specific
PoW/PoN fields as in `getblockheader`.

### getblockchaininfo

Returns chain metadata:

- `chain` - network name.
- `blocks` - best block height.
- `headers` - best header height.
- `bestblockhash`
- `difficulty`
- `verificationprogress` - block height / header height.
- `chainwork`
- `pruned` - always false.
- `size_on_disk` - total size of `--data-dir`.
- `commitments` - current number of Sprout note commitments in the commitment tree.
- `softforks` - BIP34/66/65 version-majority status objects (enforce/reject windows).
- `valuePools` - Sprout/Sapling value pool totals (with `chainValue` and `chainValueZat`).
- `total_supply` / `total_supply_zat` - transparent UTXOs + shielded value pools.
- `upgrades` - network upgrades with activation heights and status.
- `consensus` - current and next branch ids.

### getdifficulty

- Result: floating point network difficulty.

### getchaintips

- Result: array of tip objects with `height`, `hash`, `branchlen`, and `status`.
- `status` is one of `active`, `valid-fork`, or `headers-only`.

### getblocksubsidy

- Params: optional `height`.
- Result: `{ "miner": <amount> }` based on consensus rules.

### getblockhashes

- Params:
  - `high` (number) - exclusive upper bound timestamp.
  - `low` (number) - inclusive lower bound timestamp.
  - optional `options` object:
    - `noOrphans` (boolean) - include only main chain blocks.
    - `logicalTimes` (boolean) - return logical timestamps.
- Result:
  - If `logicalTimes=false`, array of block hashes.
  - If `logicalTimes=true`, array of objects `{ blockhash, logicalts }`.

Notes:
- Logical timestamps are monotonic; they may be greater than the block header time
  when multiple blocks share the same second.
- Timestamp index entries are created on block connect; a fresh sync is required
  to populate older data.

### createrawtransaction

- Params:
  - `transactions` (array) - `[{"txid":"...","vout":n,"sequence":n?}, ...]`
  - `addresses` (object) - `{"taddr": amount, ...}`
  - optional `locktime` (number, default 0)
  - optional `expiryheight` (number, Sapling-era only)
- Result: hex-encoded raw transaction bytes.

Notes:
- The transaction is unsigned; inputs have empty `scriptSig`.
- If Sapling is active for the next block, `expiryheight` defaults to `next_height + 20`.
- If `locktime != 0`, input sequences default to `u32::MAX - 1` (unless overridden).

### decoderawtransaction

- Params: `hexstring` (string)
- Result: decoded transaction object (same shape as `getrawtransaction` verbose output, without block metadata).

### decodescript

- Params: `hex` (string)
- Result: decoded script object with `asm`, `hex`, `type`, optional `reqSigs`/`addresses`, and `p2sh`.

### validateaddress

- Params: `fluxaddress` (string)
- Result:
  - If invalid: `{ "isvalid": false }`
  - If valid: `{ "isvalid": true, "address": "...", "scriptPubKey": "..." }`

### zvalidateaddress

- Params: `zaddr` (string)
- Result:
  - If invalid: `{ "isvalid": false }`
  - If valid Sprout: `{ "isvalid": true, "address": "...", "type": "sprout", "ismine": false, "payingkey": "<hex>", "transmissionkey": "<hex>" }`
  - If valid Sapling: `{ "isvalid": true, "address": "...", "type": "sapling", "ismine": false, "diversifier": "<hex>", "diversifiedtransmissionkey": "<hex>" }`

Notes:
- Uses Flux network HRPs (mainnet `za`, testnet `ztestacadia`, regtest `zregtestsapling`).
- `ismine` is always false until shielded wallet support exists.

### createmultisig

- Params:
  - `nrequired` (number) - required signatures
  - `keys` (array) - hex-encoded public keys
- Result: `{ "address": "<p2sh>", "redeemScript": "<hex>" }`

Notes:
- Wallet-backed address lookup is not available yet, so keys must be hex public keys (not t-addrs).

### verifymessage

- Params: `<fluxaddress> <signature> <message>`
  - `signature` is base64-encoded (as produced by `signmessage` in the C++ daemon).
- Result: boolean.

Notes:
- Only supports P2PKH (key) addresses; P2SH addresses return an error (matches C++).

### getrawtransaction

- Params:
  - `txid` (hex string)
  - `verbose` (boolean or numeric; default false)
- Result:
  - If `verbose=false`, hex-encoded transaction bytes.
  - If `verbose=true`, transaction object with:
    - `txid`, `version`, `size`, `overwintered`, `locktime`
    - optional `versiongroupid`, `expiryheight`
    - `vin` and `vout` with decoded script fields
    - `hex` - raw transaction bytes
    - `blockhash`, `confirmations`, `time`, `blocktime`, `height` if known

Notes:
- Mempool lookup is supported; confirmed transactions include `blockhash`/`confirmations` fields.

### fundrawtransaction

- Params:
  - `hexstring` (string)
  - optional `options` object (partial; supports `minconf`)
- Result: `{ "hex": "<funded_tx_hex>", "fee": <amount>, "changepos": <n> }`

Notes:
- Selects spendable wallet UTXOs via the address index and adds inputs + a change output when needed.
- Only supports funding with P2PKH wallet UTXOs.

### signrawtransaction

- Params:
  - `hexstring` (string)
  - optional `prevtxs` (array) - `[{"txid":"...","vout":n,"scriptPubKey":"...","amount":<amount>}, ...]`
  - optional `privkeys` (array) - `["<wif>", ...]`
  - optional `sighashtype` (string, default `ALL`)
- Result: `{ "hex": "<signed_tx_hex>", "complete": <bool>, "errors": [...]? }`

Notes:
- Only supports signing P2PKH inputs.
- Uses wallet keys by default; `privkeys` can be provided to sign without importing into the wallet.

### sendrawtransaction

- Params:
  - `hexstring` (string)
  - `allowhighfees` (boolean, optional; currently ignored)
- Result: transaction id hex string.

Notes:
- Inserts into the local in-memory mempool.
- If `--tx-peers > 0`, announces the txid to relay peers via P2P (`inv` + `getdata`/`tx`).
- Supports spending mempool parents (parents must already be present in the local mempool).

### gettxout

- Params:
  - `txid` (hex string)
  - `vout` (number)
  - `include_mempool` (boolean or numeric, default true)
- Result:
  - `null` if the output is spent.
  - Otherwise: `bestblock`, `confirmations`, `value`, `scriptPubKey`, `version`, `coinbase`.

Notes:
- If `include_mempool=true`, returns `null` when the output is spent by a mempool transaction.
- If `include_mempool=true`, can also return outputs created by a mempool transaction (`confirmations=0`, `coinbase=false`).

### gettxoutsetinfo

Returns a summary of the current transparent UTXO set.

Fields:
- `height`, `bestblock`
- `transactions` - number of transactions with at least one unspent output
- `txouts` - number of unspent outputs
- `bytes_serialized` - serialized size of the canonical UTXO stream (see notes)
- `hash_serialized` - serialized UTXO set hash (fluxd parity; see notes)
- `total_amount` - sum of all unspent output values (transparent only)
- `sprout_pool`, `sapling_pool` - shielded pool totals
- `shielded_amount` - `sprout_pool + sapling_pool`
- `total_supply` - `total_amount + shielded_amount`
- `disk_size` - byte size of the `db/` directory

Notes:
- This call scans the full UTXO set and may take time (like the C++ daemon).
- `bytes_serialized` is computed from the canonical UTXO stream and may differ from the
  legacy C++ `coins` LevelDB value sizes.
- UTXO stats are maintained incrementally in the chainstate `Meta` column under `utxo_stats_v1`.
- Shielded value pools are maintained incrementally in the chainstate `Meta` column under `value_pools_v1`.
- `*_zat` fields are provided for exact integer values.

### verifychain

Verifies the blockchain database (best-effort parity).

- Params: `(checklevel numblocks)`
  - `checklevel` (optional number, 0–4, default 3)
  - `numblocks` (optional number, default 288, 0=all)
- Result: boolean

Notes:
- This is currently a read-only consistency check (flatfile decode + header linkage + merkle root + txindex).
- It does not re-apply full UTXO/script validation like the C++ daemon.

### getblockdeltas

Returns an insight-style block+transaction summary with per-input/per-output balance deltas.

- Params: `<blockhash>` (hex string) or `<height>` (number).
- Result: object with `hash`, `confirmations`, `size`, `height`, `version`, `merkleroot`, `deltas`, `time`, `mediantime`,
  `nonce`, `bits`, `difficulty`, `chainwork`, `previousblockhash`, `nextblockhash`.

### getspentinfo

- Params: either `{"txid":"...","index":n}` or positional `<txid> <index>`.
- Result: `{ "txid": "<spending_txid>", "index": <vin>, "height": <spending_height> }`.

### getaddressutxos

Returns all unspent outputs for one or more transparent addresses.

- Params: either `"taddr"` or `{"addresses":["taddr", ...], "chainInfo": true|false}`.
- Result:
  - If `chainInfo=false` (default): array of UTXO objects.
  - If `chainInfo=true`: `{ "utxos": [...], "hash": "<best_block_hash>", "height": <best_height> }`.

Each UTXO object includes: `address`, `txid`, `outputIndex`, `script`, `satoshis`, `height`.

### getaddressbalance

Returns the balance summary for one or more transparent addresses.

- Params: either `"taddr"` or `{"addresses":["taddr", ...]}`.
- Result: `{ "balance": <zatoshis>, "received": <zatoshis> }` where `received` is the sum of positive deltas (includes change).

### getaddressdeltas

Returns all balance deltas for one or more transparent addresses.

- Params: either `"taddr"` or `{"addresses":["taddr", ...], "start": n, "end": n, "chainInfo": true|false}`.
  - Height range filtering is only applied if both `start` and `end` are provided.
- Result:
  - Default: array of delta objects.
  - If `chainInfo=true` and a height range is provided: `{ "deltas": [...], "start": {...}, "end": {...} }`.

Each delta object includes: `address`, `blockindex` (tx index within block), `height`, `index` (vin/vout), `satoshis`, `txid`.

### getaddresstxids

Returns the transaction ids for one or more transparent addresses.

- Params: either `"taddr"` or `{"addresses":["taddr", ...], "start": n, "end": n}`.
  - Height range filtering is only applied if both `start` and `end` are provided.
- Result: array of txid strings, sorted by height.

### getnetworkhashps / getnetworksolps / getlocalsolps

These currently return `0.0` and are placeholders for mining metrics.

### getmininginfo

Returns a summary of mining state (modeled after the legacy C++ daemon).

- Params: none
- Result: object with keys including:
  - `blocks` (best block height)
  - `difficulty` (derived from best header bits)
  - `pooledtx` (mempool transaction count)
  - `testnet`, `chain`
  - Various rate/size fields (currently placeholders, e.g. `networkhashps`/`networksolps` are `0.0`)

### getblocktemplate

Returns a block template suitable for pools/miners, modeled after the C++ daemon output.

- Params: optional request object.
  - Template mode (default):
    - `{"mineraddress":"t1..."}`
    - `{"address":"t1..."}` (alias)
    - If omitted, the daemon uses the configured `--miner-address` / `flux.conf` `mineraddress=...` (if set),
      otherwise it uses the first wallet key (creating one in `wallet.dat` if the wallet is empty).
  - Longpoll: include `longpollid` from a previous response to wait for a template update.
  - Proposal mode: `{"mode":"proposal","data":"<blockhex>"}`
- Result: object including standard BIP22-style fields:
  - `version`, `previousblockhash`, `finalsaplingroothash`
  - `transactions` (array of hex txs + fee/depends/sigops)
  - `coinbasetxn` (hex coinbase tx + `fee` as negative total block fees)
  - `longpollid`, `target`, `mintime`, `mutable`, `noncerange`, `sigoplimit`, `sizelimit`
  - `curtime`, `bits`, `height`, `miner_reward`
- Flux-specific payout fields (when applicable):
  - `cumulus_fluxnode_address` / `cumulus_fluxnode_payout`
  - `nimbus_fluxnode_address` / `nimbus_fluxnode_payout`
  - `stratus_fluxnode_address` / `stratus_fluxnode_payout`
  - Legacy aliases: `basic_zelnode_*`, `super_zelnode_*`, `bamf_zelnode_*`, plus `cumulus_zelnode_*`, `nimbus_zelnode_*`, `stratus_zelnode_*`.
  - Funding events: `flux_creation_address` / `flux_creation_amount` at exchange/foundation/swap heights.

Notes:
- Longpoll waits until either the best block changes or the mempool revision changes.
- Proposal mode returns `null` when the block would be accepted, otherwise a string reason (BIP22-style).
- Template mode requires a miner address; if none is provided, the daemon falls back to `--miner-address` and then the wallet.

### submitblock

Submits a block for validation and (if it extends the current best chain) connects it.

- Params:
  - `hexdata` (string) - raw block bytes in hex
  - optional `parameters` (object) - accepted for parity but currently ignored
- Result:
  - `null` when accepted and connected
  - string when rejected (e.g. `"duplicate"`, `"inconclusive"`, or a validation failure reason)

### estimatefee

Estimates an approximate fee per kilobyte (kB) needed for a transaction to begin confirmation
within `nblocks` blocks.

- Params: `nblocks` (numeric).
- Result: fee-per-kB as a numeric FLUX value.
  - Returns `-1.0` when insufficient data is available.

Notes:
- This is currently based on a rolling sample of mempool-accepted transactions.
- Samples are persisted to `fee_estimates.dat` in `--data-dir`.

### estimatepriority

Estimates the approximate priority a zero-fee transaction needs to begin confirmation within
`nblocks` blocks.

- Params: `nblocks` (numeric).
- Result: numeric priority estimate.
  - Currently returns `-1.0` (priority estimator not implemented).

### prioritisetransaction

Adds a fee/priority delta for an in-mempool transaction (or stores it for later if the tx is
not yet in the mempool). This affects *mining selection only*; it does not change the fee that
would actually be paid on-chain.

- Params:
  - `txid` (string)
  - `priority_delta` (numeric)
  - `fee_delta_sat` (numeric) - delta in zatoshis/satoshis (can be negative)
- Result: `true`

### getconnectioncount

Returns the number of active peers.

### getnettotals

Returns:
- `totalbytesrecv`
- `totalbytessent`
- `timemillis`

### getnetworkinfo

Returns a summary of networking state including version, subversion, protocol
version, connection count, and network reachability.

### getpeerinfo

Returns per-peer details:
- `addr`, `subver`, `version`, `services`, `servicesnames`, `startingheight`
- `conntime`, `lastsend`, `lastrecv`
- `bytessent`, `bytesrecv`
- `inbound` (currently always false)
- `kind` ("block", "header", or "relay")

### listbanned

Returns banned header peers (if any):
- `address`
- `banned_until`

### clearbanned

Clears the in-memory/persisted banlist.

- Result: `null`

### setban

Adds or removes a ban for a peer address.

- Params:
  - `ip|ip:port` (string)
  - `add|remove` (string)
  - `bantime` (optional integer; seconds unless `absolute=true`)
  - `absolute` (optional boolean; treat `bantime` as a unix timestamp)
- Notes:
  - If you pass an IP with no port, the network default P2P port is assumed.
  - `add` also requests an immediate disconnect if currently connected.

### addnode

Adds/removes a manual peer address, similar to the C++ daemon.

- Params: `<node> <add|remove|onetry>`
- Notes:
  - Only numeric IPs are supported right now (no DNS resolution).
  - `add` updates an in-memory added-node list (used by `getaddednodeinfo`) and seeds the address manager.
  - `onetry` seeds the address manager but does not add to the persistent added-node list.

### getaddednodeinfo

Returns the current added-node list and whether each node is currently connected.

- Params: `[dns] [node]` (`dns` is accepted for parity but currently ignored)

### disconnectnode

Requests disconnect of an active peer connection.

- Params: `<node>`
- Result: `null`

### createfluxnodekey / createzelnodekey

Generates a new fluxnode private key (WIF), matching the C++ daemon.

- Params: none
- Result: WIF-encoded secp256k1 private key string (uncompressed)
- Notes:
  - Use this value as the `privkey` field in `fluxnode.conf`.

### listfluxnodeconf / listzelnodeconf

Returns `fluxnode.conf` entries in a JSON array, augmented with best-effort on-chain fluxnode index data.

- Params: optional `filter` string (case-insensitive substring match on alias/address/txhash/status)
- Notes:
  - Fields follow the C++ daemon shape (`alias`, `status`, `privateKey`, `address`, etc.).

### getfluxnodeoutputs / getzelnodeoutputs

Returns candidate fluxnode collateral outputs.

- Params: none
- Notes:
  - Wallet support is not implemented yet. This method currently reads `fluxnode.conf` and returns entries whose collateral outpoint is present in the current UTXO set and matches a valid tier amount.

### startdeterministicfluxnode / startdeterministiczelnode

Attempts to create, sign, and broadcast a deterministic fluxnode START transaction.

- Params:
  - `alias` (string)
  - `lockwallet` (boolean; accepted for parity but currently ignored)
  - `collateral_privkey_wif` (optional string; WIF private key controlling the collateral UTXO)
  - `redeem_script_hex` (optional string; required for P2SH collateral; multisig redeem script hex)
- Notes:
  - Wallet support is not implemented yet. You must provide the collateral private key either as the 3rd parameter or via an optional extra column in `fluxnode.conf` (see below).
  - P2PKH collateral: pubkey compression is inferred by matching the collateral output script hash.
  - P2SH collateral: the redeem script must hash to the collateral output script hash, and the provided key must correspond to a pubkey in the redeem script.
- Result:
  - Object with `overall` and `detail`, plus `txid` on success.

### startfluxnode / startzelnode

Starts fluxnodes from `fluxnode.conf`.

- Params: `set` (string) `lockwallet` (boolean) `[alias]` (string)
  - `set` must be `"all"` or `"alias"`. When `"alias"`, the 3rd param is required.
  - `lockwallet` is accepted for parity but currently ignored.
- Notes:
  - Wallet support is not implemented yet. This method requires the collateral private key to be present in `fluxnode.conf` (see below). For one-off starts without modifying config, use `startdeterministicfluxnode` and pass the collateral key as the 3rd param.

### getfluxnodecount

Returns basic counts of entries in the fluxnode index.

### listfluxnodes / viewdeterministicfluxnodelist

Returns a list of fluxnode records. Fields currently include collateral info,
heights, tier, and stored pubkeys. Network and payment fields are placeholders
and may be empty.

### fluxnodecurrentwinner

Returns a per-tier winner selection derived from indexed fluxnode records.
This logic is a best-effort placeholder and will evolve toward C++ parity.

### getstartlist

Returns unconfirmed fluxnode start entries that have not yet expired.

Fields:
- `collateral` (`txid:vout`)
- `added_height`
- `payment_address`
- `expires_in` (blocks remaining before it expires)
- `amount` (collateral amount, FLUX)

### getdoslist

Returns unconfirmed fluxnode start entries that have expired, but are still in the DoS cooldown.

Fields:
- `collateral` (`txid:vout`)
- `added_height`
- `payment_address`
- `eligible_in` (blocks remaining until it can be started again)
- `amount` (collateral amount, FLUX)

### getfluxnodestatus

Partial parity implementation.

- Params:
  - no params: attempts to read the first entry in `fluxnode.conf` under `--data-dir`
  - one param: either an alias from `fluxnode.conf` or an explicit `txid:vout`
- Result:
  - object with collateral fields, tier, payment address, and time/height metadata
  - `ip` / `network` are currently empty (not yet stored in the DB)

`fluxnode.conf` parsing uses the standard C++ layout: `<alias> <ip:port> <privkey> <txid> <vout>`.

For wallet-less start RPCs, `fluxd-rust` also supports optional extra columns:
`<collateral_privkey_wif> [redeem_script_hex]`.

### getbenchmarks

Fluxnode-only benchmark query (fluxbenchd integration is not implemented yet).

- Params: none
- Result: string (`"Benchmark not running"`)

### getbenchstatus

Fluxnode-only benchmark status (fluxbenchd integration is not implemented yet).

- Params: none
- Result: string (`"Benchmark not running"`)

### startbenchmark / startfluxbenchd / startzelbenchd

Fluxnode-only benchmark daemon control (not implemented yet).

- Params: none
- Result: string (`"Benchmark daemon control not implemented"`)

### stopbenchmark / stopfluxbenchd / stopzelbenchd

Fluxnode-only benchmark daemon control (not implemented yet).

- Params: none
- Result: string (`"Benchmark daemon control not implemented"`)

### zcbenchmark

Zcash benchmark RPC (not implemented yet).

- Result: error (`-32603`, `"zcbenchmark not implemented"`)
