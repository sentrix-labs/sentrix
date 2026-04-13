# Changelog

All notable changes to Sentrix will be documented in this file.

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
- Step 5: SentrixTrie (Merkle Patricia Trie state root)
- VPS3 setup (genesis_node_3)
- Phase 2: DPoS + BFT Finality design implementation

---

## [Post-0.1.0 Incremental] — 2026-04-13

### PR #46 — Step 4: Integration Tests
- Add `tests/` directory with 8 comprehensive integration test suites
- `tests/common/mod.rs` — shared helpers: setup_single_validator, mine_empty_block, funded_wallet, make_tx, make_tx_nonce
- `tests/integration_restart.rs` — 3 tests: save/reload state, height integrity, mempool persistence
- `tests/integration_sync.rs` — 4 tests: two-node sync, duplicate block rejection, out-of-order rejection
- `tests/integration_tx.rs` — 6 tests: TX lifecycle, double-spend, nonce checks, balance, validator reward
- `tests/integration_token.rs` — 6 tests: deploy, transfer, burn, mint, cap enforcement, balance check
- `tests/integration_mempool.rs` — 7 tests: per-sender limit, global limit, TTL, future timestamp, fee priority, pending spend
- `tests/integration_supply.rs` — 6 tests: supply invariant at genesis/empty blocks/with TXs/high fees, BLOCK_REWARD increment
- `tests/integration_chain_validation.rs` — 9 tests: valid chain, wrong prev_hash, unauthorized validator, coinbase overflow, chain_id, hash tampering
- `tests/integration_sliding_window.rs` — 4 tests: eviction, window_start formula, stats metadata, restart preservation
- Add `[dev-dependencies] tempfile = "3"` to Cargo.toml
- **Total: 247 tests (202 unit + 45 integration)**

### PR #45 — Step 3: libp2p + Noise XX Encryption
- Add `src/network/transport.rs` — `build_transport()`: TCP + Noise XX handshake + Yamux multiplexing, boxed transport
- Add `src/network/behaviour.rs` — `SentrixBehaviour` (Identify + RequestResponse<SentrixCodec>); `SentrixRequest/Response` enums; length-prefixed JSON codec (10 MiB cap)
- Add `src/network/libp2p_node.rs` — `LibP2pNode` with command channel pattern; swarm event loop; chain_id validation; `verified_peers` HashSet; `broadcast_block()`, `broadcast_transaction()`
- Update `src/main.rs` — `sentrix start --use-libp2p` flag; full if/else branch (libp2p vs legacy TCP)
- Add `futures = "0.3"` and `async-trait = "0.1"` to Cargo.toml
- Update libp2p features: add `tokio`, `request-response`, `macros`
- **219 tests at time of merge**

### PR #44 — Security Audit V6: All 13 Findings Fixed
- `V6-C-01` [CRITICAL]: `compute_contract_address()` now uses `tx.txid` (deterministic) — fixes consensus divergence on multi-node
- `V6-H-01` [HIGH]: `create_block()` clones mempool txs (not drain) — txs survive if `add_block()` fails
- `V6-H-02` [HIGH]: `chain/mempool/total_minted/contracts` → `pub(crate)`; `authority/accounts` remain `pub`
- `V6-M-01` [MEDIUM]: `deploy_token/token_transfer/token_burn` → `pub(crate)` in token_ops.rs
- `V6-M-02` [MEDIUM]: cargo-deny advisories re-enabled in deny.toml
- `V6-M-03` [MEDIUM]: `IpRateLimiter` → `tokio::sync::Mutex` (async-safe, no blocking)
- `V6-M-04` [MEDIUM]: `get_address_tx_count()` returns window-aware JSON with `window_tx_count`, `is_partial` fields
- `V6-L-01` [LOW]: O(n) mempool insert TODO comment added
- `V6-L-02` [LOW]: `get_address_history()` standardized to newest-first across all paths
- `V6-L-03` [LOW]: 16 new unit tests in 5 new modules (202 total)
- `V6-L-04` [LOW]: Zero-address SRX transfer rejected (except TokenOp)
- `V6-I-01` [INFO]: `chain_stats()` + `get_address_tx_count()` expose window metadata
- `V6-I-02` [INFO]: All V5 fixes verified intact post-refactor
- **202 tests at time of merge**

### PR #43 — Step 2: Split blockchain.rs into 6 Focused Modules
- Split `blockchain.rs` (1665 lines) into:
  - `blockchain.rs` (~170 lines) — struct, constants, genesis, core state
  - `mempool.rs` — `add_to_mempool()`, `prune_mempool()`, mempool queries
  - `block_producer.rs` — `create_block()`
  - `block_executor.rs` — `add_block()` (two-pass validation + commit)
  - `token_ops.rs` — `deploy_token()`, `token_transfer()`, `token_burn()`, token queries
  - `chain_queries.rs` — `get_transaction()`, `get_address_history()`, `chain_stats()`, `richlist()`
- All `impl Blockchain` blocks; all public API unchanged
- Zero logic changes — pure refactor
- **186 tests at time of merge**

