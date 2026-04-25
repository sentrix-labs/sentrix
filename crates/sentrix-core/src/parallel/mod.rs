//! Frontier-phase parallel transaction execution.
//!
//! **Scaffold only.** This module exists as a type-system contract for the
//! Frontier-fork parallel apply work documented in
//! `internal design doc` +
//! `internal design doc`.
//!
//! Production code path is unchanged — `block_executor.rs` continues to
//! apply transactions sequentially. The `derive_access` and
//! `build_batches` functions defined in submodules are pure / no-op
//! at this stage; they exist so:
//!
//! 1. The next implementer doesn't bootstrap module structure under time
//!    pressure — they pick up at "fill in the function bodies."
//! 2. The determinism property test contract (`tests/parallel_determinism.rs`)
//!    can be wired now and run against the stub, asserting type stability.
//! 3. Reviewers can sanity-check the type surface (AccountKey, TxAccess,
//!    Batch) before any consensus-touching code lands.
//!
//! **Anti-goals for this scaffold:**
//! - Do NOT call any function from this module in `apply_block_pass2`
//! - Do NOT modify any consensus-critical state path
//! - Do NOT enable real parallelism yet (no rayon `par_iter` etc.)
//!
//! When implementation actually lands, follow Phase F-3 → F-10 in
//! `frontier-mainnet-phase-implementation-plan.md` §3.

pub mod rw_set;
pub mod scheduler;

pub use rw_set::{AccountKey, GlobalKey, TxAccess, derive_access};
pub use scheduler::{Batch, build_batches};
