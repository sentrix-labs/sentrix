# Sentrix

**Open source, EVM-compatible L1 built in Rust.**

Real chain, real blocks, real code. Sentrix (SRX) is a purpose-built Layer-1 with 1-second blocks, instant BFT finality, and Ethereum-compatible tooling — MetaMask, ethers.js, viem, and hardhat connect natively. Bitcoin's monetary discipline (fixed 315M supply, 4-year halving) plus Ethereum's programmability (revm 38).


<div align="center">
  <a href="https://sentrixchain.com">
    <img src="https://cdn.jsdelivr.net/gh/sentrix-labs/brand-kit@227404b54a1d2314d9f6127d23cb1197ce2880b8/png-transparent/sentrix-256.png" alt="Sentrix Chain" width="128">
  </a>
</div>

[![Website](https://img.shields.io/badge/website-sentrixchain.com-8A5A11)](https://sentrixchain.com)
[![CI/CD](https://github.com/sentrix-labs/sentrix/actions/workflows/ci.yml/badge.svg)](https://github.com/sentrix-labs/sentrix/actions)
[![Coverage](https://codecov.io/gh/sentrix-labs/sentrix/branch/main/graph/badge.svg)](https://codecov.io/gh/sentrix-labs/sentrix)
[![Release](https://img.shields.io/github/v/release/sentrix-labs/sentrix)](https://github.com/sentrix-labs/sentrix/releases/latest)
[![Rust](https://img.shields.io/badge/rust-stable-orange)](Cargo.toml)
[![Chain ID](https://img.shields.io/badge/chain%20ID-7119-blue)](docs/operations/NETWORKS.md)
[![License](https://img.shields.io/badge/license-BUSL--1.1-purple)](LICENSE)
[![Whitepaper](https://img.shields.io/badge/whitepaper-v1.3.0-8A5A11)](https://github.com/sentrix-labs/whitepaper)

---

## What is Sentrix?

Sentrix (SRX) is a purpose-built Layer-1 blockchain with 1-second block times, instant BFT finality, and Ethereum-compatible tooling. MetaMask, ethers.js, viem, and web3.js connect natively to JSON-RPC. Power-user clients can use the Tonic-based **gRPC + gRPC-Web** transport for binary RPC and server-streaming block events.

- **Latest release: [v2.2.11](https://github.com/sentrix-labs/sentrix/releases/tag/v2.2.11)** — production binary on mainnet + testnet since 2026-05-13 (EVM value-transfer + gas-fix forks activated at testnet h=3,787,000 / mainnet h=1,748,900). Fully signed (CycloneDX SBOM + cosign keyless OIDC + SLSA Level 3 build provenance). See [CHANGELOG.md](CHANGELOG.md) for the full ship line.
- **4 validators** running Voyager DPoS + BFT on mainnet since 2026-04-25 (h=579,047). Tokenomics v2 fork active since h=640,800 (315M cap, 4-year halving).
- **17 workspace crates + 2 binaries**, clippy clean, multiple internal Sentrix Labs / SentrisCloud audit rounds.

## Features

| | |
|---|---|
| **Consensus** | DPoS + BFT (mainnet & testnet) — Voyager active |
| **Finality** | Instant — BFT 2/3+1 vote-based |
| **Storage** | libmdbx — memory-mapped B+ tree (used by Reth/Erigon) |
| **EVM** | revm 38 — Solidity contracts, MetaMask compatible (mainnet & testnet) |
| **State** | Binary Sparse Merkle Tree (BLAKE3 + SHA-256) with proofs |
| **Tokens** | SRC-20 native + SRC-20 (ERC-20 via EVM) |
| **Network** | libp2p + Noise XX + Kademlia + Gossipsub |
| **API** | REST + JSON-RPC 2.0 (incl. `sentrix_*` native namespace) + **Tonic gRPC + gRPC-Web** ([docs](docs/operations/GRPC.md)) — `GetBlock`, `GetBalance`, server-streaming `StreamEvents` |
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
cargo test --workspace

# Run a node
SENTRIX_VALIDATOR_KEY=<key> ./target/release/sentrix start --port 30303

# Check health
curl http://localhost:8545/health
```

## Run a validator

Sentrix is a permissioned-onboarding chain for now — the consensus is open and the binary is the same one anyone can build, but admission to the active set is co-signed by the chain admin (single-key today, N-of-M target). To run a node:

```bash
# One-line installer (Ubuntu 22.04 / 24.04, x86_64 or aarch64)
curl -fsSL https://raw.githubusercontent.com/sentrix-labs/sentrix/main/scripts/install-validator.sh | bash
```

The script handles pre-flight checks (RAM ≥ 8 GiB, swap ≥ 8 GiB persistent, disk ≥ 60 GiB), apt deps, Rust 1.95+ via rustup, source clone + `cargo build --release -p sentrix-node`, keystore generation, systemd unit, and start. It's idempotent — re-runs are repair, not clobber.

After the node is healthy, email **`validators@sentrixchain.com`** with your address + pubkey (printed by the installer) + intended self-stake (≥ 15,000 SRX) + ops contact. Activation height comes back; you appear in `GET /chain/info → validators` and at [scan.sentrixchain.com/validators](https://scan.sentrixchain.com/validators).

Full operator guide: **[docs.sentrixchain.com/operations/VALIDATOR_ONBOARDING](https://docs.sentrixchain.com/operations/VALIDATOR_ONBOARDING)** (hardware, security, monitoring, recovery paths).

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
├── sentrix-primitives/     Block, Transaction, Account, Error types
├── sentrix-codec/          Wire-format encoding helpers
├── sentrix-wire/           Wire-protocol message types
├── sentrix-wallet/         Keystore (Argon2id), wallet ops
├── sentrix-trie/           Binary Sparse Merkle Tree (MDBX backend)
├── sentrix-staking/        DPoS, epoch, slashing
├── sentrix-evm/            revm 38 adapter
├── sentrix-precompiles/    EVM precompiles
├── sentrix-bft/            BFT consensus (timeout-only round advance)
├── sentrix-core/           Blockchain, authority, executor, mempool, storage
├── sentrix-network/        libp2p P2P, gossipsub, kademlia
├── sentrix-rpc/            REST API, JSON-RPC, block explorer
├── sentrix-rpc-types/      Shared RPC request/response types
├── sentrix-storage/        MDBX wrapper + ChainStorage API
├── sentrix-proto/          Generated tonic types (published as `sentrix-proto` on crates.io)
├── sentrix-grpc/           Server-side gRPC handlers (depends on sentrix-proto)
└── sentrix-prom-exporter/  Prometheus metrics exporter
bin/
├── sentrix/                Node binary + CLI
└── sentrix-faucet/         Testnet faucet HTTP service
```

17 crates + 2 binaries. Node, API, explorer, CLI all ship as one executable.

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
**Docs:** [docs.sentrixchain.com](https://docs.sentrixchain.com)
**Faucet:** [faucet.sentrixchain.com](https://faucet.sentrixchain.com) (testnet)
**Wallet:** [solux.sentriscloud.com](https://solux.sentriscloud.com) (Solux web)
**Verifier:** [verify.sentrixchain.com](https://verify.sentrixchain.com) (Sourcify)
**gRPC + gRPC-Web:** [grpc.sentrixchain.com](https://grpc.sentrixchain.com) · [grpc-testnet.sentrixchain.com](https://grpc-testnet.sentrixchain.com)
**WebSocket:** `wss://api.sentrixchain.com/ws` (mainnet) · `wss://testnet-api.sentrixchain.com/ws` (testnet)
**Telegram:** [t.me/SentrixCommunity](https://t.me/SentrixCommunity)

## Roadmap

| Phase | Status | Focus |
|-------|--------|-------|
| **Pioneer** | Completed (mainnet h=0…579,046) | PoA round-robin, MDBX storage, 1s blocks, SRC-20 tokens — succeeded by Voyager 2026-04-25 |
| **Voyager** | **Live on mainnet** | DPoS proposer rotation + BFT finality, EVM (revm 38), V4 reward distribution v2 (treasury escrow + ClaimRewards), tokenomics v2 (315M cap + 4-year halving), `StakingOp::AddSelfStake`, side-car gRPC + gRPC-Web |
| **Frontier** | Phase F-1 scaffold landed; F-2…F-10 planned | Parallel transaction execution, sub-1s block time, mainnet hard fork |
| **Odyssey** | Future | Cross-chain bridges, mature ecosystem, light clients |

## Documentation

- **[Whitepaper](https://github.com/sentrix-labs/whitepaper)** — foundational paper (vision, mission, design philosophy, protocol depth). English and Bahasa Indonesia.
- [Architecture](docs/architecture/) — consensus, state, networking, transactions
- [Operations](docs/operations/) — deployment, CI/CD, monitoring, validators
- [Claim Rewards](docs/operations/CLAIM_REWARDS.md) — how validators + delegators claim escrowed rewards from `PROTOCOL_TREASURY`
- [Security](docs/security/) — audit reports, attack vectors, pentest results
- [Tokenomics](docs/tokenomics/) — SRX economics, staking, token standards
- [Roadmap](docs/roadmap/) — phase details, changelog

## Security

See [SECURITY.md](SECURITY.md) for vulnerability reporting. Internal audits live in [docs/security/](docs/security/) (V1 → V11 numbered code reviews, plus topical audits for BFT consensus, EVM integration, dependency supply chain, validator infra, tokenomics correctness). Pentest results: [docs/security/PENTEST_RESULTS.md](docs/security/PENTEST_RESULTS.md). No third-party audit firm has reviewed the chain code yet — pursued when budget + scope align, no committed timeline.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup and PR process.

## Community

- **GitHub Discussions** — https://github.com/sentrix-labs/sentrix/discussions for design conversations, feature proposals, validator setup help, integration questions.
- **Org profile** — https://github.com/sentrix-labs for canonical contracts, brand kit, and other Sentrix Labs repos.

## License

[Business Source License 1.1](LICENSE) (BUSL-1.1). Converts to Apache 2.0 after the Change Date.
