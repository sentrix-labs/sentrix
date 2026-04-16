// evm/mod.rs — EVM integration via revm (Voyager Phase 2b)
//
// Provides Ethereum Virtual Machine execution for Sentrix chain.
// Uses revm (Rust EVM) for bytecode execution and gas metering.

pub mod database;
pub mod executor;
pub mod gas;
pub mod precompiles;
