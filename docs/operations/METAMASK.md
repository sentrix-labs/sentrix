# MetaMask Setup

Sentrix is fully MetaMask-compatible on both networks. Mainnet (chain ID 7119) and Testnet (chain ID 7120) both run Voyager DPoS+BFT consensus with EVM enabled — MetaMask reads balances, signs transactions, and deploys Solidity contracts on either network.

## Add Sentrix Testnet to MetaMask

1. Open MetaMask → Settings → Networks → Add a network → Add a network manually
2. Fill in:

   | Field | Value |
   |-------|-------|
   | **Network Name** | Sentrix Testnet |
   | **New RPC URL** | `https://testnet-rpc.sentrixchain.com/rpc` |
   | **Chain ID** | `7120` |
   | **Currency Symbol** | `SRX` |
   | **Block Explorer URL** | `https://scan.sentrixchain.com` |

3. Save. Switch to "Sentrix Testnet" in the network dropdown.

## Add Sentrix Mainnet

   | Field | Value |
   |-------|-------|
   | **Network Name** | Sentrix |
   | **New RPC URL** | `https://rpc.sentrixchain.com` |
   | **Chain ID** | `7119` |
   | **Currency Symbol** | `SRX` |
   | **Block Explorer URL** | `https://scan.sentrixchain.com` |

Mainnet supports `eth_sendRawTransaction` and Solidity contract deployment since the 2026-04-25 Voyager activation. Use mainnet for production deployments and testnet for development.

> **Block explorer.** `scan.sentrixchain.com` is the canonical block explorer — it covers both Sentrix's native TokenOp / StakingOp events + validator pages + label system AND the EVM-standard surface (ERC-20 token transfers, source verification, EIP-3091 URLs). Use it as the "Block Explorer URL" in MetaMask, the explorer URL for CoinGecko / CoinMarketCap listings, and the deeplink target for any wallet that expects an EIP-3091 explorer.

## Get Test SRX

Faucet: https://faucet.sentrixchain.com (or use a funded testnet wallet directly).

## Send SRX

Standard MetaMask Send flow. Gas price defaults to ~20 gwei (the network ignores this and uses base fee internally — actual on-chain fee is `MIN_TX_FEE = 0.0001 SRX`).

## Deploy a Contract via Remix

1. Open https://remix.ethereum.org
2. Write/paste a Solidity contract
3. Compile (Solidity 0.8.x recommended)
4. Deploy & Run Transactions tab → Environment → "Injected Provider — MetaMask"
5. Confirm MetaMask is on Sentrix Testnet
6. Click Deploy → confirm in MetaMask

The contract address appears in Remix and is queryable via `eth_getCode`.

## Verify Contract via curl

```bash
# Get deployed contract code
curl -X POST https://testnet-rpc.sentrixchain.com/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getCode","params":["0xCONTRACT_ADDR","latest"],"id":1}'

# Call a function (example: totalSupply() = 0x18160ddd)
curl -X POST https://testnet-rpc.sentrixchain.com/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_call","params":[{"to":"0xCONTRACT_ADDR","data":"0x18160ddd"},"latest"],"id":1}'
```

## Troubleshooting

**MetaMask shows wrong balance:**
- Sentrix returns balance in wei (sentri × 1e10) for compatibility
- 1 SRX = 100,000,000 sentri = 1,000,000,000,000,000,000 wei
- MetaMask interprets correctly using the 18-decimal convention

**Transaction stuck pending:**
- Check `eth_getTransactionReceipt` directly via curl — receipt may already exist
- Block time is 1s; expect confirmation within 1-2 blocks

**eth_call returns "0x":**
- The call executed but returned no data, or the call reverted silently
- Try with explicit `from` address that has balance

**Contract address differs from expected:**
- Sentrix uses standard `keccak256(rlp([sender, nonce]))[12:]` derivation
- Make sure you're computing from the correct nonce (use `eth_getTransactionCount` immediately before deploy)
