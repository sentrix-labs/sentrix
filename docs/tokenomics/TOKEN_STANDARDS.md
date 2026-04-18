# Token Standards

## SRX-20

Fungible token standard, like ERC-20. Anyone can deploy.

### Deploy

```bash
sentrix token deploy --name "My Token" --symbol "MTK" --supply 1000000 --decimals 18 --deployer-key <key> --fee 100000
```

Creates contract at `SRX20_<sha256(txid)>`. Full supply minted to deployer. Fee: 50% burn, 50% ecosystem.

### Operations

Transfer: `sentrix token transfer --contract SRX20_... --to 0x... --amount 1000 --from-key <key> --gas 10000`

Burn: `sentrix token burn --contract SRX20_... --amount 500 --from-key <key> --gas 10000`

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

| | ERC-20 | SRX-20 |
|-|--------|--------|
| Gas token | ETH | SRX |
| Contract address | EVM deployment | `SRX20_` + deterministic hash |
| Approve front-run | Vulnerable | Mitigated (reset to 0 first) |
| Max supply | Optional | Built-in, enforced in mint() |
| VM | EVM | Native engine (Pioneer), revm (Voyager) |

## SNTX

First SRC-20 token (ERC-20 via Sentrix EVM). Sentrix Utility.

| | |
|-|-|
| Supply | 10,000,000,000 |
| Ecosystem | 5B |
| Founder | 2B |
| Early Validator | 1.5B |
| Reserve | 1.5B |

Planned uses: governance, fee discounts, staking boosts.

## Planned

**SRC-20 (Voyager):** EVM-compatible via revm. Solidity ABI, gas metering, events.

**SRTX (Future):** USD-pegged stablecoin. SRX-collateralized.