### PR #42 — Step 1: Config Tooling (cargo-deny + clippy)
- Add `deny.toml`: BUSL-1.1 allowlist, ban wildcards, skip known multi-version transitive deps
- Add `.clippy.toml`: deny `unwrap_used`/`expect_used`/`panic` in prod paths; warn `large_futures`/`todo`; `allow-*-in-tests=true`
- Add `[lints.clippy]` + `[lints.rust]` to `Cargo.toml`; `license = "BUSL-1.1"`
- Fix all pre-existing clippy warnings: `collapsible_if`, `manual_is_multiple_of`, `unwrap_used`, `redundant_binds`
- CI pipeline: `cargo deny check` + `cargo clippy -- -D warnings` before build/test
- **186 tests at time of merge**

### PR #41 — Security Audit V5: All 11 Findings Fixed
- `V5-01`: `MIN_ACTIVE_VALIDATORS=3` enforced in remove/toggle; `collusion_risk()` method
- `V5-02`: Token `max_supply` cap in deploy+mint (0=unlimited); `#[serde(default)]` backward compat
- `V5-03`: `handshake_done` flag in node.rs; pre-handshake messages rejected
- `V5-04`: `tracing::warn!` if `SENTRIX_ENCRYPTED_DISK != "true"` in Storage::open()
- `V5-05`: `is_valid_sentrix_address()` check in `add_validator()` before crypto check
- `V5-06`: Per-IP rate limit (60 req/min) in routes.rs + nginx `limit_req_zone`
- `V5-07`: RBF TODO comment in `add_to_mempool()`
- `V5-08`: Dockerfile base image pinning comment
- `V5-09`: CI SSH key rotation reminder comment
- `V5-10`: `HASH_VERSION=1` constant
- `V5-11`: `MAX_ADMIN_LOG_SIZE=10_000` with `trim_admin_log()`
- **186 tests at time of merge**

### PR #40 — Argon2id Keystore v2 + Admin Audit Trail
- Wallet keystore upgraded: PBKDF2 → Argon2id (m=65536, t=3, p=4); old v1 still loadable (backward compat)
- `AdminEvent` enum + `admin_log: Vec<AdminEvent>` in `AuthorityManager`
- `GET /admin/log` endpoint (X-API-Key required)
- `trim_admin_log()` to cap log at `MAX_ADMIN_LOG_SIZE=10_000`
- **152 tests at time of merge** (+7 tests from PR #39 base)

### PR #39 — Sliding Window Chain Cache (OOM Fix)
- `CHAIN_WINDOW_SIZE=1000`: only last 1000 blocks kept in RAM (~2 MB)
- `chain` field changed from `Vec<Block>` to `VecDeque<Block>` (capacity-bounded)
- `chain_window_start()` method: `height.saturating_sub(CHAIN_WINDOW_SIZE - 1)`
- `load_blockchain()` loads only window (not full chain history)
- `height()` derived from last block index (not vec length)
- `chain_stats()` exposes `window_start_block`, `window_is_partial` fields
- **152 tests at time of merge**

### PR #38 — Security Audit V4: Low Severity Fixes
- `L-01`: Hash index migration at `Storage::open()`; O(n) fallback removed from `load_block_by_hash()`
- `L-02`: `burn = (fee+1)/2` (ceiling rounding) — odd fees no longer lost
- `L-03`: Token name (1–64 chars) + symbol (1–10 ASCII alphanumeric) validation in `ContractRegistry::deploy()`
- `L-04`: `Wallet.secret_key_hex` → `Zeroizing<[u8; 32]>`; manual Drop removed; `secret_key_hex()` method added
- `L-05`: `serde_json::to_value().unwrap()` → proper error mapping in routes.rs
- **145 tests at time of merge**

### PR #37 — Security Audit V4: Medium Severity Fixes
- `M-02`: Constant-time API key comparison (no early return on length mismatch)
- `M-03`: Mempool timestamp validation: reject >+5min future or >1h old
- `M-04`: `MEMPOOL_MAX_AGE_SECS=3600`; `prune_mempool()` called after every block
- `M-05`: `SRX20Contract::mint(caller, to, amount)` — owner check inside the method
- `M-06`: CORS default → restrictive when `SENTRIX_CORS_ORIGIN` not set
- `M-07`: `/chain/validate` requires X-API-Key + caches result per block height
- **130 tests at time of merge**

### PR #36 — Security Audit V4: Critical + High Fixes
- `C-01`: Private key removed from all server endpoints; client signs locally; faucet rewritten
- `C-02`: `ConcurrencyLimitLayer(500)` added; nginx upstream handles per-IP rate limiting
- `C-03`: `MAX_MEMPOOL_SIZE=10_000` + `MAX_MEMPOOL_PER_SENDER=100` enforced
- `H-01`: `sync.rs` validates `chain_id` in peer handshake
- `H-02`: `add_validator()` validates secp256k1 pubkey derives to given address
- `H-03`: `coinbase.verify()` requires empty sig+pubkey (blocks ECDSA bypass)
- `H-04`: `add_to_mempool()` validates `to_address` format (0x+40hex)
- `H-05`: `constant_time_eq()` fixed; REST normalizes addresses
- `M-01`: `signing_payload()` `unwrap()` → `unwrap_or_else`
- **114 tests at time of merge**

### PR #34 — Domain rename
- Updated all URLs to sentriscloud.com subdomains

### PR #33 — CI/CD VPS2 fix
- Fixed GitHub Actions deploy workflow for VPS2 multi-validator setup

### PR #32 — Explorer analytics charts
- Added analytics charts to block explorer home page
- TX volume chart, block time histogram, fee distribution
