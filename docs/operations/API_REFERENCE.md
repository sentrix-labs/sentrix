# API Reference

Complete list of Sentrix REST + JSON-RPC endpoints.

Base URL: `https://testnet-rpc.sentriscloud.com` (testnet) or `https://sentrix-rpc.sentriscloud.com` (mainnet).

---

## REST Endpoints

### Public (no auth)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/` | Node self-describe (name, version, chain_id, consensus, endpoint map, JSON-RPC namespaces) — see [Root response](#root-response) |
| GET | `/health` | Health check (`{"status":"ok"}`) |
| GET | `/metrics` | Prometheus-format metrics (block height, validators, mempool, uptime) |
| GET | `/chain/info` | Chain stats (height, supply, validators, mempool, etc.) |
| GET | `/chain/blocks?page=0&limit=20` | Paginated block list (newest first, max 100) |
| GET | `/chain/blocks/{index}` | Single block by height |
| GET | `/chain/validate` | Full chain validation (slow, auth recommended) |
| GET | `/chain/state-root/{height}` | State root hash at given height |
| GET | `/accounts/{address}/balance` | Account balance (sentri + SRX) |
| GET | `/accounts/{address}/nonce` | Account nonce |
| GET | `/validators` | List all validators (address, name, status, blocks produced) |
| GET | `/mempool` | Current mempool contents |
| GET | `/transactions?page=0&limit=20` | Latest transactions (paginated) |
| GET | `/transactions/{txid}` | Transaction detail by txid |
| GET | `/tokens` | List all deployed SRX-20 tokens |
| GET | `/tokens/{contract}` | Token info (name, symbol, supply, owner) |
| GET | `/tokens/{contract}/balance/{address}` | Token balance for address |
| GET | `/tokens/{contract}/holders` | Token holder list (sorted by balance) |
| GET | `/tokens/{contract}/trades?limit=20&offset=0` | Token trade history |
| GET | `/richlist` | Top accounts by SRX balance |
| GET | `/address/{address}/history?limit=20&offset=0` | Address transaction history |
| GET | `/address/{address}/info` | Address info (balance, nonce, tx count, is_contract) |
| GET | `/address/{address}/proof` | Merkle inclusion proof from state trie |
| GET | `/staking/validators` | DPoS validator set (Voyager) |
| GET | `/staking/delegations/{address}` | Delegations for address |
| GET | `/staking/unbonding/{address}` | Unbonding entries for address |
| GET | `/epoch/current` | Current epoch info |
| GET | `/epoch/history` | Epoch history |
| GET | `/stats/daily` | Daily tx + block count (last 14 days, for charts) |
| GET | `/admin/log` | Admin operation audit trail (requires API key) |

### Write (requires `X-API-Key` header, rate-limited 10 req/min)

| Method | Path | Description |
|--------|------|-------------|
| POST | `/transactions` | Submit a signed native SRX transaction |
| POST | `/tokens/deploy` | Deploy a new SRX-20 token (signed tx) |
| POST | `/tokens/{contract}/transfer` | Token transfer (signed tx) |
| POST | `/tokens/{contract}/burn` | Token burn (signed tx) |
| POST | `/rpc` | JSON-RPC 2.0 dispatcher (single + batch) |

### Explorer (HTML)

| Path | Page |
|------|------|
| `/explorer` | Home (stats, charts, recent blocks + txs) |
| `/explorer/blocks` | Block list |
| `/explorer/transactions` | Transaction list |
| `/explorer/validators` | Validator table |
| `/explorer/tokens` | Token list |
| `/explorer/richlist` | Rich list |
| `/explorer/mempool` | Mempool contents |
| `/explorer/block/{index}` | Block detail |
| `/explorer/tx/{txid}` | Transaction detail (with EVM badges for type + status) |
| `/explorer/address/{address}` | Address page (balance, history) |
| `/explorer/validator/{address}` | Validator detail |
| `/explorer/token/{contract}` | Token detail |

---

## JSON-RPC Methods

POST to `/rpc` with `Content-Type: application/json`.

### Ethereum-compatible

