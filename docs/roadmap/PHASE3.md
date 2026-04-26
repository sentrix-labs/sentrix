# Frontier — Parallel Execution + Ecosystem (Planned, in early implementation)

> **Status:** Phase F-1 type-scaffold and Phase F-2 shadow-mode wiring landed in `main`. Phases F-3 → F-10 (real parallel apply + testnet bake + mainnet activation) are calendar work, ~6-8 weeks. Mainnet hard-fork required at activation.

Frontier extends Voyager with three goals:

1. **Parallel transaction execution** — apply non-conflicting txs in a block concurrently rather than strictly sequentially
2. **Sub-1s block time** — tighter consensus + faster apply enables shorter rounds
3. **Ecosystem expansion** — third-party validators onboard, dApps deploy via the mature EVM surface, real-user scaling

## Phase Plan

| Phase | What | Status |
|---|---|---|
| F-1 | Type-system scaffold (`AccountKey`, `TxAccess`, `Batch`, `derive_access`, `build_batches` stubs) | ✅ Landed (PR #305) |
| F-2 | Shadow-mode wiring — `SENTRIX_FRONTIER_F2_SHADOW=1` env-gated `build_batches` observer in `apply_block_pass2`, read-only | ✅ Landed (PR #318) |
| F-3 | Real parallel-apply replacing the shadow log: actual batch apply + conflict detection + abort/retry path | 🟡 Pending |
| F-4 | Conflict-graph builder — derives read/write access per tx from EVM trace; builds dependency graph | 🟡 Pending |
| F-5 | Determinism property tests promoted from `#[ignore]` to gating (`parallel_apply_matches_sequential_apply`, `build_batches_is_deterministic_across_1000_runs`) | 🟡 Pending |
| F-6 | Mainnet shadow-mode comparison — apply both sequential + parallel, log divergence, prove byte-equivalence over weeks | 🟡 Pending |
| F-7 | Hard-fork height schedule + activation runbook | 🟡 Pending |
| F-8 | Sub-1s block time tuning (`BLOCK_TIME_SECS = 0.5? 0.25?` — depends on parallel apply latency under load) | 🟡 Pending |
| F-9 | Testnet bake (~2 weeks under load) | 🟡 Pending |
| F-10 | Mainnet activation | 🟡 Pending |

## Why This Order

Each phase pins a different invariant:

- F-1 pins the **type contract** (callers can rely on `Batch::tx_indices` etc) without changing runtime
- F-2 pins the **plumbing** — `apply_block_pass2` calls into the F-1 module without trusting its output
- F-3 pins the **semantic correctness** — parallel apply produces the same state-root as sequential apply
- F-4 pins the **conflict model** — accurate read/write set extraction is a security property (mis-detection = state corruption)
- F-5 pins the **non-flakiness** — deterministic batching across runs is a consensus property
- F-6 pins the **production correctness** — months of mainnet shadow run with zero divergence before activation
- F-7-F-10 are activation mechanics

## Risk Profile

Frontier is the highest-risk phase yet because parallel apply touches consensus state directly. Pioneer/Voyager were execution-engine swaps with clean before/after invariants; Frontier introduces concurrency into the apply path itself. Mistakes here = state divergence (= chain split = manual recovery).

The phase plan above is built around minimizing this risk via shadow-mode comparison: by F-6, the parallel path runs against every block on mainnet, but the sequential path is still the canonical state-root source. Only after weeks of zero-divergence shadow do we flip the canonical pointer.

## What Frontier is NOT

- **Not a sharding upgrade.** Throughput improvement comes from parallel apply within blocks, not from horizontal partitioning. Sharding would be a separate Odyssey-class change.
- **Not a chain reset.** Hard-fork height activates new rules; existing chain.db carries forward.
- **Not a custom VM.** EVM via revm continues. Parallel execution wraps revm calls; doesn't replace them.

## Carryover (post-Frontier, future)

- Cross-chain bridges (target: Odyssey)
- Light clients (target: Odyssey)
- Sharding if throughput demand justifies (target: Odyssey, evidence-driven decision)

## What's Next After Frontier: Odyssey

See [internal roadmap]. Odyssey is the long-horizon "fully public chain with mature ecosystem" phase. Cross-chain interop, light clients, possibly sharding, hardware wallet integrations, on-chain governance. No firm timeline — driven by ecosystem maturity rather than calendar.
