# Changelog

All notable changes to Sentrix are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Planned
- Voyager: DPoS validator elections + BFT finality
- Voyager: EVM integration via revm

---

## [1.0.0] — 2026-04-15

Pioneer release. PoA chain live with 7 validators across 3 VPS, 131K+ blocks, 11 security audit rounds.

### Milestones since 0.1.0 (Pioneer)
- 7 validators running across 3 geographically separate VPS (full mesh peering)
- CI/CD pipeline deploying to all 3 VPS with ordered stop/start and health checks
- P0 security hardening: libp2p peer limits, per-IP rate limiting, legacy TCP deprecated
- VPS3 (Sentrix Core) added as 7th validator
- Chain height 131,000+, zero downtime incidents since stabilization
- 11 security audit rounds completed (94 findings, 78 fixed, score 8.3/10)
- Full documentation suite (20 files across architecture, security, operations, tokenomics, roadmap)

---

## [0.1.0] — 2026-04-15

### Added

**Consensus & Block Production**
- Proof of Authority consensus with deterministic round-robin validator scheduling
- Account-based state model (Ethereum-style balance + nonce per address)
- Two-pass atomic block validation: dry-run (no state mutation) then commit
- ECDSA secp256k1 transaction signing and verification with nonce-based replay protection
- SHA-256 Merkle tree over transaction IDs for block integrity
- Block reward halving schedule (1 SRX per block, halves every 42,000,000 blocks; hard cap 210M SRX)
- Fee distribution: 50% to block validator, 50% permanently burned (deflationary)
- Genesis premine: 63,000,000 SRX across 4 strategic addresses
- Hardcoded genesis timestamp for deterministic identical genesis across all nodes

**SentrixTrie — Binary Sparse Merkle Tree**
- 256-level Binary Sparse Merkle Tree with BLAKE3+SHA-256 domain-separated hashing
- Membership and non-membership Merkle proofs via `GET /address/:addr/proof`
- State root committed into block hash starting at block 100,000 (`STATE_ROOT_FORK_HEIGHT`)
- Blocks from peers with mismatched state root are rejected (consensus enforcement)
- sled-backed trie persistence across three named trees (`trie_nodes`, `trie_values`, `trie_roots`)
- LRU cache layer over sled storage; configurable capacity
- Orphaned node GC sweeping both node and value trees
- Deterministic backfill from AccountDB on first trie initialization (one-time migration)
- `sentrix chain reset-trie` command for recovery scenarios

**SRX-20 Token Standard**
- Deploy fungible tokens in one CLI command; contract address derived deterministically from txid
- Full ERC-20-compatible interface: transfer, burn, mint, approve, balance_of, allowance
- Deploy fee: 50% burned, 50% to ecosystem fund
- Gas fee model: paid in SRX, split 50/50 (burn / validator)
- Token name (1–64 chars) and symbol (1–10 ASCII alphanumeric) validated at deploy time

**Networking — libp2p**
- libp2p transport: TCP + Noise XX mutual authentication + Yamux multiplexing
- Stable node identity: Ed25519 keypair persisted to `data/node_keypair`; PeerId stable across restarts
- RequestResponse protocol for block and height queries
- Periodic chain sync: request missing blocks from verified peers every 30 seconds
- Automatic peer reconnect with 30-second interval
- Chain ID verified during peer handshake; mismatched peers rejected
- Block processing in spawned tasks — swarm event loop never blocked
- Idle connection keepalive configured to prevent premature disconnects

**REST API (25+ endpoints)**
- Chain info, paginated block listing, block by index, chain window validation
- Account balance, nonce, transaction history, address summary
- Mempool contents
- Validator set and stats
- SRX-20 token operations: deploy, transfer, burn, balance, info, list, holders, trades
- Rich list (top SRX holders)
- State root by block height
- Merkle state proof by address
- Admin audit log (authenticated)
- Per-IP rate limiting: 60 requests/minute
- Global HTTP concurrency limit: 500 concurrent requests
- CORS: fail-safe restrictive default; configurable via `SENTRIX_CORS_ORIGIN`
- Constant-time API key comparison
- Cached chain validation result (O(n) scan only on height change)

