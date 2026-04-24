# Changelog

All notable changes to Sentrix are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Fixed

- **staking(commission): rate-limit `update_commission` to one change per epoch per validator** (`crates/sentrix-staking/src/staking.rs`). Previously an operator could call `update_commission` repeatedly within one block — each call stayed inside the 2% per-step cap (`MAX_COMMISSION_CHANGE_PER_EPOCH`), but cumulative drift was unbounded (N × 2% per block). This closes the V5 Voyager-blocker entry from the 2026-04-20 audit. New regression test `test_commission_stepping_attack_rejected_same_epoch` pins the invariant. `update_commission` now takes a `current_height: u64` argument; the existing `test_commission_update` was refreshed to thread the height through. New field `last_commission_change_height: u64` on `ValidatorStake` (0 = never changed) tracks the throttle; marked `#[serde(default)]` so fresh-deploy chains can slot the field in without a hard migration.
- **bft/staking(jailing): enforce `is_jailed` at consensus boundary** (`crates/sentrix-staking/src/staking.rs` + `crates/sentrix-bft/src/engine.rs`). Closes V3 Voyager-blocker from the 2026-04-20 audit: the `is_jailed` flag existed but consensus never cross-referenced it, so a jailed validator could still propose blocks as long as they sat in `active_set`. Two-layer fix: (1) `slash` / `jail` / `tombstone` / `unjail` now call `update_active_set()` so the jail status propagates to the rotation pool immediately instead of waiting for the next epoch tick; (2) `BftEngine::on_proposal` cross-references the stake registry and refuses proposals from any validator currently flagged `is_jailed`, `is_tombstoned`, or not registered at all — even if `weighted_proposer` returned their address (defense against stale-active_set races between slash-apply and next-proposal-arrive). Three new staking regression tests (`test_jail_evicts_from_active_set_immediately`, `test_tombstone_evicts_permanently`, `test_unjail_readmits_to_active_set`) + two BFT regression tests (`test_jailed_proposer_rejected_at_on_proposal`, `test_unregistered_proposer_rejected_at_on_proposal`) pin both layers. Single-HashMap-lookup overhead on the hot path — cheap.
- **bft(justification): emit REAL precommit signatures at finalization** (`crates/sentrix-bft/src/engine.rs`). Closes V1 Voyager-blocker from the 2026-04-20 audit. Before: the finalize loop at the 2/3+ precommit pivot passed `vec![]` placeholders to `BlockJustification::add_precommit`, so every finalized block carried a cryptographically meaningless proof — a silent-reorg and cross-fork replay surface the moment Voyager activated. Now: `state.precommits` is typed `HashMap<String, (Option<String>, Vec<u8>, u64)>` (block_hash + signature + stake), `on_precommit_weighted` stores `precommit.signature.clone()` alongside the vote, and the finalize emit loop only includes precommits that voted for the winning hash (nil and wrong-hash votes are correctly excluded). New regression test `test_finalize_emits_real_precommit_signatures` injects three distinct signatures + one nil-precommit + one wrong-hash-precommit and asserts only the winning-hash sigs land in the emitted justification, each non-empty and byte-preserved from input.
- **core(block_executor): snapshot + restore trie root on Pass 2 failure** (`crates/sentrix-core/src/block_executor.rs` + `crates/sentrix-trie/src/tree.rs`). The C-03 atomic-commit snapshot already restored `accounts` / `contracts` / `authority` / `mempool` / `total_minted` / `chain` on Pass 2 error, but it explicitly did NOT snapshot the state trie — a pre-PR-#184 comment claimed the trie "self-heals" because it gets rebuilt from `accounts` on each `update_trie_for_block` call. That claim was wrong: trie insert/delete walks the current `self.root` — it is NOT recomputed from scratch. So a Pass 2 that failed partway through trie updates would leave the in-memory root pointing at a half-updated state while `accounts` was reverted — silent divergence on the next block, exactly the failure class the 2026-04-20 mainnet incident showed. Fix: capture `state_trie.root_hash()` into `BlockchainSnapshot.trie_root` before Pass 2, restore it via new `SentrixTrie::set_root` on failure. Orphan MDBX nodes from the failed block's partial inserts remain in storage but are unreachable from any committed root; the scheduled `prune(keep_versions)` at TRIE_PRUNE_EVERY-aligned heights (1000 by default) GCs them. New regression test `test_set_root_rewinds_to_known_committed_root` in `sentrix-trie` pins the `get`-after-rewind contract.

