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

## Protocol Token

Sentrix is **single-token at the protocol layer** — only SRX. SRC-20 contracts are an application-level capability for third-party deployers. There is no protocol-issued utility, governance, or stablecoin token. See [SRX.md](SRX.md).

## Planned

**SRC-20 (Voyager):** EVM-compatible via revm. Solidity ABI, gas metering, events. Deploy ERC-20 bytecode directly through `eth_sendRawTransaction`.

**SRC-721 (NFT):** Native NFT standard, spec at `docs/operations/SRC721_SPEC.md`. ERC-721 also deployable via EVM today.
