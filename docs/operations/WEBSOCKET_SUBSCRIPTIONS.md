---
sidebar_position: 19
title: WebSocket Subscriptions
---

# WebSocket subscriptions

Real-time chain events at `wss://rpc.sentrixchain.com/ws`. Standard `eth_subscribe` JSON-RPC plus Sentrix-native channels. Built so dApps using ethers.js, viem, web3.js work without special-casing Sentrix.

Shipped 2026-04-28 in PR #398 (Phase 1) + PR #399 (Phase 2+3).

## Endpoints

| Network | URL |
|---|---|
| Mainnet | `wss://rpc.sentrixchain.com/ws` |
| Testnet | `wss://testnet-rpc.sentrixchain.com/ws` |

## Channels — `eth_subscribe` namespace (EVM-compat)

| Channel | Payload | Trigger |
|---|---|---|
| `newHeads` | block header (number, hash, parentHash, timestamp, miner, stateRoot, transactionsRoot, gasLimit, gasUsed, difficulty, nonce, extraData, size) | every consensus-finalized block |
| `logs` | filterable contract event (address, topics, data, blockNumber, blockHash, transactionHash, transactionIndex, logIndex, removed) | per-tx event emission |
| `newPendingTransactions` | tx hash string | every successful mempool admission |
| `syncing` | always `false` (Sentrix has no syncing mode) | once on subscribe |

## Channels — `sentrix_subscribe` namespace (Sentrix-native)

| Channel | Payload | Trigger |
|---|---|---|
| `sentrix_finalized` | `{ height, hash, justificationSigners }` | every BFT-finalized block — distinct from `newHeads` because Sentrix has instant BFT finality |
| `sentrix_validatorSet` | `{ epoch, validators[] }` | every epoch-advance (active set rotation) |
| `sentrix_tokenOps` | `{ op, contract, from, to, amount, txid, blockHeight }` | every native TokenOp dispatched (Deploy, Transfer, Burn, Mint, Approve) |
| `sentrix_stakingOps` | `{ op, validator, delegator, amount, txid, blockHeight }` | every StakingOp dispatched (RegisterValidator, Delegate, Redelegate, Undelegate, ClaimRewards, Unjail, AddSelfStake, SubmitEvidence, JailEvidenceBundle) |
| `sentrix_jail` | `{ validator, epoch, missedBlocks, blockHeight }` | per-validator inside a JailEvidenceBundle dispatch (post-JAIL_CONSENSUS_HEIGHT) |

## Quick start

### ethers.js v6

```javascript
import { WebSocketProvider } from "ethers";

const provider = new WebSocketProvider("wss://rpc.sentrixchain.com/ws");

provider.on("block", (n) => console.log("new block:", n));

const filter = {
  address: "0x4693b113e523A196d9579333c4ab8358e2656553", // WSRX
  topics: [ethers.id("Transfer(address,address,uint256)")],
};
provider.on(filter, (log) => console.log("WSRX transfer:", log));
```

### viem

```typescript
import { createPublicClient, webSocket, parseAbiItem } from "viem";

const client = createPublicClient({
  transport: webSocket("wss://rpc.sentrixchain.com/ws"),
});

const unwatch = client.watchBlocks({
  onBlock: (block) => console.log("new block:", block.number),
});

const unwatchLogs = client.watchContractEvent({
  address: "0x4693b113e523A196d9579333c4ab8358e2656553",
  abi: [parseAbiItem("event Transfer(address indexed from, address indexed to, uint256 value)")],
  onLogs: (logs) => console.log("WSRX transfers:", logs),
});
```

### Raw JSON-RPC over WebSocket

```javascript
const ws = new WebSocket("wss://rpc.sentrixchain.com/ws");

ws.onopen = () => {
  // Subscribe to newHeads
  ws.send(JSON.stringify({
    jsonrpc: "2.0", id: 1, method: "eth_subscribe", params: ["newHeads"]
  }));

  // Subscribe to logs filtered by address + first topic
  ws.send(JSON.stringify({
    jsonrpc: "2.0", id: 2, method: "eth_subscribe",
    params: ["logs", {
      address: "0x4693b113e523A196d9579333c4ab8358e2656553",
      topics: ["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"]
    }]
  }));
};

ws.onmessage = (msg) => {
  const data = JSON.parse(msg.data);
  if (data.method === "eth_subscription") {
    console.log("event:", data.params.subscription, data.params.result);
  } else {
    console.log("response:", data);
  }
};
```

## Filters — `eth_subscribe(logs)`

Filter object follows `eth_getLogs` shape:

| Field | Type | Behaviour |
|---|---|---|
| `address` | string OR string[] | match contract address(es). Empty = match any |
| `topics` | (null \| string \| string[])[] | positional topic match. `null` = wildcard at that position. `[a, b]` = match either at that position |

Per-event match applied in the listener task — unsubscribed traffic never crosses the wire (saves bandwidth + reduces client parse cost).

## Method parity — non-subscribe methods over WS

The same WebSocket connection serves all 20+ HTTP `eth_*` methods (`eth_call`, `eth_blockNumber`, `eth_getBalance`, etc) — saves dApps having to maintain two connections. Internally HTTP and WS share the same dispatcher; 100% method parity.

```javascript
ws.send(JSON.stringify({
  jsonrpc: "2.0", id: 99, method: "eth_call",
  params: [{ to: "0x4693b113...", data: "0x06fdde03" }, "latest"]
}));
```

## Lifecycle + edge cases

| Scenario | Behaviour |
|---|---|
| `eth_unsubscribe(<sub_id>)` | aborts the listener task; returns `true` first time, `false` on second call |
| connection drops | server aborts every subscription task on that connection. client must reconnect + re-subscribe |
| client too slow → broadcast buffer fills (1024 events) | server emits `eth_subscription` message with `error: subscription lagged ({skipped} events skipped); reconnect to resume`, then drops the subscription. client must reconnect |
| subscribing to same channel twice | both succeed with distinct `sub_id`s |

## Limits

| Cap | Value | Why |
|---|---|---|
| Concurrent WS connections per source IP | 10 | Defense — guards against fd exhaustion from a single client |
| Concurrent subscriptions per connection | 100 | Defense — guards against tokio task exhaustion via repeated subscribe |
| Broadcast channel capacity | 1024 events | Lagged consumers get error + drop instead of OOM |

Over-limit IP → 503 at upgrade response (no socket established). Over-limit subscriptions → JSON-RPC error `-32005`.

## Subscription ID format

`0x` + 16 hex characters. Mirrors geth/erigon — opaque token, monotonic per connection. Don't depend on the value being parseable as a number.

## Smoke test from CLI

```bash
# Install wscat: npm install -g wscat
wscat -c wss://rpc.sentrixchain.com/ws

# After connect, paste:
{"jsonrpc":"2.0","id":1,"method":"eth_subscribe","params":["newHeads"]}

# Expected: subscription ID returned, then a stream of eth_subscription
# messages with block headers every ~1 second.
```

## See also

- [API Reference](./API_REFERENCE) — HTTP JSON-RPC + REST surface
- [SDK packages](https://github.com/sentrix-labs) — `@sentrix/sdk-js` (when published)
- ethers.js [WebSocketProvider docs](https://docs.ethers.org/v6/api/providers/#WebSocketProvider) — works as-is against Sentrix
- viem [webSocket transport](https://viem.sh/docs/clients/transports/websocket) — same
