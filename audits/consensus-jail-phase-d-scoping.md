# Consensus-Jail Phase D Scoping — 2026-04-27

**Status:** DESIGN ONLY. Implementation requires fresh-brain consensus engineering.
**Predecessor PRs:** #359 (Phase A) + #365 (Phase B) + #366 (Phase C) merged.
**Activation gate:** `JAIL_CONSENSUS_HEIGHT` env var (default `u64::MAX` = disabled).

---

## What Phase D needs to deliver

Phase A/B/C laid the foundation: data types (Phase A), helper function (Phase B), dispatch verification (Phase C). But the dispatch never fires because no validator EMITS the `JailEvidenceBundle` op. Phase D wires the proposer-side emission.

**Goal:** at epoch boundaries (post-fork), the validator who proposes the block AUTOMATICALLY includes a `JailEvidenceBundle` transaction in its block, computed from its own LivenessTracker state.

---

## Architectural questions blocking implementation

### Q1: How is the JailEvidenceBundle tx signed?

**Background:** Sentrix transactions require ECDSA signature with `tx.from_address` matching the recovered public key. System-emitted txs (coinbase, fee burn, etc.) bypass this via special-case handling.

**Options:**

**A) Sign with proposer's validator wallet key.**
- `tx.from_address = proposer_address`
- `tx.signature = sign(payload, validator_secret_key)`
- Pros: clean — fits existing tx flow
- Cons: requires plumbing validator's secret key into block_producer (currently only address is passed). New attack surface — validator's wallet key now used for additional purpose.

**B) Treat as system tx (no signature, special validation).**
- `tx.from_address = PROTOCOL_TREASURY` (= `0x0000…0002`)
- `tx.signature = ""` (empty)
- Validation: special-case in tx-verify path: if from_address == PROTOCOL_TREASURY AND data is StakingOp::JailEvidenceBundle AND block.validator == proposer_who_emitted: skip signature check.
- Pros: no key plumbing needed
- Cons: new special-case attack surface — needs careful review

**C) Auth via BFT justification (block-level, not tx-level).**
- JailEvidenceBundle is included in block but not as Transaction
- New block field: `block.system_ops: Vec<SystemOp>`
- Validation tied to BFT justification (= block was finalized by 2/3+ validators, all of whom verified the system_ops match their local view)
- Pros: cleanest semantically — system ops are byproducts of consensus, not user actions
- Cons: largest refactor — adds new block field, changes block hash computation, requires hard fork beyond just env var

**Recommendation: Option B** for Phase D. Smallest scope. Special-case JailEvidenceBundle as a system tx with PROTOCOL_TREASURY sender + empty signature, validated by Pass-1 confirming the block's validator computed the same evidence locally.

### Q2: How does proposer compute evidence?

**Phase B answer:** `SlashingEngine::compute_jail_evidence(active_set)` returns deterministic Vec<JailEvidence>.

**Caveat:** for this to be deterministic across validators, ALL validators must have IDENTICAL LivenessTracker state at the moment of evidence computation. PR #356 + #362 fixed the asymmetric apply paths so libp2p-synced validators now record consistently. BUT: LivenessTracker self-corrects over LIVENESS_WINDOW = 14400 blocks (~4h). Until convergence, proposer's evidence may differ from peers' verification.

**Mitigation:** activate JAIL_CONSENSUS_HEIGHT only AFTER 4h+ of clean operation post-PR-#362 deploy (= LivenessTracker has converged). Otherwise: cited validators in proposer's evidence may not match peers' local view → block rejected → chain stall at epoch boundary.

### Q3: What if no validators meet jail threshold at epoch boundary?

**Behavior:** `compute_jail_evidence` returns empty Vec. Should proposer:

**A) Skip emission entirely** (no JailEvidenceBundle in block). Block has 0 system ops.
**B) Emit empty bundle** (`JailEvidenceBundle { evidence: vec![] }`). Block confirms "no jail this epoch".

**Recommendation: A.** Skipping emission saves block space + simplifies dispatch (no need to verify "this empty bundle was correctly empty").

### Q4: Backward-compatibility with chain history

