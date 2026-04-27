# Jail Cascade Root Cause Analysis

**Date:** 2026-04-27
**Author:** ops investigation (post 2026-04-26 marathon, 2 jail-cascade incidents)
**Related:** `audits/voyager-design.md`, `runbooks/jail-divergence-recovery.md`
**Status:** Investigation findings; informs upcoming fixes #2, #3, #4

---

## Problem statement

Mainnet experienced 2 jail-cascade stalls on 2026-04-26 within hours:
- Evening h=633599 — triggered by rolling restart for env-var rollout
- Night h=662399 — triggered by NORMAL operation (no operator action)

In both cases, **one validator** had divergent stake_registry view (saw `0x87c9...` jailed, while the other 3 saw it active). P1 BFT safety gate fired on the divergent validator (`active_set < minimum`), refusing BFT participation. Other validators reached nil-majority (no proposal converging), chain stalled.

Recovery via chain.db rsync from canonical (Treasury) takes ~5-10 min and is well-tested.

But **why does the divergence form in the first place?**

---

## Slashing architecture summary

`crates/sentrix-staking/src/slashing.rs`:

- `LivenessTracker` — per-validator sliding window of `(height, signed)` records, max `LIVENESS_WINDOW = 14_400` blocks (~4 hours at 1s blocks)
- `is_downtime(validator)` returns `true` when:
  1. window is full (≥14400 records), AND
  2. `signed_count < MIN_SIGNED_PER_WINDOW (4_320)` (= <30% signed)
- `check_liveness()` runs at **epoch boundaries** only — iterates active_set, calls `is_downtime`, if true: `slash + jail + reset window`
- `record_block_signatures(active_set, signers, height)` records each validator's signed/missed status for ONE block

The DATA layer is **deterministic in principle**: given identical `(active_set, signers, height)` calls across all validators, all `LivenessTracker` instances should be byte-identical, producing identical jail decisions.

**The bug is in the RECORDING PATH coverage, not the data layer itself.**

---

## Root cause: asymmetric `record_block_signatures` coverage

`record_block_signatures` is called from exactly **two sites** in `bin/sentrix/src/main.rs`:

1. **Self-produced finalize** (line ~2045) — when this validator proposed and BFT finalized
2. **Peer-finalize via BFT** (line ~2454) — when this validator received another's block + finalized via BFT round

