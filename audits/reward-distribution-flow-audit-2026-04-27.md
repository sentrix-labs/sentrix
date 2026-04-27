# Reward Distribution Flow Audit — 2026-04-27

**Scope:** verify reward flow is deterministic across all block-apply paths
**Triggered by:** Production-readiness audit identified `distribute_reward` is only called from main.rs validator-finalize paths but blocks can also be applied via libp2p sync paths (gossip + catch-up), creating asymmetric application

---

## Reward flow architecture (V4 reward v2, active since h=590100)

### 1. Block production: coinbase tx routes to PROTOCOL_TREASURY

`bin/sentrix/src/main.rs` block-production path:
- Validator produces block with coinbase transaction
- Coinbase pays block_reward (1 SRX) to `PROTOCOL_TREASURY` (0x0000…0002), NOT to producing validator directly
- Treasury accumulates rewards on every block

This is consensus-state mutation: `accounts.transfer(coinbase_source, PROTOCOL_TREASURY, reward, 0)`. Deterministic — every validator applying the same block transitions PROTOCOL_TREASURY balance identically.

✅ **Coinbase-to-treasury: deterministic across all apply paths.**

### 2. Reward distribution: distribute_reward updates accumulators

`bc.stake_registry.distribute_reward(proposer, signers, reward, fee)` is called from:
- main.rs:2105 (self-produced block, BFT-finalize handler)
- main.rs:2510 (peer-produced block, BFT-finalize handler)

**NOT called from:**
- libp2p gossip apply path (libp2p_node.rs:847)
- libp2p direct apply path (libp2p_node.rs:1207)
- libp2p sync catch-up path (libp2p_node.rs:1499)

