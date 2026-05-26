# sentrix-precompiles

[![crates.io](https://img.shields.io/crates/v/sentrix-precompiles.svg)](https://crates.io/crates/sentrix-precompiles)
[![docs.rs](https://docs.rs/sentrix-precompiles/badge.svg)](https://docs.rs/sentrix-precompiles)

Sentrix-reserved EVM precompile addresses (staking, slashing).

## Why this crate exists

Standard Ethereum precompiles `0x01`–`0x09` are handled by revm's `EthPrecompiles`
inside [sentrix-evm](../sentrix-evm). Sentrix's own chain-specific precompiles —
DPoS staking, slashing-evidence — need stable on-chain addresses that contract
authors can hard-code, and that future SDKs or governance tooling can reference
without depending on the full EVM stack.

Splitting these addresses into a tiny standalone crate keeps the contract surface
stable and lets external tooling import the canonical constants from one place.
Depends only on `alloy-primitives`.

## Usage

```toml
[dependencies]
sentrix-precompiles = { path = "../sentrix-precompiles" }
```

```rust
use sentrix_precompiles::{STAKING_PRECOMPILE, SLASHING_PRECOMPILE, is_sentrix_precompile};

// Reserved addresses (handler wire-up gated on a future fork height):
//   STAKING_PRECOMPILE   = 0x...0100
//   SLASHING_PRECOMPILE  = 0x...0101
assert!(is_sentrix_precompile(&STAKING_PRECOMPILE));
```

## Status

As of v2.1.80, both addresses are **reserved + advertised but unwired**. A contract
calling `0x0100` or `0x0101` today receives revm's default empty-success — not a real
handler response. `is_sentrix_precompile(...) == true` does NOT mean a handler exists.
Wiring real handlers will gate behind a fork height so historical chain.db state stays
reproducible. Never change an address — introduce a new one.

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
