# Native-Module State-Root Commitment — Activation Playbook

**Date:** 2026-06-03
**Status:** PLAYBOOK — code merged (#779), fork gates DISABLED, no activation scheduled
**Related:** PR #779 (`feat(state): commit native module state into trie/state_root`), the SIP-6 `STATE_IN_TRIE` activation precedent

---

## What this covers

PR #779 added consensus commitment of native-module state into the trie /
`state_root`, behind a fork gate that is **off by default on every network**:

- `NATIVE_STATE_IN_TRIE_HEIGHT_DEFAULT = u64::MAX` (mainnet **and** testnet)
- the NFT rail's own gate `NFT_TOKENOP_HEIGHT_DEFAULT = u64::MAX` is unrelated and stays off

While disabled, `state_root` is **bit-identical** to pre-#779 behaviour — the
commitment code captures `None` and inserts nothing. This document is the
procedure for *eventually* turning it on, testnet-first, without splitting the
chain. It is a plan, not an instruction to act: **do not activate anything from
this document without an explicit, separately-approved activation window.**

This is consensus-sensitive. Pinning the activation height on a subset of
validators, or pinning different heights, **will** fork the network.

---

## Background: what gets committed

Post-activation, `update_trie_for_block` (Phase 2f) writes two fixed trie keys
every block:

| Key domain | Value |
|---|---|
| `sentrix/v1/native_src20_registry` | `ContractRegistry::canonical_hash()` |
| `sentrix/v1/native_nft_registry` | `NftRegistry::canonical_hash()` |

`canonical_hash()` is a sorted, `HashMap`-order-independent SHA-256 over the
full registry (balances, supply, allowances for SRC-20; collections, tokens,
owners, approvals for NFT). It is a **commitment**, not a full serialization:
the trie stores the hash, not the registry. Consequence —

- divergent native state is **detected** (a diverging validator produces a
  different `state_root` and its block is rejected by the BFT majority);
- it is **not auto-healed** from the trie (you can't reconstruct a registry
  from its hash) — recovery is by resync/restore from a healthy peer, exactly
  like account-state divergence today.

This is why the pre-flight below insists every validator already agrees on the
native canonical hashes *before* the gate opens.

---

## Sequence (do not reorder)

1. Activate `NATIVE_STATE_IN_TRIE_HEIGHT` on **testnet** first.
2. Soak (see monitoring checklist). Native NFT stays disabled throughout.
3. Only after a clean testnet soak, consider a **mainnet** activation window
   (separate approval, separate runbook entry).
4. Only *much* later, and as its own separate exercise, consider
   `NFT_TOKENOP_HEIGHT`. **Never** flip both gates in the same window — keep one
   variable per activation so a `state_root` mismatch is unambiguous.

---

## Pre-flight checks (all must pass before pinning a height)

Run these across **every** validator in the active set. Any mismatch is a
stop-the-line condition — reconcile first, do not proceed.

1. **Same binary.** Every validator runs the identical build (verify the
   binary hash + a feature marker, not just the version string).
2. **Same height / tip.** Every node reports the same chain height and the same
   tip block hash, and has been stable there for a sustained window.
3. **Same `state_root`.** Every node reports the same `state_root` at the
   common tip. (Pre-activation this already excludes native state, so agreement
   here is necessary but not sufficient — continue to 4 and 5.)
4. **Same SRC-20 `ContractRegistry` canonical hash** across all nodes.
5. **Same NFT `NftRegistry` canonical hash** across all nodes (expected empty
   while the NFT rail is disabled, but verify rather than assume).
6. **No SRC-20 drift.** Items 4 + 5 prove this directly; if they disagree, a
   validator's contract state has drifted and activation would surface it as a
   chain halt at the activation block. Reconcile (resync the drifted node from a
   canonical peer) until 4 + 5 match everywhere.
7. **Backup / snapshot.** Take a cold backup of each validator's chain DB
   *before* the activation window (halt the source briefly + copy; never copy a
   live DB). Keep the backups until the soak is declared clean.

### How to compare native hashes across nodes (no new tooling)

The chain already exposes a deterministic fingerprint that, since #776, folds
in both native registries. Enable it on each node and compare the emitted
lines:

```
SENTRIX_STATE_FINGERPRINT=1   # opt-in; emits a [STATE-FP] line per block
```

`compute_state_fingerprint` hashes accounts + `total_minted` + the SRC-20 and
NFT `canonical_hash()`. At a common height, the `[STATE-FP] fp=…` value must be
identical across all validators. A divergent `fp` at the same height with
identical accounts/`total_minted` points at native-state drift — resolve it
before activation. (This env var is debug-only and changes no consensus
behaviour; it is safe to toggle.)

---

## Activation procedure (testnet)

Modelled on the SIP-6 `STATE_IN_TRIE` activation: a coordinated halt + simul-
start so every validator opens the gate at the same height with identical
pre-fork native state.

1. Complete every pre-flight check above. Do not start otherwise.
2. Choose an activation height comfortably **ahead** of the current tip — far
   enough that every validator is restarted and back in sync before the chain
   reaches it.
3. **Halt all** validators.
4. Take/refresh the cold backup (pre-flight #7) while halted.
5. On **every** validator, set the **same** activation height:

   ```
   # config/env on each validator — identical value everywhere
   NATIVE_STATE_IN_TRIE_HEIGHT=<HEIGHT>
   ```

   Note: env changes are only picked up on a full restart. If validators run
   under a container stack, recreate (not just restart) so the new env is read.
6. **Simul-start** all validators.
7. Confirm every node is back at the common tip and producing/validating
   blocks *before* the chain reaches `<HEIGHT>`.
8. Watch the activation block closely (next section).

If even one validator will not have the same `<HEIGHT>` set at the same time,
**abort** — a partial pin splits the chain at the activation block.

---

## Monitoring checklist (during + after the soak)

At and after `<HEIGHT>`:

- **Activation block applies cleanly** on all validators (no rejected-block /
  `state_root` mismatch errors at `<HEIGHT>`).
- **Block production** continues at the normal cadence (no BFT stall).
- **`state_root` agreement** across all validators at each height past
  activation.
- **`[STATE-FP]` agreement** across nodes (the fingerprint and the committed
  `state_root` now both reflect native state).
- **Validator agreement / no cascade jailing** — watch for any validator
  falling out of the active set.
- **Peer sync health** — late/restarting nodes still catch up and agree.
- **No missed-block spike.**
- **Native SRC-20 operations** (deploy/transfer/mint/burn/approve) still apply
  and now move `state_root` as expected.
- **Native NFT remains disabled** — `NFT_TOKENOP_HEIGHT` untouched; the NFT
  registry commits as an empty-but-stable hash.

Soak for a sustained, boring window before declaring success. "Boring" is the
goal — no divergence alarms, no recovery actions.

---

## Rollback / divergence handling

If a validator diverges at or after activation (different `state_root`, rejected
blocks, or it drops from the set):

1. **Do not rsync a live DB** and do not blindly restart-loop — that perpetuates
   drift. First identify the **first divergent block** (compare `[STATE-FP]` /
   `state_root` across nodes by height) to confirm it's native-state related.
2. **Single diverged validator:** halt it, restore its chain DB from a
   canonical peer (a node that stayed in agreement), restart, let it resync.
   The commitment detects divergence but cannot rebuild the registry from the
   hash — recovery is restore-from-canonical, not trie reconstruction.
3. **Widespread divergence at the activation block** = the pre-flight missed
   native drift. Roll back the activation: halt all, restore the pre-activation
   backup (pre-flight #7), **unset** `NATIVE_STATE_IN_TRIE_HEIGHT` (back to
   `u64::MAX`), simul-start. The chain returns to pre-#779 behaviour (state_root
   excludes native state). Then re-run the full pre-flight, fix the drift, and
   reschedule.
4. Capture what happened in an `audits/` RCA before retrying.

Because the gate is the only thing that changed, unsetting it is a complete and
deterministic rollback — there is no schema migration to undo.

---

## Explicit warnings

- **Do not activate on mainnet before a clean testnet soak.** Mainnet carries
  live SRC-20 contract state; activation makes that state consensus-committed,
  so any latent drift becomes a halt at the activation block.
- **Do not enable `NFT_TOKENOP_HEIGHT` together with the native state-trie
  activation.** One consensus variable per window. Activate + soak the native
  state commitment first; the NFT TokenOp rail is a separate, later decision.
- **Do not pin different heights on different validators.** Same `<HEIGHT>`
  everywhere, or abort.
- **Do not treat this document as an instruction to act.** It is the procedure
  to follow *if and when* an activation is separately approved and scheduled.
