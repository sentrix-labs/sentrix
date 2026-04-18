# Changelog

All notable changes to Sentrix are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Planned
- Mainnet hard fork to Voyager (DPoS + BFT + EVM)
- Parallel tx execution (rayon)
- Light client justification verification

### Security
- **C-06 — Removed `--validator-key <hex>` CLI flag.** Private keys passed
  as CLI arguments leak via `ps aux`, shell history, and process
  snapshots. Validators must now use `--validator-keystore <path>`
  (encrypted Argon2id v2 keystore) or `SENTRIX_VALIDATOR_KEY` env var.
  **Breaking change for validator operators.**
- **C-06 hardening — Wallet zeroize plumbed through startup.** The
  validator key no longer round-trips through an unzeroed heap `String`
  inside `cmd_start`; the `Wallet`'s `Zeroizing<[u8; 32]>` field is the
  only owner of the secret bytes after key resolution.

---

## [2.0.0] — 2026-04-18

**MDBX storage migration + high TPS + chain reset.**

### Changed
- **Storage:** sled → libmdbx (MDBX) across sentrix-trie + sentrix-core
- **BLOCK_TIME_SECS:** 3 → 1 (1-second blocks)
- **MAX_TX_PER_BLOCK:** 100 → 5000
- **Genesis addresses:** Founder v1 → v2 (0x252f...), Early Validator v1 → v2 (0x328d...)
- **Validators:** 7 → 3 (Foundation, Treasury, Core)
- **CI/CD runner:** ubuntu-latest → ubuntu-22.04 (glibc 2.35 compat)
- ChainStorage made Clone via Arc<MdbxStorage>
- MdbxStorage gained Debug impl
- All 12 crates bumped to v2.0.0

### Added
- sentrix-storage crate integrated into sentrix-trie + sentrix-core
- Cloudflare Origin Certificate for *.sentriscloud.com
- sentrixscan.sentriscloud.com explorer (nginx proxy)
- testnet-scan.sentriscloud.com explorer
- MPL-2.0 added to deny.toml allow-list (libmdbx)
- Pre-commit hook allow-paths for workspace crate paths

### Removed
- sled dependency from all Cargo.toml
- 4 decommissioned validators (Nusantara, BlockForge, PacificStake, Archipelago)

### Performance
- Benchmark: 968 txs mined, peak 616 txs/block
- Theoretical capacity: 5000 TPS

---

## [1.5.0] — 2026-04-18

Final 11-crate workspace extraction.

### Added
- **sentrix-core** crate — Blockchain, authority, block executor/producer, mempool, storage, VM
- **sentrix-network** crate — libp2p P2P, gossipsub, kademlia, request-response
- **sentrix-rpc** crate — REST API, JSON-RPC, block explorer

### Changed
- `lru` upgraded 0.12 → 0.17 (fixes Stacked Borrows vulnerability)
- All 11 crates bumped to v1.5.0

### Workspace Structure (final)
```
sentrix-primitives    types (Block, Tx, Account, Error)
sentrix-wallet        keystore + wallet
sentrix-trie          Sparse Merkle Tree
sentrix-staking       DPoS, epoch, slashing
sentrix-evm           revm adapter
sentrix-bft           BFT consensus
sentrix-core          Blockchain orchestration
sentrix-network       P2P networking
sentrix-rpc           API + explorer
bin/sentrix           CLI binary
```

---

## [1.4.0] — 2026-04-18

Binary restructure + workspace finalization.

### Changed
- **Binary split**: main.rs moved to `bin/sentrix/`, root crate is now library-only
- **Version bump**: all 8 crates bumped to 1.4.0
- **CI**: `cargo build --workspace --release` for binary crate discovery
- Library dependencies trimmed (clap, anyhow, tracing-subscriber moved to binary crate)

---

## [1.3.1] — 2026-04-18

Workspace refactor with CI fix + P2P integration tests.

### Added
- **Cargo workspace**: 6 domain crates extracted from monolith
  - sentrix-primitives (Block, Transaction, Account, Error types)
  - sentrix-wallet (keystore + wallet operations)
  - sentrix-trie (Binary Sparse Merkle Tree)
  - sentrix-staking (DPoS registry, epochs, slashing)
  - sentrix-evm (revm adapter, executor, gas)
  - sentrix-bft (Tendermint-style BFT engine)
