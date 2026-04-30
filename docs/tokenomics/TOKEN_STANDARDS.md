# Token Standards

## SRC-20

Fungible token standard, like ERC-20. Anyone can deploy.

### Deploy

```bash
sentrix token deploy --name "My Token" --symbol "MTK" --supply 1000000 --decimals 18 --deployer-key <key> --fee 100000
```

Creates contract at `SRC20_<sha256(txid)>`. Full supply minted to deployer. Fee: 50% burn, 50% ecosystem.

### Operations

Transfer: `sentrix token transfer --contract SRC20_... --to 0x... --amount 1000 --from-key <key> --gas 10000`

Burn: `sentrix token burn --contract SRC20_... --amount 500 --from-key <key> --gas 10000`

Approve: Allow a third party to spend your tokens. Must reset to 0 before setting a new allowance (prevents the classic ERC-20 front-running attack).

Mint: Owner only. Can't exceed max_supply set at deploy.

All operations need SRX for gas — you need SRX even if you're only moving tokens.

### API

| Endpoint | Method |
|----------|--------|
| `/tokens` | GET — list all tokens |
| `/tokens/{contract}` | GET — token details |
| `/tokens/{contract}/balance/{addr}` | GET — balance |
| `/tokens/deploy` | POST — deploy (needs API key) |
| `/tokens/{contract}/transfer` | POST — transfer (needs API key) |
| `/tokens/{contract}/burn` | POST — burn (needs API key) |

Explorer: `/explorer/tokens` and `/explorer/token/{contract}`.

### vs ERC-20

| | ERC-20 | SRC-20 |
|-|--------|--------|
| Gas token | ETH | SRX |
| Contract address | EVM deployment | `SRC20_` + deterministic hash |
| Approve front-run | Vulnerable | Mitigated (reset to 0 first) |
| Max supply | Optional | Built-in, enforced in mint() |
| VM | EVM | Native engine (Pioneer), revm (Voyager) |

## SRC-20 (Voyager)

EVM-compatible via revm. Solidity ABI, gas metering, events. Deploy via the `TokenFactory` canonical contract, via `TokenOp::Deploy`, or via any standard Solidity ERC-20 deployment — all three paths land in the same canonical state trie alongside account balances, and gas is always paid in SRX.

> **No protocol-issued SRC-20 token exists.** Earlier drafts proposed a chain-native utility token (SNTX) and stablecoin (SRTX) alongside SRX; that 3-token model was dropped before Voyager activation. Sentrix ships single-token (SRX-only) by design. Third-party tokens deployed on the chain are application-layer assets, the same way ERC-20s are application-layer on Ethereum.
