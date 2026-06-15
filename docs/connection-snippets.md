# Connecting to Sentrix — Client Library Snippets

Copy-paste snippets for the most common Web3 client libraries. Each example connects to Sentrix mainnet and reads a wallet balance and the latest block number.

## Network parameters

| | Mainnet | Testnet |
|---|---|---|
| **Chain ID** | `7119` | `7120` |
| **RPC URL** | `https://rpc.sentrixchain.com/rpc` | `https://testnet-rpc.sentrixchain.com/rpc` |
| **WebSocket** | `wss://rpc.sentrixchain.com/ws` | `wss://testnet-rpc.sentrixchain.com/ws` |
| **Currency** | SRX | SRX |

---

## ethers.js (v6)

```bash
npm install ethers
```

```js
import { JsonRpcProvider, formatEther } from "ethers";

const provider = new JsonRpcProvider("https://rpc.sentrixchain.com/rpc");

// Read a balance
const address = "0xYourAddressHere";
const balance = await provider.getBalance(address);
console.log("Balance:", formatEther(balance), "SRX");

// Read the latest block number
const blockNumber = await provider.getBlockNumber();
console.log("Latest block:", blockNumber);
```

For testnet, replace the RPC URL with `https://testnet-rpc.sentrixchain.com/rpc`.

---

## viem

Sentrix and Sentrix Testnet are built into [viem/chains](https://github.com/wevm/viem), so no manual chain definition is needed.

```bash
npm install viem
```

```ts
import { createPublicClient, http, formatEther } from "viem";
import { sentrix } from "viem/chains";

// Mainnet
const client = createPublicClient({
  chain: sentrix,
  transport: http(),
});

// Testnet
// const client = createPublicClient({ chain: sentrixTestnet, transport: http() });

// Read a balanceh
const address = "0xYourAddressHere" as `0x${string}`;
const balance = await client.getBalance({ address });
console.log("Balance:", formatEther(balance), "SRX");

// Read the latest block number
const blockNumber = await client.getBlockNumber();
console.log("Latest block:", blockNumber);
```

---

## web3.py

```bash
pip install web3
```

```python
from web3 import Web3

# Connect to Sentrix mainnet
w3 = Web3(Web3.HTTPProvider("https://rpc.sentrixchain.com/rpc"))

# Verify connection
assert w3.is_connected(), "Could not connect to Sentrix RPC"
print("Chain ID:", w3.eth.chain_id)   # 7119

# Read a balance
address = w3.to_checksum_address("0xYourAddressHere")
balance_wei = w3.eth.get_balance(address)
print("Balance:", w3.from_wei(balance_wei, "ether"), "SRX")

# Read the latest block number
block_number = w3.eth.block_number
print("Latest block:", block_number)
```

For testnet, replace the RPC URL with `https://testnet-rpc.sentrixchain.com/rpc` (Chain ID `7120`).

---

## WebSocket subscriptions

For real-time event streaming, connect via WebSocket:

```js
// ethers.js v6 — subscribe to new blocks
import { WebSocketProvider } from "ethers";

const provider = new WebSocketProvider("wss://rpc.sentrixchain.com/ws");

provider.on("block", (blockNumber) => {
  console.log("New block:", blockNumber);
});

// Clean up when done
// provider.destroy();
```

Full WebSocket subscription reference: [docs/operations/websocket-subscriptions](https://docs.sentrixchain.com/operations/websocket-subscriptions).
