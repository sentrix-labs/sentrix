# sentrix-primitives

[![crates.io](https://img.shields.io/crates/v/sentrix-primitives.svg)](https://crates.io/crates/sentrix-primitives)
[![docs.rs](https://docs.rs/sentrix-primitives/badge.svg)](https://docs.rs/sentrix-primitives)

Core types and error handling for Sentrix Chain.

## Why this crate exists

Every other crate in the workspace needs `Block`, `Transaction`, `Account`,
`AccountDB`, address derivation, and the canonical `SentrixError` / `SentrixResult`
types. Keeping them in a leaf crate with zero internal dependencies (only `thiserror`,
`serde`, `secp256k1`, `sha2`, `sha3`, `hex`) means everyone else can depend on it
without creating cycles.

This is the bottom of the dependency graph — [sentrix-bft](../sentrix-bft),
[sentrix-staking](../sentrix-staking), [sentrix-evm](../sentrix-evm),
[sentrix-trie](../sentrix-trie), [sentrix-core](../sentrix-core), and
[sentrix-wire](../sentrix-wire) all consume types from here.

## Usage

```toml
[dependencies]
sentrix-primitives = { path = "../sentrix-primitives" }
```

```rust
use sentrix_primitives::{
    Account, AccountDB, Block, Transaction, SentrixError, SentrixResult,
    derive_address, merkle_root, sha256_hex, SENTRI_PER_SRX,
};

// `AccountDB` is the workspace's canonical balance + nonce + code store.
// `derive_address` turns a secp256k1 pubkey into the chain's 0x-prefixed
// 20-byte address (Keccak-256 of the uncompressed pubkey, last 20 bytes).
let mut accounts = AccountDB::new();
accounts.credit(&caller_supplied_address, 1_000 * SENTRI_PER_SRX)?;
let bal = accounts.get_balance(&caller_supplied_address);
```

Re-exported submodules: `account`, `address`, `block`, `error`, `events`,
`justification`, `merkle`, `transaction`. Tested with a `proptest` invariant
sweep that asserts `total_supply()` and `total_burned` track across random op
sequences — catches supply-leak classes at the unit level.

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
