```
  ____             _        _
 / ___|  ___ _ __ | |_ _ __(_)_  __
 \___ \ / _ \ '_ \| __| '__| \ \/ /
  ___) |  __/ | | | |_| |  | |>  <
 |____/ \___|_| |_|\__|_|  |_/_/\_\

        S E N T R I X   C H A I N
```

**A high-performance Layer-1 Proof-of-Authority blockchain built from scratch in Rust.**

[![Build](https://img.shields.io/badge/build-passing-brightgreen)]()
[![Rust](https://img.shields.io/badge/rust-1.94-orange)](https://www.rust-lang.org/)
[![Tests](https://img.shields.io/badge/tests-86%20passing-brightgreen)]()
[![Consensus](https://img.shields.io/badge/consensus-PoA-blue)]()
[![License](https://img.shields.io/badge/license-BUSL--1.1-purple)](LICENSE)
[![Chain ID](https://img.shields.io/badge/chain%20ID-7119-yellow)]()

---

## What is Sentrix

Sentrix (SRX) is a purpose-built Layer-1 blockchain designed for fast, predictable settlement — payments, in-app economies, loyalty programs, and tokenized assets. Built entirely in Rust for maximum performance and safety.

Sentrix runs a **Proof of Authority** consensus where authorized validators produce blocks in round-robin order, delivering **deterministic 3-second block times** with instant finality. No mining, no staking, no wasted energy.

The chain uses an **Ethereum-compatible account model** (balance + nonce per address) with `0x` addresses. Every transaction is signed with ECDSA (secp256k1), replay-protected by sender nonce, and validated atomically against global state. Fees are split 50/50 between the block validator and a permanent burn sink, creating **deflationary pressure** as network activity grows.

On top of the base layer, Sentrix ships an **SRX-20 token standard** — a lean ERC-20-compatible interface (`transfer`, `approve`, `transfer_from`, `mint`, `balance_of`, `allowance`) that lets anyone deploy a fungible token in one CLI command.

Sentrix is **Ethereum-tooling compatible** — MetaMask, ethers.js, and web3.js can connect directly via the built-in JSON-RPC 2.0 server.

---

## Key Features

| Property | Value |
|---|---|
| **Symbol** | SRX |
| **Chain ID** | 7119 (`0x1bcf`) |
| **Consensus** | Proof of Authority (PoA), round-robin |
| **Block time** | 3 seconds |
| **Finality** | Instant |
| **Max supply** | 210,000,000 SRX (hard-capped) |
| **Block reward** | 1 SRX, halves every 42,000,000 blocks |
| **Smallest unit** | 1 sentri = 0.00000001 SRX |
| **Tx model** | Account-based (Ethereum-style) |
| **Address format** | `0x` + 40 hex (Keccak-256) |
| **Signatures** | ECDSA secp256k1 |
| **Token standard** | SRX-20 (ERC-20 compatible) |
| **Fee split** | 50% validator / 50% burned |
| **Wallet encryption** | AES-256-GCM + PBKDF2-SHA256 (600k iterations) |
| **Storage** | sled embedded database (per-block) |
| **Language** | Rust (zero unsafe, pure implementation) |
| **Binary size** | ~4.4 MB (single static binary) |

---

## Quick Start

### Prerequisites

- Rust 1.94+ (`rustup install stable`)
- Visual Studio Build Tools (Windows) or GCC (Linux/macOS)

### Build

```bash
git clone https://github.com/satyakwok/sentrix-chain.git
cd sentrix-chain
cargo build --release
```

### Initialize a new chain

```bash
# Generate a wallet
./target/release/sentrix wallet generate

# Initialize blockchain with your address as admin
./target/release/sentrix init --admin 0xYOUR_ADDRESS

# Add a validator
./target/release/sentrix validator add 0xVALIDATOR_ADDR "My Validator" PUBLIC_KEY --admin-key YOUR_PRIVATE_KEY

# Start the node
./target/release/sentrix start --validator-key VALIDATOR_PRIVATE_KEY
```

### Verify it's working

```bash
# Chain info
./target/release/sentrix chain info

# Check balance
./target/release/sentrix balance 0xYOUR_ADDRESS

# Open block explorer
# http://localhost:8545/explorer
```

---

## CLI Reference

```bash
# Blockchain
sentrix init --admin <address>              # Initialize new chain
sentrix chain info                          # Chain statistics
sentrix chain validate                      # Verify chain integrity
sentrix chain block <index>                 # Show block details

# Wallet
sentrix wallet generate [--password <pw>]   # Create new wallet
sentrix wallet import <key> [--password]    # Import from private key
sentrix wallet info <keystore_file>         # Show wallet info

# Validator Management (admin only)
sentrix validator add <addr> <name> <pubkey> --admin-key <key>
sentrix validator remove <addr> --admin-key <key>
sentrix validator toggle <addr> --admin-key <key>
sentrix validator list

# Transactions
sentrix balance <address>                   # Check SRX balance
sentrix history <address>                   # Transaction history

# SRX-20 Tokens
sentrix token deploy --name "Token" --symbol TKN --supply 1000000 --deployer-key <key>
sentrix token transfer --contract <addr> --to <addr> --amount 100 --from-key <key>
sentrix token burn --contract <addr> --amount 100 --from-key <key>
sentrix token balance --contract <addr> --address <addr>
sentrix token info --contract <addr>
sentrix token list

# Node
sentrix start [--validator-key <key>] [--port 30303] [--peers host:port]
sentrix genesis-wallets                     # Generate genesis wallet set
```

---

## API

Sentrix exposes three API layers on a single port (default: `8545`):

### REST API (20 endpoints)

All POST endpoints require `X-API-Key` header when `SENTRIX_API_KEY` env var is set.

```
GET  /health                              Health check
GET  /chain/info                          Chain statistics
GET  /chain/blocks                        List all blocks
GET  /chain/blocks/{index}                Block detail
GET  /chain/validate                      Chain integrity
GET  /accounts/{address}/balance          Account balance
GET  /accounts/{address}/nonce            Account nonce
GET  /address/{address}/info              Full account info
GET  /address/{address}/history           Transaction history
POST /transactions                        Submit transaction
GET  /transactions/{txid}                 Transaction lookup
GET  /mempool                             Pending transactions
GET  /validators                          Validator list
GET  /tokens                              List SRX-20 tokens
GET  /tokens/{contract}                   Token info
GET  /tokens/{contract}/balance/{addr}    Token balance
POST /tokens/deploy                       Deploy SRX-20 token
POST /tokens/{contract}/transfer          Transfer tokens
POST /tokens/{contract}/burn              Burn tokens
```

### JSON-RPC 2.0 (Ethereum compatible)

```
POST /rpc
```

20 methods supported — fully compatible with MetaMask, ethers.js, web3.js:

```
eth_chainId          eth_blockNumber       eth_gasPrice
eth_estimateGas      eth_getBalance        eth_getTransactionCount
eth_getBlockByNumber eth_getBlockByHash    eth_getTransactionByHash
eth_getTransactionReceipt                  eth_sendRawTransaction
eth_call             eth_syncing           eth_accounts
eth_getCode          eth_getStorageAt
net_version          net_listening         web3_clientVersion
```

Supports single requests and batch requests.

### Block Explorer

```
/explorer                        Dashboard (stats + recent blocks)
/explorer/block/{index}          Block detail with transactions
/explorer/address/{address}      Address balance + transaction history
/explorer/tx/{txid}              Transaction detail
/explorer/validators             Validator list and stats
/explorer/tokens                 Deployed SRX-20 tokens
```

---

## MetaMask Setup

Add Sentrix as a custom network in MetaMask:

| Field | Value |
|---|---|
| Network name | Sentrix Chain |
| RPC URL | `http://localhost:8545/rpc` |
| Chain ID | `7119` |
| Currency symbol | `SRX` |
| Block explorer URL | `http://localhost:8545/explorer` |

---

## SRX-20 Token Standard

SRX-20 is Sentrix's native fungible token standard, fully compatible with the ERC-20 interface.

### Deploy a token

```bash
sentrix token deploy \
  --name "My Token" \
  --symbol MTK \
  --supply 1000000000 \
  --decimals 18 \
  --deployer-key <private_key> \
  --fee 100000
```

### Transfer tokens

```bash
sentrix token transfer \
  --contract SRX20_abc123... \
  --to 0xrecipient... \
  --amount 1000 \
  --from-key <private_key>
```

### Query

```bash
sentrix token balance --contract SRX20_abc123... --address 0xuser...
sentrix token info --contract SRX20_abc123...
sentrix token list
```

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     sentrix (CLI)                            │
│                  16 commands via clap                        │
└──────────┬──────────────────────┬───────────────────────────┘
           │                      │
  ┌────────▼────────┐   ┌────────▼────────┐
  │    REST API      │   │  Block Explorer  │
  │   19 endpoints   │   │    6 pages       │
  │   + JSON-RPC     │   │   dark theme     │
  │   20 methods     │   │                  │
  └────────┬─────────┘   └────────┬────────┘
           │                      │
  ┌────────▼──────────────────────▼───────────────────┐
  │              core/blockchain.rs                     │
  │  ┌──────────┐  ┌──────────┐  ┌────────────────┐   │
  │  │ AccountDB │  │ Authority │  │ ContractRegistry│  │
  │  │ (balances │  │ (PoA      │  │ (SRX-20        │  │
  │  │  + nonces)│  │  round-   │  │  tokens)       │  │
  │  │          │  │  robin)   │  │                │  │
  │  └──────────┘  └──────────┘  └────────────────┘   │
  │  ┌──────────┐  ┌──────────┐  ┌────────────────┐   │
  │  │  Block   │  │Transaction│  │    Mempool     │   │
  │  │  chain   │  │  + ECDSA  │  │  (priority fee)│   │
  │  └──────────┘  └──────────┘  └────────────────┘   │
  └────────────────────────┬──────────────────────────┘
                           │
     ┌─────────────────────┼─────────────────────┐
     │                     │                     │
┌────▼─────┐        ┌──────▼──────┐       ┌─────▼──────┐
│  Wallet  │        │   Storage   │       │  P2P Node  │
│  ECDSA + │        │   sled DB   │       │  TCP       │
│  AES-GCM │        │  per-block  │       │  broadcast │
└──────────┘        └─────────────┘       └────────────┘
```

### Module layout

```
src/
├── main.rs              # CLI entry point (15 commands)
├── lib.rs               # Library root
├── types/error.rs       # SentrixError enum (14 variants)
├── core/
│   ├── blockchain.rs    # Chain engine, mempool, block production
│   ├── block.rs         # Block struct, hashing, validation
│   ├── transaction.rs   # ECDSA transactions, signing, verification
│   ├── account.rs       # Account state database (balance + nonce)
│   ├── authority.rs     # PoA validator management, round-robin
│   ├── merkle.rs        # SHA-256 Merkle tree
│   └── vm.rs            # SRX-20 token engine
├── wallet/
│   ├── wallet.rs        # Key generation, Keccak-256 address derivation
│   └── keystore.rs      # AES-256-GCM encrypted wallet storage
├── storage/
│   └── db.rs            # sled per-block persistent storage
├── network/
│   ├── node.rs          # TCP P2P node, message protocol
│   └── sync.rs          # Safe chain synchronization
└── api/
    ├── routes.rs        # REST API (axum)
    ├── jsonrpc.rs       # JSON-RPC 2.0 server
    └── explorer.rs      # Block explorer web UI
```

---

## Three-Token Model

Sentrix Chain operates a three-token economy:

| Token | Type | Supply | Purpose |
|---|---|---|---|
| **SRX** | Native coin | 210,000,000 (hard cap) | Gas fees, validator rewards, base currency |
| **SNTX** | SRX-20 | 10,000,000,000 | Utility — rewards, governance, staking |
| **SRTX** | SRX-20 | TBD | Payment — stablecoin for transactions |

SRX is required for all operations (gas). SNTX and SRTX are SRX-20 tokens deployed on top of the chain. Every SNTX/SRTX transfer burns SRX as gas, creating deflationary pressure on the native coin.

---

## Tokenomics

### Supply distribution

```
Total: 210,000,000 SRX (hard cap)

┌──────────────────────────────────────────────────┐
│ Premine (30%)              │ Block Rewards (70%)  │
│ 63,000,000 SRX             │ 147,000,000 SRX      │
│                            │ mined over ~16 years │
├────────────────────────────┤                      │
│ Founder:      21M (10%)    │                      │
│ Ecosystem:    21M (10%)    │  Era 0: 1 SRX/block  │
│ Early Val:  10.5M  (5%)   │  Era 1: 0.5 SRX     │
│ Reserve:   10.5M  (5%)    │  Era 2: 0.25 SRX    │
│                            │  ...halves every 42M │
└────────────────────────────┴──────────────────────┘
```

### Fee economics

Every transaction pays a fee in SRX:
- **50% to the block validator** (incentive to run a node)
- **50% permanently burned** (removed from circulation forever)

This creates **deflationary pressure**: as network activity increases, more SRX is burned. Eventually, burn rate exceeds block rewards, and total circulating supply begins to **decrease**.

---

## Tests

```bash
cargo test
```

**86 tests** across 10 test suites:

| Suite | Tests | Coverage |
|---|---|---|
| `core::merkle` | 5 | Merkle tree, SHA-256 |
| `core::account` | 5 | AccountDB, transfers, burn tracking |
| `core::transaction` | 7 | ECDSA sign/verify, nonce, chain_id replay protection |
| `core::authority` | 7 | Validator management, round-robin, min validator check |
| `core::block` | 8 | Block creation, validation, chain links |
| `core::blockchain` | 15 | Full engine: mempool, blocks, tokens, priority fee |
| `core::vm` | 19 | SRX-20: deploy, transfer, approve, burn, dispatch |
| `storage::db` | 8 | Persistence, per-block, hash index, migration |
| `wallet::wallet` | 6 | Keygen, address derivation, import |
| `wallet::keystore` | 6 | AES-256-GCM encrypt/decrypt, PBKDF2 600K |

---

## Security

### Cryptographic stack

| Component | Algorithm | Crate |
|---|---|---|
| Transaction signing | ECDSA secp256k1 | `secp256k1` |
| Block hashing | SHA-256 | `sha2` |
| Address derivation | Keccak-256 | `sha3` |
| Wallet encryption | AES-256-GCM | `aes-gcm` |
| Key derivation | PBKDF2-HMAC-SHA256 (600k iter) | `pbkdf2` |
| Memory safety | Private key zeroized on drop | `zeroize` |
| Random generation | OS CSPRNG | `rand` |

### Block validation

All blocks undergo **two-pass atomic validation**:
1. **Dry run**: validate every transaction against a working state copy
2. **Commit**: apply state changes only if ALL transactions pass

No partial state changes. No race conditions. All or nothing.

### Additional security measures

- **Checked arithmetic**: all balance operations use `checked_add`/`checked_sub` — no integer overflow/underflow
- **Chain ID replay protection**: transactions include `chain_id` in signing payload — cannot replay across networks
- **API authentication**: POST endpoints require `X-API-Key` header (configurable via `SENTRIX_API_KEY` env var)
- **Private key zeroization**: wallet secret keys are zeroed from memory on drop
- **P2P chain ID validation**: peers with mismatched chain IDs are rejected on handshake
- **Minimum validator count**: cannot remove the last active validator

### Reporting vulnerabilities

See [SECURITY.md](SECURITY.md) for responsible disclosure policy.

---

## Roadmap

- [x] **Phase 1** — PoA private chain (core engine, wallets, storage, API)
- [x] **Phase 2a** — SRX-20 tokens, block explorer, JSON-RPC, per-block storage
- [x] **Phase 2b** — Full P2P networking (listener, handler, sync, broadcast)
- [x] **Phase 2c** — Security audit + all fixes (checked arithmetic, chain_id, zeroize, API auth)
- [ ] **Phase 3** — DPoS/PoS transition, staking, governance, wallet web UI
- [ ] **Phase 4** — Smart contract VM, SDKs, cross-chain bridge, mobile wallet

---

## License

Sentrix Chain is licensed under the **Business Source License 1.1 (BUSL-1.1)**.

- **Licensor:** SentrisCloud
- **Change Date:** 2030-01-01 (converts to MIT)
- **Additional Use Grant:** You may use the Licensed Work for non-commercial purposes and for running validator nodes on the Sentrix mainnet.

See the [LICENSE](LICENSE) file for the full text.

---

## Built by SentrisCloud

Sentrix Chain is developed and maintained by **SentrisCloud**.

For commercial licensing, partnership inquiries, or validator onboarding, reach out through the official channels.

Security issues: see [SECURITY.md](SECURITY.md) — please report privately, never as a public issue.

---

## Disclaimer

All claims, content, designs, algorithms, estimates, roadmaps, specifications, and performance measurements described in this project are done with SentrisCloud's good faith efforts. It is up to the reader to check and validate their accuracy and truthfulness. Furthermore, nothing in this project constitutes a solicitation for investment.

Any content produced by SentrisCloud or developer resources that SentrisCloud provides are for educational and inspirational purposes only. SentrisCloud does not encourage, induce or sanction the deployment, integration or use of any such applications in violation of applicable laws or regulations and hereby prohibits any such deployment, integration or use. This includes the use of any such applications by the reader (a) in violation of export control or sanctions laws of any applicable jurisdiction, (b) if the reader is located in or ordinarily resident in a country or territory subject to comprehensive sanctions, or (c) if the reader is or is working on behalf of a person subject to blocking or denied party prohibitions.

The software is provided "as is", without warranty of any kind, express or implied. In no event shall SentrisCloud be liable for any claim, damages or other liability arising from the use of the software. Use at your own risk.

SRX tokens have no inherent monetary value. This project is a technology demonstration and should not be treated as a financial product, security, or investment vehicle.
