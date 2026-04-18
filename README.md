# Sentrix

Fast, secure Layer-1 blockchain built in Rust.

[![CI/CD](https://github.com/sentrix-labs/sentrix/actions/workflows/ci.yml/badge.svg)](https://github.com/sentrix-labs/sentrix/actions)
[![Release](https://img.shields.io/github/v/release/sentrix-labs/sentrix)](https://github.com/sentrix-labs/sentrix/releases/latest)
[![Tests](https://img.shields.io/badge/tests-551%20passing-brightgreen)](https://github.com/sentrix-labs/sentrix/actions)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](Cargo.toml)
[![Chain ID](https://img.shields.io/badge/chain%20ID-7119-blue)](docs/operations/NETWORKS.md)
[![License](https://img.shields.io/badge/license-BUSL--1.1-purple)](LICENSE)

---

## What is Sentrix?

Sentrix (SRX) is a purpose-built Layer-1 blockchain with 1-second block times, instant finality, and Ethereum-compatible tooling. MetaMask, ethers.js, and web3.js connect natively.

- **v2.0.0** — MDBX storage, 1s blocks, 5000 tx/block capacity, EVM on testnet
- **551 tests**, clippy clean, 11 security audit rounds
- **3 validators** across 3 nodes, zero-downtime rolling CI/CD

## Features

| | |
|---|---|
| **Consensus** | PoA round-robin (mainnet) + DPoS/BFT (testnet) |
| **Finality** | Instant — BFT 2/3+1 vote-based on testnet |
| **Storage** | libmdbx — memory-mapped B+ tree (used by Reth/Erigon) |
| **EVM** | revm 37 — Solidity contracts, MetaMask compatible (testnet) |
| **State** | Binary Sparse Merkle Tree (BLAKE3 + SHA-256) with proofs |
| **Tokens** | SRX-20 native + SRC-20 (ERC-20 via EVM) |
| **Network** | libp2p + Noise XX + Kademlia + Gossipsub |
| **API** | REST (25+ endpoints) + JSON-RPC 2.0 (20 methods) |
| **Explorer** | Built-in dark-themed block explorer |
| **Wallet** | AES-256-GCM keystore (Argon2id KDF) |
| **Fee model** | 50% burn / 50% validator (deflationary) |

## Quick Start

```bash
# Build
git clone https://github.com/sentrix-labs/sentrix.git
cd sentrix
cargo build --release

# Test
cargo test    # 551 tests

# Run a node
SENTRIX_VALIDATOR_KEY=<key> ./target/release/sentrix start --port 30303

# Check health
curl http://localhost:8545/health
```

## Connect MetaMask (Testnet)

| Field | Value |
|---|---|
| Network name | Sentrix Testnet |
| RPC URL | `https://testnet-rpc.sentriscloud.com/rpc` |
| Chain ID | `7120` |
| Symbol | `SRX` |
| Explorer | `https://testnet-scan.sentriscloud.com/explorer` |

Full guide: [docs/operations/METAMASK.md](docs/operations/METAMASK.md). Deploy a smart contract via Remix: [docs/operations/SMART_CONTRACT_GUIDE.md](docs/operations/SMART_CONTRACT_GUIDE.md). EVM internals: [docs/architecture/EVM.md](docs/architecture/EVM.md).

## Architecture

```
crates/
├── sentrix-primitives/   Block, Transaction, Account, Error types
├── sentrix-wallet/       Keystore (Argon2id), wallet ops
├── sentrix-trie/         Binary Sparse Merkle Tree (MDBX backend)
├── sentrix-staking/      DPoS, epoch, slashing
├── sentrix-evm/          revm 37 adapter
├── sentrix-bft/          BFT consensus (timeout-only round advance)
├── sentrix-core/         Blockchain, authority, executor, mempool, storage
├── sentrix-network/      libp2p P2P, gossipsub, kademlia
├── sentrix-rpc/          REST API, JSON-RPC, block explorer
├── sentrix-storage/      MDBX wrapper + ChainStorage API
bin/sentrix/              CLI binary
```

12 crates + 1 binary — node, API, explorer, CLI all ship as one executable.

## Network

| | Mainnet | Testnet |
|---|---|---|
| **Chain ID** | 7119 | 7120 |
| **RPC** | [sentrix-rpc.sentriscloud.com](https://sentrix-rpc.sentriscloud.com) | [testnet-rpc.sentriscloud.com](https://testnet-rpc.sentriscloud.com) |
| **Consensus** | PoA (3 validators) | DPoS + BFT (4 validators) |
| **Block time** | 1 second | 1 second |
| **EVM** | Disabled | Active — MetaMask compatible |
| **Explorer** | [sentrixscan.sentriscloud.com](https://sentrixscan.sentriscloud.com/explorer) | [testnet-scan.sentriscloud.com](https://testnet-scan.sentriscloud.com/explorer) |

**Wallet:** [sentrix-wallet.sentriscloud.com](https://sentrix-wallet.sentriscloud.com)
**Faucet:** [faucet.sentriscloud.com](https://faucet.sentriscloud.com)
**Telegram:** [t.me/SentrixCommunity](https://t.me/SentrixCommunity)

## Roadmap

| Phase | Status | Focus |
|-------|--------|-------|
| **Pioneer** | Live (mainnet v2.0.0) | PoA consensus, MDBX storage, 1s blocks, SRX-20 tokens |
| **Voyager** | Live (testnet) | DPoS + BFT finality, EVM (revm 37), eth_sendRawTransaction |
| **Frontier** | Planned | Mainnet hard fork, parallel execution, ecosystem |
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
