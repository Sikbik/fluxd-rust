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
- `-5` invalid address or key

## Supported methods

### General

- `help [method]`
- `getinfo`
- `ping`
- `stop`
- `restart`
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

### Transactions and UTXO

- `createrawtransaction <transactions> <addresses> [locktime] [expiryheight]`
- `decoderawtransaction <hexstring>`
- `decodescript <hex>`
- `createmultisig <nrequired> <keys>`
- `getrawtransaction <txid> [verbose]`
- `sendrawtransaction <hexstring> [allowhighfees]`
- `gettxout <txid> <vout> [include_mempool]`
- `gettxoutsetinfo`
- `validateaddress <fluxaddress>`
- `verifymessage <fluxaddress> <signature> <message>`

### Mining and mempool

- `getmempoolinfo`
- `getrawmempool [verbose]`
- `getmininginfo` (partial; rate fields return 0.0)
- `getblocktemplate` (partial; includes deterministic fluxnode payouts + basic mempool tx selection)
- `submitblock <hexdata>` (partial)
- `getnetworkhashps` (returns 0.0)
- `getnetworksolps` (returns 0.0)
- `getlocalsolps` (returns 0.0)
- `estimatefee <nblocks>`

### Fluxnode

- `getfluxnodecount`
- `listfluxnodes`
- `viewdeterministicfluxnodelist [filter]`
- `fluxnodecurrentwinner`
- `getfluxnodestatus [alias|txid:vout]` (partial; uses `--data-dir/fluxnode.conf` when called with no params)
- `getdoslist`
- `getstartlist`

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

### stop

- Result: string (`"fluxd stopping"`)

### restart

- Result: string (`"fluxd restarting ..."`).
- Note: this requests process exit; actual restart depends on your supervisor (systemd, docker, etc).

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

### sendrawtransaction

- Params:
  - `hexstring` (string)
  - `allowhighfees` (boolean, optional; currently ignored)
- Result: transaction id hex string.

Notes:
- Inserts into the local in-memory mempool.
- If `--tx-peers > 0`, announces the txid to relay peers via P2P (`inv` + `getdata`/`tx`).
- Only accepts transactions spending confirmed UTXOs (no unconfirmed parent/ancestor tracking yet).

### gettxout

- Params:
  - `txid` (hex string)
  - `vout` (number)
  - `include_mempool` (boolean or numeric, default true)
- Result:
  - `null` if the output is spent.
  - Otherwise: `bestblock`, `confirmations`, `value`, `scriptPubKey`, `coinbase`.

Notes:
- If `include_mempool=true`, returns `null` when the output is spent by a mempool transaction.

### gettxoutsetinfo

Returns a summary of the current transparent UTXO set.

Fields:
- `height`, `bestblock`
- `txouts` - number of unspent outputs
- `total_amount` - sum of all unspent output values (transparent only)
- `sprout_pool`, `sapling_pool` - shielded pool totals
- `shielded_amount` - `sprout_pool + sapling_pool`
- `total_supply` - `total_amount + shielded_amount`
- `disk_size` - byte size of the `db/` directory

Notes:
- `transactions`, `bogosize`, and `hash_serialized_2` are currently placeholders.
- UTXO stats are maintained incrementally in the chainstate `Meta` column under `utxo_stats_v1`.
- Shielded value pools are maintained incrementally in the chainstate `Meta` column under `value_pools_v1`.
- `*_zat` fields are provided for exact integer values.

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

### getblocktemplate

Returns a block template suitable for pools/miners, modeled after the C++ daemon output.

- Params: optional request object.
  - Template mode (default):
    - `{"mineraddress":"t1..."}`
    - `{"address":"t1..."}` (alias)
    - If omitted, the daemon uses the configured `--miner-address` / `flux.conf` `mineraddress=...` (if set).
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
- Until wallet integration is implemented, template mode requires a miner address either in the request or via `--miner-address` / `flux.conf`.

### estimatefee

Estimates an approximate fee per kilobyte (kB) needed for a transaction to begin confirmation
within `nblocks` blocks.

- Params: `nblocks` (numeric).
- Result: fee-per-kB as a numeric FLUX value.
  - Returns `-1.0` when insufficient data is available.

Notes:
- This is currently based on a rolling sample of mempool-accepted transactions.
- Samples are persisted to `fee_estimates.dat` in `--data-dir`.

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
- `addr`, `subver`, `version`, `startingheight`
- `conntime`, `lastsend`, `lastrecv`
- `bytessent`, `bytesrecv`
- `inbound` (currently always false)
- `kind` ("block" or "header")

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
