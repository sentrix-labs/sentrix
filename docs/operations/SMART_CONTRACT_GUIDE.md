# Deploying Smart Contracts to Sentrix

Sentrix testnet runs an EVM (revm 37) and accepts standard Ethereum tooling. This guide walks through deploying a Solidity contract via Remix in under 5 minutes.

> **Mainnet:** EVM is currently disabled. Deploy on testnet for now.

## Prerequisites

- MetaMask installed
- A funded testnet address (use the [faucet](https://faucet.sentriscloud.com))
- Browser at [remix.ethereum.org](https://remix.ethereum.org)

## 1. Connect MetaMask to Sentrix Testnet

In MetaMask → **Networks → Add network manually**:

| Field | Value |
|---|---|
| Network name | `Sentrix Testnet` |
| New RPC URL | `https://testnet-rpc.sentriscloud.com/rpc` |
| Chain ID | `7120` |
| Currency symbol | `SRX` |
| Block explorer URL | `https://sentrixscan.sentriscloud.com` |

Save and switch to Sentrix Testnet. You should see your SRX balance.

## 2. Write a Contract in Remix

Open Remix → File Explorer → create `Token.sol`:

```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

contract MyToken {
    string public name = "MyToken";
    string public symbol = "MTK";
    uint8 public decimals = 18;
    uint256 public totalSupply = 1_000_000 * 10**18;

    mapping(address => uint256) public balanceOf;
    event Transfer(address indexed from, address indexed to, uint256 value);

    constructor() {
        balanceOf[msg.sender] = totalSupply;
        emit Transfer(address(0), msg.sender, totalSupply);
    }

    function transfer(address to, uint256 amount) external returns (bool) {
        require(balanceOf[msg.sender] >= amount, "insufficient balance");
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        emit Transfer(msg.sender, to, amount);
        return true;
    }
}
```

## 3. Compile

Solidity Compiler tab → version `0.8.20+` → **Compile Token.sol**.

## 4. Deploy

Deploy & Run Transactions tab:

1. **Environment:** `Injected Provider — MetaMask`
2. **Account:** confirm your Sentrix Testnet address is shown
3. **Contract:** `MyToken`
4. Click **Deploy**
5. MetaMask popup → confirm gas → sign

Once mined (~1 second), the deployed contract appears under **Deployed Contracts**.

## 5. Interact

Expand the deployed contract and call any function:

- **Read methods** (`name`, `symbol`, `totalSupply`, `balanceOf`) — free, no gas
- **Write methods** (`transfer`) — costs gas, signed via MetaMask

You can also verify the deployment via the explorer:

```
https://sentrixscan.sentriscloud.com/tx/<your-tx-hash>
```

## Hardhat / Foundry

Same network config:

```js
// hardhat.config.js
networks: {
  sentrixTestnet: {
    url: "https://testnet-rpc.sentriscloud.com/rpc",
    chainId: 7120,
    accounts: [process.env.PRIVATE_KEY],
  },
}
```

```toml
# foundry.toml
[rpc_endpoints]
sentrix_testnet = "https://testnet-rpc.sentriscloud.com/rpc"
```

## Gas Model

Sentrix uses EIP-1559:

- Base fee: 10,000 sentri (burned)
- Priority fee: tip to validator
- Block gas limit: 30,000,000
- Block target: 15,000,000

`eth_estimateGas` works natively. Most simple contracts deploy under 200K gas.

## Common Issues

| Symptom | Fix |
|---|---|
| `nonce too low` | MetaMask cached old nonce — Settings → Advanced → Reset account |
| `insufficient funds for gas` | Get test SRX from the faucet |
| `replacement transaction underpriced` | Increase gas price slightly when re-sending |
| Tx stuck "pending" | Reset MetaMask account; resend with same nonce + higher gas |

## Architecture

For implementation details (revm version, account model, base fee burn, fork heights), see [docs/architecture/EVM.md](../architecture/EVM.md).
