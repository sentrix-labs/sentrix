//! Frontier-fork parallel apply determinism contract.
//!
//! This test is the SINGLE merge gate for any future PR that enables
//! the parallel-apply path. It asserts:
//!
//!     apply_sequential(block).state_root == apply_parallel(block).state_root
//!
//! for every block produced by the random-block generator. 10K random
//! blocks per CI invocation, zero failures tolerated.
//!
//! **Why ignored today:** the parallel apply path doesn't exist yet.
//! The current `parallel/` module is a type-system scaffold that
//! returns sequential-equivalent batches. Running the test against
//! the scaffold would pass trivially (both paths == sequential), so
//! it would not catch determinism regressions.
//!
//! **When to enable:** Phase F-5 of `frontier-mainnet-phase-implementation-plan.md`,
//! when `Blockchain::apply_block_pass2_parallel` lands behind the
//! `PARALLEL_APPLY_FORK_HEIGHT` flag. The test runs against EVERY pre-
//! mainnet PR for 1 week minimum before any operator considers
//! activating the fork.
//!
//! **Anti-goals:**
//! - Do NOT remove the `#[ignore]` until the parallel path is real
//! - Do NOT weaken the equivalence assertion (e.g. accept "states
//!   differ but both are valid" — they're not, mainnet diverges)
//! - Do NOT skip blocks with EVM txs (EVM falls to pessimistic batch,
//!   still must produce identical state_root)
//!
//! Reference: `audits/frontier-mainnet-phase-implementation-plan.md` §F-2,
//! `audits/parallel-tx-execution-design.md` §Test strategy.

#[test]
#[ignore = "Frontier parallel-apply path not yet implemented (Phase F-5)"]
fn parallel_apply_matches_sequential_apply() {
    // Placeholder. The real implementation will:
    //
    //   use proptest::prelude::*;
    //   proptest!(|(block in arbitrary_block(MAX_TXS = 1000))| {
    //       let mut bc_seq = setup_chain();
    //       let mut bc_par = setup_chain();
    //
    //       bc_seq.apply_block_pass2_sequential(block.clone()).unwrap();
    //       bc_par.apply_block_pass2_parallel(block).unwrap();
    //
    //       prop_assert_eq!(bc_seq.state_root(), bc_par.state_root());
    //       prop_assert_eq!(bc_seq.accounts.serialize(), bc_par.accounts.serialize());
    //   });
    //
    // The placeholder body intentionally panics to surface "this test
    // ran but the impl is missing" if someone removes the ignore prematurely.
    panic!(
        "Frontier parallel-apply path not yet implemented. \
         Re-enable this test only when Blockchain::apply_block_pass2_parallel \
         lands behind PARALLEL_APPLY_FORK_HEIGHT (Phase F-5 of impl plan)."
    );
}

/// A second placeholder pinning the conflict-graph determinism contract.
/// The scheduler must produce byte-identical Vec<Batch> for the same
/// input across every run. Phase F-4 of the impl plan owns this.
#[test]
#[ignore = "Frontier conflict-graph builder not yet implemented (Phase F-4)"]
fn build_batches_is_deterministic_across_1000_runs() {
    panic!(
        "Frontier conflict-graph builder not yet implemented. \
         Re-enable this test when the real build_batches() ships in Phase F-4."
    );
}
