# sentrix-evm

[![crates.io](https://img.shields.io/crates/v/sentrix-evm.svg)](https://crates.io/crates/sentrix-evm)
[![docs.rs](https://docs.rs/sentrix-evm/badge.svg)](https://docs.rs/sentrix-evm)

EVM execution layer for Sentrix Chain, built on revm 38.

## Why this crate exists

Block execution and `eth_call` both need to run EVM bytecode against Sentrix account
state. This crate provides the revm Database adapter that bridges Sentrix's account
store to revm, plus the execution helpers, gas constants, log/bloom encoding, and
receipt storage keys. [sentrix-core](../sentrix-core) drives transaction execution
through here; [sentrix-rpc](../sentrix-rpc) reuses the same path for `eth_call` and
`eth_estimateGas`.

Precompile addresses are imported from [sentrix-precompiles](../sentrix-precompiles)
so SDKs and tooling can reference them without depending on this stack.

## Usage

```toml
[dependencies]
sentrix-evm = { path = "../sentrix-evm" }
```

```rust
use sentrix_evm::{SentrixEvmDb, execute_tx, TxReceipt, commit_state_to_account_db};

// Adapter wraps the account store as a revm Database, then runs the tx and
// writes the resulting state back. Used by block_executor in sentrix-core.
let mut db = SentrixEvmDb::new(/* &mut account_db */);
let receipt: TxReceipt = execute_tx(&mut db, /* tx, block_env, ... */)?;
commit_state_to_account_db(/* state, &mut account_db */);
```

Key re-exports: `SentrixEvmDb`, `execute_tx`, `execute_call`, `execute_tx_with_state`,
`execute_call_with_state`, `commit_state_to_account_db`, `TxReceipt`, `StoredReceipt`,
`StoredLog`, `LogsBloom`, `compute_logs_bloom`, `BLOCK_GAS_LIMIT`, `INITIAL_BASE_FEE`.

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
