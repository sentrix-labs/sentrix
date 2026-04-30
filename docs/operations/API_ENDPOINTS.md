# Sentrix Chain — API Endpoints

Reference for frontend integration. Source: `crates/sentrix-rpc/src/routes/mod.rs` (REST) and `crates/sentrix-rpc/src/jsonrpc/{eth,net,web3,sentrix}.rs` (JSON-RPC).

Base URLs:
- **Mainnet:** `https://rpc.sentrixchain.com` (chain_id 7119, **Voyager DPoS+BFT** since 2026-04-25)
- **Testnet:** `https://testnet-rpc.sentrixchain.com` (chain_id 7120, **Voyager DPoS+BFT** since 2026-04-23)

Rate limits (per IP): **60 req/min global**, **10 req/min write endpoints** (POST to `/transactions`, `/tokens/deploy|transfer|burn`, `/rpc`).

Auth: most endpoints are public. Handlers tagged with `_auth: ApiKey` check the `X-API-Key` header against `SENTRIX_API_KEY` env var on the validator (16-char min, constant-time compare). If the env var isn't set, auth is skipped.

---

## REST API (49 endpoints)

### Ops & Discovery

| Method | Path | Description |
|---|---|---|
| GET | `/` | Self-describe root. Advertises REST surface + JSON-RPC namespaces. |
| GET | `/health` | Liveness. Returns `{ "status": "ok", "node": "sentrix-chain" }`. |
| GET | `/sentrix_status` | NEAR-style structured status. See shape below. |
| GET | `/metrics` | Prometheus text format (`text/plain; version=0.0.4`). Public. |

**`GET /sentrix_status`** response:
```json
{
  "version": { "version": "2.1.1", "build": "<sha|unknown>" },
  "chain_id": 7119,
  "consensus": "DPoS+BFT" | "PoA",  // current = "DPoS+BFT" post-Voyager activation; "PoA" preserved for pre-2026-04-25 historical block queries
  "native_token": "SRX",
  "sync_info": {
    "latest_block_height": 80123,
    "latest_block_hash": "<64 hex>",
    "latest_block_time": 1776624478,
    "earliest_block_height": 79123,
    "syncing": false
  },
  "validators": { "active_count": 3 },
  "uptime_seconds": 1234
}
```

### Chain

| Method | Path | Description |
|---|---|---|
| GET | `/chain/info` | High-level chain stats (height, chain_id, supply, mempool). |
| GET | `/chain/blocks` | Paginated block list. Query: `?page=0&limit=20` (max 100). |
| GET | `/chain/blocks/{index}` | Block by height. |
| GET | `/chain/validate` | Full-chain validation (slow, admin-y). |
| GET | `/chain/state-root/{height}` | State root at given height. |
| GET | `/chain/performance` | `{ blocks, avg_block_time_ms, tps_1h, tps_24h, last_block_time }`. |

**`GET /chain/info`** response (`max_supply_srx` is fork-aware — `TOKENOMICS_V2_HEIGHT=640800` activated 2026-04-26 on mainnet, so all current responses return 315M; pre-fork value 210M is historical only):
```json
{
  "chain_id": 7119,
  "height": 80123,
  "total_blocks": 80124,
  "active_validators": 3,
  "circulating_supply_srx": 63030377.988,
  "max_supply_srx": 315000000.0,
  "next_block_reward_srx": 1.0,
  "total_minted_srx": 63030978.0,
  "total_burned_srx": 600.011,
  "mempool_size": 0,
  "deployed_tokens": 0,
  "window_is_partial": false,
  "window_start_block": 79123
}
```

**Block shape** (returned by `/chain/blocks`, `/chain/blocks/{i}`, `/blocks`, `/blocks/{h}`):
```json
{
  "index": 80123,
  "hash": "<64 hex>",
  "previous_hash": "<64 hex>",
  "timestamp": 1776624478,
  "tx_count": 1,
  "validator": "0x...",
  "merkle_root": "<64 hex>",
  "round": 0,
  "has_justification": false
}
```

### Blocks (short-form aliases)

| Method | Path | Description |
|---|---|---|
| GET | `/blocks` | Alias for `/chain/blocks`. |
| GET | `/blocks/{height}` | Alias for `/chain/blocks/{height}`. |

### Accounts (frontend-style paths)

