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
pub mod mempool;
pub mod state_export;
pub mod storage;
pub mod token_ops;
pub mod vm;

// Re-export key types at crate root for convenience
pub use blockchain::Blockchain;
pub use storage::Storage;