It is **NOT called from**:
- `libp2p sync` BlocksResponse handler (when catching up via `GetBlocks` requests)
- chain.db rsync recovery (chain.db is wholesale replaced; LivenessTracker IS persisted via `#[serde(default)]` on the Blockchain struct, but we don't replay-record on recovery)
- Block replay during startup (LivenessTracker is restored from MDBX state blob, not rebuilt)

### What this means in practice

**Scenario: Validator B goes offline for 1 hour.**

| Step | Validator A (online) | Validator B (offline → online) |
|---|---|---|
| Block N | self-produces or peer-finalizes → `record_block_signatures(active, signers, N)` | (offline, no record) |
| Block N+1...N+3600 | continues recording | (offline) |
| Block N+3601 | continues | starts up, loads chain.db, **LivenessTracker has stale state from before downtime** |
| Block N+3602 | self-produces or peer-finalizes → record | (gap in B's records — never recorded N..N+3600) |

When epoch boundary fires:
- A's `LivenessTracker` for `0xValB`: 14400 records, all signed-or-missed properly
- B's `LivenessTracker` for `0xValA`: gap of 3600 entries between pre-downtime and post-recovery records

The gap matters because:
- `LivenessTracker.records` is a `Vec<LivenessRecord>` per validator
- Gap fills in as new records arrive
- After recovery, B's records list grows again → eventually has 14400 entries — but those 14400 represent a different time window than A's

**The two LivenessTrackers compute different `is_downtime` results** at the same epoch boundary because they're looking at different 14400-block windows.

### Verification path

To prove this hypothesis empirically, observability metric (fix #2) would log per-validator-per-block "expected signers vs actual signers" — divergence in this list across validators = smoking gun.

---

## Root cause: jail decision is locally-computed, not consensus-coordinated

Even if `LivenessTracker` were perfectly synchronized via observability + replay-record patches, **the jail decision itself runs locally** at epoch boundaries:

```rust
// In main.rs validator loop, called at epoch boundary on EACH validator independently:
let slashed = slashing.check_liveness(&mut bc.stake_registry, &active_set, height);
```

Each validator independently:
1. Checks its own LivenessTracker for downtime
2. If found, calls `stake_registry.slash + jail` directly on its own state

This is a **state mutation without consensus**. There's no BFT vote on "should we jail validator X". Each validator decides locally based on its own LivenessTracker.

**Even with perfectly-synchronized LivenessTrackers**, edge cases would still cause divergence:
- Race condition at epoch boundary (different validators process the boundary block at slightly different times)
- One validator panics/crashes mid-`check_liveness` (slashes some validators, jails some, then restarts and replays — inconsistent partial state)
- Genesis edge cases (early validators with incomplete window)

The CORRECT model: jail decisions should be a **consensus-applied transaction**, like any other state change. See fix #4.

---

## P1 BFT safety gate: a related issue

Once jail divergence forms, the P1 BFT safety gate AMPLIFIES it:

```rust
// validator loop refuses BFT when:
if active_set.len() < MIN_ACTIVE_FOR_BFT {  // = 4 for our 4-validator network
    skip BFT round  // chain stalls
}
```

For a 4-validator network, gate threshold = 4 (need ALL active). One jail = stall.

This was **conservative-correct** for cold-start protection (don't enter BFT until all validators online). But during steady-state operation, when 1 validator gets locally-jailed, the gate prevents the other 3 from making progress — even though they technically have 3/4 supermajority.

Per BFT theory, supermajority threshold for finality is `⌈2/3⌉ + 1 = 3` for a 4-validator network. The gate's `≥4` requirement is stricter than what BFT actually needs.

Relaxing the gate (fix #3) doesn't fix the underlying jail divergence, but provides **liveness margin** while we work on the deeper fixes.

---

## Investigation queue (now informs PRs #2, #3, #4)

### Short-term (operability / observability)

- **#2 Observability metric** — Add per-validator tracing log + Prometheus counter for signed/missed per epoch. Detect divergence early before threshold crosses, allows operator to halt-all-rsync preemptively.
- **#3 Relax P1 BFT safety gate** — `≥active_set.len()` → `≥⌈2/3⌉ × active_set.len() + 1` (= matches BFT supermajority). Liveness margin for transient jail divergence. Defer until 5+ validators is a reasonable position too.

### Medium-term (data-layer fix)

- **Replay-record on libp2p sync** — call `record_block_signatures` from BlocksResponse apply path with the block's justification.precommits (if Voyager-active height). Closes the gap-during-downtime issue.
- **Replay-record on startup** — when loading chain.db at boot, walk the last 14400 finalized blocks + replay-record liveness. One-shot reconstruction of LivenessTracker.

### Long-term (model fix)

- **#4 Consensus-computed jail decisions** — emit jail-trigger as a BFT-finalized transaction, not as local decision at epoch boundary. Each validator votes "I observed X miss Y blocks" via the precommit chain. Aggregator (BFT engine) applies jail only when 2/3+1 supermajority agrees. This is the **right** model — eliminates divergence by design.

---

## Operational mitigation (current procedure)

Until fixes ship, when jail divergence is detected (per-validator `active_count` differs):

1. Use `runbooks/jail-divergence-recovery.md` — minimal-scope chain.db rsync from canonical (Treasury) to divergent validator
2. Recovery time ~5-10 min including libp2p mesh re-convergence
3. Pattern proven 2x on 2026-04-26 (evening h=633599 + night h=662399)

This is acceptable for a 4-validator Foundation-operated network. Once external validators onboard, the determinism+consensus-compute fix (#4) becomes critical — can't ask third-party operators to chain.db rsync each other.