**What distribute_reward does:**
- Updates `stake_registry.validators[X].pending_rewards` accumulator (per validator's share of reward)
- Updates `stake_registry.delegator_rewards[X]` accumulator (per delegator's share via validator commission)

This is LOCAL STATE on each validator — it's the validator's own accumulator tracking. Critically, **`pending_rewards` and `delegator_rewards` ARE part of stake_registry which IS part of Blockchain consensus state**.

❓ **Question:** is distribute_reward a deterministic state mutation or a local-only bookkeeping?

### 3. Claim path: ClaimRewards drains accumulator → balance

`StakingOp::ClaimRewards` apply (block_executor.rs:848) takes:
- `tx.from_address` = claimer
- Drains `take_delegator_rewards(claimer)` (delegator portion)
- Drains `validators[claimer].pending_rewards` (validator portion)
- `accounts.transfer(PROTOCOL_TREASURY, claimer, total_claim, 0)` — moves treasury → claimer balance

This IS a consensus state mutation (transaction in chain). All validators applying the same ClaimRewards tx transition the same way.

✅ **Claim is deterministic.**

---

## The bug: distribute_reward asymmetric application

Here's the issue:

**Scenario:** Validator B is offline, comes back, syncs blocks N..N+1000 from peers via libp2p.

**Validator A** (online during N..N+1000): finalized blocks via BFT, called `distribute_reward` for each → updated its OWN stake_registry accumulators

**Validator B** (offline → online via sync): applied blocks via `add_block_from_peer` → state changes from coinbase tx happened (treasury balance updated) BUT `distribute_reward` was NOT called → stake_registry accumulators NOT updated

**State diff after sync:**
- A's `stake_registry.validators[X].pending_rewards` = correct (incremented each block)
- B's `stake_registry.validators[X].pending_rewards` = wrong (never incremented for blocks N..N+1000)

When user submits ClaimRewards tx:
- A applies tx → drains A's local accumulator → claimer gets correct amount
- B applies tx → drains B's local (under-accumulated) accumulator → claimer gets less

**This is divergent state mutation.** Different validators would credit different balances for the same ClaimRewards tx, breaking consensus.

❌ **Reward distribution is NOT deterministic across apply paths.**

---

## Severity

**Currently masked because:**
1. Validators rarely catch up via libp2p sync after long downtime (operators do chain.db rsync recovery instead, which copies the canonical's stake_registry state wholesale)
2. ClaimRewards is rare (no auto-claims yet, must be manually submitted)
3. The drift between validators may be small if downtime is short

**Severity rating: HIGH** (latent consensus-divergence bug, would manifest under long-running multi-validator network)

---

## Fix design

Same pattern as PR #356 (asymmetric record_block_signatures fix). Add `distribute_reward` call to libp2p apply paths.

### Approach 1: Inline in libp2p_node.rs (3 sites)

Add after each successful `add_block_from_peer`:

```rust
if let Some(j) = &block.justification {
    let proposer = &block.validator;
    let signers: Vec<(String, u64)> = j.precommits.iter()
        .map(|p| (p.validator.clone(), p.stake_weight))
        .collect();
    let reward = chain.get_block_reward();
    let validator_fee = 0;
    let _ = chain.stake_registry.distribute_reward(proposer, &signers, reward, validator_fee);
}
```

Risk: **HIGHER than PR #356** because this is monetary state mutation, not just LivenessTracker bookkeeping. Mistake here = double-distributed rewards or missed-distributed rewards.

### Approach 2: Move distribute_reward inside add_block_with_source (centralized)

Refactor: instead of caller-driven distribution, make it part of the block-apply pipeline. add_block_impl computes proposer + signers from block.justification + calls distribute_reward internally, ONCE per block-apply.

Then main.rs validator-finalize paths can REMOVE their explicit distribute_reward calls (now handled at apply time).

**Pros:** single source of truth — reward distribution happens exactly once per block-apply, regardless of whether self-produce or peer-receive or libp2p-sync
**Cons:** refactor touches consensus-critical code; needs careful regression tests

### Approach 3: Add deterministic apply-time hook (CLEANEST)

Add `Blockchain::apply_block_post_hook(&mut self, block: &Block)` method that runs deterministic bookkeeping after every successful block apply (regardless of source). Call from add_block_impl after successful Pass-2 apply.

Hook does:
1. record_block_signatures (if block has justification)
2. distribute_reward (if block has justification)
3. epoch_manager.record_block (always)

Caller paths simplify. Remove ad-hoc bookkeeping from main.rs (now centralized).

**Pros:** one chokepoint, deterministic, tested in unit tests
**Cons:** larger refactor; touches multiple call sites

---

## Recommendation

**Phase 1 (quick fix, ~1 day):** Approach 1 — add distribute_reward to 3 libp2p paths. Same pattern as PR #356. Tests for each path. Risk-managed.

**Phase 2 (refactor, ~3-5 days):** Approach 3 — centralize via apply_block_post_hook. Remove duplicate logic in main.rs. Cleaner long-term.

For tonight (autonomous mode): defer fix. The bug is latent (not actively divergent under current operational pattern of chain.db rsync recovery). Phase 1 fix needs:
- Careful design (which call sites change, test coverage)
- Mainnet deploy (halt-all + simultaneous-start for state-mutation change)
- Ideally testnet bake first

Per consensus discipline (fresh-brain review for consensus-touching), do NOT ship in this session.

---

## Investigation queue

- [ ] Confirm distribute_reward is missing from libp2p apply paths empirically
- [ ] Test: stop validator, restart, force libp2p sync, claim rewards, check if amount matches what other validators would credit
- [ ] Implement Approach 1 fix (5-7 hours implementation + testing)
- [ ] Eventually: refactor to Approach 3 (single chokepoint)

Same pattern issue applies to:
- `epoch_manager.record_block(reward)` — only called from main.rs validator-finalize paths, not libp2p apply
- May affect epoch-boundary calculations on validators that catch up via libp2p

This category of bug ("validator-loop-only bookkeeping that should be applied to all blocks regardless of source") is the parent class of the asymmetric recording bug. PR #356 fixed one instance (record_block_signatures). This audit identifies 2 more (distribute_reward, epoch_manager).

**Long-term fix:** Approach 3 (apply_block_post_hook) eliminates the entire bug class structurally.
