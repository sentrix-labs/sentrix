# Networks

## Mainnet

| | |
|-|-|
| Chain ID | 7119 (0x1bcf) |
| RPC | https://sentrix-rpc.sentriscloud.com |
| Explorer | https://sentrixscan.sentriscloud.com |
| P2P port | 30303 |
| API port | 8545 |
| Block time | 1s |
| Validators | 4 (PoA round-robin: Foundation, Treasury, Core, Beacon) |
| Native coin | SRX |
| Mode | Pioneer (forced via `SENTRIX_FORCE_PIONEER_MODE=1`; see ops note below) |

## Testnet

| | |
|-|-|
| Chain ID | 7120 (0x1bd0) |
| RPC | https://testnet-rpc.sentriscloud.com/rpc |
| Explorer | https://sentrixscan.sentriscloud.com (unified UI, toggle Testnet) |
| API | https://testnet-api.sentriscloud.com |
| P2P port | 31303–31306 |
| API port | 9545 |
| Block time | 1s |
| Validators | 4 (DPoS + BFT, 3/4 fault tolerance) |
| EVM | Active — Solidity contracts, MetaMask compatible |
| Voyager fork height | 10 |
| EVM fork height | 752 |

Testnet tokens have no real value. Use the faucet to get test SRX.

Testnet runs in Docker on VPS4 (`/opt/sentrix-testnet-docker/`) since
the 2026-04-23 migration; fresh genesis at chain_id 7120, current
height ~200K, binary v2.1.24 (`md5 a25f9d771648f6c851a6ee11867fe958`).

> **Mainnet operational note (2026-04-25):** mainnet currently runs
> forced Pioneer (`SENTRIX_FORCE_PIONEER_MODE=1` env override on every
> validator) after a Voyager activation attempt at h=557244 livelocked
> on V2 BFT wiring. Voyager mainnet activation is **blocked by issue
> [#292](https://github.com/sentrix-labs/sentrix/issues/292)**. Until
> #292 lands, `VOYAGER_FORK_HEIGHT=18446744073709551615` (u64::MAX)
> keeps the Voyager fork inert. Mainnet binary: v2.1.25
> (`md5 5ad7804c0d7e68f8cab47872f7dbc7ac`).

## Connecting

### MetaMask

Add network manually:

| Field | Mainnet | Testnet |
|-------|---------|---------|
| Network Name | Sentrix | Sentrix Testnet |
| RPC URL | https://sentrix-rpc.sentriscloud.com | https://testnet-rpc.sentriscloud.com/rpc |
| Chain ID | 7119 | 7120 |
| Symbol | SRX | SRX |
| Explorer | https://sentrixscan.sentriscloud.com | https://sentrixscan.sentriscloud.com (toggle Testnet) |

### ethers.js

```js
import { JsonRpcProvider } from "ethers";

// Testnet (for development)
const provider = new JsonRpcProvider("https://testnet-rpc.sentriscloud.com");

// Mainnet (for production)
// const provider = new JsonRpcProvider("https://sentrix-rpc.sentriscloud.com");

const height = await provider.getBlockNumber();
const balance = await provider.getBalance("0x...");
```

### curl

```bash
# Testnet (default for development)
curl -s https://testnet-rpc.sentriscloud.com/chain/info | jq

# Mainnet (production)
curl -s https://sentrix-rpc.sentriscloud.com/chain/info | jq

# JSON-RPC
curl -X POST https://testnet-rpc.sentriscloud.com/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
```

### Running your own node

```bash
# Join mainnet
sentrix init --admin 0x<genesis_admin>
sentrix start --peers [BOOTSTRAP_NODE]:30303

# Join testnet
SENTRIX_CHAIN_ID=7120 SENTRIX_API_PORT=9545 \
  sentrix init --admin 0x<your_address>
SENTRIX_CHAIN_ID=7120 SENTRIX_API_PORT=9545 \
  sentrix start --port 31303 --peers [TESTNET_BOOTSTRAP]:31303
```

## How chain_id works

The binary reads `SENTRIX_CHAIN_ID` env var at startup. Default is 7119 (mainnet). Peers with different chain_id are rejected on handshake — mainnet and testnet can't talk to each other even if they share an IP.

Transactions include chain_id in the signing payload. A tx signed for mainnet (7119) is invalid on testnet (7120) and vice versa.
