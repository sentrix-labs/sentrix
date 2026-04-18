// core/mod.rs — Re-exports from workspace crates for backward compatibility.

// From sentrix-primitives
pub mod account;
pub mod block;
pub mod merkle;
pub mod transaction;

// From sentrix-bft
pub use sentrix_bft::engine as bft;
pub use sentrix_bft::messages as bft_messages;

// From sentrix-staking
pub use sentrix_staking::epoch;
pub use sentrix_staking::slashing;
pub use sentrix_staking::staking;

// From sentrix-evm
pub mod evm;

// From sentrix-trie
pub mod trie;

// From sentrix-core
pub use sentrix_core::authority;
pub use sentrix_core::block_executor;
pub use sentrix_core::block_producer;
pub use sentrix_core::blockchain;
pub use sentrix_core::chain_queries;
pub use sentrix_core::mempool;
pub use sentrix_core::state_export;
pub use sentrix_core::token_ops;
pub use sentrix_core::vm;
