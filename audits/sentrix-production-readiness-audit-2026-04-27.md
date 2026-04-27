# Sentrix Production-Readiness Audit — 2026-04-27

**Author:** ops investigation (autonomous session marathon ~30h+)
**Scope:** Architectural review + bug catalog + action items for production-grade chain
**Status:** Findings + recommendations; implementation defers to fresh-brain sessions

---

## Executive summary

Sentrix Chain at v2.1.43 is **operational** under healthy mesh conditions but has **architectural fragility** under partial-mesh scenarios (1-of-4 validator down). Today's marathon shipped 8 PRs (#350-#357) addressing data-layer bugs + observability + gate relaxation, but identified several deeper issues that need multi-day fresh-brain engineering.

**Production-readiness verdict:** Solo-operator self-managed mainnet works. **Multi-validator (external operators) NOT YET safe** — current architecture requires operator-coordinated chain.db rsync recovery, which doesn't scale to third-party validators.

**Bottom line:** Real fix path is documented in `audits/consensus-computed-jail-design.md` — 4-6 week implementation. Until that ships, mainnet should remain Foundation-operated.

---

## 1. Bugs FIXED today

### 1.1 Asymmetric `record_block_signatures` (PR #356) — ROOT CAUSE FIX

**Symptom:** Each validator records ITSELF as 99.99% signed but PEERS as 66-68% signed (~33% baseline gap), causing jail-cascade pattern observed 2x mainnet (h=633599 + h=662399).

**Root cause:** `bc.slashing.record_block_signatures()` called from `main.rs` validator-finalize paths (self-produce + BFT-finalize) but NOT from `libp2p_node.rs` block-apply paths (gossip + sync catch-up).

**Fix:** PR #356 adds `record_block_signatures` call at 3 libp2p apply sites. Now all peer-applied blocks record liveness identically to self-produced blocks.

**Verification:** Empirical confirmation on testnet at h=558000 documented. Self-correction over 14400-block window (~4h).

### 1.2 P1 BFT safety gate too strict (PR #351)

**Symptom:** When 1 of 4 validators jailed locally (active=3), gate `active < MIN_BFT_VALIDATORS (=4)` blocks BFT entirely, even though 3/4 supermajority is mathematically achievable.

**Fix:** Fork-gated relaxation via `BFT_GATE_RELAX_HEIGHT` env var. Pre-fork: `>= 4`. Post-fork: `>= ⌈2/3 × N⌉` (= 3 for N=4).

### 1.3 L2 cold-start gate too strict (PR #355)

**Symptom:** Same fork should also relax L2 peer-mesh gate from `peer_count >= active_set - 1 (= 3)` to `>= min_active - 1 (= 2)` for 1-jail tolerance.

**Fix:** PR #355 wires L2 gate to same fork. Both gates relaxed under fork.

### 1.4 Observability log level wrong (PR #353)

**Symptom:** Jail-counter snapshot at DEBUG level was filtered by default `RUST_LOG=info`.

**Fix:** Bump to INFO level. Now visible by default for fleet-wide divergence detection.

### 1.5 Hardcoded supply display (PR #347 + #348)

**Symptom:** Explorer richlist + multiple display sites used static `MAX_SUPPLY = 210M`, showed wrong percentage post-tokenomics-v2 fork.

**Fix:** All display sites now use `bc.max_supply_for(bc.height())` (fork-aware).

---

## 2. Bugs IDENTIFIED but NOT fixed (deferred — bigger scope)

### 2.1 libp2p connection resilience under peer churn

**Symptom:** When 1 validator goes down, OTHER validators' `peer_count` temporarily drops to 0 or near-0 (should stay at N-1 = 2 for 4-validator network). Triggers L2 cold-start gate to fire on validators that should still be connected.

**Hypothesis:** Identify protocol or `verified_peers` tracking is fragile. Connection-close events for one peer might cascade to drop unrelated peer entries, OR libp2p re-runs Identify on remaining peers (slow), OR there's a race in the disconnect handler at `crates/sentrix-network/src/libp2p_node.rs:755`.