- **P2P integration tests** (4 tests: handshake, gossipsub propagation, 3-node mesh, chain_id rejection)
- **Binary archive**: CI archives previous binary in `/opt/sentrix/releases/` (keeps 3 versions)
- **Emergency rollback docs**: `docs/operations/EMERGENCY_ROLLBACK.md`

### Fixed
- **cargo-deny CI failure**: `wildcards = "allow"` for workspace path dependencies
- **CI workspace coverage**: clippy + test now use `--workspace` flag

### Note
- v1.3.0 was RECALLED due to cargo-deny blocking CI. v1.3.1 is the corrected release.
- Wire format unchanged — zero breaking changes from v1.2.0.
- BFT fix preserved (timeout-only round advance).

---

## [1.2.0] — 2026-04-16

Voyager EVM (Phase 2b). Full Ethereum compatibility.

### Added
- **EVM execution via revm 37** — Solidity smart contract deployment and execution
- **eth_sendRawTransaction** — full Ethereum tx support (legacy + EIP-1559 + EIP-2930 + EIP-4844 + EIP-7702)
- **alloy-consensus / alloy-eips / alloy-rlp** dependencies for Ethereum tx decoding + signature recovery
- **eth_call** — read-only EVM execution with disabled balance/nonce/basefee checks
- **eth_getCode / eth_getStorageAt** — query deployed contract bytecode and storage slots
- **eth_estimateGas / eth_chainId / eth_blockNumber / eth_gasPrice** — full Web3 RPC surface
- **EVM database adapter** (`SentrixEvmDb`) bridging revm to AccountDB / contract storage
- **Account model migration** — `code_hash` and `storage_root` fields added with backward-compatible serde defaults
- **Account `is_contract()`** helper + `migrate_to_evm()` one-time fork migration
- **EIP-1559 gas metering** — base fee 10K sentri, target 15M, block limit 30M
- **Precompile addresses** defined: 0x100 staking, 0x101 slashing
- **VOYAGER_EVM_HEIGHT** env var (default `u64::MAX` = disabled)
- **`activate_evm()`** wired in main validator loop at fork height
- **BFT round catch-up protocol** — `RoundStatus` gossip lets returning validators learn current (height, round)
- **BFT propose-after-advance-round** — validator that becomes proposer after timeout/skip immediately proposes block
- **Multi-validator BFT testnet** (4 validators on VPS3) with 3/4 fault tolerance
- **Wallet encryption CLI** — `wallet encrypt`/`decrypt`, `--validator-keystore`, password from env or prompt
- **CI/CD rolling restart** — validators restarted one at a time during deploys; chain never stops producing blocks
- **CI/CD covers testnet validators** — `sentrix-testnet-val1..4` services auto-updated
- **Robust health check** — 5 retries × 60s windows with cluster-max delta tolerance
- **testnet-scan / testnet-explorer** subdomains added to nginx
- **Single nginx server block** consolidates all 4 testnet subdomains; fixes MetaMask `/rpc` path bug
- **Per-IP rate limiter** bumped to 20 connections (handles VPS2 hosting 5 validators on one IP)
- **SRC-20 token standard** verified with deployed test contract
- **TPS benchmark scripts** (Python) for testnet load testing
- 519 tests, clippy clean, cargo fmt applied repo-wide
- **A1** — genesis premine credit error handling: fatal exit on failure (was silent)
- **A2** — EVM tx receipt `status=0x0` on revert; bounded `failed_evm_txs` set on AccountDB
- **A3** — mempool insertion uses `partition_point` for O(log n) compare cost
- **A4** — BFT `on_prevote` delegates to `on_prevote_weighted(prevote, 1)` (test/legacy only)
- **A5** — `txid_index` sled tree + idempotent backfill; `get_transaction()` works for blocks beyond `CHAIN_WINDOW_SIZE`
- **A6/A7** — per-endpoint write rate limit (10 req/min per IP) on `/transactions`, `/tokens/deploy|transfer|burn`, `/rpc`
- **A8** — `chrono` crate replaces hand-rolled Gregorian/leap-year math
- **A9** — explorer timestamps switch from WIB (UTC+7) to UTC across all pages and daily-stats buckets
- **A10** — `Storage::open` enforces `SENTRIX_ENCRYPTED_DISK=true` at startup; `SENTRIX_ALLOW_UNENCRYPTED_DISK=true` escape hatch for dev/CI
- **B1** — explorer transaction page surfaces Type badge (COINBASE/EVM CREATE/EVM CALL/TOKEN OP/NATIVE) + REVERTED status + gas/calldata rows
- **C1** — `SENTRIX_API_HOST` + `SENTRIX_P2P_HOST` env vars to bind listeners to a specific interface (default `0.0.0.0`); testnet validators behind nginx now bind `127.0.0.1`

