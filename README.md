# Sentrix

Fast, secure Layer-1 blockchain built in Rust.

[![CI/CD](https://github.com/satyakwok/sentrix/actions/workflows/ci.yml/badge.svg)](https://github.com/satyakwok/sentrix/actions)
[![Tests](https://img.shields.io/badge/tests-479%20passing-brightgreen)](https://github.com/satyakwok/sentrix/actions)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](Cargo.toml)
[![Chain ID](https://img.shields.io/badge/chain%20ID-7119-blue)](docs/operations/NETWORKS.md)
[![License](https://img.shields.io/badge/license-BUSL--1.1-purple)](LICENSE)

---

## What is Sentrix?

Sentrix (SRX) is a purpose-built Layer-1 blockchain with 3-second block times, instant finality, and Ethereum-compatible tooling. MetaMask, ethers.js, and web3.js connect natively.

- **v1.1.0** — DPoS + BFT consensus live on testnet, PoA on mainnet
- **479 tests**, clippy clean, 11 security audit rounds
- **7 validators** across 3 VPS, zero-downtime CI/CD

## Features

| | |
|---|---|
| **Consensus** | PoA round-robin (Pioneer) + DPoS/BFT (Voyager testnet) |
| **Finality** | Instant — BFT 2/3+1 vote-based on testnet |
| **State** | Binary Sparse Merkle Tree (BLAKE3 + SHA-256) with proofs |
| **Tokens** | SRX-20 standard (ERC-20 compatible) |
| **Network** | libp2p + Noise XX + Kademlia + Gossipsub |
| **API** | REST (25+ endpoints) + JSON-RPC 2.0 (20 methods) |
| **Explorer** | Built-in dark-themed block explorer |
| **Wallet** | AES-256-GCM keystore (Argon2id KDF) |
| **Fee model** | 50% burn / 50% validator (deflationary) |

## Quick Start

```bash
# Build
git clone https://github.com/satyakwok/sentrix.git
cd sentrix
cargo build --release

# Test
cargo test    # 479 tests

# Run a node
SENTRIX_VALIDATOR_KEY=<key> ./target/release/sentrix start --port 30303

# Check health
curl http://localhost:8545/health
```

## Architecture

```
src/
├── core/           # Blockchain engine, consensus, state trie, tokens
├── network/        # libp2p P2P: Noise XX, Kademlia, Gossipsub
├── api/            # REST + JSON-RPC + block explorer
├── wallet/         # Key generation, Argon2id keystore
└── storage/        # sled embedded database
```

Single binary — node, API, explorer, CLI all ship as one ~12 MB executable.

## Network

| | Mainnet | Testnet |
|---|---|---|
| **Chain ID** | 7119 | 7120 |
| **RPC** | [sentrix-rpc.sentriscloud.com](https://sentrix-rpc.sentriscloud.com) | [testnet-rpc.sentriscloud.com](https://testnet-rpc.sentriscloud.com) |
| **Consensus** | PoA (7 validators) | DPoS + BFT (4 validators) |

**Explorer:** [sentrixscan.sentriscloud.com](https://sentrixscan.sentriscloud.com)
**Wallet:** [sentrix-wallet.sentriscloud.com](https://sentrix-wallet.sentriscloud.com)
**Faucet:** [faucet.sentriscloud.com](https://faucet.sentriscloud.com)
**Telegram:** [t.me/SentrixCommunity](https://t.me/SentrixCommunity)

## Roadmap

| Phase | Status | Focus |
|-------|--------|-------|
| **Pioneer** | Live | PoA consensus, SRX-20 tokens, SentrixTrie, libp2p |
| **Voyager** | Testnet | DPoS + BFT finality, EVM (revm) |
| **Frontier** | Planned | Ecosystem expansion, dApps |
| **Odyssey** | Future | Cross-chain, mature ecosystem |

## Documentation

- [Architecture](docs/architecture/) — consensus, state, networking, transactions
- [Operations](docs/operations/) — deployment, CI/CD, monitoring, validators
- [Security](docs/security/) — audit reports, attack vectors, pentest results
- [Tokenomics](docs/tokenomics/) — SRX economics, staking, token standards
- [Roadmap](docs/roadmap/) — phase details, changelog

## Security

See [SECURITY.md](SECURITY.md) for vulnerability reporting.

11 audit rounds completed (116 findings, 78+ fixed). Pentest 6/6 passed on live network.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and PR process.

## License

[Business Source License 1.1](LICENSE) (BUSL-1.1). Converts to Apache 2.0 after the Change Date.