**Impact:** Even with both gates relaxed (PR #351 + #355), chain stalls when 1 validator goes offline because remaining validators temporarily lose ALL peer connections during the disturbance.

**Fix scope:** ~3-5 days fresh-brain libp2p audit.
- Investigate identify protocol timing
- Trace disconnect-event cascade
- Add resilience tests (mock 1-of-4 peer down, verify others maintain connections)
- Possibly tune libp2p connection-keepalive params

**Workaround until fixed:** Don't stop validators. Use halt-all + simultaneous-start for any operational change.

### 2.2 BFT round timeouts conservative for offline-proposer scenarios

**Current values:**
```
PROPOSE_TIMEOUT_MS  = 20_000  (20s)
PREVOTE_TIMEOUT_MS  = 12_000  (12s)
PRECOMMIT_TIMEOUT_MS = 12_000 (12s)
Per round: ~44s
```

**Problem:** When proposer is offline, validators wait 20s before nil-voting. Per round = 44s. To skip through Beacon (offline) + reach next online proposer = 44s minimum, often more if proposals don't reach all peers in time. Chain stalls 3-5+ minutes during 1-of-4 downtime.

**Original rationale (in code comment):** Bumped 10s→20s in 2026-04-25 to give proposer time to include freshly-reconnected peer in outbound send list. Without this, peer-just-reconnected scenarios would silently drop proposals.

**Conflict:** Long timeouts good for reconnect scenarios, bad for offline-proposer scenarios.

**Resolution path:** PR `#1d` rebroadcast logic (`audits/v2-locked-block-repropose-implementation-plan.md` §1d) was supposed to fix the root cause (proposer re-broadcasts to late-arriving peers). Verify rebroadcast is solid + reduce timeouts back to 10s/5s/5s = ~20s/round.

**Fix scope:** ~1 day. Audit rebroadcast, then PR with reduced timeouts.

### 2.3 State_root divergence under partial finality (CRITICAL)

**Symptom:** When BFT can't finalize at height N (proposer offline, nil-majority), validators continue trying. Each validator stages a different block locally. Their local chain.db state diverges. After 5+ min stuck, MD5 of `mdbx.dat` differs across all validators.

**Why:** Per CLAUDE.md state-recovery rules, "state_root stamping at apply time" means each validator stamps its own state_root. If state_roots already differ (e.g., stale auto-jail counter), block hashes differ for the same proposer + prev_hash, leading to permanent divergence even after recovery.

**Implication:** Recovery from any stall REQUIRES chain.db rsync from canonical (well-tested today, ~5-10 min downtime). Not feasible at scale (multi-validator network can't ask each operator to rsync from others).

**Fix scope:** Architectural — `audits/consensus-computed-jail-design.md` proposed JailTransaction model would eliminate one source of divergence (jail decisions become consensus-applied state mutations). 4-6 weeks impl.

### 2.4 Reward distribution + epoch tracking missing on libp2p sync

**Related to 1.1 but different concern:**

`add_block_from_peer` (libp2p paths) doesn't call:
- `bc.epoch_manager.record_block(reward)` — epoch tracking (total_blocks_produced, total_rewards in current epoch)
- `bc.stake_registry.distribute_reward(...)` — reward distribution

For epoch_manager: each validator should track ITS OWN epoch state. Missing record on sync paths means catch-up validators have wrong epoch state. Could cause epoch transitions to mis-fire.

For reward distribution: rewards are ON-CHAIN state mutation. Reward distribution must be deterministic + happen exactly once. Need careful audit:
- Currently called from main.rs validator-finalize path → distributes once (on the validator that finalized via local BFT path)
- libp2p sync path applies block but doesn't distribute → if running validator was offline during finalize, its local view of rewards is stale
- Question: Is the on-chain stake_registry.delegator_rewards updated via consensus state apply (deterministic) or via local distribution call (per-validator)?

**Fix scope:** ~2-4 days investigation + fix. Requires understanding reward flow + tests.

### 2.5 Cargo dep audit (RUSTSEC-2025-0055)

**Vulnerability:** `tracing-subscriber 0.2.25` (transitive via revm/ark stack). ANSI escape injection.

**Fix scope:** Coordinated revm + ark-* ecosystem bump. ~1-2 days. Risk of API breaks (cargo update broke `revm-handler 18.1.0` ↔ `revm-inspector 18.0.0` earlier today).

### 2.6 Test coverage gaps

Test count: 775 (post-PR #356). For consensus-critical code, this is light. Specifically:
- Few integration tests for partial-mesh scenarios
- No tests for libp2p disconnect → reconnect cycles
- No tests for state divergence detection
- No tests for chain.db rsync recovery procedure

**Fix scope:** Multi-week test infrastructure investment.

---

## 3. Operational learnings

### 3.1 Halt-all + simultaneous-start is the safe restart pattern

Validated multiple times today. Rolling restart triggers jail-cascade divergence. Halt-all + simultaneous-start avoids it.

Memory: `feedback_mainnet_restart_cascade_jailing.md` updated.

### 3.2 chain.db rsync recovery is well-tested

5+ successful recoveries today. Procedure: `runbooks/jail-divergence-recovery.md`.

Time: ~5-10 min including libp2p mesh re-convergence. No data loss (forensic backups preserved).

### 3.3 1-of-4 validator down DOES break chain (architectural)

Even with all today's fixes (gate relaxation + asymmetric recording fix), stopping 1 validator on mainnet for 5 min caused chain to stall. Recovery required chain.db rsync.

This is the core production-readiness blocker. Fix scope = "consensus-computed jail" (4-6 weeks).

### 3.4 Observability metric (PR #350) is working

Confirmed on testnet: per-validator (signed/missed) snapshots fire at 1000-block intervals. Operator can grep across fleet to detect divergence early before BFT stall fires.

---

## 4. Action items prioritized (for fresh-brain sessions)

### P0 — Architectural fixes (multi-week)

1. **Consensus-computed jail (Phase A)** — `audits/consensus-computed-jail-design.md` Phase A (data plumbing). 5-7 days. Eliminates source of state_root divergence.
2. **libp2p resilience audit + fix** — investigate peer_count drops + connection cascade. 3-5 days.
3. **Reward distribution audit** — confirm reward flow is deterministic across all apply paths. 2-4 days.

### P1 — Operational improvements (multi-day each)

4. **BFT round timeout tuning** — verify rebroadcast (#1d) is solid, reduce timeouts back to 10s/5s/5s. ~1 day.
5. **Cargo dep audit fix** — coordinated revm+ark bump for RUSTSEC-2025-0055. ~1-2 days.
6. **Sourcify integration** (Eco Tier 1) — Docker on VPS + sentrixscan UI. 3-7 days.
7. **Canonical contracts deploy** (Eco Tier 1) — WSRX, Multicall3, Safe-fork, ERC-20 factory. 5-10 days.

### P2 — Testing infrastructure (multi-week)

8. **Integration tests** — partial-mesh, disconnect cycles, state divergence detection. ~1-2 weeks.
9. **Chaos testing** — induced jail, network partition, slow validator scenarios. ~1 week.

### P3 — Documentation + operational

10. **Update WHITEPAPER §2.6** to reflect production-readiness honest assessment.
11. **Operator runbook for multi-validator coordination** — once Phase A ships, document chain-recovery in multi-operator setting.

---

## 5. Production-readiness verdict

**Solo-operator (Foundation-only):** ✓ Operational. Recovery procedures tested. v2.1.43 deployed.

**Multi-validator (3rd-party operators):** ✗ NOT YET. Current chain.db rsync recovery requires Foundation operator-level coordination. Can't ask 3rd-party validators to rsync from each other.

**Production-grade (high-stakes):** ✗ Architectural fixes needed first (consensus-jail Phase A → mainnet activation = ~6-8 weeks calendar).

**Honest path forward:**
1. Stay solo-operator while consensus-jail Phase A is implemented (4-6 weeks dev + testnet bake)
2. Phase A activation on mainnet = jail-cascade structurally eliminated
3. Then onboard external validators safely

---

## 6. Marathon ledger (2026-04-26 → 2026-04-27 ~32h)

- Tokenomics v2 fork activated end-to-end (315M cap, 126M halving, BTC-parity 4-year)
- 4 P0 mainnet stalls recovered via chain.db rsync (h=604547, 633599, 662399, 685708)
- 1 testnet stall + 1 mainnet stall during fix-validation
- 26+ PRs merged (#316 → #357)
- 11 binary releases (v2.1.31 → v2.1.43)
- 3 GitHub Releases tagged + published (v2.1.39, v2.1.40, v2.1.41, v2.1.43)
- Brand split + 35 subdomains + 18 emails
- docs.sentrixchain.com self-hosted live
- Chainlist PR #8266 submitted (waiting upstream)
- Comprehensive runbook `runbooks/jail-divergence-recovery.md`
- Empirical confirmation of asymmetric recording bug from testnet
- This audit doc (production-readiness honest assessment)

---

## 7. Recommendation

**Stop tonight.** Chain healthy. Marathon at 32+ hours. Going further = diminishing returns + risk.

**Next session focus:** P0 item #1 (Consensus-jail Phase A data plumbing) — fresh-brain, multi-day, real fix for production readiness.

**No more mainnet experiments tonight.** Each restart accumulates risk. Trust the recovery procedure if needed but don't induce.