### Changed
- **SHA-256 weighted proposer** — replaces old `(height*31+round)*7` selector that always picked first validator with equal stakes
- **Account struct** extended with `code_hash` (`EMPTY_CODE_HASH` for EOA) and `storage_root` (`EMPTY_STORAGE_ROOT` for EOA)
- **eth_chainId / net_version** now read actual chain_id (was hardcoded to 7119)
- **axum 0.8** route syntax migration (`/:param` → `/{param}`) — production-critical fix
- **libp2p 0.54 → 0.56** — adapted `RrEvent` patterns for new `connection_id` field
- **revm features:** added `optional_balance_check` + `optional_no_base_fee` for read-only `execute_call()`
- **rustls-webpki** updated to 0.103.12 (closes RUSTSEC-2026-0098/0099)

### Fixed
- **BFT SkipRound** now calls `advance_round()` instead of resetting engine to round 0 — fixes endless desync loop
- **Mempool zero-address guard** allows EVM CREATE txs (to=0x0)
- **Block_executor** routes EVM txs through revm + stores runtime bytecode (not init bytecode) on CREATE
- **Chain.db corruption** workaround documented (`sentrix chain reset-trie` rebuilds from canonical AccountDB)

### Security
- **Removed exposed deployer private key** from `benchmark/*.py` (was leaking the Early Validator key on the public repo since v1.2.0-rc commits). Key drained on mainnet to a freshly generated address; git history scrubbed via `git filter-repo` and force-pushed.
- **Removed hardcoded validator IPs** from benchmark scripts and committed history; replaced with `SENTRIX_RPC` env var defaulting to `127.0.0.1`.
- **Removed `__pycache__/*.pyc`** that shipped pre-compiled bytecode containing the validator IP.
- Added `*.pyc`, `*.pyo`, `__pycache__/` to `.gitignore`.

---

## [1.1.0] — 2026-04-15

Voyager DPoS + BFT. The consensus upgrade.

### Added
- DPoS staking: register validator (15K SRX min), delegate, undelegate, redelegate
- Epoch system: 28,800 blocks (~24h), validator set rotation, unbonding release
- Slashing: downtime (1% + jail) and double-sign (20% + permaban)
- BFT consensus: Tendermint-style propose/prevote/precommit with 2/3+1 stake-weighted finality
- BFT message types in P2P layer (proposal, prevote, precommit broadcast)
- Fork transition: VOYAGER_FORK_HEIGHT env var (default u64::MAX, mainnet safe)
- Testnet live: chain_id 7120, port 9545, VPS3
- REST endpoints: /staking/validators, /staking/delegations, /staking/unbonding, /epoch/current, /epoch/history
- Staking tx types: RegisterValidator, Delegate, Undelegate, Redelegate, Unjail, SubmitEvidence
- Block fields: round + justification (serde default, backward compat with Pioneer blocks)
- 113 new tests (463 total, was 357)

### Changed
- chain_id configurable via SENTRIX_CHAIN_ID env var
- Block production loop: Voyager bookkeeping (rewards, liveness, epoch) after fork height
- Blockchain struct: added StakeRegistry, EpochManager, SlashingEngine

---

## [1.0.0] — 2026-04-15

Pioneer release. PoA chain live with 7 validators across 3 VPS, 141K+ blocks, 11 security audit rounds.

### Milestones since 0.1.0 (Pioneer)
- 7 validators running across 3 geographically separate VPS (full mesh peering)
- CI/CD pipeline deploying to all 3 VPS with ordered stop/start and health checks
- P0 security hardening: libp2p peer limits, per-IP rate limiting, legacy TCP deprecated
- VPS3 (Sentrix Core) added as 7th validator
- Chain height 141,000+, zero downtime incidents since stabilization
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

[Unreleased]: https://github.com/satyakwok/sentrix/compare/v1.2.0...HEAD
[1.2.0]: https://github.com/satyakwok/sentrix/releases/tag/v1.2.0
[1.1.0]: https://github.com/satyakwok/sentrix/releases/tag/v1.1.0
[1.0.0]: https://github.com/satyakwok/sentrix/releases/tag/v1.0.0
[0.1.0]: https://github.com/satyakwok/sentrix/releases/tag/v0.1.0