Pre-fork blocks have no JailEvidenceBundle. Post-fork blocks at epoch boundary will include them. Mixed-history validators (replaying from genesis through fork height) must:
- Reject JailEvidenceBundle txs in pre-fork blocks (Phase C does this)
- Accept JailEvidenceBundle txs in post-fork blocks at epoch boundaries

Pass-1 validation should ALSO check: if post-fork AND height is epoch boundary AND active validators have downtime AND block doesn't contain JailEvidenceBundle → reject (proposer omitted required system op).

This adds a "required-presence" check. Phase D scope.

---

## Implementation steps (Phase D, ~5-7 days fresh-brain)

### Step 1: Resolve Q1 (signing)
- Pick option (recommend B)
- Document decision in `audits/consensus-jail-phase-d-implementation-plan.md`

### Step 2: Add helper `Blockchain::build_jail_evidence_system_tx`
- Returns `Option<Transaction>`: `None` if no evidence (Q3-A) or not at epoch boundary or pre-fork
- Otherwise: builds `Transaction { from=PROTOCOL_TREASURY, to=PROTOCOL_TREASURY, amount=0, data=encoded JailEvidenceBundle, signature="" }`
- Tests for all 4 paths (pre-fork, post-fork-no-evidence, post-fork-with-evidence, non-boundary)

### Step 3: Wire into block_producer
- In `build_block` post-fork: if epoch boundary, prepend system tx (after coinbase, before mempool)
- Tests for proposer-side emission

### Step 4: Wire into Pass-1 validation
- Add system tx verification: if PROTOCOL_TREASURY sender + JailEvidenceBundle data, skip signature check
- Add "required-presence" check at epoch boundaries (Q4)
- Tests for both verification paths

### Step 5: Integration test (CRITICAL)
- 4-validator in-process test
- Stop 1 validator for 14400 blocks (full window) — induces downtime
- At next epoch boundary post-fork, verify:
  - Proposer's block contains JailEvidenceBundle for the jailed validator
  - Other validators verify + apply jail
  - stake_registry.is_jailed = true on all 4 validators
  - Chain advances past epoch boundary

### Step 6: Testnet bake (24-48h)
- Deploy Phase D binary to testnet docker validators with `JAIL_CONSENSUS_HEIGHT=<low>`
- Run for 24-48h, induce downtime, verify consensus-applied jail across fleet
- Compare to legacy behavior (which would have caused jail-cascade stall)

### Step 7: Mainnet activation
- After testnet bake confirms stability
- Set `JAIL_CONSENSUS_HEIGHT=<future_height>` on all 4 mainnet validator env files
- Halt-all + simultaneous-start (per `feedback_mainnet_restart_cascade_jailing` rule)
- Chain crosses fork — consensus-jail active
- Monitor observability metric (PR #350) — should show convergent counts post-fork

---

## Why this can't ship autonomously tonight

1. **Q1 architectural decision** requires operator input on system-tx auth model (Option A vs B vs C)
2. **Step 5 integration test** requires multi-validator in-process simulation infrastructure (not currently in test suite)
3. **Step 6 testnet bake** is 24-48h calendar, can't be compressed
4. **Step 7 mainnet activation** is operator-triggered halt-all event

**Conservative effort estimate:** 5-7 days fresh-brain implementation + 24-48h testnet bake + ~30min mainnet activation = ~1-2 weeks calendar.

**Aggressive estimate (skip testnet bake per operator authorization):** 3-5 days implementation + ~30min mainnet activation = ~1 week.

---

## Honest closing statement

After 35+ hour autonomous marathon shipping 19 PRs, Phase A+B+C of consensus-jail is now in main + tested + clippy clean. Phase D is the activation step — requires architectural decision (Q1) + multi-day implementation + testnet validation.

This is the appropriate boundary for autonomous-mode work. Pushing into Phase D without fresh-brain risks shipping a consensus break that activates only after operator sets `JAIL_CONSENSUS_HEIGHT` — by which time the bug is unfixable without a hard rollback.

**Recommendation:** Stop. Operator decides on Q1, fresh-brain session implements Phase D, mainnet activation = next week.
