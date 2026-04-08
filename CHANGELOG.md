# Changelog

All notable changes to Sentrix Chain will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.1.0] — 2026-04-09

### Added

**Core blockchain engine**
- Proof of Authority (PoA) consensus with round-robin validator scheduling
- Account-based state model (Ethereum-style balance + nonce)
- ECDSA secp256k1 transaction signing and verification
- Two-pass atomic block validation (dry-run → commit)
- SHA-256 Merkle tree for transaction integrity
- Halving block reward schedule (1 SRX, halves every 42M blocks)
- Fee split: 50% validator / 50% permanently burned
- Genesis premine: 63,000,000 SRX across 4 strategic addresses

**SRX-20 token standard**
- Deploy fungible tokens in one CLI command
- Full ERC-20 compatible interface: transfer, approve, transfer_from, mint, balance_of, allowance
- Contract registry with deploy fee (50% burn / 50% ecosystem fund)
- Gas fee model: paid in SRX, split 50/50

**Wallet**
- ECDSA secp256k1 key generation
- Ethereum-style address derivation (Keccak-256, 0x format)
- AES-256-GCM encrypted keystore with PBKDF2-SHA256 (200k iterations)
- Import/export via private key hex

**Storage**
- sled embedded database (pure Rust, zero external deps)
- Per-block storage (1 sled key per block)
- Backward-compatible migration from single-blob format
- Block-by-hash and range queries

**REST API (19 endpoints)**
- Chain info, blocks, validation
- Account balance, nonce, transaction history
- Mempool management
- Validator list
- SRX-20 token operations (deploy, transfer, balance, info, list)
- Address info and history

**JSON-RPC 2.0 (20 methods)**
- Ethereum-compatible: eth_chainId, eth_blockNumber, eth_getBalance, eth_getBlockByNumber, etc.
- Single and batch request support
- MetaMask, ethers.js, web3.js compatible
- Chain ID: 7119 (0x1bcf)

**Block Explorer**
- Dark-themed web UI served directly from the binary
- Pages: home (stats + blocks), block detail, address detail, transaction detail, validators, tokens

**CLI (15 commands)**
- init, wallet (generate/import/info), validator (add/remove/toggle/list)
- chain (info/validate/block), balance, history
- token (deploy/transfer/balance/info/list)
- start (node with optional validator), genesis-wallets

**Networking**
- TCP P2P protocol with length-prefixed JSON messages
- 9 message types: Handshake, NewBlock, NewTransaction, GetChain, Ping/Pong, etc.
- Chain sync with sandbox validation

**Infrastructure**
- Single static binary (4.4 MB release)
- 81 tests across 10 suites
- Mempool priority fee ordering (highest fee first)

---

## [Unreleased]

### Planned
- Full P2P listener + message handler
- Multi-node deployment and testing
- GitHub Actions CI pipeline
- Public documentation site
