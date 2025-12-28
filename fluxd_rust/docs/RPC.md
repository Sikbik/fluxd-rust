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
curl -u "$(cat ./data/rpc.cookie)" \
  "http://127.0.0.1:16124/daemon/getblockhashes?params=[1700000000,1699990000,{\"noOrphans\":true}]"
```

Type notes:
- Some methods require strict booleans (for example `getblockheader` verbose). Use `true`/`false`, not `1`/`0`.
- `verbosity` for `getblock` must be numeric (0, 1, or 2).

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
- `getnetworkinfo`
- `getpeerinfo`
- `getnettotals`
- `getconnectioncount`
- `listbanned`

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

- `getrawtransaction <txid> [verbose]`
- `sendrawtransaction <hexstring> [allowhighfees]`
- `gettxout <txid> <vout> [include_mempool]`
- `gettxoutsetinfo`

### Mining and mempool

- `getmempoolinfo`
- `getrawmempool [verbose]`
- `getblocktemplate` (not implemented)
- `getnetworkhashps` (returns 0.0)
- `getnetworksolps` (returns 0.0)
- `getlocalsolps` (returns 0.0)

### Fluxnode

- `getfluxnodecount`
- `listfluxnodes`
- `viewdeterministicfluxnodelist [filter]`
- `fluxnodecurrentwinner`
- `getfluxnodestatus` (not implemented)
- `getdoslist` (not implemented)

### Indexer endpoints (placeholders)

- `getblockdeltas` (not implemented)
- `getspentinfo`
- `getaddressutxos`
- `getaddressbalance`
- `getaddressdeltas`
- `getaddresstxids`
- `getaddressmempool`
- `gettxoutproof` (not implemented)
- `verifytxoutproof` (not implemented)

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
- `relayfee` - 0.0.
- `errors` - empty string.

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
- Inserts into the local in-memory mempool; P2P relay is not implemented yet.
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

### getfluxnodecount

Returns basic counts of entries in the fluxnode index.

### listfluxnodes / viewdeterministicfluxnodelist

Returns a list of fluxnode records. Fields currently include collateral info,
heights, tier, and stored pubkeys. Network and payment fields are placeholders
and may be empty.

### fluxnodecurrentwinner

Returns a per-tier winner selection derived from indexed fluxnode records.
This logic is a best-effort placeholder and will evolve toward C++ parity.
