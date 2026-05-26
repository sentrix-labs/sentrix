# sentrix-core

[![crates.io](https://img.shields.io/crates/v/sentrix-core.svg)](https://crates.io/crates/sentrix-core)
[![docs.rs](https://docs.rs/sentrix-core/badge.svg)](https://docs.rs/sentrix-core)

Blockchain orchestration for Sentrix Chain — state machine, block execution, mempool, genesis.

## Why this crate exists

This is where the workspace's leaf crates ([sentrix-primitives](../sentrix-primitives),
[sentrix-trie](../sentrix-trie), [sentrix-staking](../sentrix-staking),
[sentrix-evm](../sentrix-evm), [sentrix-bft](../sentrix-bft),
[sentrix-storage](../sentrix-storage)) are wired together into one `Blockchain` type.
The validator binary and [sentrix-rpc](../sentrix-rpc) / [sentrix-grpc](../sentrix-grpc)
both depend on this crate; they hold `Arc<RwLock<Blockchain>>` and read/write through it.

Modules cover block production, block execution (revm + state-trie writeback), the
mempool, authority + genesis loading, fork-height activations, and chain query helpers
the RPC layer fans out to.

## Usage

```toml
[dependencies]
sentrix-core = { path = "../sentrix-core" }
```

```rust
use sentrix_core::{Blockchain, Genesis, Storage};

// Load genesis from disk (or use `Genesis::mainnet()` for the embedded
// mainnet genesis), open MDBX storage, then construct the blockchain
// the validator main loop and the RPC stack both share.
let genesis = Genesis::from_path("genesis.toml")?;
let _storage = Storage::open("data/chain.db")?;
let mut blockchain = Blockchain::new_with_genesis(
    "0x...admin_address".to_string(),
    &genesis,
);
```

Key re-exports: `Blockchain`, `Genesis`, `GenesisError`, `Storage`. Submodules:
`block_executor`, `block_producer`, `mempool`, `authority`, `chain_params`,
`chain_queries`, `state_export`, `fork_heights`, `tokenomics`, `token_ops`, `vm`,
`parallel`.

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
