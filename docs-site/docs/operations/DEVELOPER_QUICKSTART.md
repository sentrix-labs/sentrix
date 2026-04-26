# Developer Quickstart

Build on Sentrix in 10 minutes. Deploy a smart contract, read chain state, and send transactions.

## Connect to Sentrix

### Testnet (recommended for development)

| | |
|---|---|
| RPC URL | `https://testnet-rpc.sentriscloud.com/rpc` |
| Chain ID | `7120` |
| Explorer | `https://sentrixscan.sentriscloud.com` |
| Faucet | `https://faucet.sentriscloud.com` |

### MetaMask setup

Settings → Networks → Add network manually:

| Field | Value |
|---|---|
| Network name | `Sentrix Testnet` |
| RPC URL | `https://testnet-rpc.sentriscloud.com/rpc` |
| Chain ID | `7120` |
| Symbol | `SRX` |
| Block Explorer | `https://sentrixscan.sentriscloud.com` |

Get test SRX from the [faucet](https://faucet.sentriscloud.com).

## Deploy a Smart Contract (Remix)

1. Open [remix.ethereum.org](https://remix.ethereum.org)
2. Create `Token.sol`:

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

contract MyToken {
    string public name = "MyToken";
    uint256 public totalSupply = 1_000_000 * 10**18;
    mapping(address => uint256) public balanceOf;

    constructor() {
        balanceOf[msg.sender] = totalSupply;
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(balanceOf[msg.sender] >= amount);
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        return true;
    }
}
```

3. Compile → Solidity 0.8.20+
4. Deploy → Environment: "Injected Provider — MetaMask" → Deploy
5. Confirm in MetaMask → mined in ~1s

## ethers.js / viem

```typescript
import { createPublicClient, createWalletClient, http } from 'viem'
import { privateKeyToAccount } from 'viem/accounts'

// Read-only client
const client = createPublicClient({
  transport: http('https://testnet-rpc.sentriscloud.com/rpc'),
})

const height = await client.getBlockNumber()
const balance = await client.getBalance({ address: '0x...' })

// Signing client (for transactions)
const account = privateKeyToAccount('0x...')
const wallet = createWalletClient({
  account,
  transport: http('https://testnet-rpc.sentriscloud.com/rpc'),
})

const hash = await wallet.sendTransaction({
  to: '0x...',
  value: 1000000000000000000n, // 1 SRX in wei
})
```

## REST API

```bash
# Chain info
curl https://testnet-rpc.sentriscloud.com/chain/info

# Get balance
curl https://testnet-rpc.sentriscloud.com/accounts/0xYOUR_ADDRESS/balance

# List validators
curl https://testnet-rpc.sentriscloud.com/validators

# Prometheus metrics
curl https://testnet-rpc.sentriscloud.com/metrics
```

## JSON-RPC Methods

All standard Ethereum JSON-RPC methods are supported:

```bash
# Chain ID
curl -X POST https://testnet-rpc.sentriscloud.com/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'

# Block number
curl -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
  https://testnet-rpc.sentriscloud.com/rpc

# Get balance (returns wei)
curl -X POST -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getBalance","params":["0xYOUR_ADDRESS","latest"],"id":1}' \
  https://testnet-rpc.sentriscloud.com/rpc
```

Full method list: `eth_chainId`, `eth_blockNumber`, `eth_getBalance`, `eth_getTransactionCount`, `eth_getCode`, `eth_getStorageAt`, `eth_call`, `eth_estimateGas`, `eth_gasPrice`, `eth_sendRawTransaction`, `eth_getTransactionByHash`, `eth_getTransactionReceipt`, `eth_getBlockByNumber`, `eth_getBlockByHash`, `net_version`, `net_listening`.

## Hardhat / Foundry

```javascript
// hardhat.config.js
module.exports = {
  networks: {
    sentrixTestnet: {
      url: "https://testnet-rpc.sentriscloud.com/rpc",
      chainId: 7120,
      accounts: [process.env.PRIVATE_KEY],
    },
  },
};
```

```toml
# foundry.toml
[rpc_endpoints]
sentrix_testnet = "https://testnet-rpc.sentriscloud.com/rpc"
```

## Gas Model

Sentrix uses EIP-1559:

| Parameter | Value |
|---|---|
| Base fee | 10,000 sentri (burned) |
| Block gas limit | 30,000,000 |
| Block gas target | 15,000,000 |
| 1 SRX | 10^18 wei = 10^8 sentri |

## Next Steps

- [Smart Contract Guide](SMART_CONTRACT_GUIDE.md) — full Remix walkthrough
- [MetaMask Setup](METAMASK.md) — step-by-step with screenshots
- [API Reference](API_REFERENCE.md) — all REST + RPC endpoints
- [Network Info](NETWORKS.md) — mainnet vs testnet config
- [Validator Guide](VALIDATOR_GUIDE.md) — run your own node
