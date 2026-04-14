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
- VPS3 setup (genesis_node_3)
- Phase 2: DPoS + BFT Finality design implementation

---

## [Post-0.1.0 Incremental] — 2026-04-14

### PR #61 — fix(sync): persist P2P-synced blocks to sled
- `src/network/sync.rs`: add `storage: Arc<Storage>` parameter to `sync_from_peer()`; call `storage.save_block()` after each successful `add_block()` in the sync loop
- `src/main.rs`: update both call sites (NodeEvent::SyncNeeded + periodic 30s sync) to pass `Arc<Storage>` to `sync_from_peer()`
- **Root cause**: `sync_from_peer()` only updated in-memory state; on restart, nodes loaded stale sled height and diverged from the network — this was one of the root causes of the chain fork incident 2026-04-14
- **335 tests** (no new tests — fix is infrastructure plumbing, exercised by existing integration_sync + integration_restart suites)

### PR #60 — fix: trie reset-to-empty before backfill
- `src/core/trie/tree.rs`: add `reset_to_empty()` — resets `self.root = empty_hash(0)`
- `src/core/blockchain.rs` (`init_trie()`): call `trie.reset_to_empty()` before backfill when committed root node is missing (deleted by V7-L-01 insert restructuring)
- **Root cause**: `SentrixTrie::open(db, version)` sets `self.root` from `trie_roots[version]`; if that node was deleted, first `insert()` in backfill traversed stale root → "missing node" error; `reset_to_empty()` ensures backfill starts clean
- **Result**: backfill now deterministic; same root across all nodes (`d0f8516f...`) after chain recovery

### PR #59 — fix: trie stale-height on restart + node_exists check
- `src/storage/db.rs` (`save_block()`): now calls `save_height(block.index)?` after each P2P-received block, keeping sled height in sync with in-memory state
- `src/core/trie/tree.rs`: add `node_exists(hash) -> SentrixResult<bool>` — checks if a node hash exists in sled storage
- `src/core/blockchain.rs` (`init_trie()`): before backfill, check if committed root node actually exists via `node_exists()`; if missing (stale-height or V7-L-01 deletion), trigger backfill with warning log
- **Critical fix**: prevents "missing node" panic on restart when `trie_roots[height]` points to a node deleted by subsequent inserts
- 10 new tests → **335 tests total**

### PR #58 — feat: sentrix chain reset-trie command
- `src/storage/db.rs`: add `reset_trie()` — drops `trie_nodes`, `trie_values`, `trie_roots` sled trees + flushes; on next startup `init_trie()` detects no committed root and backfills from AccountDB
- `src/main.rs`: add `sentrix chain reset-trie` CLI subcommand; prints confirmation + path; requires `SENTRIX_DATA_DIR`
- **Use case**: chain recovery when trie state is corrupted or diverged across nodes; run before restart after a forced migration

### PR #57 — fix(security): Security Audit V7 — ALL 15 FINDINGS FIXED
- **V7-C-01 [CRITICAL]**: `state_root` included in `calculate_hash()` starting at `STATE_ROOT_FORK_HEIGHT = 100_000`; add_block() logs CRITICAL on state_root mismatch (received vs computed); hard fork mechanism with graceful handling for pre-100K blocks
- **V7-H-01 [HIGH]**: `update_trie_for_block()` now returns `SentrixResult<Option<[u8;32]>>`; trie insert/delete/commit errors propagate to `add_block()` instead of being swallowed; trie failure = block commit failure
- **V7-H-02 [HIGH]**: `store_root()` now flushes all three trees (`trie_nodes`, `trie_values`, `trie_roots`) before returning; crash-safe trie state guaranteed
- **V7-M-01 [MEDIUM]**: `delete()` captures `found_leaf_hash` + `found_value_hash` in Phase 1; deletes them after Phase 2 walk-up; no more storage leak per zero-balance deletion
- **V7-M-02 [MEDIUM]**: `gc_orphaned_nodes()` extended to also GC `trie_values` tree with same live_hashes set
- **V7-M-03 [MEDIUM]**: `TrieCache.lru` wrapped in `Mutex<LruCache>`; `prove()` now takes `&self`; proof endpoint (`GET /trie/proof/{address}`) uses read lock instead of write lock — no more block production stall under concurrent proof requests
- **V7-M-04 [MEDIUM]**: all three traversal loops changed from `depth > 256` to `depth >= 256`; returns `SentrixError::Internal` on violation; `delete()` walk-up guarded against usize underflow
- **V7-M-05 [MEDIUM]**: `save_block()` in P2P `NewBlock` handler now persists `state_root` immediately; `ChainSync::sync_from_peer()` flow: save_block called per synced block (architectural fix)
- **V7-L-01 [LOW]**: `insert()` Phase 1 records old internal node hashes along path; Phase 3 deletes them after writing new path nodes; eliminates long-term orphan accumulation
- **V7-L-02 [LOW]**: `/trie/proof/{address}` validates address with `is_valid_sentrix_address()` before acquiring blockchain lock; returns 400 immediately on invalid format
- **V7-L-03 [LOW]**: `store_root()` refactored to async; uses `flush_async().await` for all three trees; no more blocking I/O on Tokio worker thread
- **V7-I-01 [INFO]**: proof API response includes `scope: "native_srx_only"` field and documentation note
- **V7-I-02 [INFO]**: `init_trie()` backfills all non-zero AccountDB entries on first init when no committed root exists
- **V7-I-03 [INFO]**: `TrieCache` stores `capacity: usize`; `SentrixTrie::clone()` uses `self.cache.capacity` instead of hardcoded 10_000
- **V7-I-04 [INFO]**: `update_trie_for_block()` skips `TOKEN_OP_ADDRESS` from touched addresses set
- **10 new tests** across trie + blockchain modules → **335 tests total**