**JSON-RPC 2.0 (20 methods)**
- Ethereum-compatible: `eth_chainId`, `eth_blockNumber`, `eth_getBalance`, `eth_getTransactionCount`, `eth_getBlockByNumber`, `eth_getBlockByHash`, `eth_getTransactionByHash`, `eth_sendRawTransaction`, `eth_call`, `net_version`, `eth_gasPrice`, and others
- Single and batch request support; hard batch size cap
- MetaMask, ethers.js, and web3.js connect natively (Chain ID 7119)

**Block Explorer**
- Dark-themed 12-page web UI served directly from the binary (no external assets)
- Pages: dashboard, blocks, block detail, transactions, tx detail, validators, validator detail, tokens, token detail, address detail, rich list, mempool
- XSS-safe: all user-facing values HTML-escaped at render time
- Daily stats endpoint for analytics charts

**Wallet**
- ECDSA secp256k1 key generation
- Ethereum-style address derivation (Keccak-256, `0x` prefix, 40 hex chars)
- AES-256-GCM encrypted keystore — Argon2id KDF (m=65536, t=3, p=4) for v2 keystores
- PBKDF2-SHA256 v1 keystores remain loadable (backward compatible)
- v1 → v2 migration utility
- Secret key stored as `Zeroizing<[u8; 32]>` — automatically zeroed on drop
- `sentrix genesis-wallets` command generates the 7-role genesis wallet set

**Storage**
- sled embedded database (pure Rust, no external server)
- Per-block storage: one sled key per block plus hash → index lookup
- Sliding window: last 1,000 blocks in RAM; full history on disk; O(1) startup regardless of chain height
- On-demand historical block access from sled outside the window
- Hash index migration on open for nodes pre-dating the index
- Graceful recovery when blocks are missing in the window: adjusts height and re-syncs from peers
- State save uses read lock — API remains responsive during block production

**Node Lifecycle**
- Graceful shutdown on SIGTERM and SIGINT: saves full blockchain state before exit
- Disk encryption warning at startup if `SENTRIX_ENCRYPTED_DISK` is not set
- Validator loop: write lock released before disk I/O; API reads are never stalled

**CLI (17 commands)**
- `init`, `chain` (info / validate / block / reset-trie)
- `wallet` (generate / import / info)
- `validator` (add / remove / toggle / rename / list)
- `balance`, `history`
- `token` (deploy / transfer / burn / balance / info / list)
- `start`, `genesis-wallets`
- Private keys resolved from env vars with CLI fallback; CLI arg usage produces a security warning

**Testing**
- 335 tests: 269 unit tests + 66 integration tests across 9 suites
- Integration suites: restart persistence, chain sync, transactions, tokens, mempool, supply invariant, chain validation, sliding window, trie state

### Security

- Multiple rounds of security review covering cryptographic correctness, consensus safety, network security, API security, storage integrity, and wallet security
- Validator authorization checked against round-robin schedule before any state mutation
- Token operation target addresses validated as well-formed Sentrix addresses
- Mempool txid deduplication; per-sender and global size limits; TTL-based pruning
- Block timestamps bounded (≥ previous block, ≤ now + 15s)
- Minimum active validator floor enforced on remove and toggle operations (BFT quorum)
- Admin operations append-only audit log (bounded to 10,000 entries)
- Allowance reset-to-zero required before setting a new non-zero value (ERC-20 double-spend mitigation)

### Changed

- Validator loop restructured: write lock held only during `create_block` + `add_block`; disk I/O runs under read lock
- Block save uses `save_block()` (fast, per-block) supplemented by full state save

### Fixed

- Deterministic genesis block and coinbase transaction timestamps (hardcoded, not wall time)
- Contract addresses derived from transaction ID — identical result on every node for the same transaction
- Trie update order uses `BTreeSet` (sorted, deterministic) — eliminates state root divergence between nodes
- Token operation address validation prevents undetected transfer to malformed addresses
- Mempool does not drain on `create_block` — transactions survive if `add_block` fails

---

[Unreleased]: https://github.com/satyakwok/sentrix/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/satyakwok/sentrix/releases/tag/v1.0.0
[0.1.0]: https://github.com/satyakwok/sentrix/releases/tag/v0.1.0
