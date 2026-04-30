# Networks

## Mainnet

| | |
|-|-|
| Chain ID | 7119 (0x1bcf) |
| RPC | https://rpc.sentrixchain.com |
| Explorer | https://scan.sentrixchain.com — native TokenOps + StakingOps + validator UX, EIP-3091 compliant, ERC-20 transfers, source verification |
| P2P port | 30303 |
| API port | 8545 |
| Block time | 1s |
| Validators | 4 (DPoS proposer rotation under BFT finality: Foundation, Treasury, Core, Beacon) |
| Native coin | SRX |
| Consensus | **Voyager** (DPoS + BFT, `voyager_activated=true` since h=579047 / 2026-04-25) |
| EVM | Active — `evm_activated=true` since the same height; MetaMask compatible |
| Reward distribution | **V4 reward v2** active since h=590100 / 2026-04-25 — coinbase routes to `PROTOCOL_TREASURY` (0x0000…0002), validators + delegators claim via `ClaimRewards` staking op |
| Binary | v2.1.48 |

## Testnet

| | |
|-|-|
| Chain ID | 7120 (0x1bd0) |
| RPC | https://testnet-rpc.sentrixchain.com/rpc |
| Explorer | https://scan.sentrixchain.com (unified UI, toggle Testnet) |
| API | https://testnet-api.sentrixchain.com |
| P2P port | 31303–31306 |
| API port | 9545 |
| Block time | 1s |
| Validators | 4 (DPoS + BFT, 3/4 fault tolerance) |
| EVM | Active — Solidity contracts, MetaMask compatible |
| Voyager fork height | 10 |
| EVM fork height | 752 |

Testnet tokens have no real value. Use the faucet to get test SRX.

Testnet runs in Docker on build host (`/opt/sentrix-testnet-docker/`) since
the 2026-04-23 migration; fresh genesis at chain_id 7120, current
post tokenomics-v2 fork at h=381651, binary v2.1.48.

> **Mainnet operational note (2026-04-25, post-Voyager):** mainnet successfully
> transitioned from Pioneer PoA to Voyager DPoS+BFT at h=579047. EVM was
> activated in the same window. The first activation attempt at h=557244
> livelocked on a peer-mesh partition; root cause was fixed in v2.1.26
> (L1 multiaddr advertisements + L2 cold-start gate per PRs #297–#306)
> and v2.1.27 (cold-start race PR #307). The `SENTRIX_FORCE_PIONEER_MODE`
> emergency override is no longer set on any mainnet validator.

## Connecting

### MetaMask

Add network manually:

| Field | Mainnet | Testnet |
|-------|---------|---------|
| Network Name | Sentrix | Sentrix Testnet |
| RPC URL | https://rpc.sentrixchain.com | https://testnet-rpc.sentrixchain.com/rpc |
| Chain ID | 7119 | 7120 |
| Symbol | SRX | SRX |
| Explorer | https://scan.sentrixchain.com | https://scan.sentrixchain.com (toggle Testnet) |

> Sentrix uses a single block explorer: `scan.sentrixchain.com`. It covers both
> the native protocol surface (TokenOp / StakingOp events, validator pages,
> label system) and the EVM-standard surface (ERC-20 holders, source
> verification, EIP-3091-style URLs that wallets and listing sites expect).
> Use this URL in MetaMask's "Block Explorer URL" field, in CoinGecko /
> CoinMarketCap listings, and as the deeplink target for any wallet that
> follows EIP-3091.

### ethers.js

```js
import { JsonRpcProvider } from "ethers";

// Testnet (for development)
const provider = new JsonRpcProvider("https://testnet-rpc.sentrixchain.com");

// Mainnet (for production)
// const provider = new JsonRpcProvider("https://rpc.sentrixchain.com");

const height = await provider.getBlockNumber();
const balance = await provider.getBalance("0x...");
```

### curl

```bash
# Testnet (default for development)
curl -s https://testnet-rpc.sentrixchain.com/chain/info | jq

# Mainnet (production)
curl -s https://rpc.sentrixchain.com/chain/info | jq

# JSON-RPC
curl -X POST https://testnet-rpc.sentrixchain.com/rpc \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
```

### Running your own node

```bash
# Join mainnet
sentrix init --admin 0x<genesis_admin>
sentrix start --peers [BOOTSTRAP_NODE]:30303

# Join testnet
SENTRIX_CHAIN_ID=7120 SENTRIX_API_PORT=9545 \
  sentrix init --admin 0x<your_address>
SENTRIX_CHAIN_ID=7120 SENTRIX_API_PORT=9545 \
  sentrix start --port 31303 --peers [TESTNET_BOOTSTRAP]:31303
```

## How chain_id works

The binary reads `SENTRIX_CHAIN_ID` env var at startup. Default is 7119 (mainnet). Peers with different chain_id are rejected on handshake — mainnet and testnet can't talk to each other even if they share an IP.

Transactions include chain_id in the signing payload. A tx signed for mainnet (7119) is invalid on testnet (7120) and vice versa.
