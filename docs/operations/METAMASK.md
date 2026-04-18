# MetaMask Setup

Sentrix Testnet is fully MetaMask-compatible (chain ID 7120). Mainnet (chain ID 7119) currently runs Pioneer PoA without EVM — MetaMask will read balances but contracts won't deploy until the Voyager hard fork activates EVM.

## Add Sentrix Testnet to MetaMask

1. Open MetaMask → Settings → Networks → Add a network → Add a network manually
2. Fill in:

   | Field | Value |
   |-------|-------|
   | **Network Name** | Sentrix Testnet |
   | **New RPC URL** | `https://testnet-rpc.sentriscloud.com/rpc` |
   | **Chain ID** | `7120` |
   | **Currency Symbol** | `SRX` |
   | **Block Explorer URL** | `https://sentrixscan.sentriscloud.com` |

3. Save. Switch to "Sentrix Testnet" in the network dropdown.

## Add Sentrix Mainnet (read-only for now)

   | Field | Value |
   |-------|-------|
   | **Network Name** | Sentrix |
   | **New RPC URL** | `https://sentrix-rpc.sentriscloud.com` |
   | **Chain ID** | `7119` |
   | **Currency Symbol** | `SRX` |
   | **Block Explorer URL** | `https://sentrixscan.sentriscloud.com` |

## Get Test SRX

Faucet: https://faucet.sentriscloud.com (or use a funded testnet wallet directly).

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
curl -X POST https://testnet-rpc.sentriscloud.com/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getCode","params":["0xCONTRACT_ADDR","latest"],"id":1}'

# Call a function (example: totalSupply() = 0x18160ddd)
curl -X POST https://testnet-rpc.sentriscloud.com/rpc \
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
