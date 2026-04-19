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

pub use database::{SentrixEvmDb, parse_sentrix_address};
pub use executor::{execute_tx, execute_call, TxReceipt};
pub use gas::{BLOCK_GAS_LIMIT, INITIAL_BASE_FEE};
pub use logs::{
    add_log_to_bloom, bloom_contains, bloom_union, compute_logs_bloom, empty_bloom, log_key,
    log_key_prefix, LogsBloom, StoredLog,
};
