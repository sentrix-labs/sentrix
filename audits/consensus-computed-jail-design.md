# Consensus-Computed Jail Decisions — Design Doc

**Date:** 2026-04-27
**Status:** DRAFT — design phase, no implementation
**Estimated effort:** 4-6 weeks (design + implement + test + audit + fork-gated activation)
**Related:** `audits/jail-cascade-root-cause-analysis.md`, fix #1, #2, #3

---

## Problem

Current jail mechanism (`SlashingEngine::check_liveness` at epoch boundaries) is **locally-computed**: each validator independently decides jail based on its own `LivenessTracker`. Multi-source divergence (asymmetric recording paths, race conditions, replay gaps) causes per-validator stake_registry to disagree, triggering BFT stalls.

**See `audits/jail-cascade-root-cause-analysis.md` for full RCA.**

Even with perfect data-layer synchronization (fix #2 + replay-record patches), the model is still wrong — state mutations on consensus-relevant data should go through BFT, not local opinion.

---

## Goal

Replace local-jail with **consensus-computed jail**: validator-set agrees on missed-block tally via BFT precommit chain, jail-trigger emitted as on-chain transaction, applied uniformly across all validators.

---

## High-level design

### Two-phase model

**Phase 1: per-block participation observation**

Each finalized block contains a `participation_record` field: the BFT justification's precommit list. This is already on-chain (see `Block.justification.precommits`).

No protocol change needed for Phase 1 — the data already exists in finalized blocks.

**Phase 2: consensus-computed jail decision**

At epoch boundary, the proposer of the boundary block includes a special `JailTransaction` in the block's transaction list. The transaction contains:

```rust
pub struct JailTransaction {
    pub epoch: u64,
    pub validators_to_jail: Vec<JailEvidence>,
    pub proposer: String,
    pub signature: Signature,
}

pub struct JailEvidence {
    pub validator: String,
    pub epoch_start_block: u64,
    pub epoch_end_block: u64,
    pub signed_count: u64,
    pub missed_count: u64,
    pub justification_hashes: Vec<String>,  // hashes of blocks where missed
}
```

The proposer scans the last `LIVENESS_WINDOW` blocks' `justification.precommits`, computes per-validator signed/missed count, and includes the jail evidence in the boundary block.

Other validators VERIFY the JailTransaction during Pass-1 validation:
- Walk same blocks
- Recompute signed/missed for each cited validator
- Reject the block if cited evidence doesn't match their independent computation

If block accepted (BFT supermajority signs justification), JailTransaction applies → validators jailed atomically across all nodes. No divergence possible because all validators verified same evidence before signing.

### Edge cases

- **Proposer fails to include JailTransaction when needed** — peers can detect (count mismatch) → vote nil → next round proposer includes correct evidence
- **Proposer includes false JailTransaction** — peers reject via Pass-1 validation → block fails → next round
- **Network split during epoch transition** — usual BFT liveness fallback (skip rounds, eventual finality)

---

## Activation strategy: fork-gated

Roll out as new fork: `JAIL_CONSENSUS_HEIGHT_DEFAULT = u64::MAX`, env-overridable per `TOKENOMICS_V2_HEIGHT` pattern.

Pre-fork: existing local-jail behavior (unchanged)
Post-fork: new consensus-jail behavior

Operators set `JAIL_CONSENSUS_HEIGHT=<future_height>` env var on each validator. Chain reaches fork height → new path activates.

Old path (`SlashingEngine::check_liveness`) becomes inert post-fork (gated by `is_jail_consensus_height(h)` check).

Fallback: if fork mechanism has bug, env var lets us roll back per-validator (set far-future height) without rewinding chain.

---

## Implementation phases

### Phase A: Wire data plumbing (no behavior change)

1. Add `audits/consensus-computed-jail-design.md` (this doc) — DONE
2. Add `JailTransaction` struct + serialization in `crates/sentrix-primitives`
3. Add Pass-1 validation for `JailTransaction` (no-op if absent — matches current behavior)
4. Add `compute_jail_evidence_at_epoch_boundary(blockchain, height)` function in `sentrix-staking`
5. Tests for evidence computation + serialization roundtrip
6. PR — non-fork-activating, doesn't change runtime behavior

### Phase B: Fork gate + dispatch

1. Add `JAIL_CONSENSUS_HEIGHT` const + env var pattern (mirror `TOKENOMICS_V2_HEIGHT`)
2. Add `Blockchain::is_jail_consensus_height(h)` dispatch helper
3. Wire fork-aware dispatch:
   - Pre-fork: `SlashingEngine::check_liveness` (current behavior)
   - Post-fork: `JailTransaction` evidence path
4. Pre-fork integration test: behavior unchanged
5. Post-fork integration test: jail via consensus-applied transaction
6. PR — fork-activating but inert (env var defaults disable)

### Phase C: Testnet bake

1. Set `JAIL_CONSENSUS_HEIGHT` on all 4 testnet docker validators
2. Run for 7+ days under varied load (induce missed blocks, restart cascades, etc.)
3. Verify jail decisions converge (no divergence) through normal operation + adversarial testing
4. Document any edge cases discovered

### Phase D: Mainnet activation

1. Schedule activation height (current_height + ~1 week buffer)
2. Set `JAIL_CONSENSUS_HEIGHT` env on all 4 mainnet validators (halt-all + simultaneous-start per discipline)
3. Chain crosses fork height — new path active
4. Monitor for divergence (observability metric from fix #2)
5. Old `SlashingEngine::check_liveness` retained but gated by `is_jail_consensus_height` — defensive against fork-gate bugs

---

## Risks + mitigations

| Risk | Mitigation |
|---|---|
| Determinism bug in evidence computation | Phase A includes property tests; testnet bake exercises real network conditions |
| Proposer can omit jail evidence | Peer validation rejects boundary block on count mismatch → next-round proposer fixes |
| Backward-compat with chain.db | `JailTransaction` is new tx type; pre-fork chain.db has none; restore-then-fork is safe |
| Performance regression at epoch boundary | Evidence computation is O(LIVENESS_WINDOW × validators) = O(14_400 × ~10) = 144k ops, fast |
| Coordinated rollback if fork has bug | Env var per-validator allows individual rollback without consensus break |

---

## Effort estimate

- Design (this doc): 1 day — DONE
- Phase A (data plumbing PR): 5-7 days
- Phase B (fork gate PR): 3-5 days
- Phase C (testnet bake): 7+ days calendar (1-2 days work)
- Phase D (mainnet activation): 1 day operator action
- **Total calendar: 4-6 weeks**

Per consensus discipline (operator runbook): each phase = separate PR with regression test, fresh-brain review, no self-merge.

---

## Out of scope (this design)

- Slashing-amount changes — keep `DOWNTIME_SLASH_BP = 100` (1% slash) unchanged
- Tombstone (permanent ban) logic — keep current "double-sign = tombstone" path unchanged
- Pre-Voyager validators — not relevant; Voyager is active on mainnet since h=579047
- Self-unjail mechanism — separate concern, defer

---

## Open questions

1. **Should JailTransaction be applied automatically by chain validation, or via explicit on-chain admin tx?** Auto via Pass-1 = simpler. Admin tx = more flexible. **Decision: auto via Pass-1** — matches consensus-mutation pattern for staking ops.

2. **Should evidence include hash list of missed-block justifications, or just the count?** Hash list = prove-each-miss. Count = trust-the-proposer. **Decision: include hash list initially**, allows peers to selective-verify any subset; if perf becomes issue, can drop later.

3. **What happens if 2 validators are simultaneously eligible for jail at the same epoch?** Both included in same JailTransaction (Vec<JailEvidence>). Atomic apply.

4. **Does this affect existing jail/unjail RPC + CLI?** No — those still mutate stake_registry directly, just the AUTOMATIC jail path changes. Manual unjail still works as before.

---

## Next steps

1. ☐ Operator review this design doc + decide go/no-go
2. ☐ If go: schedule Phase A start (separate fresh-brain session)
3. ☐ Open Phase A tracking issue on `sentrix-labs/sentrix`
4. ☐ Begin implementation (5-7 days for Phase A, fresh-brain session)