### Added

- **rpc(rest): `GET /chain/finalized-height`** — REST alias for the existing `sentrix_getFinalizedHeight` JSON-RPC method. Closes the A5 audit finding. Returns `finalized_height` / `finalized_hash` / `latest_height` / `blocks_behind_finality` / `consensus`. On Pioneer PoA every committed block is final — endpoint returns the tip. On Voyager BFT walks back from the tip for the newest block with `justification.is_some()`. Lets light clients + dashboards + Prometheus exporters learn finality lag without speaking JSON-RPC.
- **rpc(metrics): supply + burn counters exposed at `/metrics`** — three new Prometheus gauges/counters: `sentrix_total_minted_sentri` (counter, all SRX ever minted), `sentrix_total_burned_sentri` (counter, all SRX burned from fee split + explicit burns), `sentrix_circulating_supply_sentri` (gauge = minted − burned). Unlocks supply-curve and burn-rate charts in Grafana + lets Prometheus alert on supply-invariant violations (e.g. `sentrix_total_minted_sentri > MAX_SUPPLY_sentri` should NEVER fire on a healthy chain). Raw sentri integers (not SRX floats) so rates/deltas stay exact across 1 SRX = 100M sentri scale.

### Fixed

- **bin,rpc(metrics): surface silent P2P block-save failures (BACKLOG #16)** (`bin/sentrix/src/main.rs` + `crates/sentrix-rpc/src/routes/ops.rs`). A `warn!`-only log on `save_block` failure at `NodeEvent::NewBlock` would silently drop a block from MDBX while chain state had already advanced in memory via `add_block_from_peer` — once CHAIN_WINDOW_SIZE (1000 blocks) rolled past, that block was gone from everywhere, creating a permanent TABLE_META gap. Exactly the shape the 2026-04-23 PR #226 sweep test surfaced on live mainnet chain.db: 7,352 missing `block:N` keys, longest contiguous run 5,042 at h=139,703. Fix is observability-only for now: (1) log escalates to `error!` with explicit "BACKLOG #16" tag + block index + hash + MDBX error, (2) new `sentrix_peer_block_save_fails_total` counter exported on `/metrics` (new `pub static PEER_BLOCK_SAVE_FAILS` AtomicU64 in `sentrix_rpc::routes::ops`, incremented by main's peer-block handler on each failure), (3) new Prometheus alert rule `PeerBlockSaveFailing` fires critical on `rate > 0` — operator now gets Telegram alert at the moment of the gap-creating event, not weeks later via sweep test. Durable fix (atomic `add_block_from_peer` + `save_block` with rollback on persist failure) requires storage plumbing into `sentrix-core` — out of scope for this observability patch.

## [2.1.11] — 2026-04-23 — MIN_ACTIVE_VALIDATORS: 3 → 1 (bootstrap-friendly)

Patch release. Unlocks a legitimate ops pattern that the previous hard floor blocked: running the chain with as few as one validator during bootstrap, disaster-recovery, or a deliberate centralisation window.

### Changed

- **core(authority): MIN_ACTIVE_VALIDATORS 3 → 1** (`crates/sentrix-core/src/authority.rs`). The guard now only stops the admin from deactivating or removing the *last* validator — anything above that is fine. The old floor of 3 baked a specific topology into the protocol and wouldn't let the operator scale back under it even when consensus was fine with any count ≥ 1. Round-robin math (`height % active.len()`) already handled any validator count, so this is a CLI-only guard change — consensus, block validation, and gossip paths are untouched.
- Four test expectations refreshed to the new floor: `test_h03_toggle_cannot_deactivate_last_validator`, `test_v501_remove_enforces_min_active_validators`, `test_v501_toggle_enforces_min_active_validators`, `test_h03_toggle_allows_deactivate_with_others`. All now assert the MIN=1 boundary.

### Operator note

`sentrix validator toggle` / `remove` must be run on every validator's chain DB to keep the `is_active` flag consistent cluster-wide — admin ops are local-node-state, not transactions. Centralising to one validator means zero fault tolerance (the single host is a hard single point of failure); treat it as a temporary state, not a destination.

## [2.1.10] — 2026-04-23 — Network tuning + sentrix-wire split

Patch release bundling three network-layer improvements. Zero consensus impact — all changes are network/observability config or pure refactors.

### Added

- **network: tune gossipsub + RR for small validator mesh** (`crates/sentrix-network/src/behaviour.rs`). Defaults retuned for Sentrix's current mesh sizes (3 mainnet, 4 testnet — separate meshes per chain_id). Heartbeat 5s→300ms, flood_publish true, mesh_n_low 5→2, history trimmed, RR request timeout 60s→15s. Addresses the #1d rebroadcast-livelock symptom. PR #219.
- **network: env-var overrides for gossipsub + RR tunables** (`crates/sentrix-network/src/behaviour.rs`). Every tunable overridable via `SENTRIX_GOSSIP_*` / `SENTRIX_RR_REQUEST_TIMEOUT_SECS` env vars so operators can retune for mesh growth (hundreds-to-thousands of validators) WITHOUT rebuilding the binary. Defaults preserve PR #219 values; no behavior change today. PR #220.

### Changed

- **refactor(network): split sentrix-wire crate (Tier 1 #5)** (`crates/sentrix-wire/`, `crates/sentrix-network/src/behaviour.rs`). Extracts `SentrixRequest`, `SentrixResponse`, `GossipBlock`, `GossipTransaction`, `SENTRIX_PROTOCOL`, `BLOCKS_TOPIC`, `TXS_TOPIC`, `MAX_MESSAGE_BYTES` into their own crate so downstream tooling (future `sentrix-sdk`, monitoring, light clients) can reference canonical wire types without pulling the full libp2p stack. Back-compat shim keeps existing imports working. Pure refactor; bincode encoding unchanged. PR #221.

## [2.1.9] — 2026-04-23 — Divergence rate-alarm for silent state_root drift

Patch release over v2.1.8. Single change: adds a rate-limited LOUD alarm when a validator rejects peer blocks at a sustained rate — motivated by the 2026-04-23 mainnet fork investigation, where VPS3 had been silently rejecting peer blocks for ≥4 hours (~4000 state_root mismatches per hour) without any operator signal. The existing per-event `CRITICAL #1e` log line was accurate but emitted at ~1/s during real divergence, filling journald rotation so the earliest mismatches were evicted before the operator checked.

### Added

- **core: divergence rate-alarm** (`crates/sentrix-core/src/blockchain.rs`, `crates/sentrix-core/src/block_executor.rs`). New `DivergenceTracker` records state_root-mismatch rejections in a rolling 5-minute window; when the count crosses 100 within the window, emits one `ERROR`-level alarm with the rsync-from-peer recovery playbook inline. Rate-limited to one alarm per 60 seconds so the signal stays punchy rather than spamming. Counter is in-memory only (resets on restart — a validator that diverged 6h ago but is clean now shouldn't keep alarming). Not a consensus change; pure observability. PR #217.

## [2.1.8] — 2026-04-22 — Sentrix-style liveness thresholds

Patch release over v2.1.7. Adds exactly one change: retuned the validator liveness-slashing thresholds from Tendermint's demo defaults to Sentrix-style values calibrated for 1-second block time + solo-operator ops. Config-only; no algorithm change.

### Changed

- **consensus(staking): retune liveness thresholds to Sentrix-style values** (`crates/sentrix-staking/src/slashing.rs`). Previous config was Tendermint's reference demo default (100-block window, 50% threshold) which at Sentrix's 1-second block time meant a ~50-second downtime budget before auto-jail — far too tight for realistic operator ops. New values:

  ```
  LIVENESS_WINDOW        100    → 14_400   (100s  → ~4h      @ 1s)
  MIN_SIGNED_PER_WINDOW  50     → 4_320    (50%   → 30%)
  DOWNTIME_SLASH_BP      100    → 10       (1%    → 0.1%)
  DOWNTIME_JAIL_BLOCKS   200    → 600      (3.3m  → 10m     @ 1s)
  DOUBLE_SIGN_SLASH_BP   2000             (unchanged, 20%)
  ```

  Tolerance contract: weekly 10-min deploys absorbed, 30-min emergency recoveries absorbed, 2-hour debugging sessions absorbed, 3-hour outages jailed. **Mainnet impact today: zero** — Pioneer PoA doesn't consult the staking/slashing registry (that's `authority_manager.rs`). Takes effect on Voyager activation. On testnet: active next deploy; should eliminate the auto-jail cascades observed during rolling `fast-deploy` today. PR #215.

## [2.1.7] — 2026-04-22 — Post-fork hardening (3-way state_root fork follow-up)

Post-mortem release after the 2026-04-21 mainnet 3-way state_root fork. The fork itself was recovered ops-side via frozen-rsync of VPS1 canonical chain.db to VPS2 + VPS3 (see `founder-private/incidents/2026-04-21-mainnet-3way-fork.md`). This release closes the code-level gaps that let the incident develop silently on the v2.1.6 binary after a pre-v2.1.5 `state_import` had already damaged VPS3's trie.

### Fork follow-ups

- **fix(trie): boot-time integrity check — refuse to start on orphan trie references** (`crates/sentrix-trie/src/tree.rs`, `crates/sentrix-core/src/blockchain.rs`). New `SentrixTrie::verify_integrity()` walks the current root and fails fast if any referenced node or leaf-value is missing from `trie_nodes` / `trie_values`. Wired into `Blockchain::init_trie`: hard-fail past `STATE_ROOT_FORK_HEIGHT`, warn-only below. In the 2026-04-21 incident, VPS3's chain.db had a top-level root that existed but referenced an orphaned subtree, so the existing backfill-mismatch and missing-root-node guards didn't fire — it just produced `state_root=None` blocks that strict peers then rejected. PR #206.
- **fix(cli): guard `sentrix state import` and `sentrix chain reset-trie` against non-genesis chain** (`bin/sentrix/src/main.rs`). Both commands now refuse on `height > 0` with an error pointing at rsync-from-peer as the correct recovery. Env-var escape hatch for devnet (`SENTRIX_ALLOW_STATE_IMPORT_ON_NONZERO_HEIGHT=1` / `SENTRIX_ALLOW_RESET_TRIE_ON_NONZERO_HEIGHT=1`), intentionally ugly names. PR #207.
- **fix(network): boundary-reject state_root=None blocks past fork height** (`crates/sentrix-network/src/libp2p_node.rs`). New `block_boundary_reject_reason()` helper rejects obvious-bad blocks at network ingest (both RequestResponse and Gossipsub paths) before spawning an apply task. Moves the existing v2.1.5 execution-time state_root guard earlier in the pipeline — same blocks rejected, just without contending for the chain write lock and without the flood of CRITICAL logs that previously filled the journald cap within hours during an incident. PR #208.

### Added

- **feat(rpc): real gas estimation via EVM dry-run** (`crates/sentrix-rpc/src/jsonrpc/eth.rs`). `eth_estimateGas` now delegates to a shared `run_evm_dry_run()` helper that executes the call through `execute_call`, replacing the flat 21_000 / 100_000 heuristic with `receipt.gas_used`. Reverting transactions surface `-32000` with revert-data hex (Geth-compatible — a reverting tx has no meaningful gas estimate). PR #210.
- **feat(rpc): add `effectiveGasPrice` + `type` to EVM tx receipts** (`crates/sentrix-rpc/src/jsonrpc/eth.rs`). MetaMask / ethers.js / web3.js now see `type = 0x2` for EVM-envelope txs, `0x0` for native, and `effectiveGasPrice = INITIAL_BASE_FEE` for EVM. `gasUsed` remains a flat `21_000` pending a per-tx schema change. PR #211.

### Changed

- **chore(rpc): tighten RPC input validation** (`crates/sentrix-rpc/src/jsonrpc/eth.rs`). `eth_estimateGas` rejects non-object params[0] with `-32602` instead of silently defaulting to `21_000`. `eth_getCode` and `eth_getStorageAt` now call `normalize_rpc_address` like the balance/nonce endpoints, rejecting malformed addresses at ingress. `eth_getStorageAt` also validates the storage slot is valid hex ≤ 32 bytes. PR #205.
- **test(staking): add delegation-sum invariant proptest** (`crates/sentrix-staking/src/staking.rs`). Runs 500 random delegate/undelegate/redelegate/slash ops against 4 validators × 6 delegators and asserts `Σ per-delegator entries to V == validators[V].total_delegated` after each op. Fixed-seed LCG keeps it reproducible. 74 staking tests pass (was 73). PR #205.
- **test(p2p): unhardcode integration-test ports via new `listen_addrs()` API** (`crates/sentrix-network/src/libp2p_node.rs`, `tests/integration_p2p.rs`). New `LibP2pNode::listen_addrs()` method + `bind_random_port()` test helper. All 4 P2P integration tests now bind on port 0 (OS-assigned) and dial the reported port, removing the parallel-run port collisions that used to happen on 39101-39104. PR #209.

### Dependencies

- **chore(deps): audit + clean deny.toml skip[] list** (`deny.toml`). Removed 4 stale skip entries whose duplicates have resolved (`bitflags`, `parking_lot`, `parking_lot_core`, `redox_syscall`). Kept 11 entries with per-entry justification comments explaining the upstream split + what needs to happen for the skip to drop. PR #212.
- **deps: sweep open security advisories (2026-04-22)** (`deny.toml`, `Cargo.lock`). One real vulnerability fixed: `RUSTSEC-2026-0104` (rustls-webpki 0.103.12 CRL parsing panic) bumped to 0.103.13 via `cargo update`. Five informational / unmaintained advisories documented + ignored with per-advisory reachability analysis: `RUSTSEC-2025-0055` (tracing-subscriber 0.2 ANSI — never `.init()`ed), `RUSTSEC-2026-0105` (core2 yanked — no security surface), `RUSTSEC-2025-0141` (bincode unmaintained — sentrix-codec wraps v1 API pending migration), `RUSTSEC-2024-0388` + `RUSTSEC-2024-0436` (derivative / paste — proc-macro only). `cargo deny check` now passes all four sections. PR #213.

## [2.1.6] — 2026-04-21 — RPC validation hardening

### Fixed
- **fix(rpc): reject wei values not divisible by 1e10 in eth_sendRawTransaction**
  (`crates/sentrix-rpc/src/jsonrpc/eth.rs`). Sentrix's on-chain unit is sentri
  (1 SRX = 1e8 sentri = 1e18 wei), so values below 1e10 wei are sub-sentri
  dust. Truncating them silently meant a caller sending 10_000_000_001 wei
  saw 1 sentri transferred and 9 wei unaccounted — neither burned, refunded,
  nor credited. The handler now returns JSON-RPC `-32602` if
  `value_wei % 1e10 != 0`, surfacing the mismatch at the boundary.
- **fix(rpc): eth_getBlockByNumber returns -32602 on invalid hex**
  (`crates/sentrix-rpc/src/jsonrpc/eth.rs`). Previously a typo like `0xZZ`
  or a non-hex string silently mapped to block 0 (genesis), so clients
  took genesis data at face value for broken inputs. Now the parse error
  is propagated with the offending value quoted.
- **fix(rpc): panic at startup when SENTRIX_CORS_ORIGIN is unparseable**
  (`crates/sentrix-rpc/src/routes/mod.rs`). A malformed env value used to
  silently fall back to the literal "null" header, producing a router
  that rejected every browser request without any signal to the operator.
  Now an invalid value aborts `create_router` with a clear panic message
  naming the offending string — fail-fast at config validation time.

## [2.1.5] — 2026-04-21 — Trie backfill divergence guard (bug #3)

### Fixed
- **fix(trie): refuse to start when backfill diverges from stored block
  state_root (bug #3)** (`crates/sentrix-core/src/blockchain.rs`). The
  incremental path (`update_trie_for_block`) only inserts accounts
  touched by a block, while the backfill path (`init_trie` at height > 0)
  inserts every account with balance > 0 — including premines that
  were never touched by a tx. For the same logical state the two paths
  produce different leaf sets. A validator that recovered via
  `sentrix state import` + PR #187's auto-reset therefore rebuilt a
  trie whose state_root silently disagreed with peers, and every block
  it produced tripped the #1e strict-reject guard. This was the exact
  shape of the 2026-04-21 mainnet freeze (VPS2 drifted ~45 SRX/min
  from canonical after state-import; chain halted).

  We cannot align the two paths without changing consensus history
  (sync-from-genesis would fail). Instead `init_trie` now reads the
  stored `state_root` on the block at that height and refuses to start
  if the freshly-backfilled root disagrees. Operators hitting this must
  recover via `rsync /opt/sentrix/data/chain.db` from a healthy peer
  with all validators stopped — a whole-trie copy preserves the
  incremental shape. New regression test
  `test_reset_trie_then_init_refuses_on_backfill_divergence` pins
  the safeguard; it fails on main and passes with the fix.

## [2.1.4] — 2026-04-20 — Extended #1d rebroadcast window

### Fixed
- **fix(bft): widen proposer rebroadcast window 9s → 14s + cover Prevote
  phase too** (`bin/sentrix/src/main.rs`). v2.1.3's 3 retries × 3s
  helped but didn't fully close #1d on testnet — bake logs showed the
  4th validator (often after restart) still missed the proposal during
  the 9s window because it took ~10s+ to enter the proposer's
  `verified_peers` set. v2.1.4 bumps the retry budget to 7 attempts at
  2s each (= 14s of live retry) and lets the proposer keep
  rebroadcasting after it has moved into Prevote phase, so a peer that
  finally appears mid-prevote can still validate the prevotes it's
  receiving from the proposer. 14s fits inside the 20s propose
  timeout, so the additional retries don't push past the round
  boundary.

## [2.1.3] — 2026-04-20 — Runtime trie divergence guard (backlog #1e)

### Fixed
- **fix(consensus): reject peer blocks with state_root=None past
  STATE_ROOT_FORK_HEIGHT** (`crates/sentrix-core/src/block_executor.rs`).
  Root cause of the 2026-04-20 mainnet fork that recurred even after
  v2.1.2: `apply_block_pass2` treated every block arriving with
  `state_root = None` as self-produced and silently stamped its own
  computed root + recomputed the block hash. When a peer with a broken
  trie broadcast a block with `state_root = None`, local nodes accepted
  it, stamped a different root, stored a different hash — the next
  block's `previous_hash` check failed against the peer's view and the
  chain forked.

  Fix: admission path now threads a `BlockSource` enum. New
  `Blockchain::add_block_from_peer` routes incoming P2P / BFT-finalized
  blocks through a strict branch that returns
  `ChainValidationFailed` on `state_root = None` past fork height.
  `Blockchain::add_block` stays as the permissive self-proposed path
  (used by `build_block` where `state_root = None` is legitimate
  because Pass 2 stamps it). All `sentrix-network` sync/gossip call
  sites updated to `add_block_from_peer`. CRITICAL-level logs emitted
  on both the None-from-peer and the `received_root != computed_root`
  branches so operators see trie divergence loud in journalctl.

## [2.1.2] — 2026-04-20 — Trie-init hardening hotfix

### Fixed
- **fix(consensus): hard-fail trie init above STATE_ROOT_FORK_HEIGHT**
  (`crates/sentrix-core/src/storage.rs`). Before: init_trie failure was
  a tracing warn and the node continued, producing blocks with
  `state_root = None` while peers with working tries produced
  `state_root = Some(...)` — the hashes diverge and the chain forks.
  Mainnet stalled at block 100,004 on 2026-04-20 via exactly this path:
  VPS3's trie had a missing node (`24afba5f…`) so block 100,004 got
  saved with `state_root = null`, VPS1 had a functional trie and block
  100,004 with `state_root = Some(…)` → different block hashes → VPS1
  rejected VPS3's block 100,005 as "invalid previous hash". Post-fix,
  any node whose trie cannot init past fork height refuses to start —
  a silently diverging validator is worse for the network than an
  offline one. Below fork height the old hash format ignores
  `state_root`, so the warn-only path is still safe there.

### Refactored
- **refactor(rpc): transactions + epoch handlers out of
  `routes/mod.rs`** (backlog #12 phase 2f + 2g, final slices) — 5
  handlers moved into two new modules:
  - `routes/transactions.rs` — `send_transaction`, `get_transaction`,
    `get_mempool`. `sentrix_primitives::transaction::Transaction` import
    follows the handler.
  - `routes/epoch.rs` — `epoch_current`, `epoch_history`.
  `routes/mod.rs` is now down to router wiring + the shared `api_err`
  helper; the per-resource handler modules are complete. Zero route /
  behaviour change. Closes backlog #12 phase 2.
- **refactor(rpc): accounts handlers out to `routes/accounts.rs`**
  (backlog #12 phase 2e) — 9 address-indexed handlers moved:
  `get_balance`, `get_nonce`, `get_wallet_info`, `list_transactions`,
  `get_richlist`, `get_address_history`, `get_address_info`,
  `get_address_proof` (state-trie Merkle proof), `get_state_root`
  (per-height root lookup). `sentrix_trie::address` imports follow
  the handlers into their new home. Zero route / behaviour change.
- **refactor(rpc): chain handlers out to `routes/chain.rs`** (backlog
  #12 phase 2d) — 4 handlers moved: `chain_info` (`GET /chain/info`),
  `get_blocks` (paginated listing), `get_block` (by index),
  `validate_chain` (full-chain scan, auth-gated with result cache).
  The `VALIDATE_CACHE_HEIGHT` / `VALIDATE_CACHE_RESULT` statics +
  their M-07 cache tests moved with the handler. Zero route /
  behaviour change.
- **refactor(rpc): ops handlers out to `routes/ops.rs`** (backlog #12
  phase 2c) — five handlers moved: `root` (`GET /`), `health`
  (`GET /health`), `sentrix_status` (`GET /sentrix_status`, re-exported
  pub for integration tests), `metrics` (`GET /metrics`), `get_admin_log`
  (`GET /admin/log`, auth-gated). Shared `START_TIME` `OnceLock` moved
  with them. Zero route / behaviour change.
- **refactor(rpc): staking handlers out to `routes/staking.rs`**
  (backlog #12 phase 2b) — 4 handlers moved: `get_validators`
  (PoA authority set, `GET /validators`), `staking_validators`
  (DPoS set, `GET /staking/validators`), `staking_delegations`
  (`GET /staking/delegations/{addr}`), `staking_unbonding`
  (`GET /staking/unbonding/{addr}`). Zero route / behaviour change.
- **refactor(rpc): token handlers out to `routes/tokens.rs`** (backlog
  #12 phase 2a) — the 8 SRC-20 handlers (list / info / balance /
  holders / trades / deploy / transfer / burn) move to their own
  module. `api_err` is now `pub(super)` so modules can reuse it.
  No route path or behaviour change. Follow-up slices (staking, ops,
  chain, accounts, transactions, epoch) get the same treatment.
- **refactor(rpc): split jsonrpc handlers by namespace** (backlog #11
  phase 2) — the 900-line match in `jsonrpc/mod.rs::jsonrpc_handler`
  is now a prefix-dispatch to four namespace modules: `eth.rs`
  (20 methods), `net.rs` (2), `web3.rs` (1), `sentrix.rs` (7). Each
  module exposes a `dispatch(method, &params, &state) -> DispatchResult`
  async fn; `mod.rs` routes by `method.starts_with("eth_" | "net_" |
  "web3_" | "sentrix_")`. Error paths converted from the old
  `return Json(JsonRpcResponse::err(id, code, msg))` pattern to
  `Err((code, msg))`; the envelope wrap happens once in the top-level
  handler. No behaviour change, all tests pass, clippy clean.

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
