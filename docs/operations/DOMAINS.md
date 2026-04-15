# Domains

All Sentrix services run under `sentriscloud.com`. DNS managed via Cloudflare.

## Services

| Domain | What |
|--------|------|
| sentrix.sentriscloud.com | Landing page |
| sentrixscan.sentriscloud.com | Block explorer |
| sentrix-api.sentriscloud.com | REST API |
| sentrix-rpc.sentriscloud.com | Mainnet JSON-RPC |
| testnet-rpc.sentriscloud.com | Testnet JSON-RPC |
| sentrix-wallet.sentriscloud.com | Wallet UI |
| sentrixlaunch.sentriscloud.com | Token launchpad |
| coinblast.sentriscloud.com | CoinBlast |
| faucet.sentriscloud.com | Testnet faucet |

## Mainnet Endpoints

```
RPC:      https://sentrix-rpc.sentriscloud.com
API:      https://sentrix-api.sentriscloud.com
Explorer: https://sentrixscan.sentriscloud.com
Wallet:   https://sentrix-wallet.sentriscloud.com
Faucet:   https://faucet.sentriscloud.com
Chain ID: 7119
```

## Testnet Endpoints

```
RPC:      https://testnet-rpc.sentriscloud.com
Chain ID: 7120
```

## For Developers

Connect to testnet (for development and testing):
```
RPC URL:  https://testnet-rpc.sentriscloud.com
Chain ID: 7120
Symbol:   SRX
```

Testnet tokens have no real value. Use the faucet to get test SRX.

```bash
curl https://testnet-rpc.sentriscloud.com/chain/info
curl -X POST https://testnet-rpc.sentriscloud.com/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
```

## For Production

Connect to mainnet:
```
RPC URL:  https://sentrix-rpc.sentriscloud.com
Chain ID: 7119
Symbol:   SRX
```

## Community

- GitHub: https://github.com/satyakwok/sentrix
- Telegram: https://t.me/SentrixCommunity
