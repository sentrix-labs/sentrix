//! sentrix-core — Blockchain state, block execution, authority, mempool, storage.
//!
//! This crate contains the core blockchain orchestration logic that ties
//! together primitives, consensus, staking, EVM, and storage.

#![allow(missing_docs)]

pub mod authority;
pub mod block_executor;
pub mod block_producer;
pub mod blockchain;
pub mod chain_queries;
pub mod genesis;
pub mod mempool;
pub mod parallel;
pub mod state_export;
pub mod storage;
pub mod token_ops;
pub mod vm;

// Re-export key types at crate root for convenience
pub use blockchain::Blockchain;
pub use genesis::{Genesis, GenesisError};
pub use storage::Storage;

/// Crate-level test utilities. Shared across child modules so any test
/// that mutates fork-gate env vars (JAIL_CONSENSUS_HEIGHT,
/// BFT_GATE_RELAX_HEIGHT, TOKENOMICS_V2_HEIGHT, ...) can serialize
/// itself with one global lock. Without this, cargo's default parallel
/// runner races env-var-touching tests across modules.
#[cfg(test)]
pub(crate) mod test_util {
    use std::sync::{Mutex, MutexGuard, OnceLock};

    pub fn env_test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
}
