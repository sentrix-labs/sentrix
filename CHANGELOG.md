# Changelog

All notable changes to Sentrix are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
This project uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

> Empty placeholder — next release will land on this branch.

---

## [2.1.48] — 2026-04-30 — BFT FinalizeBlock hash-mismatch guard (closes recurring chain.db divergence)

> **Closes the recurring chain.db divergence bug** that produced the ~30-min mainnet halts at h=773012 (vps5, 2026-04-28), h=921604 + h=932488 (2026-04-29), and h=1014804 + h=1015365 (2026-04-30, twice in one day). The audit at `audits/2026-04-30-eager-write-investigation.md` traces the actual mechanism: BFT engine's `FinalizeBlock` action carried the supermajority `block_hash` for the round, but the validator-loop handler discarded it (`block_hash: _`) and `.take()`'d whatever was stashed in `proposed_block`. If the validator missed the actual round-N proposal but the cluster's round-N precommits crossed our local supermajority threshold, we'd write a stale stash (a previous round's block) attached to the current round's justification. Next height's `parent_hash` references the cluster-canonical hash; our local height's hash doesn't match; libp2p sync rejects forward blocks with `Invalid block: invalid previous hash`; BFT can't progress. Recovery required chain.db rsync from a canonical peer + halt-all + simul-start.

### Fixed

- **`bin/sentrix/src/main.rs`** (both FinalizeBlock arms — own-propose path at L2084 and peer-propose path at L2585) — bind the action's `block_hash` and refuse to write when `proposed_block.as_ref().hash != block_hash`. On mismatch: log the divergence at WARN, drop the stash, and break out of the round handler. The chain advances when peer-gossip ships the canonical finalised block (with its justification), which the libp2p add-block path applies via the same `add_block_from_peer` entry the recovery rsync target uses. Net effect: previously-divergent validators now stay LAGGED instead — chain liveness gap closes via the existing gossip path instead of requiring operator rsync.

### Internal

- **Workspace `Cargo.toml` + every internal crate + bin** — bumped `version = "2.1.47" → "2.1.48"` uniformly. `tools/*` keep their independent `0.1.0`.
- **Build via Docker `rust:1.95-bullseye`** to target glibc 2.31 (Debian Bullseye baseline). Building on the local host (Ubuntu 24.04 / glibc 2.39) produces a binary that requires GLIBC_2.38, which the older mainnet validator hosts don't ship.

### Operational notes

- Deployed via halt-all + scp + simul-start. vps2's `chain.db` was corrupted during the failed glibc-2.38 attempt and recovered via tar-pipe rsync from vps1 canonical (`05b6b374ce01857e8556058d2688ab05`).
- All four mainnet validators on `v2.1.48`, advancing in lockstep at h≈1019300+ post-deploy.

---

## [2.1.47] — 2026-04-28 — eth_call → revm wiring + EIP-7825 gas cap fix + version bump

> **Mainnet `eth_call` now actually executes against revm + live chain state.** Previously stubbed; canonical-contract reads (`WSRX.name()`, `Multicall3.getCurrentBlockTimestamp()`, etc) returned `0x` empty. Two PRs land the full fix: #389 wires the revm dispatch path, #391 caps the dry-run gas at the EIP-7825 limit so revm doesn't reject every call.

### Added / Fixed

- **`crates/sentrix-rpc/src/jsonrpc/eth.rs`** (#389) — `eth_call` and `eth_estimateGas` now route through revm execution against the live chain state via `SentrixEvmDb::from_account_db(&bc.accounts)` (was `InMemoryDB` which only pre-loaded contract code, leaving every storage slot zero). Address normalization added so EIP-55 checksummed addresses resolve correctly against the lowercase `AccountDB` keys.
- **`crates/sentrix-rpc/src/jsonrpc/eth.rs` + `crates/sentrix-evm/src/gas.rs`** (#391) — add `TX_GAS_LIMIT_CAP = 16_777_216` constant (EIP-7825) and clamp eth_call dry-run gas at this value instead of `BLOCK_GAS_LIMIT` (30M). revm with SpecId ≥ Osaka rejects `gas_limit > cap` with `TxGasLimitGreaterThanCap` even for read-only dry-runs; pre-fix every eth_call returned `0x` empty. Live-discovered after #389 deploy when `cast call <WSRX> "name()(string)"` still returned the ABI-decode error.

### Documentation

- **`genesis/mainnet.toml`** (#390) — clarifying comment block explains the v2 → v3 founder admin transfer history (block 444070, 2026-04-24). Comment-only; genesis hash regression test continues to pass.
- **`README.md`** + **`docs/operations/NETWORKS.md`** + **`CHANGELOG.md`** (#392, #394) — bump v2.1.44 references to v2.1.46 across public docs after the mainnet redeploy; correct stale `[2.1.42]` CHANGELOG label to `[2.1.45]` to match git tag history.

### Internal

- **Workspace `Cargo.toml`** — bumped `version = "2.1.46" → "2.1.47"` across root + every internal crate + bin (uniform). `tools/*` keep their independent `0.1.0`.

---

## [2.1.46] — 2026-04-28 — AddSelfStake activation + reset-trie CLI flag + workspace version bump

> **Mainnet activation of `StakingOp::AddSelfStake`** for non-phantom validator self-bond. One previously-jailed validator was unjailed via real-SRX self-bond. 4-of-4 validators active. Workspace version bumped 2.1.44 → 2.1.46 across root + every internal crate.

### Added / Fixed

- **`StakingOp::AddSelfStake`** (#385) — supply-invariant validator self-bond. Validators can bond real SRX into their own `self_stake` without the phantom-mint that pre-PR-#384 `force-unjail` produced. Fork-gated at `ADD_SELF_STAKE_HEIGHT=731245` on mainnet. Recovery path for slashed validators with `self_stake < MIN_SELF_STAKE`.
- **`reset-trie` CLI flag** (#384) — unblock force-unjail recovery on non-genesis chains. Operator-side recovery printout for the force-unjail one-way trap.
- **Operator-host references scrub** (#386, #387) — IP-allowlist references in tools, source comments, audit docs replaced with generic `RPC_BASE_URL` env var pattern. Final grep across tracked files returns zero matches for the prior internal-host shorthand.

### Internal

- **Workspace `Cargo.toml`** (#388) — bumped `version = "2.1.44" → "2.1.46"` across root + every internal crate + bin (uniform). `tools/*` keep their independent `0.1.0`.

---

## [2.1.45] — 2026-04-27 (later) — Phase A→D consensus-jail full stack + testnet bootstrap

> **The asymmetric-application bug class is now fixed at the protocol level.** Phase A (data types) shipped earlier this day in #359; this batch ships Phase B (helpers + fork gate), Phase C (dispatch verification), and Phase D (proposer emission + Pass-1/Pass-2 wiring + 4-validator determinism harness). Default behavior is unchanged: `JAIL_CONSENSUS_HEIGHT=u64::MAX` on default builds means the entire dispatch path is unreachable until an operator opts in.
>
> Also bootstraps testnet activation: `genesis/testnet.toml` (chain ID 7120) and a standalone faucet HTTP service binary, plus an internal-references hygiene scrub.

### Added

- **`genesis/testnet.toml`** — full testnet genesis (chain_id 7120). Mirrors mainnet allocations + a dedicated faucet wallet entry. Loaded at runtime via `--genesis genesis/testnet.toml`.
- **`bin/sentrix-faucet/`** — standalone HTTP faucet service:
  - `POST /faucet/drip` builds + signs + submits a drip transaction from a pre-loaded keystore
  - `GET /faucet/health` reports current nonce + faucet address
  - Per-IP rate limiting (3 drips per hour default), per-recipient cooldown (24h default)
  - Keystore password loaded from `SENTRIX_FAUCET_PASSWORD` env var (never logged)
  - All defaults configurable via CLI flags + `SENTRIX_FAUCET_*` env vars
- **`crates/sentrix-primitives/src/transaction.rs`**:
  - `Transaction::new_jail_evidence_bundle()` — builds system tx (sender = `PROTOCOL_TREASURY`, sig/pubkey empty, data = JSON-encoded `StakingOp::JailEvidenceBundle`)
  - `Transaction::is_system_tx()` — universal predicate for Phase D system txs
  - `Transaction::is_jail_evidence_bundle_tx()` — payload sniff
  - `Transaction::verify()` — bypasses standard signature verification for system txs (auth via consensus dispatch recompute-and-compare)
- **`crates/sentrix-core/src/blockchain.rs`**:
  - `Blockchain::build_jail_evidence_system_tx(next_height, ts) -> Option<Transaction>` — proposer helper. Returns `None` pre-fork, at non-boundary heights, or with no evidence (Q3-A: skip empty bundles); otherwise `Some(tx)`
  - `JAIL_CONSENSUS_HEIGHT` env reader + `is_jail_consensus_height(height)` helper (default `u64::MAX` = disabled)
  - `compute_jail_evidence(active_set)` on `SlashingEngine` — produces deterministic `Vec<JailEvidence>` from local LivenessTracker
- **`crates/sentrix-core/src/block_producer.rs`** — `build_block` now calls `build_jail_evidence_system_tx` after coinbase. Pre-fork: helper returns `None`, behavior unchanged.
- **`crates/sentrix-core/src/block_executor.rs`**:
  - `validate_block` Q4 required-presence check at epoch boundaries post-fork
  - Pass-1 + Pass-2 paths skip system tx for nonce/fee/balance bookkeeping
  - Phase C dispatch handler: cited-epoch check + local recompute compare + per-validator jail apply
- **`crates/sentrix-core/tests/phase_d_4validator_determinism.rs`** — 4-Blockchain integration harness: positive (all 4 converge on identical jail state + identical `state_root`) + negative (peer with diverging LivenessTracker rejects via dispatch recompute-and-compare)
- **`crates/sentrix-core/src/lib.rs`** — `pub(crate) mod test_util` with `env_test_lock()` so fork-gate tests serialize across modules under cargo's default parallel runner
- **CI** — workflow uploads `sentrix-faucet` release binary as a workflow artifact alongside `sentrix`

### Hygiene

- Scrubbed all remaining internal-tooling and operator-host references from public source files, audits, and docs (`CHANGELOG`, `docs/operations/EMERGENCY_ROLLBACK.md`, `docs/operations/MONITORING.md`, `docs/brand/HERO_COPY.md`, `docs/brand/PRESS_BOILERPLATE.md`, `docs-site/README.md`, several inline doc-comments). Replaces them with generic terms ("operator runbooks", "validator host", "internal documentation"). Final grep across tracked files returns zero matches.

### Activation

`JAIL_CONSENSUS_HEIGHT` defaults to `u64::MAX` (disabled). To activate, operators set the env var on all validators in a coordinated halt-all + simultaneous-start. Activation prerequisites:

1. Verify `LivenessTracker` has converged across the fleet (≥ 4h of clean operation post asymmetric-record fixes shipped earlier today)
2. Bake on testnet 24-48h with `JAIL_CONSENSUS_HEIGHT=<low>`
3. Mainnet activation: halt-all + simultaneous-start with `JAIL_CONSENSUS_HEIGHT=<future_height>`

---

## [2.1.41] — 2026-04-27 — Jail-cascade observability + fork-gated BFT safety gate relaxation

> **Liveness fix bundle for the jail-cascade pattern.** Two mainnet stalls on 2026-04-26 (h=633599 evening, h=662399 night) traced to per-validator stake_registry divergence (one validator sees another as jailed, others see active). The P1 BFT safety gate then refused to participate (active < MIN_BFT_VALIDATORS=4), stalling the chain.
>
> This release ships:
> 1. **Observability metric** — DEBUG-level tracing snapshot of per-validator (signed_count, missed_count) every 1000 blocks. Operators can diff per-validator counts via `journalctl -u sentrix-node -g 'jail counter snapshot'` to detect divergence early.
> 2. **Fork-gated BFT safety gate relaxation** — new `BFT_GATE_RELAX_HEIGHT` env var (default `u64::MAX` = disabled). Pre-fork: gate uses `MIN_BFT_VALIDATORS (=4)` (current behavior, unchanged on default). Post-fork: gate uses `⌈2/3 × total⌉` supermajority (= 3 for 4-validator network = 1-jail tolerance).
>
> **Default behavior is unchanged.** Operators must explicitly set `BFT_GATE_RELAX_HEIGHT=<height>` on each validator to activate the relaxation. Coordinated rollout required (testnet bake, then mainnet halt-all + simultaneous-start with env var).

### Added

- `crates/sentrix-staking/src/slashing.rs` — DEBUG tracing per-validator participation snapshot every 1000 blocks (PR #350)
- `crates/sentrix-core/src/blockchain.rs`:
  - `BFT_GATE_RELAX_HEIGHT_DEFAULT = u64::MAX` const
  - `get_bft_gate_relax_height()` env reader
  - `Blockchain::is_bft_gate_relax_height(h) -> bool` static check
  - `Blockchain::min_active_for_bft(h, total) -> usize` dispatch helper
  - 2 regression tests: `test_bft_gate_relax_fork_threshold`, `test_bft_gate_relax_disabled_by_default`
- `bin/sentrix/src/main.rs` — P1 BFT safety gate uses `min_active_for_bft(h, total)` lookup
- `audits/jail-cascade-root-cause-analysis.md` — full RCA (asymmetric record_block_signatures, locally-computed jail decision)
- `audits/consensus-computed-jail-design.md` — long-term fix design (4-6 weeks: JailTransaction model)
- Operator runbook: jail-divergence recovery procedure (internal)

### Migration

Drop-in chain.db compatible with v2.1.40. Hot-swap binary at any time. Behavior unchanged until operator sets `BFT_GATE_RELAX_HEIGHT` env var.

To activate (after testnet bake):
```
# Set env on each validator's systemd EnvironmentFile, choose height in future
BFT_GATE_RELAX_HEIGHT=<future_height>
# Halt all + simultaneous-start (per feedback_mainnet_restart_cascade_jailing)
```

Pre-activation chain operates exactly as v2.1.40. Post-activation: chain stays live with active=3 of total=4.

### Tests

`cargo test --workspace`: 774 passed, 0 failed, 11 ignored (was 772 in v2.1.40, +2 new regression tests). Clippy clean.

### Related

- 2 incidents recovered via chain.db rsync from canonical (`incidents/2026-04-26-evening-rolling-restart-jail-cascade-stall.md`, `incidents/2026-04-26-night-jail-divergence-stall-h662399.md`)
- Long-term fix (consensus-computed jail) is the real solution; this release is mitigation while that ships (4-6 weeks)

### PRs

- #350 — audit + observability + design docs (merged 2026-04-27)
- #351 — fork-gated BFT safety gate relaxation (merged 2026-04-27, fresh-brain reviewed by autonomous session, regression tests added)

---

## [2.1.40] — 2026-04-27 — Explorer richlist percentage display fork-aware

> **Display-only polish PR.** Closes the last 3 sites that still used static `MAX_SUPPLY` (210M) for percentage-of-supply display, even though the consensus + `/chain/info` RPC display were already fork-aware in v2.1.39. After the tokenomics v2 fork activated on mainnet at h=640800, the `/explorer/richlist` HTML page, `GET /accounts` REST endpoint, and `GET /accounts/richlist` REST endpoint were still calculating percentages against the pre-fork 210M cap — making top holders look ~33% smaller than reality.

### Fixed

- `crates/sentrix-rpc/src/explorer.rs` — Rich List HTML: percentage calc + "Total supply: N SRX" footer string both now use `bc.max_supply_for(bc.height())`. Pre-fork: 210M, post-fork: 315M.
- `crates/sentrix-rpc/src/explorer_api.rs` — `GET /accounts` (paginated) percentage calc now fork-aware. Removed unused `use sentrix_core::blockchain::MAX_SUPPLY` import.
- `crates/sentrix-rpc/src/routes/accounts.rs` — `GET /accounts/richlist` REST endpoint percentage calc now fork-aware.

### No consensus impact

This release touches only `sentrix-rpc` (display layer). No changes to consensus crates (`sentrix-bft`, `sentrix-core::block_executor`, `sentrix-trie`, `sentrix-staking::distribute_reward`, `sentrix-evm::executor`). Drop-in chain.db compatible with v2.1.39.

### Migration

No operator action required. Hot-swap binary at any time. Restart causes ~10s downtime per validator if rolling, or ~5s halt-all+simultaneous-start (recommended per `feedback_mainnet_restart_cascade_jailing.md`).

### Tests

`cargo test --workspace`: 772 passed, 0 failed, 11 ignored. `cargo clippy --workspace --tests -- -D warnings`: clean.

---

## [2.1.39] — 2026-04-26 — Tokenomics v2 fork (BTC-parity halving + 315M cap) — **ACTIVE on mainnet**

> **Consensus fork — ACTIVE end-to-end on mainnet since h=640800 (2026-04-26 evening).** Both consensus dispatch (cap enforcement, halving math) and RPC display (`/chain/info` `max_supply_srx`) now report v2 schedule. Re-targets emission curve to BTC-parity 4-year halving (126M blocks at 1s) + raises MAX_SUPPLY from 210M to 315M. Closes the v1 math gap (geometric series asymptoted at 84M from mining → 147M effective max, not the 210M originally documented). Side benefit: validator runway extended to ~year 20, premine ratio drops 30% nominal → 20% (industry-leading optics).

### Added (consensus)

- `MAX_SUPPLY_V2 = 315_000_000 × 100_000_000 sentri` (315M SRX)
- `HALVING_INTERVAL_V2 = 126_000_000 blocks` (4-year cadence at 1s blocks)
- `TOKENOMICS_V2_HEIGHT_DEFAULT = u64::MAX` — env-gated activation (set `TOKENOMICS_V2_HEIGHT` env var on each validator). Same fork-gate pattern as `VOYAGER_REWARD_V2_HEIGHT`. Default = inert.
- `Blockchain::is_tokenomics_v2_height(h) -> bool` — static check
- `Blockchain::max_supply_for(&self, h)` — runtime-aware dispatch (210M pre-fork, 315M post-fork)
- `Blockchain::halving_interval_for(&self, h)` — runtime-aware dispatch (42M pre-fork, 126M post-fork)
- `Blockchain::halvings_at(h)` — fork-aware halving count: pre-fork uses `h / 42M`, post-fork uses `(h - fork) / 126M`. Assumes activation while still in v1 era 0 (`fork_height < 42M`) so cumulative halvings don't reset at fork moment.
- `get_block_reward()` migrated to use the dispatchers — single dispatch surface.

### Added (RPC display)

- `chain_queries.rs` `chain_stats()` `max_supply_srx` is fork-aware (was static `MAX_SUPPLY`). Pre-fork RPC reports 210M, post-fork reports 315M. (PR #337 follow-up.)

### Fixed
- v1 tokenomics math gap. With v1 constants (1 SRX × 42M halving), geometric series asymptoted at 84M from mining + 63M premine = 147M effective max — the 210M cap was unreachable. v2 (1 SRX × 126M × 2 = 252M from mining + 63M premine = 315M) closes the gap.

### Activation procedure (operator-driven, EXECUTED 2026-04-26 evening)

1. ✅ Build v2.1.39 binary (docker bullseye, glibc 2.31 compat)
2. ✅ Deploy to all 4 mainnet validators
3. ✅ Set `TOKENOMICS_V2_HEIGHT=640800` in each validator's systemd EnvironmentFile (`/etc/sentrix/sentrix-node.env`, etc.)
4. ✅ Halt all + simultaneous start (per `feedback_mainnet_restart_cascade_jailing.md` rule)
5. ✅ Chain reached fork height h=640800 — consensus auto-switched
6. ✅ Verified post-fork: `/chain/info` reports `max_supply_srx: 315000000`, `next_block_reward_srx: 1.0`

**Display-fix binary swap (~h=646200, evening):** Initial v2.1.39 binary was built before PR #337 merged (RPC display fix). Mainnet was running consensus-fork-correct binary but display layer reported stale 210M. Resolved via halt-all + simultaneous-start binary swap to v2.1.39+#337 binary. Procedure: SCP new binary → halt all 4 → cp-stage-mv replace → simultaneous start → verify display flipped. Clean recovery, ~5 min downtime, no jail divergence.

### Tests
- `test_tokenomics_v2_fork_boundary_no_reward_jump` — verifies smooth halving transition at fork moment (no reward jump up or down)
- `test_tokenomics_v2_geometric_reaches_315m_cap` — confirms geometric series math: 1 × 126M × 2 + 63M premine = 315M (within 5B-sentri integer-truncation tolerance)

### Migration
- Drop-in chain.db compatible with v2.1.38. Pre-fork blocks unaffected. `total_minted` continues monotonically. Cap check uses fork-aware `max_supply_for(h)`.

### 2026-04-26 evening incident — rolling-restart jail divergence (recovered)

During the rolling restart used to load `TOKENOMICS_V2_HEIGHT` env var into validator processes, sequential per-validator restarts caused auto-jail divergence: validators that were down for their proposing slot were jailed locally on Foundation+Beacon's view but seen as active on Treasury+Core's view. Active-set divergence (3 vs 4) tripped the P1 BFT safety gate ("active set < minimum 4"), stalling the chain at h=633599.

**Recovery:** halt all 4 → forensic backup divergent chain.db → tar-pipe Treasury (frozen canonical) → Foundation/Core/Beacon → MD5 parity confirmed (`975f9d67a7c3206dbea346f6b90f4826`) → simultaneous start → BFT resumed within seconds. Per-validator hash parity verified at h=633650 (`8e2166e9962da5aa...`).

**Lesson:** rolling restart on mainnet has the same jail-cascade pattern previously documented for testnet (2026-04-20 incident). For env-var changes or any restart where consensus rules don't change between old/new state, prefer **halt-all + simultaneous-start** over rolling. See `EMERGENCY_ROLLBACK.md` § 2.

### PRs
- #336 — feat(consensus): tokenomics v2 fork — 126M halving + 315M cap (BTC-parity 4-year)
- #337 — fix(rpc): /chain/info max_supply_srx is fork-aware (tokenomics v2 follow-up)

---

## [2.1.38] — 2026-04-26 — Legacy TCP-path deletion + cumulative skip metric

> **Hardening on top of v2.1.37.** Same fix surface — the libp2p sync race-induced cascade-bail (RCA: `incidents/2026-04-26-libp2p-sync-cascade-bail-stall.md`). Bundles legacy-path deletion (eliminate parallel dead code with the same bug pattern) + observability counter (detect re-emergence).

### Removed

- `crates/sentrix-network/src/sync.rs` deleted entirely (158 LOC, zero production callers; legacy TCP path superseded by libp2p in PR #82).
- `crates/sentrix-network/src/node.rs` trimmed 645 → 36 LOC. Kept only `NodeEvent`, `SharedBlockchain`, `DEFAULT_PORT` (still imported by `libp2p_node.rs` + `bin/sentrix/main.rs`). Deleted: raw-TCP `Node` struct + impl, `Message` enum, `Peer` struct, `SharedPeers` type, `MAX_MESSAGE_SIZE`, `MAX_CONNECTIONS_PER_IP`, `RATE_LIMIT_WINDOW`, `MAX_PEERS`, `ConnectionCounts`, full test module.

Both deleted sites had the same `for block in batch` cascade-bail pattern that caused the v2.1.36 stall — carrying the dead code with a known bug invited future regressions.

### Added

- `static SYNC_SKIPPED_TOTAL: AtomicU64` in `libp2p_node.rs` accumulates duplicate-block skips across handler invocations.
- Threshold-crossing WARN log at 10/100/1k/10k/100k cumulative skipped → surfaces re-emergence of the concurrent-GetBlocks race so operators can grep for `cumulative skipped (already-applied) crossed` and decide when to ship single-flight coalescing.
- Existing per-batch INFO log preserved (`synced N blocks ... skipped K already-applied`).

### Migration

- Drop-in chain.db compatible with v2.1.37.

---

## [2.1.37] — 2026-04-26 — libp2p sync cascade-bail fix (mainnet stall RCA)

> **P0 hotfix.** Mainnet stalled at h=604547 for ~1h 45min on 2026-04-26 morning. Root cause: `libp2p_node.rs` BlocksResponse handler bailed on the first already-applied block in a batch and dropped the rest of valid forward blocks in the same response. Concurrent GetBlocks paths (periodic `sync_interval` + `TriggerSync` + reactive chain-on-full-batch) all read `our_height` and ask `from: our_height+1`. Responses overlap. Cumulative drift over thousands of sync rounds → 4-way chain.db divergence at h=604547 across the 4 mainnet validators.

### Fixed

- **`crates/sentrix-network/src/libp2p_node.rs`** BlocksResponse loop: filter `block.index <= chain.height()` BEFORE `add_block_from_peer`. Skip duplicates silently, keep applying forward blocks. Loop only breaks on real validation errors (gap, bad signature, etc.).

### Tests

- `test_libp2p_sync_loop_skips_duplicates_and_applies_remaining` in `crates/sentrix-core/tests/fork_determinism.rs`: chain at h=3 receives racy batch `[b2, b3, b4, b5]`. Pre-fix: bails on b2 with "expected 4 got 2", chain stalls at h=3. Post-fix: skipped=2 synced=2, chain advances to h=5.

### Recovery (operator-driven)

State divergence at h=604547 was the cumulative effect of the bug, not directly fixed by the binary patch. Recovery procedure:

1. Forensic backup divergent chain.db on each validator (`chain.db.divergent-604547-<ts>/`)
2. Treasury picked as canonical (most progressed at h=604548, self-consistent prev-link, signer-set matched majority)
3. Tar-pipe Treasury chain.db → Foundation, Core, Beacon (tar-over-ssh, no-same-owner)
4. MD5 parity confirmed across all 4 (`mdbx.dat` md5 = `567c7165301fff7e95ded23d03df63cd`)
5. v2.1.37 binary deployed (docker bullseye, glibc 2.31)
6. Rolling restart: Treasury → Foundation → Core → Beacon
7. Chain advanced past h=604548 within seconds; per-validator hash parity verified at h=604650

### Migration

- Drop-in chain.db compatible with v2.1.36, but if your validator is divergent from the canonical at any height, follow the chain.db rsync procedure documented in `docs/operations/EMERGENCY_ROLLBACK.md`.

---

## [2.1.36] — 2026-04-26 — tx validate: staking-op amount=0 exemption

> **Hotfix**: tx validation rejected ClaimRewards (and other no-fund-movement staking ops) because the `amount > 0` check exempted only token + EVM ops, not staking ops. Surfaced 2026-04-26 when first ClaimRewards submission was rejected with `"amount must be > 0 (unless token/EVM operation)"` even though `tx.data = {"op":"claim_rewards"}` is a valid op. Fix exempts staking ops.

### Fixed

- **`Transaction::validate` allows `amount=0` for staking ops**. `crates/sentrix-primitives/src/transaction.rs:328`. The check now reads:
  ```rust
  if self.amount == 0
      && !TokenOp::is_token_op(&self.data)
      && !self.is_evm_tx()
      && !StakingOp::is_staking_op(&self.data)
  ```
  Affects `ClaimRewards` (most common), `Unjail`, `SubmitEvidence` — the variants that don't move funds via `tx.amount` (apply-time treasury credit handles it).

### Migration

- Drop-in chain.db compatible with v2.1.35.

---

## [2.1.35] — 2026-04-26 — voyager_mode_for migration sweep + claim-rewards tool

> **Maintenance release.** Bundles defensive migrations of remaining `is_voyager_height` callsites (#327) plus the new `tools/claim-rewards` standalone binary (#328). No consensus or chain-state changes.

### Changed (#327)

- **All production callsites of static `Blockchain::is_voyager_height(h)` migrated to runtime-aware `voyager_mode_for(&self, h)`** (introduced in #324). Defensive sweep — same env-var-default-`u64::MAX` foot-gun could affect any path using the static check.
- Sites: `sentrix-rpc/jsonrpc/sentrix.rs:{79,409}`, `sentrix-core/block_executor.rs:988`, `bin/sentrix/main.rs:1702`.

### Added (#328)

- **`tools/claim-rewards`** — standalone binary; takes validator privkey on stdin, submits `StakingOp::ClaimRewards` tx to drain `pending_rewards` from `PROTOCOL_TREASURY` into validator balance. Sibling to existing `tools/transfer-amount` and `tools/drain-once`.

### Migration

- Drop-in chain.db compatible with v2.1.34.
- Per-validator rolling restart picks up new binary.

---

## [2.1.34] — 2026-04-26 — Connection-limits hotfix (max_established_per_peer 1→2)

> **Hotfix on top of v2.1.33.** v2.1.33's `max_established_per_peer(Some(1))` proved too restrictive for the 4-validator mainnet mesh — gossipsub propagation became too sparse, BFT block rate dropped from ~1/s to ~3/min with skip-rounds. Loosened to `Some(2)`. Restores normal block rate while keeping accumulation cap.

### Fixed (#326)

- **`max_established_per_peer` raised 1 → 2** in `connection_limits::Behaviour`. Allows ONE simultaneous-bidirectional-dial duplicate to settle without rejection while still preventing unbounded accumulation. New `SENTRIX_MAX_CONN_PER_PEER` env override for future tuning.

---

## [2.1.33] — 2026-04-26 — Voyager auth runtime-aware fix + connection_limits hardening

> **🟢 Closes the 2026-04-26 mainnet stall root cause** (env-var-defaulted `VOYAGER_FORK_HEIGHT=u64::MAX` + `validate_block` static check ⇒ Pioneer-auth rejection of valid Voyager skip-round blocks). Plus bundles the connection_limits Behaviour for max-1-established-per-peer enforcement.

### Fixed (#324)

- **`Blockchain::voyager_mode_for(height)` runtime-aware check.** New instance method that ORs the env-var fork-height check with the chain.db `voyager_activated` runtime flag. `validate_block` (both Pass-1 read-only at line 117-132 and Pass-2 commit at line 364-374) migrated to use it. Result: post-Voyager-activation chains accept Voyager blocks correctly regardless of env var state.
- **Why this matters:** the original static `is_voyager_height(height)` reads `VOYAGER_FORK_HEIGHT` env var with default `u64::MAX`. When set/defaulted to u64::MAX, returns false for any height. Pre-activation that's the correct mainnet-safe-default; post-activation it's a foot-gun. validate_block falling into Pioneer auth rejected legitimate Voyager skip-round blocks (locked-block re-propose at round N has validator field of round-N proposer, not Pioneer round-robin's round-0 proposer).
- **Operator hot-fix on production fleet** (set `VOYAGER_FORK_HEIGHT=579047` to make the static check work) becomes belt-and-suspenders; no longer load-bearing.

### Added (#323)

- **`connection_limits::Behaviour` wired into SentrixBehaviour** with `max_established_per_peer(Some(1))`. Defence-in-depth for the dial-tick connected-peers pre-check (#319 + #321). Even if both sides converge a duplicate (simultaneous bidirectional dials crossing on the wire), the swarm rejects the late connection at the libp2p layer. Three layers together prevent connection accumulation:
  1. #319: dial-tick skips dialing peers already in connected set
  2. #321: validator adverts include `/p2p/<peer_id>` so #319's check actually works
  3. THIS: even if #319/#321 miss a duplicate, swarm enforces 1-per-peer

### Operational note

After the v2.1.32 deploy attempt earlier today (rolled back due to env-var bug surfacing under load), the v2.1.33 release bundles all the cumulative work since v2.1.31:
- v2.1.32 fix (#321 /p2p suffix)
- v2.1.33 hardening (#323 connection_limits + #324 voyager_mode_for)

Mainnet currently runs v2.1.31 + operator env hot-fix. Deploying this release per-validator rolling restart picks up the runtime-aware voyager check (bug-fix-in-perpetuity) plus the libp2p hardening (kills the connection accumulation pattern that caused the day's two earlier stalls).

### Migration

- Drop-in chain.db compatible with v2.1.31 / v2.1.32.
- Per-validator rolling restart picks up new binary. `VOYAGER_FORK_HEIGHT=579047` env var on production fleet stays as belt-and-suspenders (harmless).
- After deploy + 30 min mesh-stable convergence, expected: connection counts plateau at ~6-12 per validator, BFT round-0 finalize rate ~1/s, no skip rounds under nominal load.

---

## [2.1.32] — 2026-04-26 — libp2p Tier 4 fix: /p2p/<peer_id> in advert multiaddrs

> **🟢 Closes the gap from v2.1.31's partial libp2p fix.** With this release, the dial-tick connected-peers pre-check actually fires (was falling back to "dial anyway" because cached advert multiaddrs lacked `/p2p/<peer_id>` suffix). Connection accumulation should now plateau at the steady-state mesh size (~6-12 per validator for the 4-validator mainnet) instead of climbing toward gossipsub-thrashing thresholds.

### Fixed (#321)

- **Advert builder appends `/p2p/<own_peer_id>` to each broadcast multiaddr** — `bin/sentrix/src/main.rs` advert construction site. `LibP2pNode.local_peer_id` (already public Copy field) is read at advert build time + suffixed onto each filtered listen_addr. Defensive: skip the suffix if the address already contains "/p2p/" (listen_addrs() shouldn't return such, but tolerate it).
- **Net effect when paired with v2.1.31's #319 fix**: peer_id extraction in the dial-tick connected-peers pre-check actually returns `Some(peer_id)` instead of always `None`. Validators stop re-dialing peers they're already connected to. Connection pool stops accumulating. The 2026-04-25 mainnet stall pattern (h=583002, h=585217, h=590000-area, h=592192) should not recur once all 4 validators run v2.1.32.

### Empirical signal

| Build | Connection growth pattern (4-validator mainnet) |
|---|---|
| v2.1.30 (pre-fix) | 7 → 800+ over hours, periodic stall |
| v2.1.31 (#319 only, partial) | 6-7 → 27 over 90 seconds, slower stall |
| v2.1.32 (this PR) | Expected: plateau at ~6-12 indefinitely |

Operator validation: monitor `ss -tn state established '( sport = :30303 or dport = :30303 )' \| wc -l` across 24 hours. Pre-fix this counter climbed monotonically; post-fix should sit in a tight band.

### Migration

- Drop-in chain.db compatible with v2.1.31.
- Per-validator deploy: rolling restart picks up new binary. Within ~10 min, advert broadcasts will carry the new /p2p suffix; within ~30 min, full mesh-stable steady state (allowing for advert gossip propagation + peers picking up new cached entries).

---

## [2.1.31] — 2026-04-25 — Late-night ship: BFT signing v2 foundation + Frontier F-2 shadow + libp2p connection-leak fix + V4 reward v2 fork activated

> **🟢 Three substantial improvements landed late on 2026-04-25, plus the V4 reward v2 mainnet fork activated at h=590100.** The libp2p connection leak fix is the most operationally important — it closes the root cause of two production stalls earlier in the day.

### Fixed (#319)

- **L1 dial-tick connection leak** — `bin/sentrix/src/main.rs:1550-1581`. The dial-tick comment claimed `connect_peer` was idempotent ("duplicate dials to an already-connected peer are no-ops at the swarm level") — that's FALSE in libp2p 0.56 / libp2p-swarm 0.47. Every `swarm.dial()` enqueues a fresh pending connection regardless of existing connection state. Every 30s tick × 3 active peers × N hours = unbounded accumulation. After a few hours each validator reached 568-918 active TCP connections (vs expected ~6-12 for a 4-validator mesh), gossipsub heartbeat thrashed mesh on the oversized pool, BFT request_response messages dropped mid-round, and consensus deadlocked. **Fix**: new `LibP2pNode::connected_peers() -> HashSet<PeerId>` query (uses libp2p's native `swarm.connected_peers()`) + dial-tick now snapshots connected set once per tick + skips active-set members whose multiaddr's `/p2p/<peer_id>` suffix is already in the connected set. Falls back to dialing if multiaddr lacks `/p2p/` suffix (legacy advert format compatibility). Two production stalls (h=583002, h=585217) directly attributable to this bug; recovery procedure was "parallel restart of all 4 validators" which clears the pool.
- **`max_established_per_peer(1)` defence-in-depth** deferred — libp2p-swarm 0.47 `Config` doesn't expose it directly; needs the `connection_limits::Behaviour` wired into `SentrixBehaviour`. Tracked as follow-up. The dial-tick fix above is sufficient on its own to stop accumulation.

### Added (#317 — BFT signing v2 foundation)

- **`BFT_SIGNING_V2_FORK_HEIGHT = u64::MAX`** constant (inert; v2 path never fires until operators flip it in a coordinated fork ceremony).
- **`BFT_V2_MAGIC = 0x20`** magic byte (distinct from existing 0x01-0x04 message domain separators + 0x10 for MultiaddrAdvertisement).
- **`signing_payload_v2(..., chain_id)`** variants on `Proposal`, `Prevote`, `Precommit`, `RoundStatus`. Each prepends `[0x20][chain_id BE u64]` to the v1 payload layout.
- **`signing_payload_for_height(...)`** dispatch helper that picks v1 or v2 based on the height parameter.
- 7 new tests pinning v2 wire-format invariants + cross-chain replay protection + dispatch inertness with default `u64::MAX` fork height.

This closes Bug A from `audits/bft-signing-fork-design.md`: cross-chain BFT vote replay where a mainnet (7119) signature can verify on testnet (7120) at the same height/round/hash. The specific exploit (nil-vote replay where `block_hash="NIL"` is byte-identical across chains) is fixed under v2 — chain_id in the signed payload makes mainnet-NIL ≠ testnet-NIL. **Phase 2 (call-site refactor) deferred** to a dedicated session per consensus discipline. Phase 5 (operator activation flip) is operator ceremony, separate.

### Added (#318 — Frontier F-2 shadow-mode wiring)

- **`SENTRIX_FRONTIER_F2_SHADOW=1`** env var gate in `apply_block_pass2`. When set, the F-1 scaffold's `build_batches` is called over each block's non-coinbase transactions and the result is logged via `tracing::info!` under target `frontier::f2_shadow`. Read-only — does NOT mutate state. Sequential apply still drives block execution.
- Default OFF — env-var read uses `std::env::var_os().is_some_and(...)` which short-circuits without allocation when missing.
- Calibration step before F-3 (real parallel apply). Lets operators observe scheduler determinism on real chain traffic without committing to parallel execution.

### Operational change (no code in this release for the V4 fork)

- **V4 reward v2 mainnet fork ACTIVATED at h=590100.** `VOYAGER_REWARD_V2_HEIGHT=590100` set on all 4 mainnet validators, rolling restart, fork crossed at 04:27:04 local. Behaviour from h=590100 onwards:
  - Coinbase 1 SRX/block routes to `PROTOCOL_TREASURY` (`0x0000000000000000000000000000000000000002`) instead of validator address
  - `reset_reward_accumulators_for_fork_activation` fired once at h=590100 (cleared pre-fork pending_rewards + delegator_rewards to maintain the supply invariant `accounts[TREASURY] == sum(pending_rewards) + sum(delegator_rewards)`)
  - Validators + delegators must now use the `ClaimRewards` staking op to transfer earned share from treasury to their balance
  - Treasury accumulating ~1 SRX/block as designed (verified post-fork: 458 SRX at h=590562, 462 blocks past fork; ~4-block sample-timing lag)

### Migration

- Drop-in chain.db compatible with v2.1.30.
- Pre-deploy: ensure `VOYAGER_REWARD_V2_HEIGHT` is consistent across all 4 validator env files (already set during fork ceremony — kept).
- Per-validator deploy: rolling restart picks up new binary. The libp2p connection-leak fix takes effect immediately on the validator that restarts; full fleet benefit when all 4 are upgraded.

---

## [2.1.30] — 2026-04-25 — Voyager mainnet active, RPC consensus reporting fixed

> **🟢 Mainnet now reports `consensus: "DPoS+BFT"` correctly across every endpoint.** v2.1.30 is the canonical release on mainnet and testnet after the Voyager activation sequence completed. Source tree depersonalized of internal infrastructure references in the same window.

### Fixed (#310)

- **RPC consensus string `"BFT"` → `"DPoS+BFT"`** across `routes/ops.rs` (`/`, `/sentrix_status`), `routes/chain.rs` (`/chain/finalized-height`), `jsonrpc/sentrix.rs` (`sentrix_status`, `sentrix_consensus`, `sentrix_getFinalizedHeight`, `sentrix_getValidators`). The shorter `"BFT"` was technically incomplete — Sentrix runs DPoS proposer rotation on top of BFT finality, and the public string should reflect both layers. Five call sites updated; no schema break (string-typed field).

### Changed (#311, #312)

- **Public repo depersonalized.** Removed internal infrastructure references (host nicknames, operator handles, private path conventions) from public docs, source comments, scripts, CI workflows, and tests. No behavioural change — comment cleanup only. The depersonalization audit covered `docs/`, `crates/`, `bin/`, `scripts/`, `.github/workflows/`, and integration tests.

### Operational state on this release

- **Mainnet:** `consensus_mode="voyager"`, `voyager_activated=true`, `evm_activated=true`, height ~580K, 4 validators in DPoS+BFT.
- **Testnet:** same flags, height ~200K.
- **Source ↔ deployed parity:** workspace `Cargo.toml` reads `version = "2.1.30"`; mainnet validators run a binary built from this same tree.

---

## [2.1.29] — 2026-04-25 — EVM mainnet activation

> **🟢 EVM activated on mainnet in the same window as the Voyager flip.** `Blockchain::activate_evm()` ran once, `evm_activated=true` persisted to chain.db, and `eth_sendRawTransaction` started accepting raw Ethereum transactions on chain ID 7119. Mainnet now mirrors the testnet EVM surface.

### Operational change (no code in this release; activation triggered via admin tooling)

- `evm_activated` flag flipped from `false` to `true` on every mainnet validator's chain.db.
- All existing mainnet accounts back-filled with `code_hash = EMPTY_CODE_HASH` and `storage_root = EMPTY_STORAGE_ROOT` per `activate_evm()`.
- `eth_call`, `eth_sendRawTransaction`, `eth_estimateGas`, `eth_getCode`, `eth_getStorageAt` accepted on mainnet RPC.
- `chain_stats()` JSON now reports `evm_activated: true` at `/chain/info`.

### Migration

- No operator action required for nodes already on v2.1.28+ — flag is read from chain.db at every loop iteration.
- Wallets / explorers / contract tooling configured for testnet now work on mainnet by changing chain ID 7120 → 7119.

---

## [2.1.28] — 2026-04-25 — Voyager mainnet activation (re-attempt #2 successful) + RPC consensus-mode exposure

> **🟢 Voyager DPoS+BFT activated on mainnet at h=579047.** Second attempt converged after the v2.1.26 / v2.1.27 peer-mesh fixes shipped. `consensus_mode` now exposed on `/chain/info` so block explorers, wallets, and indexers can stop inferring mode from block-level justification presence.

### Added

- **`consensus_mode`, `voyager_activated`, `evm_activated` fields on `chain_stats()`.** Surfaces the consensus engine the runtime is actually using to RPC consumers. Pre-fix, `/chain/info` had no consensus-mode field — clients had to infer mode from justification presence on blocks, which was awkward and wrong for the Pioneer→Voyager transition window.
- **`bc.voyager_activated` runtime flag drives RPC handlers.** Replaces the `chain_id == 7119 ? PoA : BFT` heuristic and the `is_voyager_height()` env-var fork-height check in 4 places (`routes/ops.rs`, `routes/chain.rs`, `jsonrpc/sentrix.rs` × 2). The fork-height check was returning `consensus=PoA` while runtime was actually BFT, because mainnet activated Voyager via the `voyager_activated` chain.db flag while `VOYAGER_FORK_HEIGHT` stayed at `u64::MAX` for operational safety.

### Operational change

- `voyager_activated=true` set on every mainnet validator's chain.db. Validator loops migrated from Pioneer PoA round-robin to DPoS proposer rotation under 3-phase BFT.
- `SENTRIX_FORCE_PIONEER_MODE` env override removed from every mainnet validator's env file (was the v2.1.25 emergency rollback flag from the first activation attempt).
- Mainnet height at activation: 579047. Pioneer ran from genesis through h=579046.

### Recovery context

- Activation #2 itself converged cleanly after the v2.1.27 cold-start gate fixed the BFT entry race that contributed to the activation #1 livelock. State divergence at h=578006 during the second attempt's brief mesh-flap was recovered via frozen-rsync from a canonical peer (chain.db only — `identity/node_keypair` and `wallets/sentrix-node.keystore` restored from forensic backup; **lesson: never rsync whole data dir, chain.db only**).

### Known issues

- BFT signing v2 (chain_id in signing payload + low-S enforcement) **still deferred** per `audits/bft-signing-fork-design.md`. Hard-fork gated; implementation deferred to a dedicated session. Defence-in-depth, not blocking.

---

## [2.1.27] — 2026-04-25 — L2 cold-start gate (close cold-start BFT entry race)

> **🟢 Closes the cold-start race where a validator restarting with `voyager_activated=true` already in chain.db could enter BFT before the L1 mesh re-converged.** v2.1.26's L2 gate only fired on activation transitions; this hotfix adds a second gate at the top of the validator loop that fires every iteration when `voyager_activated=true`.

### Fixed (#307)

- **Cold-start BFT entry gate.** `bin/sentrix/src/main.rs` validator loop now checks `peer_count >= active_set.len() - 1` at the top of every loop iteration when `voyager_activated=true`, not only at the activation transition. A validator that crashed/restarted with `voyager_activated=true` already persisted to chain.db would otherwise enter BFT immediately on cold start, before L1 multiaddr advertisements re-converged the mesh — same livelock failure mode as the original 2026-04-25 incident, just triggered by restart instead of fork-height crossing. Gate uses the same `check_bft_peer_mesh_eligible` + `force_bft_insufficient_peers_set` helpers introduced in v2.1.26 (strict `=="1"` env override, no empty-string bypass).

### Migration

- Drop-in chain.db compatible with v2.1.26.
- No new env vars required.
- Operational note: `SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS=1` remains the emergency override for both gates (activation transition and cold-start). Reject any validator setting it via env file leakage.

---

## [2.1.26] — 2026-04-25 — Bug-fix sweep + L1/L2 peer auto-discovery + Frontier scaffold

> **Operational rollup of 9 PRs landed after the 2026-04-25 Voyager activation incident.** Addresses the actual root cause (peer mesh partition — `--peers` config not scaling), ships defensive guards across the consensus + EVM + storage paths, lays the wire-format foundation for the Frontier-fork parallel transaction execution work. **Mainnet stays in Pioneer mode (`SENTRIX_FORCE_PIONEER_MODE=1`) for this release** — Voyager activation re-attempt remains gated on the BFT signing v2 fork (`audits/bft-signing-fork-design.md`) and a coordinated testnet rehearsal (`runbooks/voyager-mesh-rehearsal.md`).

### Added — peer auto-discovery (L1 + L2)

- **L2 pre-flight peer-mesh gate (#298).** Validator loop refuses to flip into Voyager BFT mode unless `peer_count >= active_set.len() - 1`. The 2026-04-25 livelock would have been caught at every VPS — Beacon node had only 1 libp2p peer (Foundation node) at activation, gate would have held the flip until L1 self-healing converged the mesh. Strict `SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS=="1"` env override (rejects empty string + non-1 values to close the misconfiguration footgun).
- **L1 multiaddr advertisement (#300, #301, #302).** New gossipsub topic `sentrix/validator-adverts/1`. Each validator broadcasts a signed `MultiaddrAdvertisement` on startup + every 10 minutes (sequence persisted to `<data_dir>/.advert-sequence` so restart doesn't reset). Receivers verify against on-chain stake registry pubkey, store latest-by-sequence in a 4096-entry LRU cache (lowest-sequence eviction). Periodic dial-tick (every 30s) reads `active_set` and dials any cached members not currently peered. Self-healing mesh from a single bootstrap peer; manual `--peers` lists no longer required at scale.

### Fixed

- **PoLC mismatch in BFT engine (#297).** When prevote supermajority moved the lock from hash A to hash B without staging hash B's bytes, `locked_block` retained stale `bytes_A`. `locked_proposal_bytes()` then returned `(B, bytes_A)` — bytes hashing to A under a lock claiming B. Re-propose path would broadcast garbage. Now invalidates `locked_block` whenever `locked_hash` changes, before staging promotion. Pinned by 3 integration tests + 1 unit test.
- **Storage trie-init silent failure at fork boundary (#303).** Pre-fix, `init_trie` failure below `STATE_ROOT_FORK_HEIGHT` logged a warn and continued with `state_trie=None`. At the fork-crossing boundary, that validator would produce blocks with `state_root=None` while peers compute real roots → silent ghost validator forking the network. Now refuses block production with `state_trie=None` past the fork boundary, with operator recovery instructions in the error message.
- **EVM cold-contract bytecode pre-load (#304).** Cross-contract calls (DELEGATECALL / CALL / STATICCALL to a SECOND contract) were failing because only the per-tx CALL target's bytecode was pre-loaded into `SentrixEvmDb.code`. revm's `code_by_hash` errored on the second contract's hash. Now `from_account_db` bulk-loads every contract's bytecode at construction. O(N_contracts × bytecode_size) memory per EVM tx; bounded at realistic chain sizes.
- **`total_minted` overflow hardening (#299).** Replaced unchecked `+=` with `saturating_add` for coinbase-amount accumulation. No semantic difference at production reward levels (mainnet is ~3 orders of magnitude below `u64::MAX`); defensive against inflated-reward testnets and future block-reward tuning. Failure mode shifts from silent wrap (catastrophic supply divergence) to controlled block rejection via the existing `MAX_SUPPLY` guard.

### Added — Frontier-fork scaffold (#305)

- **`crates/sentrix-core/src/parallel/` module.** Type-system contracts for the Fork H+5 parallel transaction execution per `audits/parallel-tx-execution-design.md`. `AccountKey` / `GlobalKey` / `TxAccess` / `Batch` / `derive_access` / `build_batches`. Stubs return pessimistic / sequential-equivalent values so the production code path is unchanged. 12 unit tests pin the conflict-detection, batching, and `BTreeSet`-determinism contracts. Two `#[ignore]`d property tests at `tests/parallel_determinism.rs` placeholder for the apply-equivalence + scheduler-determinism contracts that will gate the real impl. **Not called from `apply_block_pass2` — production unchanged.**

### Known issues / deferred

- **BFT signing v2 (chain_id + low-S enforcement) NOT shipped.** Design + 5-phase rollout in `audits/bft-signing-fork-design.md`. Cross-chain replay protection + signature malleability defence. Required before Voyager mainnet activation. Implementation deferred to dedicated session per consensus discipline.
- **Voyager mainnet activation NOT performed.** Mainnet stays `SENTRIX_FORCE_PIONEER_MODE=1` until BFT signing v2 ships + testnet rehearsal completes (`runbooks/voyager-mesh-rehearsal.md`).
- **Frontier parallel apply NOT implemented.** Scaffold ships per `audits/frontier-mainnet-phase-implementation-plan.md` Phase F-1; real work is Phases F-2 through F-10 (~6-8 weeks calendar including testnet bake + mainnet shadow mode).

### Migration

- Drop-in chain.db compatible with v2.1.24 / v2.1.25.
- Add to validator env (optional, for L1 peer-discovery):
  - No new required env vars.
- `SENTRIX_FORCE_PIONEER_MODE=1` and `VOYAGER_FORK_HEIGHT=18446744073709551615` env vars from v2.1.25 stay required to prevent inadvertent Voyager re-activation.

---

## [2.1.25] — 2026-04-25 — SENTRIX_FORCE_PIONEER_MODE emergency override (Voyager activation rollback)

> **🚨 v2.1.25 hotfix released after the 2026-04-25 mainnet Voyager activation incident.** v2.1.24 was deployed cleanly to mainnet to close #268, then `VOYAGER_FORK_HEIGHT` was flipped to activate Voyager DPoS+BFT at h=557244. Activation succeeded (validators migrated, `voyager_activated=true` set persistently per PR #277), but BFT livelocked immediately due to V2 locked-block-repropose wiring gap (Steps 4-5 main.rs never shipped — see `audits/v2-locked-block-repropose-implementation-plan.md`). Rolled back to Pioneer via this hotfix's emergency override env. Mainnet stable on Pioneer with `SENTRIX_FORCE_PIONEER_MODE=1` set per validator. **Voyager activation re-attempt blocked on V2 Steps 4-5 implementation.**

### Added (#290)

- **`SENTRIX_FORCE_PIONEER_MODE` env var.** When set, the validator-loop's local `voyager_activated` and `evm_activated` booleans are forced to `false` at startup regardless of the persistent flags on `Blockchain` (PR #277). Used as emergency rollback path when Voyager BFT cannot finalise. The persistent flags stay set on chain.db; clearing them requires a separate operation. Once V2 main.rs wiring lands and BFT is verified live on testnet, the flag can be unset and the validator resumes Voyager mode based on its persistent state.

### Default behaviour unchanged

- Without `SENTRIX_FORCE_PIONEER_MODE`, the validator loads `voyager_activated` / `evm_activated` from chain.db as before. v2.1.25 is fully backward compatible with v2.1.24 chain.db.

### Operational status post-incident

- Mainnet at h=557415+, all 4 vals lockstep on emergency v2.1.25 binary in Pioneer mode
- `SENTRIX_FORCE_PIONEER_MODE=1` set in each validator's env file
- `VOYAGER_FORK_HEIGHT` reset to `u64::MAX` (redundant given FORCE_PIONEER, but defense-in-depth)
- Voyager state on chain.db (stake_registry, epoch_manager, voyager_activated=true) intact but unused while Pioneer runs
- Next Voyager activation attempt blocked on V2 main.rs Steps 4-5 implementation + testnet activation rehearsal

Full incident analysis at `internal operator runbook`.

---

## [2.1.24] — 2026-04-25 — Phase 1 mainnet legacy-compat (#268 closed via Path B)

> **🟢 v2.1.24 UNFREEZES MAINNET DEPLOYMENT** with `SENTRIX_LEGACY_VALIDATION_HEIGHT` set per validator. The actual root cause of #268 was identified today: mainnet's chain.db carries historical state_root artifacts (BACKLOG #16 patches at h=32688/89, h=507499 anomaly, possibly more) from past repair operations. v2.1.16+ binaries enforce strict `#1e` validation that correctly rejects these. v2.1.15 has weaker validation that tolerates them. v2.1.24 adds env-gated tolerance: blocks below the cutoff are warn-only, blocks at/above are strictly validated. Operator-opt-in per validator. Default unset = strict (today's behaviour). Mainnet upgrade flow: set cutoff = current_tip + 1000, rolling restart with v2.1.24 binary.

### Added (#288)

- **`SENTRIX_LEGACY_VALIDATION_HEIGHT` env var.** When set on a validator, the strict `CRITICAL #1e: state_root mismatch` check in `apply_block_pass2` is downgraded to warn-only for blocks with `index < cutoff`. The block's stamped state_root is retained (so block hash chain stays intact), the `divergence_tracker` still records the mismatch (visible in metrics, just doesn't fire the rate-alarm), and apply continues normally. Above the cutoff, strict reject behaviour is unchanged.
- **Test harness `test_legacy_validation_height_branches`** in `crates/sentrix-core/tests/fork_determinism.rs` documents the three behavioural branches (env unset = strict; env set & block.index < cutoff = tolerate; env set & block.index ≥ cutoff = strict). Marked `#[ignore]` because reproducing strict #1e in unit tests requires blocks past `STATE_ROOT_FORK_HEIGHT` (100,000); operator-driven manual verification via `apply_canonical_block_to_forensic` against the Beacon node forensic backup is the integration-level test (verified empirically: env=600000 → tolerate, env=100000 → strict, env unset → strict).

### Closed by Path B vs the alternative

The other path considered was a chain.db rebuild via genesis-replay — produce a clean canonical chain.db with v2.1.23-correct state_roots, halt all 4 mainnet validators, replace chain.db, restart on v2.1.24, then activate Voyager. The chain.db rebuild path was rejected because it changes block hashes for affected heights → all subsequent blocks' `previous_hash` chain breaks → effectively a chain reorganisation/restart from the first patched height, breaking external services that cache block hashes (block explorer, RPC clients). Multi-day operation. Path B preserves block hashes and unblocks Phase 1 within days.

Full design + trade-off analysis at `internal operator runbook`. RCA evidence trail at `internal operator runbook`.

### Operational rollout this release enables

1. Choose `legacy_height = mainnet_tip + 1000` at deploy moment.
2. Update each validator's env file in lockstep (md5sum parity check), rolling restart with v2.1.24 binary (~30s halt per validator, mainnet round-robin tolerates).
3. Verify chain producing post-deploy with all 4 vals.
4. Voyager activation env flips (`VOYAGER_FORK_HEIGHT`, `VOYAGER_REWARD_V2_HEIGHT`) at heights ABOVE the legacy cutoff so post-cutoff blocks are fully strictly validated.

---

## [2.1.23] — 2026-04-25 — init_trie empty-hash false-positive fix (partial #268 close)

> **🚨 v2.1.21–v2.1.23 DEPLOYMENT FROZEN ON MAINNET** — v2.1.23 closes ONE disk-roundtrip divergence class identified by yesterday's repro harness, but mainnet's specific v2.1.21 canary symptom (non-empty roots, immediate mismatch against v2.1.15 peers) is **not** explained by this fix. Mainnet stays on v2.1.15 until further #268 investigation lands. v2.1.23 deployed to testnet docker for soak.

### Fixed (#279 — partial #268 close)

- **`Blockchain::init_trie` `node_exists` false-positive on `empty_hash(0)`**. The empty-trie sentinel is never materialised in `TABLE_TRIE_NODES` because the binary SMT short-circuits empty subtrees, so `node_exists(empty_hash(0))` always returns false. The init_trie node-missing check at `blockchain.rs:847` mistook that for "node missing from storage" and triggered a spurious backfill from AccountDB on chains where every committed root equalled the empty hash (coinbase-only test, genuinely-quiet recovery windows). Three things compounded:
  1. Backfill rebuilt a non-empty root from genesis premine.
  2. `trie.commit(height)` wrote that backfilled root to MDBX BEFORE the divergence safeguard fired.
  3. The safeguard correctly returned `Err`, but `Storage::load_blockchain` swallows that Err below `STATE_ROOT_FORK_HEIGHT` (warn-only) — silent permanent corruption of the chain.db.
- Fix short-circuits `node_exists` for `empty_hash(0)`. The empty subtree is trivially valid without any storage entry, so it's safe to treat as "node exists" and skip backfill. Trigger conditions for the bug are narrow (chains with empty trie state) so mainnet at h=553K is not affected — but the destructive-backfill path is closed for any future operator scenario that hits this regime.
- Drops `#[ignore]` on `test_mdbx_roundtrip_then_peer_block` in `crates/sentrix-core/tests/fork_determinism.rs` — now a permanent CI regression guard.

### Honest framing

This release closes **one** disk-roundtrip divergence class. Mainnet's specific `#268` symptom (v2.1.21 canary on Beacon node with non-empty rsync'd chain.db, immediate `#1e` mismatch against v2.1.15 peers) is **not** explained by this fix and remains under investigation. v2.1.23 ships the protection where it applies + locks the regression test.

### Follow-up tracked

- **Destructive-backfill-before-safeguard**: even with this fix, any future `needs_backfill = true` path writes the recomputed root to MDBX BEFORE the safeguard runs. If safeguard fires, on-disk state is left inconsistent. Hardening: compute backfill root in a scratch space, compare, persist only on agreement.
- **`Storage::load_blockchain` swallows init_trie Err below 100K**: warn-only path was justified for P2P-recoverable failures, but the interaction with the destructive backfill means a silently-warned init_trie Err can leave corrupted state. Worth revisiting.

---

## [2.1.22] — 2026-04-25 — Phase 1 prep: voyager activation idempotency + #268 repro harness

> **🚨 v2.1.21 + v2.1.22 DEPLOYMENT FROZEN** — issue #268 disk-roundtrip trie divergence remains unresolved. v2.1.22 ships a fast unit-test reproducer + the Phase 1 idempotency hard-gate identified during the pre-implementation scan, but does NOT close #268. Mainnet stays on v2.1.15 until #268 is fixed and a v2.1.23 ships clean against the new repro harness.

### Added (#276)

- **`crates/sentrix-core/tests/fork_determinism.rs`** — 4-test harness for state-root determinism. 3 active in-memory parity tests (self-produced ↔ peer-applied path convergence on short coinbase chains, 200-block coinbase chains, and tx-bearing chains) become permanent regression guards. The 4th test (`#[ignore]`'d) reproduces the actual #268-class disk-roundtrip divergence at unit-test scale: producer commits a trie root, freshly loaded `Blockchain` reading the same MDBX gets a different root for the same height. Reference in-memory peer-replay matches the producer, isolating the bug to the disk roundtrip path. Currently smells like bug #3 class — committed root garbage-collected by a subsequent insert that PR #184's `is_committed_root()` doesn't fully cover. Fast feedback loop (~3s) replaces the docker canary cycle for the next #268 bisect attempt.

### Fixed (#277 — Phase 1 prep, hard-gate)

- **Persistent `voyager_activated` + `evm_activated` flags on `Blockchain`** (`#[serde(default)]` for chain.db forward-compat). The validator-loop activation guard at `bin/sentrix/src/main.rs` was previously a local boolean that reset on every restart — past a Voyager fork height, that meant `activate_voyager` re-fired on every boot, re-registering validators (warning-spammed but consensus-safe today) and re-running `update_active_set` + `epoch_manager.initialize` redundantly. The Phase 1 pre-implementation scan flagged this as a non-negotiable pre-fork fix because any future non-deterministic mutation in the activation path would propagate into state_root divergence.
- Each `activate_*` early-returns if the flag is already set; the validator loop reads the flags on entry to seed its local fast-path booleans, skipping the read-then-write-lock sequence after the first tick post-restart.
- Behaviour on existing chains: testnet docker (post-Voyager-fork) experiences a one-time idempotent re-run on first restart with v2.1.22 (chain.db deserialises with `voyager_activated = false`), then sets the flag and skips cleanly on subsequent restarts. Mainnet (`VOYAGER_FORK_HEIGHT = u64::MAX`) sees no behavioural change — `activate_voyager` has never fired on prod.

### Designed against

- `internal operator runbook` — Q1 (#268 hypothesis re-rank), Q2 point 1 (Phase 1 hard-gate). The scan ranks PR #273's `txid_index` as a top suspect for #268 but rules it out via the commit-message disclaimer; the actual top hypothesis is `init_trie` backfill firing on reload because committed root nodes are GC'd by subsequent inserts. The ignored test in this release validates that hypothesis at unit-test scale.

---

## [2.1.21] — 2026-04-24 — Observability + startup perf (no consensus change)

> **🚨 DEPLOYMENT FROZEN** — 2026-04-24 Beacon canary (Beacon node) triggered immediate `CRITICAL #1e: state_root mismatch` against v2.1.15 peers even with fork envs unset. Rolled back to v2.1.15; divergence persisted through 3 rsync recovery attempts. Root cause remains unresolved — see GitHub issue #268. Do NOT deploy v2.1.21 to any mainnet VPS until the issue closes.

Maintenance patch collecting three bite-sized improvements merged over the 2026-04-24 session. No consensus, wire, or storage format change; `VOYAGER_*_HEIGHT` env vars remain the sole activation gates for Voyager behaviour.

### Added (#269)

- **`Blockchain::reset_reward_accumulators_for_fork_activation` extracted helper** with a unit test pinning the V4 Step 3 accumulator-reset invariant: both `pending_rewards` and `delegator_rewards` zeroed, validator entries themselves preserved. Closes the v2.1.19 CHANGELOG follow-up flag at the unit level. `apply_block_pass2` call site + gate predicate unchanged.

### Added (#271 — bug #1d diagnostic pass 1)

- **`SentrixRequest::variant_name() -> &'static str`** tag for all 10 request variants.
- **`pending_variants: HashMap<OutboundRequestId, &'static str>`** tracked at all 5 outbound `send_request` sites in the libp2p swarm task, released on both successful `Response` and `OutboundFailure`.
- **`RrEvent::OutboundFailure` log now includes the variant**: `libp2p: outbound failure to {peer} ({variant}): {error}`. Lets the next testnet bake finally distinguish BFT-proposal timeouts from background-traffic noise. Pure observability — call paths, timeouts, retry logic all untouched.

### Added + fixed (#273 — issue #268 diagnostic)

- **`Blockchain::backfill_txid_index` fast path**: on a warm chain (latest block's last tx already indexed), skip the whole-chain scan. Previously every startup did `height + 1` redundant MDBX reads with zero writes — on a 500K-block chain that's a silent several-minute CPU phase between MDBX open and the validator loop's first log, matching the "process alive, journal empty" shape operators saw on the first Voyager activation attempt.
- **Cold-path progress log every 50K blocks scanned** so operators see activity rather than a silent freeze during first-ever backfill on a large chain.
- **`load_blockchain` startup banner** emitted after the window is populated, reporting `height {n} ({window_len} blocks in window)`. Gives a clean marker between MDBX open and the first validator-loop tick.

### Diagnostic findings from this session

Local repro of #268 against a clean 1 GB mainnet `chain.db` snapshot rsynced from Foundation node (via 28 s halt window):

- v2.1.20 release binary + no fork envs → clean startup, height 506078 loaded, 4 validators detected, idle.
- v2.1.20 release binary + fork envs (`VOYAGER_FORK_HEIGHT=502000` / `VOYAGER_REWARD_V2_HEIGHT=502100`, both below current) → same clean startup.

Conclusion: the v2.1.20 binary itself is not the regression source. #268 remains OPEN and the most likely remaining cause is 4-validator peer-gossip interaction on shared mainnet state — not reproducible in a single-process test harness. Mainnet activation runbook stays flagged BLOCKED.

### Mainnet / testnet impact

No runtime behaviour change on Pioneer chains. `VOYAGER_*_HEIGHT` env vars default `u64::MAX`; without explicit opt-in this release runs identically to v2.1.20 on a pre-fork chain.

## [2.1.20] — 2026-04-25 — Full StakingOp dispatch (Delegate / Undelegate / Redelegate / Unjail / RegisterValidator / SubmitEvidence)

v2.1.19 wired only ClaimRewards. This release closes the last code-side Voyager launch blocker by wiring every remaining `StakingOp` variant into the `block_executor` dispatch, all escrowed through `PROTOCOL_TREASURY` so the supply invariant holds across the full delegate → reward → unbond cycle.

### Added (staking-via-tx dispatch)

All variants gated on `is_reward_v2_height(block.index)` + require `tx.to_address == PROTOCOL_TREASURY`:

- **`RegisterValidator { self_stake, commission_rate, public_key }`** — self_stake escrowed to treasury via outer transfer; `stake_registry.register_validator` + authority mirror. Enables community validators without admin involvement. `add_validator_unchecked` made non-test-only for this path.
- **`Delegate { validator, amount }`** — `tx.amount == amount` enforced; outer transfer escrows to treasury; `stake_registry.delegate` records bookkeeping.
- **`Undelegate { validator, amount }`** — `tx.amount == 0`; `stake_registry.undelegate` queues unbonding.
- **`Redelegate { from, to, amount }`** — `tx.amount == 0`; `stake_registry.redelegate` moves delegation bookkeeping between validators.
- **`Unjail`** — `tx.amount == 0`; `stake_registry.unjail` clears the jail flag.
- **`SubmitEvidence { ... }`** — `tx.amount == 0`; `slashing.process_double_sign` applies slash + tombstone. Bounty-to-submitter deferred (schema needs separate `submitter` field).

### Fixed (unbonding maturity)

- **main.rs: post-fork unbonding release transfers from treasury** — pre-fork path used `accounts.credit` (mint from nowhere). Post-fork path now correctly `accounts.transfer(PROTOCOL_TREASURY, delegator, amount, 0)`. Both self-produce + peer-finalize unbonding paths fixed. Supply invariant holds across the full escrow lifecycle.

### Validation posture

Wrong `to_address` → `Err(InvalidTransaction)` → Pass 2 snapshot rollback reverts the block's mutations. Prevents accidental loss of SRX by users sending staking ops to wrong address.

### Tests

All existing 180 core + 88 staking + 76 bft + 5 harness tests pass. No new regression tests for the newly-wired variants — covered by the underlying `stake_registry` tests which exercise the delegation/unbonding/slashing logic directly.

### Mainnet / testnet impact

No runtime impact today — both `VOYAGER_FORK_HEIGHT` + `VOYAGER_REWARD_V2_HEIGHT` default `u64::MAX`. Enables end-to-end staking-via-tx flow when operator coordinates the hard-fork activation.

## [2.1.19] — 2026-04-25 — V4 Step 3 — treasury-escrow + ClaimRewards dispatch (gated fork)

Closes the V4 design. `PROTOCOL_TREASURY` address reserved, coinbase routing gated on `VOYAGER_REWARD_V2_HEIGHT` env var, `StakingOp::ClaimRewards` dispatch wired in `block_executor`. Supply invariant restored: post-fork, every SRX block reward lands in treasury, drains only via a claim tx that decrements the per-delegator / per-validator accumulator.

### Added

- **primitives: `PROTOCOL_TREASURY` address** (`crates/sentrix-primitives/src/transaction.rs`). `0x0000000000000000000000000000000000000002`. No private key → no one can sign as treasury; treasury state only moves via consensus-level dispatch in `block_executor`.
- **core: `VOYAGER_REWARD_V2_HEIGHT` env-var fork gate** (`crates/sentrix-core/src/blockchain.rs`). Default `u64::MAX` = disabled (mainnet-safe pre-fork behaviour preserved). `Blockchain::is_reward_v2_height(h)` + `is_reward_v2_active()` helpers. Coordinated operator rollout required for activation (consensus change).
- **core: coinbase routing gate** (`crates/sentrix-core/src/block_executor.rs:apply_block_pass2`). Post-fork: coinbase credits `PROTOCOL_TREASURY` instead of the proposer. Pre-fork path unchanged.
- **core: accumulator reset at fork activation** — on the single transition block (`is_reward_v2_height(block.index) && !is_reward_v2_height(block.index - 1)`), all pre-existing `pending_rewards` + `delegator_rewards` are zeroed. Pre-fork accumulator values represented rewards ALREADY credited via coinbase → proposer balance; without the reset, claim would double-mint. Resets keep supply invariant load-bearing from block 0 of the post-fork era.
- **core: `StakingOp::ClaimRewards` dispatch** (`block_executor.rs:apply_block_pass2`). Decodes `StakingOp` from `tx.data`, on `ClaimRewards`: drains `take_delegator_rewards(sender)` + `std::mem::take(validator.pending_rewards)` (if sender is a validator), transfers `PROTOCOL_TREASURY → sender` for the sum. Gated on `is_reward_v2_height(block.index)` so pre-fork chains ignore it. Other `StakingOp` variants (Delegate/Undelegate/Redelegate/Unjail/SubmitEvidence) silently no-op pending a follow-up staking-via-tx dispatch PR.

### Tests

- `test_v4_reward_v2_fork_height_default_disabled` pins the mainnet-safe default (`u64::MAX`) in `block_executor::tests`.
- Existing 179 sentrix-core + 88 sentrix-staking + 76 sentrix-bft + 5 harness tests all pass.

### Testnet bake

v2.1.19 binary `ecb18d63a4e0a5da` on 4-validator Voyager testnet with `VOYAGER_REWARD_V2_HEIGHT=188836`. Post-fork treasury accrued **70 SRX per 70 blocks = 1 SRX/block** exactly — supply invariant holds at the coinbase-routing level. Chain sustained 2.33 blocks/sec, zero skip-round / nil-majority / CRITICAL errors.

Accumulator-reset activation not exercised on this testnet (chain had already crossed the old fork before the reset-logic binary deployed). Next-session task: spin up a clean testnet (wipe `data/val*/chain.db`) + verify reset fires at the fork transition block. Until then, reset is compile-tested + logic-reviewed.

### Still open for Voyager mainnet activation

- **Staking-via-tx dispatch for other `StakingOp` variants** (Delegate/Undelegate/Redelegate/Unjail/SubmitEvidence) — current release wires only ClaimRewards. Users can't submit Delegate transactions yet. Separate follow-up PR.
- **Operator coordination**: set `VOYAGER_FORK_HEIGHT` + `VOYAGER_REWARD_V2_HEIGHT` env vars on all 4 mainnet validators at the same target height + coordinated restart.

### No mainnet impact today

Both `VOYAGER_FORK_HEIGHT` and `VOYAGER_REWARD_V2_HEIGHT` default `u64::MAX`. Mainnet runs Pioneer PoA unchanged.

## [2.1.18] — 2026-04-25 — V4 reward distribution v2 Step 2 (multi-signer pro-rata payout)

Step 2 of V4 per `audits/reward-distribution-fix-design.md`. `distribute_reward` now accepts a signers list and pays every precommit signer in the justification pro-rata by stake, splitting each signer's share into commission + self-stake + per-delegator accumulator. Step 1 (delegator_rewards field) shipped in PR #262; Step 3 (claim CLI + blockchain::apply wire) deferred.

### Changed

- **staking: distribute_reward now takes `signers: &[(String, u64)]`** (`crates/sentrix-staking/src/staking.rs`). Legacy single-proposer payout kicks in when signers is empty (Pioneer chains with no justification). Voyager path extracts signers from `block.justification.precommits`. Back-compat preserved for integration test + Pioneer chain.db state.
- **bft finalize path in main.rs** passes `justification.precommits` as `(validator, stake_weight)` tuples into `distribute_reward` at both BFT FinalizeBlock call-sites.

### Fixed (economics, not user-visible today)

Every signer gets their pro-rata share split between their commission (`pending_rewards`), their self-stake share (also `pending_rewards`), and their delegators' pool (accumulated into `delegator_rewards[delegator]`). Pre-V4 behaviour concentrated all reward on the proposer and dropped delegator share entirely. V4 Step 2 fixes the accumulation — Step 3 will fix the claim path so delegators can drain accumulated rewards into SRX balance.

### Tests (4 new V4 regression)

- `test_v4_distribute_reward_multi_signer_equal_stakes` — 4-validator chain, equal stakes, asserts each validator gets commission+self-share and each delegator gets pro-rata accumulator entry.
- `test_v4_distribute_reward_unknown_signer_skipped` — defensive skip of signers not in stake_registry.
- `test_v4_distribute_reward_empty_signers_legacy_fallback` — Pioneer back-compat.
- `test_v4_claim_rewards_after_distribute` — end-to-end accumulator drain via `take_delegator_rewards`.

88/88 sentrix-staking tests pass. Testnet bake: 556 blocks / 5 min sustained 1.85 blocks/sec on v2.1.18, one V2 skip-round recovered cleanly.

### No mainnet impact today

Mainnet runs Pioneer PoA with no BFT finalize path → `reward_signers` always empty → legacy fallback preserves old payout behaviour.

## [2.1.17] — 2026-04-25 — V1 real precommit signatures (Voyager #251 closed)

Re-applies PR #237's precommit-tuple widening + finalize-emit filter on top of v2.1.16's V2 M-15. The prior bisect wrongly pinned V1 as the v2.1.12 livelock cause — the real trigger was PR #244's hot-path fsync (closed v2.1.15), and the secondary livelock that v2.1.14 exposed was the V2 locked-block-re-propose scenario (closed v2.1.16). With both of those gone, V1 is now a pure emission-path improvement with no runtime consequences for consensus timing.

Closes issue #251 and the last of four originally-flagged Voyager BFT blockers from the 2026-04-20 audit (V1, V2, V3, V5 — all now shipped).

### Fixed

- **bft: emit REAL precommit signatures in BlockJustification (#251)** (`crates/sentrix-bft/src/engine.rs`). `BftRoundState.precommits` tuple widened from `(Option<String>, u64)` to `(Option<String>, Vec<u8>, u64)` so the ECDSA sig bytes from each peer's Precommit message are stored alongside the vote + stake. At finalize, the emit loop filters to precommits that voted for the winning hash and attaches their real signatures to the emitted `BlockJustification`. Nil precommits and precommits for other hashes are correctly excluded. The previous `vec![]` placeholder was unforgeable-but-unverifiable — a silent-reorg surface at Voyager activation. `test_finalize_emits_real_precommit_signatures` un-`#[ignore]`'d.

### Testnet bake evidence

4-validator Voyager docker stack on v2.1.17 binary `7181d37e56b00316`: **1318 blocks / 10 min sustained at 2.2 blocks/sec**, zero skip-round / nil-majority / CRITICAL warnings, zero state_root mismatches. V2 M-15 re-propose events fired where appropriate (2 events in first 5min window) — chain recovers from partial-quorum situations without stalling.

### Voyager blocker ledger (all shipped)

| # | Blocker | Release |
|---|---|---|
| V1 | Real precommit signatures | **v2.1.17** (this) |
| V2 | Locked-block re-propose | v2.1.16 |
| V3 | Jailing enforcement at consensus | v2.1.13 |
| V5 | Commission rate-limit | v2.1.12 |
| V6 | Liveness thresholds retune | v2.1.12 (#215) |

### Remaining before Voyager activation

- **V4 reward distribution v2** (delegator reverse-lookup + claim flow) — 2-3 week scope, design at `audits/reward-distribution-fix-design.md`.
- Operator coordination: `VOYAGER_FORK_HEIGHT` env var set on all 4 mainnet validators + coordinated restart window.

## [2.1.16] — 2026-04-25 — V2 M-15 locked-block re-propose shipped (Voyager liveness)

Closes the second of four Voyager mainnet blockers from the 2026-04-20 audit. V2 M-15 was the "locked-block re-propose" liveness gap — when an earlier round reached 2/3+ prevote quorum but failed precommit, locked validators would prevote nil on later rounds' fresh-built blocks (safety invariant), leaving the chain stuck until the lock expired. With V2, a locked validator elected proposer in a later round re-broadcasts the CACHED block bytes from the round it locked on. Chain unsticks at tempo.

### Added

- **bft: V2 M-15 locked-block re-propose, end-to-end** (`crates/sentrix-bft/src/engine.rs` + `bin/sentrix/src/main.rs`). Four-step rollout across PRs #257, #258, and this release:
  - Step 1 (#257): `BftRoundState.locked_block` + `staging_block` fields with `#[serde(default)]` backward-compat + lifecycle hooks (`new`, `advance_round`, `advance_height`).
  - Step 2 (#258): `stash_proposal_bytes(hash, bytes)` method — validator loop stashes before routing proposal hash into on_proposal.
  - Step 3 (#258): prevote-supermajority branch promotes `staging_block` → `locked_block` when the staged hash matches. Wrong-hash staging discarded (byzantine protection).
  - Step 4+5 (this release): main.rs `build_or_reuse_proposal` helper replaces 7 proposer call-sites — checks `locked_proposal_bytes()` first, re-broadcasts cached block if locked, else calls `create_block_voyager`. Peer-proposal path at `BftMessage::Propose` also stashes bytes via `stash_proposal_bytes`.
- 5 new BFT engine regression tests pin: promotion on supermajority, advance_round preserves locked clears staging, new_height clears both, unlocked returns None, wrong-hash staging discarded.

Evidence: 4-validator testnet deploy produces 1.6-1.8 blocks/sec sustained over 8+ minutes with 7 V2 re-propose events observed in logs (`V2 M-15: re-proposing locked block <hash>... at height N round K`). Chain recovers from skip-round conditions that previously would have livelocked — same class as the symptom tracked in issue #252.

### Impact on Voyager mainnet launch

- **Closed:** V2 M-15 (this release)
- **Closed:** V3 jailing enforcement (#236 trim in v2.1.13)
- **Closed:** V5 commission rate-limit (#235 in v2.1.12)
- **Still open:** V1 real precommit signatures (#251) + V4 reward distribution v2 (design exists)

Mainnet today is Pioneer PoA (VOYAGER_FORK_HEIGHT=u64::MAX env var). V2 wiring only exercises on the Voyager BFT path, so this release is a no-op at runtime on the current mainnet chain. It's loaded for the future Voyager activation moment.

## [2.1.15] — 2026-04-25 — Closes #252 livelock (revert #244 hot-path persist) + #253 liveness signers fix

Third bisect pass on the v2.1.12 bundle found **PR #244** (BACKLOG #16 inline durable persist) as the residual trigger. Without #244, testnet runs at 4.47 blocks/sec sustained for 180s with zero skip-round / nil-majority warnings (955 blocks / 6 minutes clean). With all of #236-else / #237 / #244 reverted, the remaining bundle is safe to ship.

Also closes the deterministic Voyager-chain cascade-jail from #253: `record_block_signatures` now receives the full list of precommit signers from `justification.precommits` instead of `vec![proposer]`, so non-proposer validators stop being counted as MISSED every block.

### Fixed

- **core(block_executor): remove inline `persist_block_durable` from commit hot path (#252)** (`crates/sentrix-core/src/block_executor.rs`). PR #244's inline persist did 3 MDBX puts + 1 `sync()` on every block under the blockchain write-lock. On a 4-validator Voyager testnet that pushed BFT rounds past the 12s precommit timeout under sustained load, producing the prevote→nil-precommit flip livelock pattern tracked in #252. The gap-formation risk it was guarding against (BACKLOG #16 / PR #226's 7,352 missing `block:N` keys) is already covered without hot-lock cost via #243 (loud save_block failure + Prometheus alert) and #225 (GetBlocks serves evicted blocks from MDBX for peer sync recovery). `persist_block_durable` stays on `Blockchain` as an opt-in tool for CLI / recovery flows.
- **bft/staking: count ALL precommit signers in liveness, not just proposer (#253)** (`bin/sentrix/src/main.rs`, `crates/sentrix-staking/src/slashing.rs`). Both BFT FinalizeBlock call sites were passing `signers = vec![proposer]` to `record_block_signatures`, so on a 4-validator chain each validator was counted as SIGNED only 25% of blocks vs the 30% `MIN_SIGNED_PER_WINDOW` threshold — deterministic cascade-jail every 14400 blocks (~80 min at 3 blocks/sec). Fix extracts signers from `justification.precommits`. Two regression tests pin the semantics: `test_full_justification_no_cascade_jail` (healthy) + `test_proposer_only_signers_triggers_cascade_jail` (load-bearing proof the old model was broken).

### Added (test infrastructure)

- **4-validator in-memory BFT harness** (`crates/sentrix-bft/tests/four_validator_harness.rs`). 5 tests drive `BftEngine` through full Propose→Prevote→Precommit→Finalize cycles with a shared `StakeRegistry`, no libp2p. The missing infrastructure that would have caught #237 pre-merge. Happy path, three-consecutive-heights, round-advance, prevote + precommit dedup.

## [2.1.14] — 2026-04-25 — Revert PR #237 (V1 reopened) — close v2.1.12 livelock

Second bisect pass over v2.1.12 exposed PR #237 (real precommit signatures) as the underlying cause of the prevote→nil-precommit flip livelock that v2.1.13's #236 trim did not fully close. Reverting #237's tuple extension and restoring the pre-V1 blanket `vec![]` placeholder emit loop allows 4-validator testnet to produce at baseline rate (2.69 blocks/sec sustained, 484 blocks over 180 seconds, zero skip-round warnings).

V1 (empty justification signatures, Voyager blocker) is REOPENED by this revert. Voyager activation is gated on V1 plus the remaining P0 blockers. A follow-up PR must re-implement real-sig emission without triggering the precommit flip — the obvious suspects are (a) tuple widening on `BftRoundState.precommits` interacting with dedup timing under libp2p message fan-in, or (b) the finalize-emit filter on `vote_hash == block_hash` excluding precommits that should still count. Next-session work.

Everything else in v2.1.12's bundle stays shipped: #215 V6 liveness retune, #235 V5 commission rate-limit, #238 C-03 trie-root rollback, #239 REST finalized-height, #240 audit log context, #241 TABLE_BLOOM visibility, #242 supply + burn metrics, #243 peer-save fail alerting, #244 BACKLOG #16 durable persist, #245 REST/JSON-RPC parity. And v2.1.13's #236 trim stays (jailed/tombstoned check preserved, miss-from-registry rejection dropped).

### Reverted

- **bft: revert PR #237's real precommit signatures** (`crates/sentrix-bft/src/engine.rs`). `BftRoundState.precommits` tuple restored to `(Option<String>, u64)`. Finalize emit loop restored to blanket `add_precommit(val, vec![], w)` over all precommits. `test_finalize_emits_real_precommit_signatures` marked `#[ignore]` pending V1 re-implementation. V3 jail-check (#236) and V5 commission (#235) unaffected.

## [2.1.13] — 2026-04-25 — Fix v2.1.12 testnet livelock (#247 partial)

v2.1.12 bundled 10 PRs and was reproducibly livelocked on 4-validator BFT testnet bake — validators prevoted block, precommitted nil 12s later, rounds skipped forever. The v2.1.12 GitHub release is flagged `--prerelease` with operator warning.

Bisected to PR #236's `on_proposal` check: the else-branch (reject when proposer is missing from `stake_registry.validators`) was too strict for real operational state where registry-vs-active_set drift happens. `weighted_proposer` already gated the proposer upstream, so the extra registry-miss reject was belt-and-suspenders that tripped itself.

Fix preserves the original #236 intent (jailed/tombstoned validators cannot drive consensus) and drops the else-branch rejection. No other runtime logic changed.

### Fixed

- **bft: drop `stake_registry.get_validator()` miss-reject in `on_proposal`** (`crates/sentrix-bft/src/engine.rs`, #248). Bisected root cause of the v2.1.12 testnet livelock. With the else-branch removed, testnet immediately produced clean blocks at baseline rate for the first 82 blocks of bake before a separate cascade (PR #215 tight liveness thresholds interacting with PR #236 `update_active_set()` eviction) surfaced — documented in issue #247 for next-session follow-up. Regression test flipped: `test_unregistered_proposer_rejected_at_on_proposal` retired, replaced with `test_active_set_proposer_accepted_when_missing_from_registry_validators` pinning the new invariant. 71/71 BFT tests green.

## [2.1.12] — 2026-04-24 — Voyager-blocker sweep + BACKLOG #16 durable + REST/JSON-RPC parity

Patch release bundling the 2026-04-24 late-night marathon: three Voyager blockers closed (V1 real precommit signatures, V3 jailing enforcement at consensus, V5 commission rate-limit), the C-03 trie-root rollback gap sealed, BACKLOG #16 gap formation eliminated at the durable fix point (atomic apply + MDBX persist with rollback), fresh-brain review follow-up aligning REST `/chain/finalized-height` semantics with JSON-RPC, plus observability + metrics surface work (supply + burn counters, silent P2P block-save alerting, TABLE_BLOOM visibility).

Safe to upgrade on existing chain.db — new field `last_commission_change_height` is `#[serde(default)]` on `ValidatorStake`; `BlockchainSnapshot` gained a `trie_root` slot but it's internal (`pub(crate)`) and not serialized; `state.precommits` tuple widened inside BFT engine but `BftRoundState` is in-memory only. No MDBX schema change.

### Fixed

- **rpc(rest): align `/chain/finalized-height` fallback with `sentrix_getFinalizedHeight`** (`crates/sentrix-rpc/src/routes/chain.rs`). Follow-up to #239 surfaced during fresh-brain review: when BFT was active but no justified block sat in the in-memory sliding window, REST returned `finalized_height=0` / `finalized_hash=""` while JSON-RPC returned `latest.index` / `latest.hash`. Clients hitting each endpoint on the same chain state saw different answers. Sliding window rolls older blocks out of memory long before they drop off disk, so on a BFT chain (post #244 durable persistence) zero-justified-in-window is normal right after boot, during long idle periods, or when syncing from an older peer. Fixed by mirroring JSON-RPC's `h = latest.index; hash = latest.hash.clone()` initialisation — falls back to latest only when the walk finds no justified block. Identical payload across both endpoints.
- **staking(commission): rate-limit `update_commission` to one change per epoch per validator** (`crates/sentrix-staking/src/staking.rs`). Previously an operator could call `update_commission` repeatedly within one block — each call stayed inside the 2% per-step cap (`MAX_COMMISSION_CHANGE_PER_EPOCH`), but cumulative drift was unbounded (N × 2% per block). This closes the V5 Voyager-blocker entry from the 2026-04-20 audit. New regression test `test_commission_stepping_attack_rejected_same_epoch` pins the invariant. `update_commission` now takes a `current_height: u64` argument; the existing `test_commission_update` was refreshed to thread the height through. New field `last_commission_change_height: u64` on `ValidatorStake` (0 = never changed) tracks the throttle; marked `#[serde(default)]` so fresh-deploy chains can slot the field in without a hard migration.
- **bft/staking(jailing): enforce `is_jailed` at consensus boundary** (`crates/sentrix-staking/src/staking.rs` + `crates/sentrix-bft/src/engine.rs`). Closes V3 Voyager-blocker from the 2026-04-20 audit: the `is_jailed` flag existed but consensus never cross-referenced it, so a jailed validator could still propose blocks as long as they sat in `active_set`. Two-layer fix: (1) `slash` / `jail` / `tombstone` / `unjail` now call `update_active_set()` so the jail status propagates to the rotation pool immediately instead of waiting for the next epoch tick; (2) `BftEngine::on_proposal` cross-references the stake registry and refuses proposals from any validator currently flagged `is_jailed`, `is_tombstoned`, or not registered at all — even if `weighted_proposer` returned their address (defense against stale-active_set races between slash-apply and next-proposal-arrive). Three new staking regression tests (`test_jail_evicts_from_active_set_immediately`, `test_tombstone_evicts_permanently`, `test_unjail_readmits_to_active_set`) + two BFT regression tests (`test_jailed_proposer_rejected_at_on_proposal`, `test_unregistered_proposer_rejected_at_on_proposal`) pin both layers. Single-HashMap-lookup overhead on the hot path — cheap.
- **bft(justification): emit REAL precommit signatures at finalization** (`crates/sentrix-bft/src/engine.rs`). Closes V1 Voyager-blocker from the 2026-04-20 audit. Before: the finalize loop at the 2/3+ precommit pivot passed `vec![]` placeholders to `BlockJustification::add_precommit`, so every finalized block carried a cryptographically meaningless proof — a silent-reorg and cross-fork replay surface the moment Voyager activated. Now: `state.precommits` is typed `HashMap<String, (Option<String>, Vec<u8>, u64)>` (block_hash + signature + stake), `on_precommit_weighted` stores `precommit.signature.clone()` alongside the vote, and the finalize emit loop only includes precommits that voted for the winning hash (nil and wrong-hash votes are correctly excluded). New regression test `test_finalize_emits_real_precommit_signatures` injects three distinct signatures + one nil-precommit + one wrong-hash-precommit and asserts only the winning-hash sigs land in the emitted justification, each non-empty and byte-preserved from input.
- **core(block_executor): snapshot + restore trie root on Pass 2 failure** (`crates/sentrix-core/src/block_executor.rs` + `crates/sentrix-trie/src/tree.rs`). The C-03 atomic-commit snapshot already restored `accounts` / `contracts` / `authority` / `mempool` / `total_minted` / `chain` on Pass 2 error, but it explicitly did NOT snapshot the state trie — a pre-PR-#184 comment claimed the trie "self-heals" because it gets rebuilt from `accounts` on each `update_trie_for_block` call. That claim was wrong: trie insert/delete walks the current `self.root` — it is NOT recomputed from scratch. So a Pass 2 that failed partway through trie updates would leave the in-memory root pointing at a half-updated state while `accounts` was reverted — silent divergence on the next block, exactly the failure class the 2026-04-20 mainnet incident showed. Fix: capture `state_trie.root_hash()` into `BlockchainSnapshot.trie_root` before Pass 2, restore it via new `SentrixTrie::set_root` on failure. Orphan MDBX nodes from the failed block's partial inserts remain in storage but are unreachable from any committed root; the scheduled `prune(keep_versions)` at TRIE_PRUNE_EVERY-aligned heights (1000 by default) GCs them. New regression test `test_set_root_rewinds_to_known_committed_root` in `sentrix-trie` pins the `get`-after-rewind contract.

### Added

- **rpc(rest): `GET /chain/finalized-height`** — REST alias for the existing `sentrix_getFinalizedHeight` JSON-RPC method. Closes the A5 audit finding. Returns `finalized_height` / `finalized_hash` / `latest_height` / `blocks_behind_finality` / `consensus`. On Pioneer PoA every committed block is final — endpoint returns the tip. On Voyager BFT walks back from the tip for the newest block with `justification.is_some()`. Lets light clients + dashboards + Prometheus exporters learn finality lag without speaking JSON-RPC.
- **rpc(metrics): supply + burn counters exposed at `/metrics`** — three new Prometheus gauges/counters: `sentrix_total_minted_sentri` (counter, all SRX ever minted), `sentrix_total_burned_sentri` (counter, all SRX burned from fee split + explicit burns), `sentrix_circulating_supply_sentri` (gauge = minted − burned). Unlocks supply-curve and burn-rate charts in Grafana + lets Prometheus alert on supply-invariant violations (e.g. `sentrix_total_minted_sentri > MAX_SUPPLY_sentri` should NEVER fire on a healthy chain). Raw sentri integers (not SRX floats) so rates/deltas stay exact across 1 SRX = 100M sentri scale.

### Fixed

- **bin,rpc(metrics): surface silent P2P block-save failures (BACKLOG #16)** (`bin/sentrix/src/main.rs` + `crates/sentrix-rpc/src/routes/ops.rs`). A `warn!`-only log on `save_block` failure at `NodeEvent::NewBlock` would silently drop a block from MDBX while chain state had already advanced in memory via `add_block_from_peer` — once CHAIN_WINDOW_SIZE (1000 blocks) rolled past, that block was gone from everywhere, creating a permanent TABLE_META gap. Exactly the shape the 2026-04-23 PR #226 sweep test surfaced on live mainnet chain.db: 7,352 missing `block:N` keys, longest contiguous run 5,042 at h=139,703. Fix is observability-only for now: (1) log escalates to `error!` with explicit "BACKLOG #16" tag + block index + hash + MDBX error, (2) new `sentrix_peer_block_save_fails_total` counter exported on `/metrics` (new `pub static PEER_BLOCK_SAVE_FAILS` AtomicU64 in `sentrix_rpc::routes::ops`, incremented by main's peer-block handler on each failure), (3) new Prometheus alert rule `PeerBlockSaveFailing` fires critical on `rate > 0` — operator now gets Telegram alert at the moment of the gap-creating event, not weeks later via sweep test. Durable fix (atomic `add_block_from_peer` + `save_block` with rollback on persist failure) requires storage plumbing into `sentrix-core` — out of scope for this observability patch.
- **core(block_executor): BACKLOG #16 durable fix — atomic apply + persist with rollback** (`crates/sentrix-core/src/blockchain.rs` + `crates/sentrix-core/src/block_executor.rs`). After the observability patch above surfaced gap events, this commit closes the underlying race so new gaps can't form at all. `Blockchain::persist_block_durable(&block)` is a new method that writes the block to MDBX via the pre-existing `mdbx_storage: Option<Arc<MdbxStorage>>` handle using the same byte layout as `sentrix-storage::Storage::save_block` (TABLE_META `block:{N}` + TABLE_BLOCK_HASHES reverse index + `height` marker + `sync()`). `add_block_impl` now calls this INSIDE the same snapshot scope that protects Pass-2 rollback: on persist failure, the existing snapshot-restore path fires (accounts, contracts, authority, mempool, total_minted, chain.truncate, trie.set_root) and the function returns Err. In-memory state and disk are in lock-step — a crash mid-persist leaves chain tip at N-1 on disk with matching in-memory state on restart, instead of the old "applied-in-memory-advanced, disk still at N-1, next CHAIN_WINDOW_SIZE blocks evict block N from both = permanent gap". Works for self-produced (Pioneer PoA), peer-received (gossip + BFT), and any future source — the fix lives at the common commit point. No-op for unit-test paths that don't bind `mdbx_storage` (returns Err only on real MDBX failures). Full workspace tests green, clippy clean, no state-root-path code changed (persist is stateless w.r.t. state_root).

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

Patch release over v2.1.8. Single change: adds a rate-limited LOUD alarm when a validator rejects peer blocks at a sustained rate — motivated by the 2026-04-23 mainnet fork investigation, where Core node had been silently rejecting peer blocks for ≥4 hours (~4000 state_root mismatches per hour) without any operator signal. The existing per-event `CRITICAL #1e` log line was accurate but emitted at ~1/s during real divergence, filling journald rotation so the earliest mismatches were evicted before the operator checked.

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

Post-mortem release after the 2026-04-21 mainnet 3-way state_root fork. The fork itself was recovered ops-side via frozen-rsync of Foundation node canonical chain.db to Treasury node + Core node (see `internal operator runbook`). This release closes the code-level gaps that let the incident develop silently on the v2.1.6 binary after a pre-v2.1.5 `state_import` had already damaged Core node's trie.

### Fork follow-ups

- **fix(trie): boot-time integrity check — refuse to start on orphan trie references** (`crates/sentrix-trie/src/tree.rs`, `crates/sentrix-core/src/blockchain.rs`). New `SentrixTrie::verify_integrity()` walks the current root and fails fast if any referenced node or leaf-value is missing from `trie_nodes` / `trie_values`. Wired into `Blockchain::init_trie`: hard-fail past `STATE_ROOT_FORK_HEIGHT`, warn-only below. In the 2026-04-21 incident, Core node's chain.db had a top-level root that existed but referenced an orphaned subtree, so the existing backfill-mismatch and missing-root-node guards didn't fire — it just produced `state_root=None` blocks that strict peers then rejected. PR #206.
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
  shape of the 2026-04-21 mainnet freeze (Treasury node drifted ~45 SRX/min
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
  Core node's trie had a missing node (`24afba5f…`) so block 100,004 got
  saved with `state_root = null`, Foundation node had a functional trie and block
  100,004 with `state_root = Some(…)` → different block hashes → Foundation node
  rejected Core node's block 100,005 as "invalid previous hash". Post-fix,
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
work natively, and deploy moves from CI to `fast-deploy.sh` on build host.

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
- **fast-deploy.sh primary deploy path** (PR #139) — builds on build host,
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
  regardless of build host host OS. Fixes crash-loop on commit e49e01d where
  a build host 24.04 native build (glibc 2.39) failed to load on Foundation node/Treasury node.
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
- scan.sentrixchain.com explorer (nginx proxy)
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
- **Multi-validator BFT testnet** (4 validators on Core node) with 3/4 fault tolerance
- **Wallet encryption CLI** — `wallet encrypt`/`decrypt`, `--validator-keystore`, password from env or prompt
- **CI/CD rolling restart** — validators restarted one at a time during deploys; chain never stops producing blocks
- **CI/CD covers testnet validators** — `sentrix-testnet-val1..4` services auto-updated
- **Robust health check** — 5 retries × 60s windows with cluster-max delta tolerance
- **testnet-scan / testnet-explorer** subdomains added to nginx
- **Single nginx server block** consolidates all 4 testnet subdomains; fixes MetaMask `/rpc` path bug
- **Per-IP rate limiter** bumped to 20 connections (handles Treasury node hosting 5 validators on one IP)
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
- Testnet live: chain_id 7120, port 9545, Core node
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
- Core node (Sentrix Core) added as 7th validator
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