| Method | Path | Description |
|---|---|---|
| GET | `/accounts/{address}/balance` | SRX balance. Response: `{ "address", "balance": <sentri> }`. |
| GET | `/accounts/{address}/nonce` | Account nonce. Response: `{ "address", "nonce": <u64> }`. |
| GET | `/accounts/{address}/history` | Paginated tx history. Query: `?page=0&limit=20`. |
| GET | `/accounts/{address}/tokens` | SRC-20 tokens held + balance per contract. |
| GET | `/accounts/{address}/code` | Contract bytecode if contract account, else empty. |
| GET | `/accounts/top` | Rich-list (top holders by SRX balance). |

### Address (older paths, kept for back-compat)

| Method | Path | Description |
|---|---|---|
| GET | `/address/{address}/history` | Same as `/accounts/{a}/history`. |
| GET | `/address/{address}/info` | Summary: balance + nonce + tx_count + contract flag. |
| GET | `/address/{address}/proof` | Merkle proof of membership against current state root. |

### Wallet (single-screen alias for APK / faucet)

| Method | Path | Description |
|---|---|---|
| GET | `/wallets/{address}` | Combined: balance + nonce + tx_count + summary. |

### Transactions

| Method | Path | Description |
|---|---|---|
| POST | `/transactions` | Submit a pre-signed native tx. **Write-rate-limited.** |
| GET | `/transactions` | Recent txs. Query: `?limit=20&offset=0`. |
| GET | `/transactions/{txid}` | Tx by txid (64 hex, with or without `0x`). |

**POST `/transactions`** body:
```json
{ "transaction": <SignedTransaction> }
```
`<SignedTransaction>` fields (all required): `txid`, `from_address`, `to_address`, `amount` (sentri), `fee` (sentri), `nonce`, `data`, `timestamp`, `chain_id`, `signature` (compact ECDSA hex), `public_key` (uncompressed secp256k1 hex).

**Response** (on accept): `{ "success": true, "txid": "...", "status": "pending_in_mempool" }`.
**Error:** `400` with `{ "success": false, "error": "..." }` — e.g. `"amount must be > 0"`, `"Invalid nonce: expected X, got Y"`, `"too many pending transactions from this sender"`.

### Mempool

| Method | Path | Description |
|---|---|---|
| GET | `/mempool` | Current unconfirmed txs. Response: `{ "size", "transactions": [...] }`. |

### Tokens (SRC-20)

| Method | Path | Description |
|---|---|---|
| GET | `/tokens` | All deployed tokens. Response: `{ tokens: [...], total }`. |
| GET | `/tokens/{contract}` | Token metadata. 404 if unknown. |
| GET | `/tokens/{contract}/balance/{addr}` | SRC-20 balance of `addr` on `contract`. |
| GET | `/tokens/{contract}/holders` | Legacy holders list. |
| GET | `/tokens/{contract}/holders-v2` | Preferred — includes `{ holders, total, percentage }` per holder. |
| GET | `/tokens/{contract}/trades` | Transfer history. Query: `?limit=20&offset=0`. |
| GET | `/tokens/{contract}/transfers` | Explorer-shaped transfer feed (paginated). |
| POST | `/tokens/deploy` | Deploy new SRC-20. **Write-rate-limited.** `_auth: ApiKey`. |
| POST | `/tokens/{contract}/transfer` | Transfer. **Write-rate-limited.** `_auth: ApiKey`. |
| POST | `/tokens/{contract}/burn` | Burn. **Write-rate-limited.** `_auth: ApiKey`. |

All three POSTs take `{ "transaction": <SignedTransaction> }` where `tx.data` is a JSON-encoded `TokenOp::Deploy | Transfer | Burn` and `tx.to_address = "0x0…00"` (TOKEN_OP_ADDRESS).

### Validators

| Method | Path | Description |
|---|---|---|
| GET | `/validators` | Validator set (name, address, is_active, blocks_produced). Pre-Voyager (`h<579047` mainnet) this was the Pioneer PoA authority set; post-Voyager it's the DPoS active set. |
| GET | `/validators/{address}/delegators` | Delegators to a validator (DPoS). |
| GET | `/validators/{address}/rewards` | Reward-history summary for a validator. |
| GET | `/validators/{address}/blocks-over-time` | Blocks produced per epoch (time series). |

### Staking (Voyager DPoS)

| Method | Path | Description |
|---|---|---|
| GET | `/staking/validators` | DPoS active set with stake + commission + uptime. |
| GET | `/staking/delegations/{address}` | Active delegations for a delegator. |
| GET | `/staking/unbonding/{address}` | Pending unbonding entries. |