| Method | Description |
|--------|-------------|
| `eth_chainId` | Chain ID (hex) |
| `eth_blockNumber` | Latest block number (hex) |
| `eth_getBalance` | Account balance in wei (hex) |
| `eth_getTransactionCount` | Account nonce (hex) |
| `eth_getCode` | Contract bytecode at address |
| `eth_getStorageAt` | Contract storage slot value |
| `eth_call` | Read-only EVM execution (no tx, no gas cost) |
| `eth_estimateGas` | Estimate gas for a tx |
| `eth_gasPrice` | Current gas price (hex) |
| `eth_sendRawTransaction` | Submit RLP-encoded signed Ethereum tx |
| `eth_getTransactionByHash` | Tx detail by hash |
| `eth_getTransactionReceipt` | Tx receipt (status, gasUsed, blockNumber) |
| `eth_getBlockByNumber` | Block by number |
| `eth_getBlockByHash` | Block by hash |
| `net_version` | Network ID (string) |
| `net_listening` | Always `true` |

### Sentrix-specific

| Method | Description |
|--------|-------------|
| `sentrix_sendTransaction` | Submit a pre-signed Sentrix native transaction |
| `sentrix_getBalance` | Balance in wei (hex, × 10^10 conversion from sentri) |

### Sentrix Native Methods (Sprint 1 — 2026-04-19)

Methods that expose chain features MetaMask / `eth_*` cannot represent:
DPoS validator set, delegation ledger, BFT finality, staking rewards.
All amounts returned in **wei hex** (`sentri × 10^10`) so the same
bignum libraries used for `eth_*` work here too.

#### `sentrix_getValidatorSet`

Returns the current DPoS validator set plus voting power distribution.

Request:
```json
{"jsonrpc":"2.0","method":"sentrix_getValidatorSet","params":[],"id":1}
```

