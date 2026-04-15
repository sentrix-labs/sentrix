# Networks

## Mainnet

| | |
|-|-|
| Chain ID | 7119 (0x1bcf) |
| RPC | https://sentrix-rpc.sentriscloud.com |
| Explorer | https://sentrixscan.sentriscloud.com |
| P2P port | 30303 |
| API port | 8545 |
| Block time | 3s |
| Validators | 7 (PoA round-robin) |
| Native coin | SRX |

## Testnet

| | |
|-|-|
| Chain ID | 7120 (0x1bd0) |
| RPC | https://testnet-rpc.sentriscloud.com |
| P2P port | 31303 |
| API port | 9545 |
| Block time | 3s |
| Validators | 1 |

Testnet tokens have no real value. Use the faucet to get test SRX.

## Connecting

### MetaMask

Add network manually:

| Field | Mainnet | Testnet |
|-------|---------|---------|
| Network Name | Sentrix | Sentrix Testnet |
| RPC URL | https://sentrix-rpc.sentriscloud.com | https://testnet-rpc.sentriscloud.com |
| Chain ID | 7119 | 7120 |
| Symbol | SRX | SRX |
| Explorer | https://sentrixscan.sentriscloud.com | — |

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