### Epoch

| Method | Path | Description |
|---|---|---|
| GET | `/epoch/current` | Current epoch `{ epoch_number, start_height, end_height, progress }`. |
| GET | `/epoch/history` | Past N epochs. Query: `?limit=10`. |

### Stats

| Method | Path | Description |
|---|---|---|
| GET | `/stats/daily` | Daily aggregates (tx count, new addresses, avg block time). |

### Rich list

| Method | Path | Description |
|---|---|---|
| GET | `/richlist` | Top SRX holders. Response: `{ holders: [{ address, balance, percentage }], total_accounts }`. |

### Admin

| Method | Path | Description |
|---|---|---|
| GET | `/admin/log` | Audit log (rolling 10k entries). **`_auth: ApiKey`.** |

### Explorer (built-in HTML UI, not JSON)

Nested under `/explorer/*`. Paths: `/explorer`, `/explorer/blocks`, `/explorer/block/{i}`, `/explorer/transactions`, `/explorer/tx/{txid}`, `/explorer/validators`, `/explorer/validators/{addr}`, `/explorer/tokens`, `/explorer/tokens/{c}`, `/explorer/address/{a}`, `/explorer/richlist`, `/explorer/mempool`. Dark-themed SSR pages served directly from the validator.

### JSON-RPC endpoint

| Method | Path | Description |
|---|---|---|
| POST | `/rpc` | JSON-RPC 2.0 dispatcher. **Write-rate-limited.** `_auth: ApiKey`. Accepts single request OR batch (max 100). |

---

## JSON-RPC 2.0 (30 methods)

Envelope: `{ "jsonrpc": "2.0", "method": "<name>", "params": [...], "id": <any> }`. Response: `{ "jsonrpc": "2.0", "result": <value>, "id": <same> }` or `{ "error": { "code", "message" } }`.

### `eth_*` — Ethereum compatibility (20 methods)

| Method | Params | Result |
|---|---|---|
| `eth_chainId` | `[]` | hex chain_id |
| `eth_blockNumber` | `[]` | hex latest height |
| `eth_gasPrice` | `[]` | hex (1 Gwei fixed) |
| `eth_estimateGas` | `[{ from, to, data, value }]` | hex (21k for transfer, 100k with data) |
| `eth_getBalance` | `[address, blockTag]` | hex wei |
| `eth_getTransactionCount` | `[address, blockTag]` | hex nonce |
| `eth_getBlockByNumber` | `[tag\|hex, fullTx]` | block or null |
| `eth_getBlockByHash` | `[hash, fullTx]` | block or null |
| `eth_getTransactionByHash` | `[txid]` | tx or null |
| `eth_getTransactionReceipt` | `[txid]` | receipt or null |
| `eth_getBlockReceipts` | `[tag\|hash\|{blockHash\|blockNumber}]` | array of receipts or null |
| `eth_sendRawTransaction` | `[rawHex]` | tx hash |
| `eth_call` | `[{ from, to, data, gas }, blockTag]` | hex output |
| `eth_getLogs` | `[{ fromBlock, toBlock, address, topics }]` | array of log objects |
| `eth_feeHistory` | `[blockCount, newest, percentiles]` | `{ oldestBlock, baseFeePerGas[], gasUsedRatio[], reward[][] }` |
| `eth_maxPriorityFeePerGas` | `[]` | hex (flat INITIAL_BASE_FEE) |
| `eth_syncing` | `[]` | `false` |
| `eth_accounts` | `[]` | `[]` (server never holds keys) |
| `eth_getCode` | `[address, blockTag]` | hex bytecode or `"0x"` |
| `eth_getStorageAt` | `[address, slot, blockTag]` | hex 32-byte value |

Block range on `eth_getLogs` is capped at 10 000; exceeding it returns error code `-32005` "query returned more than 10000 results".

### `net_*` (2 methods)

| Method | Params | Result |
|---|---|---|
| `net_version` | `[]` | chain_id as decimal string |
| `net_listening` | `[]` | `true` |

### `web3_*` (1 method)

| Method | Params | Result |
|---|---|---|
| `web3_clientVersion` | `[]` | `"Sentrix/<ver>/Rust"` |

### `sentrix_*` — Native namespace (7 methods)