Response:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "consensus": "PoA",
    "active_count": 3,
    "total_count": 3,
    "total_active_stake": "0x0",
    "epoch_number": 0,
    "validators": [
      {
        "address": "0x...",
        "name": "Foundation",
        "stake": "0x0",
        "commission": 0.0,
        "status": "active",
        "blocks_produced_epoch": 1234,
        "uptime": 1.0,
        "voting_power": "0x..."
      }
    ]
  }
}
```

`consensus` ∈ `"PoA" | "DPoS"`. The DPoS branch populates real stakes,
commissions, slashing state (`jailed`, `tombstoned`); the PoA branch
returns a flat voting power split equally across active validators
and zeroes for stake/commission — those fields become meaningful
post-Voyager when the DPoS registry takes over.

`status` ∈ `"active" | "jailed" | "tombstoned" | "unbonding"` (DPoS)
or `"active" | "unbonding"` (PoA).
`commission` is a float 0..1 (basis points ÷ 10 000 in DPoS mode).
`uptime` = `blocks_signed / (blocks_signed + blocks_missed)` in DPoS,
always `1.0` in PoA (the manager tracks `is_active` only).

#### `sentrix_getDelegations`

Returns the delegations made by one address. Active delegations plus
entries currently unbonding. Params: `[address: string]`.

```json
{"jsonrpc":"2.0","method":"sentrix_getDelegations","params":["0xdel..."],"id":1}
```

Each row:

```json
{
  "validator": "0xval...",
  "validator_name": "Foundation",
  "amount": "0x... (wei)",
  "pending_reward": "0x... (wei, estimate)",
  "delegated_at_epoch": 12,
  "status": "active",
  "unbonding_complete_epoch": null
}
```

`pending_reward` is a stake-weighted estimate against the validator's
pending pool — per-delegator reward accounting is not yet persisted
(tracked for a later sprint).

#### `sentrix_getStakingRewards`

Returns reward accrual for a delegator. Params:
`[address: string, { from_epoch?: u64, to_epoch?: u64 }]`. Default window
is the last 30 epochs.

```json
{
  "total_lifetime": "0x... (wei)",
  "pending_claimable": "0x... (wei)",
  "from_epoch": 14,
  "to_epoch": 44,
  "by_epoch": [
    { "epoch": 44, "validator": "0xval...", "reward": "0x...", "claimed": false }
  ]
}
```

Historical per-epoch per-delegator rewards are not persisted in the
current chain state; the response returns the current epoch's
stake-weighted share. Exact per-claim history requires the
reward-ledger follow-up.

#### `sentrix_getBftStatus`

Returns consensus mode + finality view.

PoA (mainnet today):

```json
{
  "consensus": "PoA",
  "current_leader": "0x...",
  "last_finalized_height": 44700,
  "last_finalized_hash": "..."
}
```

BFT (post-Voyager / testnet):

```json
{
  "consensus": "BFT",
  "current_round": null,
  "current_view": null,
  "current_leader": "0xproposer_for_next_round_0",
  "phase": null,
  "rounds_since_last_block": 0,
  "last_finalized_height": 21300,
  "last_finalized_hash": "..."
}
```

Live `current_round` / `phase` require the validator-loop `BftEngine`
snapshot to be published into shared state — tracked as a Sprint 2
dependency. Chain-side fields (leader, finality) are always populated.

#### `sentrix_getFinalizedHeight`

Shortcut to the finality view from `sentrix_getBftStatus`.

```json
{
  "finalized_height": 44700,
  "finalized_hash": "...",
  "latest_height": 44700,
  "blocks_behind_finality": 0
}
```

PoA: `finalized_height == latest_height` (instant finality per
round-robin signer). BFT: `latest - finalized` = number of blocks
still in the pipeline.

### Batch requests

Send an array of JSON-RPC objects. Max batch size: 100.

```json
[
  {"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1},
  {"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":2}
]
```

---

## Rate Limits

| Scope | Limit | Window |
|-------|-------|--------|
| Global (all endpoints) | 60 req/IP | 60s |
| Write endpoints (POST /transactions, /tokens/*, /rpc) | 10 req/IP | 60s |
| Body size | 1 MiB max | per request |
| Batch RPC | 100 items max | per request |
| Concurrency | 500 simultaneous | per node |

---

## Authentication

POST endpoints require `X-API-Key` header when `SENTRIX_API_KEY` env var is set on the node. If not set, all requests are allowed (development mode).

```bash
curl -X POST http://localhost:8545/transactions \
  -H "Content-Type: application/json" \
  -H "X-API-Key: your-api-key-here" \
  -d '{"transaction": { ... }}'
```

---

## Root response

`GET /` returns the node self-describe payload used by wallets and
explorers for chain discovery.

```json
{
  "name": "Sentrix",
  "version": "2.0.0",
  "chain_id": 7119,
  "consensus": "PoA",
  "native_token": "SRX",
  "docs": {
    "rpc_jsonrpc": "POST /rpc",
    "rest": {
      "chain_info": "/chain/info",
      "blocks": "/chain/blocks",
      "transactions": "/transactions",
      "accounts": "/accounts/{address}",
      "tokens": "/tokens",
      "validators": "/validators",
      "staking": "/staking",
      "epoch": "/epoch/current",
      "mempool": "/mempool"
    },
    "ops": {
      "health": "/health",
      "metrics": "/metrics",
      "explorer_builtin": "/explorer"
    }
  },
  "jsonrpc_namespaces": {
    "eth_": "Ethereum-compatible (MetaMask, ethers.js, Hardhat)",
    "net_": "Network info",
    "web3_": "Client version",
    "sentrix_": "Native Sentrix (validators, BFT, staking, delegations, finality)"
  }
}
```

`consensus` is derived from `chain_id` (`7119` → `PoA`, otherwise →
`BFT`). `native_token` is `SRX` on every network. The endpoint map
lists the canonical path per resource — handlers are registered in
`crates/sentrix-rpc/src/routes.rs`.

---

## Error Format

REST errors:
```json
{"success": false, "error": "error message"}
```

JSON-RPC errors:
```json
{"jsonrpc":"2.0","error":{"code":-32602,"message":"invalid params"},"id":1}
```

Standard JSON-RPC error codes: -32700 (parse), -32600 (invalid request), -32601 (method not found), -32602 (invalid params), -32603 (internal).
