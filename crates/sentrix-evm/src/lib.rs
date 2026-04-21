//! sentrix-evm — EVM execution layer (revm 37) for Sentrix blockchain.
//!
//! Provides:
//! - `SentrixEvmDb` — revm Database adapter bridging Sentrix account state
//! - `execute_tx` / `execute_call` — EVM transaction execution
//! - `TxReceipt` — execution results
//! - Gas model constants (EIP-1559)
//! - Precompile address definitions

#![allow(missing_docs)]

pub mod database;
pub mod executor;
pub mod gas;
pub mod logs;
pub mod precompiles;
pub mod writeback;

pub use database::{SentrixEvmDb, parse_sentrix_address};
pub use executor::{
    TxReceipt, execute_call, execute_call_with_state, execute_tx, execute_tx_with_state,
};
pub use writeback::commit_state_to_account_db;
pub use gas::{BLOCK_GAS_LIMIT, INITIAL_BASE_FEE};
pub use logs::{
    LogsBloom, StoredLog, add_log_to_bloom, bloom_contains, bloom_union, compute_logs_bloom,
    empty_bloom, log_key, log_key_prefix,
};