| Method | Params | Result |
|---|---|---|
| `sentrix_sendTransaction` | `[<SignedTransaction>]` | `{ txid, status: "pending_in_mempool" }` |
| `sentrix_getBalance` | `[address]` | hex wei (alias for `eth_getBalance`) |
| `sentrix_getValidatorSet` | `[]` | See shape below. |
| `sentrix_getDelegations` | `[address]` | `{ delegator, delegations: [...] }` |
| `sentrix_getStakingRewards` | `[address, { from_epoch?, to_epoch? }?]` | `{ total_lifetime, pending_claimable, from_epoch, to_epoch, by_epoch[] }` |
| `sentrix_getBftStatus` | `[]` | See shape below. |
| `sentrix_getFinalizedHeight` | `[]` | `{ finalized_height, finalized_hash, latest_height, blocks_behind_finality }` |

**`sentrix_getValidatorSet`** result (Voyager DPoS+BFT mainnet, current):
```json
{
  "consensus": "DPoS+BFT",
  "active_count": 4,
  "total_count": 4,
  "total_active_stake": "0x14d1120d7b160000",
  "epoch_number": 22,
  "validators": [
    {
      "address": "0x...",
      "name": "Foundation",
      "stake": "0x0",            // on DPoS: wei hex of total stake
      "commission": 0.0,          // on DPoS: 0.0–1.0
      "status": "active" | "jailed" | "tombstoned" | "unbonding",
      "blocks_produced_epoch": 123,
      "uptime": 1.0,
      "voting_power": "0x3b9aca00"  // wei hex
    }
  ]
}
```
Post-Voyager (DPoS) switches `consensus` to `"DPoS"` and populates real stake values.

**`sentrix_getBftStatus`** result (PoA mode):
```json
{
  "consensus": "PoA",
  "current_leader": "0x...",
  "last_finalized_height": 80123,
  "last_finalized_hash": "<64 hex>"
}
```
BFT mode adds `current_round`, `current_view`, `phase`, `rounds_since_last_block`.

---

## Error codes (JSON-RPC)

| Code | Meaning |
|---|---|
| `-32700` | Parse error (body not JSON) |
| `-32600` | Invalid request / batch too large (>100) |
| `-32601` | Method not found |
| `-32602` | Invalid params |
| `-32603` | Internal error (mempool reject, chain state) |
| `-32000` | EVM not active (pre-Voyager on testnet) |
| `-32005` | eth_getLogs range > 10 000 blocks |

---

## Notes for integrators

