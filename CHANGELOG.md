# Changelog

All notable changes to Sentrix are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Refactored
- **refactor(rpc): token handlers out to `routes/tokens.rs`** (backlog
  #12 phase 2a) — the 8 SRC-20 handlers (list / info / balance /
  holders / trades / deploy / transfer / burn) move to their own
  module. `api_err` is now `pub(super)` so modules can reuse it. No
  route path or behaviour change. Follow-up phases will peel off
  chain / accounts / staking / epoch / ops the same way.

### Added
- **feat(cli): `validator force-unjail` operator-recovery command**
  (backlog #1b) — unlocks the chicken-and-egg state where every
  validator has been auto-slashed below `MIN_SELF_STAKE` and the
  normal `unjail` refuses. Bumps `self_stake` back to
  `MIN_SELF_STAKE` if below, clears `is_jailed` + `jail_until`,
  skips the cooldown. Tombstoned validators are still rejected.
  Operator-only: bypasses consensus, so operator must run it on
  every peer's chain DB before BFT resumes so all peers agree on
  the recovered state. Backed by `StakeRegistry::force_unjail` +
  4 unit tests (stake restore, stake preservation when already
  above min, tombstone refusal, cooldown skip). **Mainnet gate**
  (PR #157): on `chain_id == 7119` the command refuses unless
  `--i-understand-phantom-stake` is passed — restoring `self_stake`
  via direct DB edit creates phantom stake (SRX is not minted, but
  the stake counter goes up) and violates the supply invariant.
  Safe on testnet; break-glass on mainnet.
- **feat(rpc): `eth_getBlockReceipts`** (backlog #8) — batch receipt
  query matching the Ethereum JSON-RPC spec. Input: block tag
  (`latest` / `earliest` / `pending` / `safe` / `finalized` / hex
  number) OR block hash OR `{blockHash}` / `{blockNumber}` object.
  Output: array of receipt objects in transaction order with
  `transactionIndex`, `from`, `to`, and monotonic `cumulativeGasUsed`
  on top of the existing `eth_getTransactionReceipt` shape (status,
  logs, logsBloom, gasUsed). Non-existent block → `null`. Collapses
  the N-round-trip receipt fan-out explorers used to do into one
  call. 8 integration tests.

### Refactored
- **refactor(rpc): split routes.rs into submodules** (backlog #12 phase
  1) — `routes.rs` → `routes/mod.rs`; auth extractor +
  `constant_time_eq` moved to `routes/auth.rs`; rate-limiter types +
  middleware + `extract_client_ip` moved to `routes/ratelimit.rs`;
  response / request DTOs (`ApiResponse`, `SendTxRequest`,
  `SignedTxRequest`) moved to `routes/types.rs`. Public API preserved
  via `pub use` re-exports. Handlers (~900 lines) stay in mod.rs
  pending phase 2. mod.rs shrinks 1593 → 1368 LOC. No behaviour change.
- **refactor(rpc): jsonrpc helpers extracted to submodule** (backlog
  #11 phase 1, PR #152) — `jsonrpc.rs` → `jsonrpc/mod.rs` and all
  shared helpers (to_hex, normalize_rpc_*, parse_hex_u64,
  resolve_block_tag, parse_address_filter, parse_topic_filter,
  log_matches, collect_logs, load_logs_for_tx, block_gas_used_ratio,
  TopicFilter) moved to `jsonrpc/helpers.rs`. mod.rs 1302 → 1057 LOC.
  No behaviour change. Phase 2 (per-namespace handler split) pending.

### Planned
- Mainnet hard fork to Voyager (DPoS + BFT + EVM)
- Parallel tx execution (rayon)
- Light client justification verification
- `sentrix_getBftStatus` live round/phase (needs BftEngine snapshot
  plumbed from validator loop into SharedState — Sprint 2)
- Per-delegator per-epoch reward ledger (turns
  `sentrix_getStakingRewards` history into exact claim records —
  Sprint 2)

---

## [2.1.1] — 2026-04-19

**Testnet BFT livelock fix + mainnet block-time tuning + new status
endpoint.** Patch release on top of v2.1.0 with one P0 liveness fix,
one mainnet performance improvement, and one additive RPC endpoint.

### Added
- **feat(rpc): `/sentrix_status` endpoint** (backlog #13, PR #149,
  hardened #150) — NEAR-style structured node-status snapshot for
  operators. Returns version/build, chain_id, consensus tag,
  `sync_info` (latest block height/hash/time, earliest retained block,
  syncing flag), active validator count (PoA reads `authority`,
  BFT reads `stake_registry`), and process uptime in seconds. Distinct
  from `/` (API surface) and `/chain/info` (chain-wide stats). Uptime
  is pinned at router init so the first call reports boot-to-now, not
  zero. 5 integration tests.

### Fixed
- **fix(bft): stake-weighted f+1 round skipping (issue #143, PR
  #147)** — the legacy `on_round_status` only triggered catch-up when
  a single peer was 2+ rounds ahead, which could not resolve a
  persistent 1-round drift between validator clusters. Testnet
  reproduced this repeatedly (rounds climbed past 140, 2/4 validators
  always lagging). The engine now tracks each peer's highest-observed
  round + their stake at the current height, and skips to the largest
  round where f+1 stake (strictly > 1/3 of `total_active_stake`) of
  distinct peers have converged. Matches standard Tendermint round-skip.
  Back-compat `on_round_status` wrapper retained for call sites that
  lack peer stake; main validator loop uses the new
  `on_round_status_weighted`. 9 new tests cover the f+1 path,
  single-peer anti-trigger, stake refresh on epoch rotation, and
  cache reset on height advance.

### Changed
- **perf(validator): Pioneer-mode poll 3s → 200ms + `BLOCK_TIME_SECS`
  gate** (PR #148) — the Pioneer/PoA validator loop previously slept a
  fixed 3s between block attempts, so the effective mainnet block time
  oscillated around 3s instead of the configured 1s. The loop now
  polls every 200ms and only attempts to build a block when at least
  `BLOCK_TIME_SECS` has elapsed since the last one, giving a
  consistent ~1s cadence with at most 200ms jitter. Verified on
  mainnet: +42 blocks in ~35s post-restart window (was +9 blocks).
  No change to block validation rules.
- **refactor(token): rename SRX-20 → SRC-20** (PR #146) — code +
  docs. Address prefix `SRX20_` → `SRC20_`. **Breaking:** contract
  address prefix changed. Safe — zero native tokens deployed
  pre-rename. Matches industry pattern (ERC-20 / BEP-20 / TRC-20 —
  Sentrix Request for Comments).

### Known issues
- **Transitive dependency advisories (to be addressed in a future
  patch):**
  - RUSTSEC-2025-0055 — `tracing-subscriber 0.2.25` pulled in via
    `ark-r1cs-std` → `ark-relations`. Fix requires upstream `ark-*`
    crate updates; main `tracing-subscriber` is already 0.3.x.
  - `bincode 1.3.3` unmaintained (RUSTSEC-2025-0141) — used directly
    by `sentrix-trie` for MDBX value encoding. Migration to
    `bincode 2.x` planned as a separate task.
  - `paste 1.0.15` unmaintained (RUSTSEC-2024-0436) — transitive via
    `netlink-packet-core` (not reached at runtime on any supported
    platform).
  - `derivative 2.2.0` — transitive via `ark-ff` / `ruint` (same
    upstream track as the tracing-subscriber advisory).

---

## [2.1.0] — 2026-04-19

**Hardening sweep + EVM RPC expansion + native `sentrix_*` namespace +
fast-deploy workflow.** ~60 PRs since v2.0.0. Most of this release is
C-/H-/M-level security fixes from the Sprint 1 audit; on top of that,
Sprint 2 RPC adds event log / fee RPC so MetaMask and dApp indexers
work natively, and deploy moves from CI to `fast-deploy.sh` on VPS4.

### Added
- **Ethereum event log + fee RPC (Sprint 2)** (PR #144) — `eth_getLogs`,
  `eth_feeHistory`, `eth_maxPriorityFeePerGas`, plus real logs on
  `eth_getTransactionReceipt` (was hardcoded `[]`). New MDBX tables
  `TABLE_LOGS` (height + tx_index + log_index BE → bincode StoredLog)
  and `TABLE_BLOOM` (height → 2048-bit bloom per yellow-paper §4.4.3).
  Block executor persists logs + bloom on Pass 2; address filter runs
  through the per-block bloom prefilter. Range capped at 10 000 blocks
  (`-32005`). Fee history returns flat `INITIAL_BASE_FEE` for now (no
  dynamic base-fee yet); `gasUsedRatio` reflects real per-block EVM
  consumption. Unlocks MetaMask gas estimation + dApp event indexing.
- **Sentrix native JSON-RPC namespace (Sprint 1)** (PR #137) — five
  methods that expose chain features `eth_*` cannot represent:
  `sentrix_getValidatorSet`, `sentrix_getDelegations`,
  `sentrix_getStakingRewards`, `sentrix_getBftStatus`,
  `sentrix_getFinalizedHeight`. Amounts in wei hex so existing bignum
  libraries keep working. See `docs/operations/API_REFERENCE.md`.
- **Explorer + wallet REST endpoints** (PR #136) — consolidated
  endpoint surface for `sentrix-scan` (explorer) and `wallet-web`
  (dApp wallet) so neither has to scrape HTML.
- **Genesis externalization** (PR #104, #105) — `--genesis <path>` CLI
  flag + embedded mainnet default; `Blockchain::new` now driven from
  Genesis TOML. Testnets no longer need a custom binary.
- **fast-deploy.sh primary deploy path** (PR #139) — builds on VPS4,
  uploads binary via wg1 SCP, rolling restart with bounded health
  check. ~3–5 min vs ~10–12 min for CI cold cargo cache. CI `deploy`
  job disabled (`if: false`); CI still runs tests for audit trail.
- **emergency-deploy.sh break-glass script** (PR #135) — skips the
  preflight test gate, requires strict confirmation phrase. For chain
  halt / active exploit / CI outage only.

### Changed
- **Root endpoint self-describe** (PR #140) — `GET /` now returns the
  full REST endpoint map (accounts, staking, epoch, mempool, metrics)
  plus a `jsonrpc_namespaces` section advertising `eth_*`, `net_*`,
  `web3_*`, `sentrix_*`. Adds `consensus` (PoA/BFT from chain_id) and
  `native_token` ("SRX") so wallets can discover chain semantics.
- **fast-deploy builds in bullseye container** (PR #141) — runs inside
  `rust:1.95-bullseye` (glibc 2.31) so binaries work on every target
  regardless of VPS4 host OS. Fixes crash-loop on commit e49e01d where
  a VPS4 24.04 native build (glibc 2.39) failed to load on VPS1/VPS2.
- **chain_id consolidation** (PR #104) — removed hardcoded `CHAIN_ID`
  fallback in EVM; all call sites must pass `chain_id`. `MAX_SUPPLY`
  constants deduped and imported from `blockchain` module.
- **Dep hygiene** — `secp256k1` 0.29 → 0.31, `rand` 0.8 → 0.9, GitHub
  Actions → v5 (Node.js 24).

### Fixed
- **BFT re-propose on round timeout** (PR #113, issue: testnet stall
  ~10500) — engine now re-proposes on `TimeoutAdvanceRound` and
  `SkipRound`. Was the P1 root cause of testnet BFT liveness gaps.
- **BFT catch_up silent validator** (PR #134, issue #133) — after
  catch-up the validator stayed silent in Propose because
  `our_prevote_cast` was never flipped. Now casts an explicit nil
  prevote + flips the flag.
- **BFT refuse rounds below MIN_BFT_VALIDATORS** (PR #124) — prevents
  a degenerate single-validator BFT round from rubber-stamping state.
- **Trie LRU/MDBX race** (PR #126) — closed race in `get_node` and
  `delete_node` where an LRU evict could clobber a concurrent read.
- **sentrix_getValidatorSet PoA fallback** (PR #138) — pre-Voyager
  `stake_registry` is empty; handler now falls back to
  `AuthorityManager` and tags the response `consensus: "PoA"`.
- **EVM sentri→wei conversion** (PR #121) — correct unit conversion at
  the EVM boundary, EIP-170 bytecode cap enforced, `fits_in_block`
  wired so block gas limit is actually checked.
- **Storage integrity check on blockchain load** (M-05, PR #128) —
  chain header checksum verified on startup before serving traffic.
- **Mempool invariant doc** (M-13, PR #128) — documented sender/nonce
  ordering invariants that downstream code relied on implicitly.
- **RPC batch pre-deserialize size check** (M-03, PR #127) — bounds
  batch size before decode to prevent memory amplification.
- **RPC strict address/hash validation** (M-11, PR #127) — rejects
  malformed 0x-prefixed inputs at parse time instead of propagating.
- **Staking slash ceiling rounding + per-validator unbonding cap**
  (PR #123) — closes off a rounding path that could over-slash and a
  missing per-validator cap on concurrent unbonds.
- **Consensus + RPC overflow hardening batch** (PR #119) — mixed set
  of integer overflow + validation tightening.
- **BFT channel SendError + validator task join on shutdown** (C-07,
  C-08, PR #117) — log errors instead of silently dropping; join the
  validator task so shutdown is clean.
- **Pre-validate in BFT finalize path** (PR #132) — read-only
  pre-validate runs before finalize so a malformed block fails fast.
- **Abort process on panic — systemd restarts** (PR #130) — was
  previously swallowing panics in worker tasks.
- **CI deploy workflow aligned with actual v2.0.0 VPS state** (commit
  0b59b42) — stale assumptions from pre-v2.0 purged.

### Security
- **C-01 — BFT signature verify at network boundary** (PR #107) — BFT
  verification moved to the libp2p boundary; validator-set membership
  enforced on inbound BFT; `RoundStatus` messages now signed + verified.
- **H-01 — fast-reject cross-chain NewBlock / NewTx at libp2p boundary**
  (PR #107) — mismatched chain_id rejected before deserialize.
- **C-03 — atomic rollback of Pass 2 mutations on Err** (PR #116) —
  previously a mid-Pass-2 failure could leave the account DB in a
  partially-mutated state. Now all Pass 2 writes are staged and
  rolled back atomically on any Err.
- **C-04 — prevent COINBASE tx forgery in block validation** (commit
  d886023) — block validator now rejects non-producer COINBASE txs.
- **C-05 / H-06 — reject duplicate txid and (sender, nonce) at block
  layer** (PR #114) — closes the Merkle CVE-2012-2459 dedup gap plus
  the H-06 duplicate-nonce double-spend.
- **H-09 — only broadcast block after persistence succeeds** (PR #115)
  — previously a persistence failure could still emit the block gossip.
- **C-06 — removed `--validator-key <hex>` CLI flag** (PR #109) —
  private keys as CLI args leak via `ps aux`, shell history, and
  process snapshots. Validators must now use
  `--validator-keystore <path>` (Argon2id v2) or
  `SENTRIX_VALIDATOR_KEY` env var. **Breaking for validator operators.**
- **C-06 hardening — Wallet zeroize plumbed through startup** (PR #109)
  — validator key no longer round-trips through an unzeroed heap
  `String` in `cmd_start`; the `Wallet`'s `Zeroizing<[u8; 32]>` is the
  only owner of the secret bytes after key resolution.
- **C-09 — clamp max_commission_rate to MAX_COMMISSION on registration**
  (PR #118) — validator registration now enforces the protocol ceiling.
- **RPC gas cap, API-key minimum length, socket-IP rate limiting**
  (PR #120) — closes three independent P1 abuse vectors.
- **yamux 0.12 upgrade + non-reachability note** (PR #108) — documented
  that the advisoried code path isn't reachable in our config; upgrade
  applied anyway to stay on the supported track.
- **CI/CD workflow least-privilege permissions** (PR #129) — declared
  minimal `permissions:` blocks on every workflow.

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

**SRC-20 Token Standard**
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
- SRC-20 token operations: deploy, transfer, burn, balance, info, list, holders, trades
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

[Unreleased]: https://github.com/sentrix-labs/sentrix/compare/v2.1.1...HEAD
[2.1.1]: https://github.com/sentrix-labs/sentrix/compare/v2.1.0...v2.1.1
[2.1.0]: https://github.com/sentrix-labs/sentrix/compare/v2.0.0...v2.1.0
[2.0.0]: https://github.com/sentrix-labs/sentrix/compare/v1.5.0...v2.0.0
[1.5.0]: https://github.com/sentrix-labs/sentrix/compare/v1.4.0...v1.5.0
[1.4.0]: https://github.com/sentrix-labs/sentrix/compare/v1.3.1...v1.4.0
[1.3.1]: https://github.com/sentrix-labs/sentrix/compare/v1.3.0...v1.3.1
[1.3.0]: https://github.com/sentrix-labs/sentrix/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/sentrix-labs/sentrix/compare/v1.1.0...v1.2.0
[1.1.0]: https://github.com/sentrix-labs/sentrix/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/sentrix-labs/sentrix/compare/v0.1.0...v1.0.0
[0.1.0]: https://github.com/sentrix-labs/sentrix/releases/tag/v0.1.0