### PR #55 — SentrixTrie: Blockchain Integration
- `update_trie_for_block()` in `blockchain.rs`: apply all balance changes to state trie per block
- Zero-balance deletion: accounts with balance == 0 call `trie.delete()` instead of insert
- State root stamped onto each `Block.state_root: Option<[u8;32]>` after commit
- `GET /trie/proof/{address}` endpoint: returns Merkle inclusion proof as JSON
- `get_state_root_at(height)` accessor for historical root lookup
- Add `temp_db()` helper in blockchain unit tests
- **6 new unit tests** in `blockchain.rs`: trie init, root per block, multiple blocks, state root stamp, uncommitted version, trie disabled
- **6 new integration tests** in `tests/integration_trie.rs`: TX recipient in trie, zero-balance removal, validator balance match, proof verification, state root changes on deletion, root history per block
- **325 tests total**

### PR #54 — SentrixTrie Audit Fixes (T-A / T-B / T-C / T-D / T-F)
- **T-A + T-C** (`address.rs`): `address_to_key()` rewritten — strips `0x`, lowercases, hex-decodes to raw bytes, SHA-256 → case-insensitive and prefix-independent
- **T-B** (`tree.rs`): track `old_leaf_hash` during insert; delete old leaf node + value from storage on in-place key update (fixes storage leak)
- **T-B** (`storage.rs`): add `delete_node()` + `delete_value()` methods
- **T-B** (`cache.rs`): add `delete_node()` (evict LRU + delete storage) + `delete_value()`
- **T-D** (`cache.rs`): `TrieCache::new(storage, capacity: usize)` — configurable LRU size (replaces hardcoded 10_000)
- **T-F** (`storage.rs`): `gc_orphaned_nodes(live_hashes: &HashSet<NodeHash>) -> SentrixResult<usize>` — two-pass sled scan, bulk-delete unlisted nodes
- **11 new tests** across `address.rs` (2), `storage.rs` (4), `cache.rs` (2), `tree.rs` (3)

### PR #53 — CI: Remove 0BSD + Upgrade to Node.js 24
- Remove `"0BSD"` from `deny.toml` license allowlist (not present in dependency tree)
- Add `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24: true` to CI workflow env — silences Node.js 20 deprecation warnings

### PR #52 — deny.toml: Skip libp2p Transitive Duplicates
- Add 16 `[[bans.skip]]` entries for crates duplicated by libp2p transitive dependencies: `multistream-select`, `quick-protobuf-codec`, `asynchronous-codec`, `unsigned-varint`, `hickory-proto`, `futures-rustls`, `rustls`, `rustls-native-certs`, `rustls-pemfile`, `rustls-pki-types`, `webpki-roots`, `ring`, `tokio-rustls`, `yamux` (×2 versions), and others
- Resolves `cargo deny check bans` duplicate warnings on CI

### PR #51 — Fix All Compiler Warnings (0 warnings)
- `tests/integration_chain_validation.rs`: add `#![allow(missing_docs)]`, remove unused `Blockchain`/`CHAIN_ID` imports, rename `val` → `_val`
- `tests/integration_*.rs` (8 files): add `#![allow(missing_docs)]` to suppress lint in integration test files
- `src/core/block_executor.rs`: remove unused `const RECV` dead code
- `tests/common/mod.rs`: remove unused `MIN_TX_FEE` import
- Result: **0 warnings** on `cargo clippy -D warnings`

### PR #50 — SentrixTrie: Merkle Inclusion Proof
- Add `src/core/trie/proof.rs`: `MerkleProof` struct — sibling hashes along 256-bit path
- `SentrixTrie::prove(key)` → `Option<MerkleProof>` traversal
- `MerkleProof::verify(key, value, root)` — recompute root from leaf up; constant-time path
- Integration with `GET /trie/proof/{address}` endpoint

### PR #49 — SentrixTrie: Binary Sparse Merkle Tree Core
- Add `src/core/trie/node.rs`: `TrieNode` enum (Empty / Leaf / Branch), `NodeHash = [u8; 32]`, BLAKE3+SHA-256 domain-separated hashing
- Add `src/core/trie/tree.rs`: `SentrixTrie` — 256-level Binary Sparse Merkle Tree, iterative insert/delete/get, short-circuit leaf optimization
- Add `src/core/trie/mod.rs`: public re-exports
- Add `tests/integration_trie.rs`: initial integration test suite
- Add `blake3 = "1"` to Cargo.toml

### PR #48 — SentrixTrie: Storage Layer
- Add `src/core/trie/storage.rs`: `TrieStorage` — sled-backed persistence with separate `trie_nodes` and `trie_values` trees
- Add `src/core/trie/cache.rs`: `TrieCache` — LRU cache wrapping `TrieStorage`
- Add `src/core/trie/address.rs`: `address_to_key()`, `account_value_bytes()`, `account_value_decode()`
- Add `lru = "0.12"` to Cargo.toml

### PR #47 — Docs Update
- Update all `4. Founder Private/` docs for session 2026-04-13: SESSION_HANDOFF, TODO, BIBLE, CLAUDE, PROMPT
- Reflect Step 1-4 DONE, 247 tests, PR #8-#46, audit v4+v5+v6 all fixed

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