- **Units:** chain stores amounts in *sentri* (1 SRX = 1e8 sentri). EVM + JSON-RPC surface uses *wei* (1 SRX = 1e18 wei = 1e10 sentri). REST endpoints generally return sentri; JSON-RPC `eth_getBalance` / `sentrix_getBalance` return wei hex.
- **Block time:** 1s under nominal load on both mainnet + testnet (Voyager DPoS+BFT). Slower during BFT round timeouts (skip-rounds when proposer offline).
- **Finality:** Voyager DPoS+BFT finalizes on 3/4 stake-weighted precommit supermajority — typically round-0 (within ~1s of proposal). Skip-rounds extend finality latency. Pre-Voyager (`h<579047` mainnet) blocks finalized immediately under Pioneer PoA round-robin.
- **Address format:** `0x` + 40 hex lowercase. SRC-20 contract addresses: `SRC20_` + 40 hex.
- **chain_id:** 7119 = mainnet, 7120 = testnet.
- **Gas accounting:** today flat 21k per tx for non-EVM ops. EIP-1559 dynamic base fee queued (#9 backlog).
- **WebSocket:** not yet available (`eth_subscribe` queued as #5/#6 backlog).
- **CORS:** restrictive default. Set `SENTRIX_CORS_ORIGIN=*` on the validator for dev, specific origin for prod.

Cross-check any shape by `curl`ing the live endpoint — responses are authoritative, this doc is a summary.

---

## Copy-pasteable examples

### Chain state
```bash
curl -s https://rpc.sentrixchain.com/chain/info
# {
#   "chain_id": 7119, "height": 80123, "total_blocks": 80124,
#   "active_validators": 3, "circulating_supply_srx": 63030377.988,
#   "max_supply_srx": 315000000.0, "next_block_reward_srx": 1.0,
#   "mempool_size": 0, "deployed_tokens": 0,
#   "window_is_partial": false, "window_start_block": 79123
# }

curl -s https://rpc.sentrixchain.com/sentrix_status
# { "version": {"version":"2.1.1","build":"unknown"}, "chain_id": 7119,
#   "consensus": "PoA", "native_token": "SRX",
#   "sync_info": { "latest_block_height": 80123, ... },
#   "validators": { "active_count": 3 }, "uptime_seconds": 1234 }
```

### Account balance + nonce (REST)
```bash
curl -s https://rpc.sentrixchain.com/accounts/0x682126f5f973bddda2c92fb0dfce8a4ba275c99b/balance
# { "address": "0x682126...", "balance": 664000000000 }   // in sentri

curl -s https://rpc.sentrixchain.com/accounts/0x682126f5f973bddda2c92fb0dfce8a4ba275c99b/nonce
# { "address": "0x682126...", "nonce": 9 }
```

### Account balance (JSON-RPC, wei)
```bash
curl -s -X POST https://rpc.sentrixchain.com/rpc \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getBalance",
       "params":["0x682126f5f973bddda2c92fb0dfce8a4ba275c99b","latest"],"id":1}'
# { "jsonrpc": "2.0", "result": "0x169872033d686c1a000", "id": 1 }
```

### Latest block
```bash
curl -s https://rpc.sentrixchain.com/blocks/80123
# { "index": 80123, "hash": "f35cd...", "previous_hash": "abc...",
#   "timestamp": 1776625635, "tx_count": 1, "validator": "0x...",
#   "merkle_root": "...", "round": 0, "has_justification": false }
```

### Validator set (PoA)
```bash
curl -s -X POST https://rpc.sentrixchain.com/rpc \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"sentrix_getValidatorSet","params":[],"id":1}'
# {
#   "result": {
#     "consensus": "PoA", "active_count": 3, "total_count": 3,
#     "total_active_stake": "0x0", "epoch_number": 2,
#     "validators": [{
#       "address": "0x245785...", "name": "Foundation",
#       "stake": "0x0", "commission": 0.0, "status": "active",
#       "blocks_produced_epoch": 26789, "uptime": 1.0,
#       "voting_power": "0x13d92d400"   // flat 1/N on PoA
#     }, ...]
#   }
# }
```

### Deploy SRC-20 token (wallet side)
1. Build `TokenOp` payload:
```json
{ "Deploy": { "name": "Demo SRC-20 Token", "symbol": "DEMO",
              "decimals": 18, "supply": 10000000000, "max_supply": 0 } }
```
2. Put that JSON string into `tx.data`, set `tx.to_address = "0x0000000000000000000000000000000000000000"`, sign.
3. Fetch nonce via JSON-RPC `eth_getTransactionCount`.
4. `POST /tokens/deploy`:
```bash
curl -s -X POST https://testnet-rpc.sentrixchain.com/tokens/deploy \
  -H 'Content-Type: application/json' \
  -d '{
    "transaction": {
      "txid": "<sha256 of canonical payload>",
      "from_address": "0x682126...",
      "to_address": "0x0000000000000000000000000000000000000000",
      "amount": 0, "fee": 100000, "nonce": 0,
      "data": "{\"Deploy\":{\"name\":\"Demo SRC-20 Token\",\"symbol\":\"DEMO\",\"decimals\":18,\"supply\":10000000000,\"max_supply\":0}}",
      "timestamp": 1776625635, "chain_id": 7120,
      "signature": "<128 hex compact>", "public_key": "<130 hex uncompressed>"
    }
  }'
# {
#   "success": true, "txid": "1379d177...",
#   "deployer": "0x682126...",
#   "name": "Demo SRC-20 Token", "symbol": "DEMO",
#   "total_supply": 10000000000, "max_supply": 0,
#   "status": "pending_in_mempool"
# }
```

After the next block, `GET /tokens` lists the new contract at `SRC20_<40 hex>`, e.g. `SRC20_df98a9e4407bc2d28cd7e9046698e2d1cb0834ae`.

### Send SRX (native transfer)
Same pattern, amount > 0 and empty `data`:
```bash
curl -s -X POST https://testnet-rpc.sentrixchain.com/transactions \
  -H 'Content-Type: application/json' \
  -d '{"transaction": { "from_address":"0x...", "to_address":"0x...",
                        "amount": 1000000000, "fee": 10000, "nonce": 3,
                        "data": "", "timestamp": 1776625635,
                        "chain_id": 7120,
                        "txid":"...", "signature":"...", "public_key":"..." }}'
# { "success": true, "txid": "...", "status": "pending_in_mempool" }
```

### Block receipts (batch)
```bash
curl -s -X POST https://rpc.sentrixchain.com/rpc \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getBlockReceipts","params":["latest"],"id":1}'
# { "result": [{
#     "transactionHash": "0x...", "transactionIndex": "0x0",
#     "blockNumber": "0x138ab", "blockHash": "0x...",
#     "from": "0x...", "to": "0x...",
#     "status": "0x1", "gasUsed": "0x5208", "cumulativeGasUsed": "0x5208",
#     "logs": [], "logsBloom": "0x0000..."
# }] }
```

### EVM event logs (filter)
```bash
curl -s -X POST https://rpc.sentrixchain.com/rpc \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getLogs","params":[{
        "fromBlock":"0x0","toBlock":"latest",
        "address":"0x5fbdb2315678afecb367f032d93f642f64180aa3",
        "topics":["0xddf252ad..."]
      }],"id":1}'
# { "result": [ { address, topics, data, blockNumber, transactionHash, ... } ] }
```

---

## Error response samples

### REST (400 Bad Request)
```json
// POST /transactions with amount=0 on a native transfer
{ "success": false,
  "error": "Invalid transaction: amount must be > 0 (unless token/EVM operation)" }

// POST /transactions with out-of-order nonce
{ "success": false,
  "error": "Invalid nonce: expected 4, got 16" }

// POST /transactions when too many pending from one sender
{ "success": false,
  "error": "Invalid transaction: too many pending transactions from this sender" }

// POST /tokens/deploy with data that isn't TokenOp::Deploy
{ "success": false,
  "error": "expected Deploy operation in tx.data" }

// POST /tokens/{c}/transfer where URL contract ≠ data contract
{ "success": false,
  "error": "contract in data does not match URL" }
```

### Rate-limited (429)
```json
{ "error": "rate limit exceeded", "limit": 10, "window_secs": 60 }
```
`limit` is 10 on write endpoints, 60 on the global bucket.

### JSON-RPC error envelope
```json
{ "jsonrpc": "2.0",
  "error": { "code": -32602, "message": "address must be 42 chars (0x + 40 hex)" },
  "id": 1 }

{ "jsonrpc": "2.0",
  "error": { "code": -32601, "message": "method not found: eth_madeUp" },
  "id": 1 }

{ "jsonrpc": "2.0",
  "error": { "code": -32005, "message": "query returned more than 10000 results" },
  "id": 1 }

{ "jsonrpc": "2.0",
  "error": { "code": -32600,
             "message": "batch too large: max 100 requests, got 101" },
  "id": null }

{ "jsonrpc": "2.0",
  "error": { "code": -32000, "message": "EVM not active yet" },
  "id": 1 }
```

### Auth-required without key (401)
```
HTTP/1.1 401 Unauthorized
```
No JSON body (axum default). Set `X-API-Key: <16+ char key>` on endpoints with `_auth: ApiKey`: `/admin/log`, `/tokens/deploy|transfer|burn`, `/rpc`, and `POST /transactions`.

### Not found (404)
```
HTTP/1.1 404 Not Found
```
`GET /tokens/{unknown_contract}`, `GET /transactions/{txid_outside_window}`, etc. No JSON body.

---

## Tx signing recipe (native REST + JSON-RPC)

For `POST /transactions`, `POST /tokens/*`, `sentrix_sendTransaction`:

1. Build a canonical signing payload — sorted-key JSON with exactly these 8 fields (extras break the hash):
```json
{ "amount": <u64>, "chain_id": <u64>, "data": "<string>",
  "fee": <u64>, "from": "<addr>", "nonce": <u64>,
  "timestamp": <unix-sec>, "to": "<addr>" }
```
BTreeMap-sorted; `serde_json::to_string` on the server matches bit-for-bit.

2. `sha256(canonical_json)` → 32-byte digest.

3. ECDSA sign the digest with secp256k1 (non-recoverable, compact 64 bytes) → 128 hex → `signature`.

4. `public_key` = uncompressed secp256k1 (65 bytes, `0x04` + 32 + 32) → 130 hex.

5. `txid` = `sha256(canonical_json).hex()` — same digest, different use.

6. POST the full Transaction with all 11 fields.

For `eth_sendRawTransaction` the chain decodes the RLP and recovers the sender — that path doesn't need `signature` / `public_key`. Sentrix maps the resulting Ethereum tx onto an internal `Transaction` with `data = "EVM:{gas_limit}:{hex_calldata}"`, `signature = <full raw hex>`, `public_key = ""`.
