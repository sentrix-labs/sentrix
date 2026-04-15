# Sentrix

**Layer-1 Proof-of-Authority blockchain — fast, predictable settlement in Rust.**

[![Build](https://github.com/satyakwok/sentrix/actions/workflows/ci.yml/badge.svg)](https://github.com/satyakwok/sentrix/actions)
[![Rust](https://img.shields.io/badge/rust-1.94+-orange)](https://www.rust-lang.org/)
[![Tests](https://img.shields.io/badge/tests-342%20passing-brightgreen)]()
[![License](https://img.shields.io/badge/license-BUSL--1.1-purple)](LICENSE)
[![Chain ID](https://img.shields.io/badge/chain%20ID-7119-blue)]()

---

## Overview

Sentrix (SRX) is a purpose-built Layer-1 blockchain for fast, predictable settlement. It uses a **Proof of Authority** consensus where authorized validators produce blocks in deterministic round-robin order — delivering **3-second block times** with instant finality.

Key design principles:

- **No mining, no staking** — block production is permissioned and validator-authorized
- **Ethereum-compatible** — `0x` addresses, ECDSA secp256k1 signatures, JSON-RPC 2.0; MetaMask and ethers.js connect natively
- **Cryptographically committed state** — every block includes a `state_root` (Binary Sparse Merkle Tree) from block 100,000 onward, making account state auditable and forkable
- **Single binary** — node, API server, block explorer, and CLI ship as one 4.4 MB binary

---

## Features

- **PoA round-robin consensus** — deterministic validator rotation, instant finality, no wasted compute
- **SentrixTrie** — Binary Sparse Merkle Tree (256-level, BLAKE3+SHA-256) for verifiable account state with membership and non-membership proofs
- **SRX-20 token standard** — ERC-20-compatible fungible tokens (deploy, transfer, burn, mint, approve) in one CLI command
- **libp2p transport** — TCP + Noise XX encrypted P2P with Yamux multiplexing; stable PeerId across restarts
- **Sliding window RAM model** — last 1,000 blocks in RAM; full history in sled; O(1) startup regardless of chain height
- **REST API + JSON-RPC 2.0** — 19 REST endpoints and 20 JSON-RPC methods on a single port
- **Built-in block explorer** — dark-themed 12-page web UI served from the binary; no external dependencies
- **AES-256-GCM encrypted keystores** — Argon2id key derivation (m=65536, t=3, p=4); PBKDF2 v1 backward compatible
- **Deflationary fee model** — 50% of all transaction fees permanently burned; 50% to the block validator
- **Graceful shutdown** — SIGTERM/SIGINT handler saves state before exit; no in-flight corruption

---

## Quick Start

### Prerequisites

- Rust 1.94+ — `rustup install stable`
- Linux (production) or Windows/macOS (development)

### Build

```bash
git clone https://github.com/satyakwok/sentrix.git
cd sentrix
cargo build --release
```

### Run tests

```bash
cargo test
# 342 tests across 11 suites — unit + integration
```

### Bootstrap a local node

```bash
# 1. Generate an admin wallet
./target/release/sentrix wallet generate

# 2. Initialize the chain
./target/release/sentrix init --admin 0xYOUR_ADDRESS

# 3. Add a validator
./target/release/sentrix validator add 0xVAL_ADDR "My Validator" VAL_PUBKEY

# 4. Start the node (validator mode)
SENTRIX_VALIDATOR_KEY=YOUR_PRIVATE_KEY ./target/release/sentrix start --port 30303

# 5. Verify
./target/release/sentrix chain info
curl http://localhost:8545/health
# Block explorer: http://localhost:8545/explorer
```

---

## Architecture

```
sentrix/
├── src/
│   ├── main.rs                  # CLI entry point, node orchestration
│   ├── core/
│   │   ├── blockchain.rs        # Blockchain struct, constants, genesis
│   │   ├── block.rs             # Block structure, hash computation
│   │   ├── block_producer.rs    # create_block() — mempool selection
│   │   ├── block_executor.rs    # add_block() — two-pass validation + commit
│   │   ├── transaction.rs       # Transaction types, signing, coinbase
│   │   ├── mempool.rs           # Mempool: priority queue, TTL, limits
│   │   ├── account.rs           # AccountDB: balance + nonce
│   │   ├── authority.rs         # ValidatorSet, admin operations, audit log
│   │   ├── vm.rs                # SRX-20 ContractRegistry (deploy/transfer/mint/burn)
│   │   ├── token_ops.rs         # Token operation encoding/decoding
│   │   ├── chain_queries.rs     # History, stats, rich list, address info
│   │   ├── merkle.rs            # SHA-256 Merkle tree (tx integrity)
│   │   └── trie/
│   │       ├── tree.rs          # SentrixTrie — 256-level Binary Sparse Merkle Tree
│   │       ├── storage.rs       # sled-backed trie persistence + GC
│   │       ├── cache.rs         # LRU cache over storage
│   │       ├── proof.rs         # Merkle inclusion/non-inclusion proofs
│   │       ├── node.rs          # TrieNode (Empty / Leaf / Branch)
│   │       └── address.rs       # Address → 256-bit key, account value encoding
│   ├── network/
│   │   ├── libp2p_node.rs       # libp2p swarm: Noise XX, sync, broadcast
│   │   ├── behaviour.rs         # SentrixBehaviour (Identify + RequestResponse)
│   │   ├── node.rs              # Legacy node interface, rate limiting
│   │   └── sync.rs              # Chain sync protocol
│   ├── api/
│   │   ├── routes.rs            # Axum REST router, middleware, handlers
│   │   ├── jsonrpc.rs           # JSON-RPC 2.0 dispatcher (20 methods)
│   │   └── explorer.rs          # Block explorer HTML generation
│   ├── storage/
│   │   └── db.rs                # sled storage: per-block, state, hash index
│   ├── wallet/
│   │   ├── wallet.rs            # Key generation, address derivation
│   │   └── keystore.rs          # AES-256-GCM encrypted keystore (Argon2id v2)
│   └── types/
│       └── error.rs             # SentrixError enum
└── tests/                       # 9 integration test suites
```

**Data flow — block production:**

```
Mempool → create_block() → add_block() [Pass 1: validate] → [Pass 2: commit]
       → update_trie_for_block() → state_root → broadcast via libp2p
```

**State root enforcement** (block ≥ 100,000):
Blocks include `state_root` in their hash. Peers that compute a different root have their block rejected — guaranteeing consensus on account state across all validators.

---

## CLI Reference

All commands use the `sentrix` binary. Private keys should be passed via environment variables, not CLI flags.

### Chain

```bash
sentrix init --admin <address>          # Initialize a new chain with admin address
sentrix chain info                      # Show chain statistics (height, supply, validators)
sentrix chain validate                  # Verify integrity of the in-memory chain window
sentrix chain block <index>             # Show full block details by index
sentrix chain reset-trie                # Drop trie state for rebuild on next startup
```

### Wallet

```bash
sentrix wallet generate [--password <pw>]          # Generate new wallet (keystore or raw)
sentrix wallet import <private_key_hex> [--password] # Import from private key hex
sentrix wallet info <keystore.json>                # Inspect a keystore file
```

### Validator Management (admin only)

```bash
sentrix validator add <address> <name> <pubkey>    # Add a new validator
sentrix validator remove <address>                 # Remove a validator
sentrix validator toggle <address>                 # Toggle validator active/inactive
sentrix validator rename <address> <new_name>      # Rename a validator
sentrix validator list                             # List all validators and stats

# Admin key: use SENTRIX_ADMIN_KEY env var (preferred) or --admin-key flag
```

### Accounts

```bash
sentrix balance <address>               # Show SRX balance (in sentri and SRX)
sentrix history <address>               # Show last 20 transactions for an address
```

### SRX-20 Tokens

```bash
sentrix token deploy --name <name> --symbol <SYM> --supply <n> [--decimals 18] [--fee 100000]
sentrix token transfer --contract <addr> --to <addr> --amount <n> [--gas 10000]
sentrix token burn --contract <addr> --amount <n> [--gas 10000]
sentrix token balance --contract <addr> --address <addr>
sentrix token info --contract <addr>
sentrix token list

# Signing key: use SENTRIX_DEPLOYER_KEY / SENTRIX_FROM_KEY env vars
```

### Node

```bash
sentrix start \
  [--validator-key <hex>] \    # Run as validator (or set SENTRIX_VALIDATOR_KEY)
  [--port 30303] \             # P2P listen port
  [--peers host:port,...]      # Bootstrap peers (comma-separated)

sentrix genesis-wallets         # Generate the 7 genesis wallet set
```

---

## API Reference

All APIs share a single port (default `8545`, override with `SENTRIX_API_PORT`).

### REST API

| Method | Endpoint | Auth | Description |
|--------|----------|------|-------------|
| GET | `/` | — | Node info and API index |
| GET | `/health` | — | Health check |
| GET | `/chain/info` | — | Chain stats (height, supply, validators, trie) |
| GET | `/chain/blocks[?page=0&limit=20]` | — | Paginated block list (newest first) |
| GET | `/chain/blocks/:index` | — | Block by index |
| GET | `/chain/validate` | Key | Validate chain window (cached per height) |
| GET | `/chain/state-root/:height` | — | Committed state root at block height |
| GET | `/accounts/:address/balance` | — | SRX balance |
| GET | `/accounts/:address/nonce` | — | Account nonce |
| GET | `/mempool` | — | Current mempool contents |
| GET | `/validators` | — | Validator set and stats |
| GET | `/transactions` | — | Recent transactions |
| POST | `/transactions` | — | Submit signed transaction |
| GET | `/transactions/:txid` | — | Transaction by ID |
| GET | `/tokens` | — | All deployed SRX-20 tokens |
| GET | `/tokens/:contract` | — | Token metadata |
| GET | `/tokens/:contract/balance/:addr` | — | Token balance |
| POST | `/tokens/deploy` | — | Deploy new SRX-20 token |
| POST | `/tokens/:contract/transfer` | — | Transfer tokens |
| POST | `/tokens/:contract/burn` | — | Burn tokens |
| GET | `/address/:address/history` | — | Paginated transaction history |
| GET | `/address/:address/info` | — | Address summary |
| GET | `/address/:address/proof` | — | Merkle state proof |
| GET | `/richlist` | — | Top SRX holders |
| GET | `/admin/log` | Key | Admin operation audit trail |
| POST | `/rpc` | — | JSON-RPC 2.0 dispatcher |
| GET | `/explorer/*` | — | Block explorer pages |

### JSON-RPC 2.0

Connect MetaMask, ethers.js, or web3.js to `http://HOST:8545/rpc` with Chain ID `7119`.

Key methods: `eth_chainId`, `eth_blockNumber`, `eth_getBalance`, `eth_getTransactionCount`,
`eth_getBlockByNumber`, `eth_getBlockByHash`, `eth_getTransactionByHash`,
`eth_sendRawTransaction`, `eth_call`, `net_version`, `eth_gasPrice`.

Batch requests supported. Single-call and batch responses follow JSON-RPC 2.0 spec.

---

## Configuration

All configuration is via environment variables. No config file required.

| Variable | Default | Description |
|----------|---------|-------------|
| `SENTRIX_API_PORT` | `8545` | REST API + JSON-RPC listen port |
| `SENTRIX_DATA_DIR` | `<binary_dir>/data` | Chain database and wallet directory |
| `SENTRIX_API_KEY` | _(unset)_ | API key for protected endpoints (`X-API-Key` header). If unset, protected endpoints are open. |
| `SENTRIX_CORS_ORIGIN` | _(unset)_ | Allowed CORS origin. Unset = no cross-origin. `*` = all (dev only). |
| `SENTRIX_VALIDATOR_KEY` | _(unset)_ | Validator private key hex. Set to enable block production. |
| `SENTRIX_ADMIN_KEY` | _(unset)_ | Admin private key for validator management commands. |
| `SENTRIX_ENCRYPTED_DISK` | _(unset)_ | Set to `true` to suppress disk encryption warning at startup. |

---

## Security

See [SECURITY.md](SECURITY.md) for vulnerability reporting and the security policy.

The Sentrix codebase has undergone multiple rounds of security review covering:

- Cryptographic correctness (ECDSA signing, Merkle proof verification, address derivation)
- Consensus safety (state root enforcement, validator authorization, chain fork handling)
- Network security (Noise XX mutual authentication, chain_id handshake, peer rate limiting)
- API security (constant-time key comparison, CORS policy, concurrency limits, input validation)
- Storage integrity (atomic block commit, trie GC, graceful shutdown state save)
- Wallet security (Argon2id key derivation, Zeroizing secret key storage, no server-side key handling)

Known limitations: PoA consensus (Pioneer phase) requires trust in the validator set by design. Validators are controlled by the admin address. The Voyager upgrade will introduce DPoS to distribute control.

---

## Roadmap

| Phase | Timeline | Focus |
|-------|----------|-------|
| **Pioneer** ✅ | Complete | PoA core, SRX-20 tokens, SentrixTrie, libp2p Noise, 342 tests, live network |
| **Voyager** | 2026-05 → 2026-07 | DPoS validator elections, BFT finality, EVM (revm) |
| **Frontier** | 2026-08 → 2026-10 | Ecosystem expansion, dApps, real users |
| **Odyssey** | 2027+ | Full public chain, cross-chain bridges, mature ecosystem |

---

## Network

- **Chain ID:** 7119 (`0x1bcf`)
- **RPC:** `https://rpc.sentrixchain.com`
- **Block Explorer:** [sentrixscan.com](https://sentrixscan.com)
- **Wallet:** [wallet.sentrixchain.com](https://wallet.sentrixchain.com)
- **Landing:** [sentrixchain.com](https://sentrixchain.com)
- **Faucet:** [faucet.sentrixchain.com](https://faucet.sentrixchain.com)
- **Telegram:** [t.me/sentrixchain](https://t.me/sentrixchain)
- **GitHub:** [github.com/satyakwok/sentrix](https://github.com/satyakwok/sentrix)

---

## License

Sentrix is licensed under the [Business Source License 1.1](LICENSE) (BUSL-1.1).

The Change Date is four years from the initial release date. After the Change Date, the software will be available under the Apache 2.0 license.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for development setup, PR process, and coding standards.
