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
[![Tests](https://img.shields.io/badge/tests-335%20passing-brightgreen)]()
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
| **Wallet encryption** | AES-256-GCM + Argon2id (m=65536, t=3, p=4) / PBKDF2 v1 backward compat |
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
git clone https://github.com/satyakwok/sentrix.git
cd sentrix
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

### REST API (19 endpoints)

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
/explorer                        Dashboard (stats + recent blocks + analytics charts)
/explorer/block/{index}          Block detail with transactions
/explorer/address/{address}      Address balance + transaction history
/explorer/tx/{txid}              Transaction detail
/explorer/validators             Validator list and stats
/explorer/validator/{address}    Validator detail
/explorer/tokens                 Deployed SRX-20 tokens
/explorer/token/{contract}       Token detail
/explorer/richlist               Top addresses by balance
/explorer/mempool                Pending transactions
/explorer/search                 Search by block/tx/address
```

---

## MetaMask Setup

Add Sentrix as a custom network in MetaMask:

| Field | Value |
|---|---|
| Network name | Sentrix |
| RPC URL | `https://sentrix-rpc.sentriscloud.com` |
| Chain ID | `7119` |
| Currency symbol | `SRX` |
| Block explorer URL | `https://sentrixscan.sentriscloud.com` |

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
│                  17 commands via clap                        │
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
│  ECDSA + │        │   sled DB   │       │  libp2p    │
│  AES-GCM │        │  per-block  │       │  Noise XX  │
└──────────┘        └─────────────┘       └────────────┘
```

### Module layout

```
src/
├── main.rs              # CLI entry point (17 commands)
├── lib.rs               # Library root
├── types/error.rs       # SentrixError enum (14 variants)
├── core/
│   ├── blockchain.rs    # Chain engine, genesis, constants, Blockchain struct
│   ├── mempool.rs       # add_to_mempool(), prune_mempool(), mempool queries
│   ├── block_producer.rs# create_block() — coinbase + priority-fee mempool
│   ├── block_executor.rs# add_block() — two-pass atomic validation + commit
│   ├── token_ops.rs     # deploy_token(), token_transfer(), token_burn()
│   ├── chain_queries.rs # get_transaction(), get_address_history(), richlist()
│   ├── block.rs         # Block struct, hashing, validation
│   ├── transaction.rs   # ECDSA transactions, signing, verification, chain_id
│   ├── account.rs       # AccountDB (balance + nonce, checked arithmetic)
│   ├── authority.rs     # PoA validators, round-robin, min validator count
│   ├── merkle.rs        # SHA-256 Merkle tree for TX root
│   ├── vm.rs            # SRX-20 token engine (mint, burn, transfer, approve)
│   └── trie/
│       ├── mod.rs       # Public re-exports
│       ├── node.rs      # TrieNode (Empty/Leaf/Branch), BLAKE3+SHA-256 hashing
│       ├── tree.rs      # SentrixTrie — 256-level Binary Sparse Merkle Tree
│       ├── storage.rs   # TrieStorage — sled-backed node+value persistence
│       ├── cache.rs     # TrieCache — configurable LRU in front of storage
│       ├── proof.rs     # MerkleProof — inclusion proof generation + verify()
│       └── address.rs   # address_to_key(), account encode/decode
├── wallet/
│   ├── wallet.rs        # Key generation, Keccak-256 address derivation
│   └── keystore.rs      # AES-256-GCM encrypted wallet storage (Argon2id v2)
├── storage/
│   └── db.rs            # sled per-block persistent storage + hash index
├── network/
│   ├── transport.rs     # libp2p: TCP + Noise XX + Yamux boxed transport
│   ├── behaviour.rs     # libp2p: SentrixBehaviour (Identify + RequestResponse)
│   └── libp2p_node.rs   # libp2p: LibP2pNode, command channel, broadcast, auto-reconnect
└── api/
    ├── routes.rs        # REST API (axum) + API key auth
    ├── jsonrpc.rs       # JSON-RPC 2.0 server (20 methods)
    └── explorer.rs      # Block explorer web UI (12 pages)
```

---

## Three-Token Model

Sentrix operates a three-token economy:

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

**335 tests** — 284 unit tests across all modules + 51 integration tests across 9 suites:

**Integration test suites (`tests/`):**

| Suite | Tests | Coverage |
|---|---|---|
| `integration_restart` | 3 | Save/reload state, height integrity, mempool persistence |
| `integration_sync` | 4 | Two-node sync, duplicate block rejection, out-of-order rejection |
| `integration_tx` | 6 | TX lifecycle, double-spend, nonce checks, balance, validator reward |
| `integration_token` | 6 | Deploy, transfer, burn, mint, cap enforcement, balance check |
| `integration_mempool` | 7 | Per-sender limit, global limit, TTL, future TS, fee priority, pending spend |
| `integration_supply` | 6 | Supply invariant genesis/empty/TX/high-fee, BLOCK_REWARD increment |
| `integration_chain_validation` | 9 | Valid chain, wrong prev_hash, unauthorized validator, coinbase overflow, chain_id, tampering |
| `integration_sliding_window` | 4 | Eviction, window_start formula, stats metadata, restart preservation |
| `integration_trie` | 6 | TX recipient in trie, zero-balance removal, validator balance match, proof verification, state root changes, root history per block |

---

## SentrixTrie — State Root

Sentrix ships a **Binary Sparse Merkle Tree (256-level)** state trie that produces a cryptographic state root per block — proving account balances without replaying history.

### Design

| Property | Detail |
|---|---|
| **Tree type** | Binary Sparse Merkle Tree, 256-level depth |
| **Key** | SHA-256 of normalized address bytes (32 bytes) |
| **Node hashing** | BLAKE3 + SHA-256 domain separation |
| **Leaf value** | `[balance: 8 bytes BE][nonce: 8 bytes BE]` |
| **LRU cache** | Configurable capacity (default 10,000 nodes ≈ 1 MB) |
| **Persistence** | sled embedded DB (separate `trie_nodes` + `trie_values` trees) |
| **State root** | Stamped on every `Block.state_root` after commit |

### State root per block

```
Block N committed → update_trie_for_block() →
  for each touched address:
    balance == 0 → trie.delete(key)       ← zero-balance accounts removed
    balance  > 0 → trie.insert(key, value) ← upsert with orphan cleanup
  state_root = trie.root_hash() → Block.state_root
```

### Merkle inclusion proofs

```bash
GET /trie/proof/{address}
→ { "key": "0x...", "value": "...", "proof": [...32 siblings...], "root": "0x..." }
```

Proofs are self-verifiable: given `(key, value, proof, root)`, any client can recompute the root from the leaf up and confirm inclusion without trusting the node.

### Audit status (T-A ~ T-H + V7)

| Finding | Fix | Status |
|---|---|---|
| T-A: case-sensitive address lookup | `address_to_key()` lowercases before hashing | ✅ Fixed PR #54 |
| T-B: orphaned leaf on update | Track `old_leaf_hash`, delete after write | ✅ Fixed PR #54 |
| T-C: 0x prefix inconsistency | `trim_start_matches("0x")` before processing | ✅ Fixed PR #54 |
| T-D: hardcoded LRU capacity | `TrieCache::new(storage, capacity)` | ✅ Fixed PR #54 |
| T-E: missing inclusion proofs | `proof.rs` + `prove()` + `verify()` | ✅ Fixed PR #50 |
| T-F: no GC for orphaned nodes | `gc_orphaned_nodes(live_hashes)` | ✅ Fixed PR #54 |
| T-G: missing deny.toml for trie deps | `blake3`, `lru` added to Cargo.toml | ✅ Satisfied |
| T-H: missing integration tests | `tests/integration_trie.rs` (6 tests) | ✅ Fixed PR #55 |
| V7-C-01: state_root not in block hash | `STATE_ROOT_FORK_HEIGHT=100_000`; hash includes state_root for blocks ≥ 100K | ✅ Fixed PR #57 |
| V7-H-01: trie errors swallowed | `update_trie_for_block()` propagates errors | ✅ Fixed PR #57 |
| V7-H-02: crash-unsafe store_root | Flush all 3 sled trees before returning | ✅ Fixed PR #57 |
| V7-M-01~M-05: storage leaks, DoS, panic | Full cleanup, read lock, depth guard, P2P persist | ✅ Fixed PR #57 |
| V7-L-01~L-03: path cleanup, validation | Old internals deleted, address validated, async flush | ✅ Fixed PR #57 |
| V7-I-01~I-04: backfill, clone, TOKEN_OP | AccountDB backfill, capacity stored, filter TOKEN_OP | ✅ Fixed PR #57 |
| Stale root traversal on restart | `node_exists()` + `reset_to_empty()` before backfill | ✅ Fixed PR #59+#60 |

### Chain recovery procedure

If trie state diverges across nodes (detect via CRITICAL state_root mismatch logs):

```bash
# 1. Stop node
systemctl stop sentrix-node   # or sentrix-val{N}

# 2. Reset trie (drops trie_nodes/trie_values/trie_roots sled trees)
./sentrix chain reset-trie

# 3. Restart — init_trie() will backfill from AccountDB automatically
systemctl start sentrix-node
```

All nodes running from the same AccountDB state will produce identical backfill roots (deterministic Binary SMT).

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

- **Address↔pubkey binding**: transaction verify() checks that the public key maps to from_address
- **Validator authorization**: add_block() verifies the block producer is the expected validator for that height
- **Token API authentication**: POST token endpoints require private key proof of ownership
- **HTML escaping**: block explorer sanitizes all user-controlled data to prevent XSS
- **Canonical signing payload**: BTreeMap + serde_json for injection-proof JSON serialization
- **Checked arithmetic**: all balance operations use `checked_add`/`checked_sub` — no integer overflow/underflow
- **Chain ID replay protection**: transactions include `chain_id` in signing payload — cannot replay across networks
- **API authentication**: POST endpoints require `X-API-Key` header (configurable via `SENTRIX_API_KEY` env var)
- **Constant-time API key comparison**: prevents timing-based key extraction attacks
- **CORS**: configurable via `SENTRIX_CORS_ORIGIN` env var (default: allow all)
- **Private key zeroization**: wallet secret keys are zeroed from memory on drop (no Clone)
- **P2P encryption**: all connections encrypted via Noise XX handshake (mutual authentication)
- **P2P transport**: libp2p with TCP + Noise XX + Yamux multiplexing
- **P2P chain ID validation**: peers with mismatched chain IDs are rejected on handshake
- **P2P auto-reconnect**: bootstrap peers reconnected every 30s; idle connection timeout set to MAX
- **Block timestamp validation**: rejects blocks with timestamps before previous block or >15s in future
- **Minimum validator count**: cannot remove or deactivate the last active validator
- **RPC batch limit**: max 100 requests per JSON-RPC batch
- **Paginated endpoints**: blocks and address history use limit/offset to prevent OOM
- **ERC-20 approve race condition**: requires reset to 0 before changing allowance, plus increase/decrease helpers

### Known limitations (Phase 1)

> **Single admin key**: The current PoA implementation uses a single admin key for validator management. This is a known centralization risk accepted for Phase 1. **Mitigation planned**: multi-sig admin requiring 2/3 signatures (Phase 3). Until then, store the admin key in a hardware wallet or HSM — never on the node server.

> **Full chain in memory**: The entire chain is loaded into RAM. For large chains (>100K blocks), consider archival strategies. **Mitigation planned**: state pruning + archival storage (Phase 4).

### Reporting vulnerabilities

See [SECURITY.md](SECURITY.md) for responsible disclosure policy.

---

## Roadmap

- [x] **Phase 1** — PoA private chain (core engine, wallets, storage, API)
- [x] **Phase 2a** — SRX-20 tokens, block explorer, JSON-RPC, per-block storage
- [x] **Phase 2b** — Full P2P networking (listener, handler, sync, broadcast)
- [x] **Phase 2c** — Security audit v1 + all fixes (checked arithmetic, chain_id, zeroize, API auth)
- [x] **Phase 2d** — Security audit v2-v6: 47+ findings, all fixed (PR #36-#44)
- [x] **Step 1** — cargo-deny + clippy -D warnings enforcement (PR #42)
- [x] **Step 2** — blockchain.rs split → 6 focused modules (PR #43)
- [x] **Step 3** — libp2p + Noise XX encryption (PR #45)
- [x] **Step 4** — 9 integration test suites, 335 tests total (PR #46 + #55)
- [x] **Step 5** — SentrixTrie Binary Sparse Merkle Tree state root (PR #48-#55)
- [x] **Security Audit V7** — 15 findings, all fixed; STATE_ROOT_FORK_HEIGHT=100K; trie errors fatal (PR #57)
- [x] **Chain Recovery Tools** — `sentrix chain reset-trie`; stale-height fix; deterministic backfill (PR #58-#60)
- [x] **ChainSync Persistence** — P2P-synced blocks persisted to sled immediately; prevents state divergence on restart (PR #61)
- [x] **Graceful Recovery** — load_blockchain handles missing blocks gracefully; adjusts height and re-syncs from peers (PR #62)
- [x] **CI/CD Graceful Deploy** — stop services before binary replace; prevents mid-trie-write kills (PR #63)
- [x] **libp2p Default Transport** — libp2p Noise XX is the ONLY transport; legacy TCP removed; auto-reconnect + idle timeout fix (PR #65-#69)
- [ ] **Phase 2** — DPoS + BFT Finality + EVM compatibility
- [ ] **Phase 3** — Sharding, cross-chain bridge, governance
- [ ] **Phase 4** — SDK, mobile wallet, DEX, NFT platform

---

## Contributing

Sentrix is open for contributions under the BUSL-1.1 license.

- **Bug reports**: Open a GitHub issue
- **Bug bounty**: Critical bugs rewarded in SRX — see [SECURITY.md](SECURITY.md)
- **Developer grants**: Build on Sentrix and apply for ecosystem fund grants
- **Validator nodes**: Contact us to become a genesis validator

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

---

## Community

- Explorer: [sentrixscan.sentriscloud.com](https://sentrixscan.sentriscloud.com)
- API: [sentrix-api.sentriscloud.com](https://sentrix-api.sentriscloud.com)
- RPC: [sentrix-rpc.sentriscloud.com](https://sentrix-rpc.sentriscloud.com)
- GitHub: [github.com/satyakwok/sentrix](https://github.com/satyakwok/sentrix)
- Email: sentriscloud@gmail.com

---

## License

Sentrix is licensed under the **Business Source License 1.1 (BUSL-1.1)**.

- **Licensor:** SentrisCloud
- **Change Date:** 2030-01-01 (converts to MIT)
- **Additional Use Grant:** You may use the Licensed Work for non-commercial purposes and for running validator nodes on the Sentrix mainnet.

See the [LICENSE](LICENSE) file for the full text.

---

## Built by SentrisCloud

Sentrix is developed and maintained by **SentrisCloud**.

For commercial licensing, partnership inquiries, or validator onboarding, reach out through the official channels.

Security issues: see [SECURITY.md](SECURITY.md) — please report privately, never as a public issue.

---

## Disclaimer

All claims, content, designs, algorithms, estimates, roadmaps, specifications, and performance measurements described in this project are done with SentrisCloud's good faith efforts. It is up to the reader to check and validate their accuracy and truthfulness. Furthermore, nothing in this project constitutes a solicitation for investment.

Any content produced by SentrisCloud or developer resources that SentrisCloud provides are for educational and inspirational purposes only. SentrisCloud does not encourage, induce or sanction the deployment, integration or use of any such applications in violation of applicable laws or regulations and hereby prohibits any such deployment, integration or use. This includes the use of any such applications by the reader (a) in violation of export control or sanctions laws of any applicable jurisdiction, (b) if the reader is located in or ordinarily resident in a country or territory subject to comprehensive sanctions, or (c) if the reader is or is working on behalf of a person subject to blocking or denied party prohibitions.

The software is provided "as is", without warranty of any kind, express or implied. In no event shall SentrisCloud be liable for any claim, damages or other liability arising from the use of the software. Use at your own risk.

SRX tokens have no inherent monetary value. This project is a technology demonstration and should not be treated as a financial product, security, or investment vehicle.
