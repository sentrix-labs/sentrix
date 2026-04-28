# Deploying Smart Contracts to Sentrix

Sentrix runs an EVM (revm 37) on both mainnet and testnet, and accepts standard Ethereum tooling. This guide walks through deploying a Solidity contract via Remix in under 5 minutes.

> **Network choice:** Use **testnet** (chain ID 7120) for development — get free SRX from the [faucet](https://faucet.sentrixchain.com). Use **mainnet** (chain ID 7119) for production deployments. EVM has been live on mainnet since the 2026-04-25 Voyager activation.

## Prerequisites

- MetaMask installed
- A funded testnet address (use the [faucet](https://faucet.sentrixchain.com))
- Browser at [remix.ethereum.org](https://remix.ethereum.org)

## 1. Connect MetaMask to Sentrix Testnet

In MetaMask → **Networks → Add network manually**:

| Field | Value |
|---|---|
| Network name | `Sentrix Testnet` |
| New RPC URL | `https://testnet-rpc.sentrixchain.com/rpc` |
| Chain ID | `7120` |
| Currency symbol | `SRX` |
| Block explorer URL | `https://scan.sentrixchain.com` |

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
https://scan.sentrixchain.com/tx/<your-tx-hash>
```

## Hardhat / Foundry

Same network config:

```js
// hardhat.config.js
networks: {
  sentrixTestnet: {
    url: "https://testnet-rpc.sentrixchain.com/rpc",
    chainId: 7120,
    accounts: [process.env.PRIVATE_KEY],
  },
}
```

```toml
# foundry.toml
[rpc_endpoints]
sentrix_testnet = "https://testnet-rpc.sentrixchain.com/rpc"
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

## Canonical Contracts (deployed on both chains)

For most dApp use cases, you don't need to deploy your own infrastructure contracts — Sentrix has a canonical set already live:

| Contract | Mainnet (7119) | Testnet (7120) | Use case |
|---|---|---|---|
| **WSRX** (wrapped SRX, ERC-20) | `0x4693b113e523A196d9579333c4ab8358e2656553` | `0x85d5E7694AF31C2Edd0a7e66b7c6c92C59fF949A` | DEX integration, ERC-20-only protocols |
| **Multicall3** (batch read calls) | `0xFd4b34b5763f54a580a0d9f7997A2A993ef9ceE9` | `0x7900826De548425c6BE56caEbD4760AB0155Cd54` | Efficient frontend reads (used by ethers.js, viem, wagmi out of the box) |
| **TokenFactory** (one-tx SRC-20 deploy) | `0xc753199b723649ab92c6db8A45F158921CFDEe49` | `0x7A2992af0d4979aDD076347666023d66d29276Fc` | Deploy minimal ERC-20 tokens without writing/auditing code yourself |
| **SentrixSafe** (multisig wallet contract) | `0x6272dC0C842F05542f9fF7B5443E93C0642a3b26` | `0xc9D7a61D7C2F428F6A055916488041fD00532110` | Multi-party treasury for your dApp / DAO |

**Source code, ABIs, and integration examples:** [`sentrix-labs/canonical-contracts`](https://github.com/sentrix-labs/canonical-contracts) (BUSL-1.1 / MIT mix; see repo for per-contract licensing).

**Why use the canonical set?**
- **WSRX** is required if you're building or integrating with DEX/lending — most protocols only accept ERC-20 tokens, not native SRX
- **Multicall3** address matches the well-known `mds1/multicall` deployment on other chains (the JS libraries auto-detect it)
- **TokenFactory** lets non-Solidity-experts deploy a token in one transaction, no copy-paste-deploy ritual needed
- **SentrixSafe** is the same contract Sentrix's own treasury uses (currently 1-of-1, expansion-ready)

For complete walkthrough — deploying via Hardhat / Foundry / wagmi / viem — see [`canonical-contracts/docs/INTEGRATION.md`](https://github.com/sentrix-labs/canonical-contracts/blob/main/docs/INTEGRATION.md).

## Decimal Note

Native SRX uses **8 decimals** (1 SRX = 100,000,000 sentri). WSRX uses **18 decimals** to match ERC-20 convention. The `wrap()` and `unwrap()` functions on WSRX handle the 1 SRX → 10^10 wSRX conversion automatically — but if you're reading raw chain state or building integrations directly, divide native SRX values by 1e8 (not 1e18).

## Architecture

For implementation details (revm version, account model, base fee burn, fork heights), see [docs/architecture/EVM.md](../architecture/EVM.md).
