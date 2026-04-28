# Sentrix

**Where real assets live.**

Sentrix is the financial infrastructure for the real economy — starting with Indonesia. We bring real-world assets on-chain with Bitcoin's monetary discipline (fixed 315M supply, 4-year halving) and Ethereum's programmability (EVM-native, Solidity-ready) — built for Southeast Asia's 600 million people first, then the world.

[![Website](https://img.shields.io/badge/website-sentrixchain.com-8A5A11)](https://sentrixchain.com)
[![CI/CD](https://github.com/sentrix-labs/sentrix/actions/workflows/ci.yml/badge.svg)](https://github.com/sentrix-labs/sentrix/actions)
[![Release](https://img.shields.io/github/v/release/sentrix-labs/sentrix)](https://github.com/sentrix-labs/sentrix/releases/latest)
[![Tests](https://img.shields.io/badge/tests-551%2B%20passing-brightgreen)](https://github.com/sentrix-labs/sentrix/actions)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](Cargo.toml)
[![Chain ID](https://img.shields.io/badge/chain%20ID-7119-blue)](docs/operations/NETWORKS.md)
[![License](https://img.shields.io/badge/license-BUSL--1.1-purple)](LICENSE)

---

## What is Sentrix?

Sentrix (SRX) is a purpose-built Layer-1 blockchain with 1-second block times, instant finality, and Ethereum-compatible tooling. MetaMask, ethers.js, and web3.js connect natively. The chain serves as a settlement and tokenization layer for real-world assets — designed to bring institutional-grade financial primitives on-chain with the monetary discipline of Bitcoin and the programmability of Ethereum.

- **v2.1.47** — MDBX storage, 1s blocks, 5000 tx/block capacity, Voyager DPoS+BFT live on mainnet, EVM (revm 37) with `eth_call` wired to revm execution against real chain state, V4 reward distribution v2 active, **tokenomics v2 fork ACTIVE on mainnet since h=640800** (BTC-parity 4-year halving + 315M cap), `StakingOp::AddSelfStake` ACTIVE since h=731245, libp2p sync race-safe
- **551+ tests**, clippy clean, 11 security audit rounds
- **4 validators** across 4 nodes (Foundation, Treasury, Core, Beacon) on the maintainer fleet

## Features

| | |
|---|---|
| **Consensus** | DPoS + BFT (mainnet & testnet) — Voyager active |
| **Finality** | Instant — BFT 2/3+1 vote-based |
| **Storage** | libmdbx — memory-mapped B+ tree (used by Reth/Erigon) |
| **EVM** | revm 37 — Solidity contracts, MetaMask compatible (mainnet & testnet) |
| **State** | Binary Sparse Merkle Tree (BLAKE3 + SHA-256) with proofs |
| **Tokens** | SRC-20 native + SRC-20 (ERC-20 via EVM) |
| **Network** | libp2p + Noise XX + Kademlia + Gossipsub |
| **API** | REST (60+ endpoints) + JSON-RPC 2.0 (25 methods, incl. `sentrix_*` native namespace) |
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
cargo test    # 551+ tests

# Run a node
SENTRIX_VALIDATOR_KEY=<key> ./target/release/sentrix start --port 30303

# Check health
curl http://localhost:8545/health
```

## Connect MetaMask (Testnet)

| Field | Value |
|---|---|
| Network name | Sentrix Testnet |
| RPC URL | `https://testnet-rpc.sentrixchain.com/rpc` |
| Chain ID | `7120` |
| Symbol | `SRX` |
| Explorer | `https://scan.sentrixchain.com` (toggle to Testnet in UI) |

Full guide: [docs/operations/METAMASK.md](docs/operations/METAMASK.md). Deploy a smart contract via Remix: [docs/operations/SMART_CONTRACT_GUIDE.md](docs/operations/SMART_CONTRACT_GUIDE.md). EVM internals: [docs/architecture/EVM.md](docs/architecture/EVM.md).

## Architecture

```
crates/
├── sentrix-primitives/   Block, Transaction, Account, Error types
├── sentrix-codec/        Wire-format encoding helpers
├── sentrix-wire/         Wire-protocol message types
├── sentrix-wallet/       Keystore (Argon2id), wallet ops
├── sentrix-trie/         Binary Sparse Merkle Tree (MDBX backend)
├── sentrix-staking/      DPoS, epoch, slashing
├── sentrix-evm/          revm 37 adapter
├── sentrix-precompiles/  EVM precompiles
├── sentrix-bft/          BFT consensus (timeout-only round advance)
├── sentrix-core/         Blockchain, authority, executor, mempool, storage
├── sentrix-network/      libp2p P2P, gossipsub, kademlia
├── sentrix-rpc/          REST API, JSON-RPC, block explorer
├── sentrix-rpc-types/    Shared RPC request/response types
├── sentrix-storage/      MDBX wrapper + ChainStorage API
bin/sentrix/              CLI binary (main.rs at bin/sentrix/src/main.rs)
```

14 crates + 1 binary — node, API, explorer, CLI all ship as one executable.

## Network

| | Mainnet | Testnet |
|---|---|---|
| **Chain ID** | 7119 | 7120 |
| **RPC** | [rpc.sentrixchain.com](https://rpc.sentrixchain.com) | [testnet-rpc.sentrixchain.com](https://testnet-rpc.sentrixchain.com) |
| **Consensus** | DPoS + BFT (4 validators) | DPoS + BFT (4 validators) |
| **Block time** | 1 second | 1 second |
| **EVM** | Active — MetaMask compatible | Active — MetaMask compatible |
| **Explorer** | [scan.sentrixchain.com](https://scan.sentrixchain.com) | [scan.sentrixchain.com](https://scan.sentrixchain.com) (same unified UI, toggle Testnet) |

**Website:** [sentrixchain.com](https://sentrixchain.com)
**Faucet:** [faucet.sentrixchain.com](https://faucet.sentrixchain.com) (testnet)
**Wallet:** [solux.sentriscloud.com](https://solux.sentriscloud.com) (Solux web)
**Docs:** [sentrixchain.com/docs/faucet](https://sentrixchain.com/docs/faucet)
**Telegram:** [t.me/SentrixCommunity](https://t.me/SentrixCommunity)

## Roadmap

| Phase | Status | Focus |
|-------|--------|-------|
| **Pioneer** | Completed (mainnet h=0…579058) | PoA round-robin, MDBX storage, 1s blocks, SRC-20 tokens — succeeded by Voyager 2026-04-25 |
| **Voyager** | **Live on mainnet (v2.1.47)** | DPoS proposer rotation + BFT finality, EVM (revm 37) with `eth_call` against real chain state, `eth_sendRawTransaction`, L1 peer auto-discovery + connection-limits hardening, V4 reward distribution v2 (treasury escrow + ClaimRewards), runtime-aware Voyager dispatch, race-safe block sync, tokenomics v2 fork (315M cap + 4-year halving), `StakingOp::AddSelfStake` for non-phantom validator self-bond |
| **Frontier** | Phase F-1 scaffold landed; F-2…F-10 planned | Parallel transaction execution, sub-1s block time, mainnet hard fork |
| **Odyssey** | Future | Cross-chain bridges, mature ecosystem, light clients |

## Documentation

- [Architecture](docs/architecture/) — consensus, state, networking, transactions
- [Operations](docs/operations/) — deployment, CI/CD, monitoring, validators
- [Claim Rewards](docs/operations/CLAIM_REWARDS.md) — how validators + delegators claim escrowed rewards from `PROTOCOL_TREASURY`
- [Security](docs/security/) — audit reports, attack vectors, pentest results
- [Tokenomics](docs/tokenomics/) — SRX economics, staking, token standards
- [Roadmap](docs/roadmap/) — phase details, changelog

## Security

See [SECURITY.md](SECURITY.md) for vulnerability reporting.

11 audit rounds completed (116 findings, 78+ fixed). Pentest 6/6 passed on live network.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and PR process.

## Community

- **GitHub Discussions** — https://github.com/sentrix-labs/sentrix/discussions for design conversations, feature proposals, validator setup help, integration questions.
- **Org profile** — https://github.com/sentrix-labs for canonical contracts, brand kit, and other Sentrix Labs repos.

## License

[Business Source License 1.1](LICENSE) (BUSL-1.1). Converts to Apache 2.0 after the Change Date.
