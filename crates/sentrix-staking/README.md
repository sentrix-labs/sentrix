# sentrix-staking

[![crates.io](https://img.shields.io/crates/v/sentrix-staking.svg)](https://crates.io/crates/sentrix-staking)
[![docs.rs](https://docs.rs/sentrix-staking/badge.svg)](https://docs.rs/sentrix-staking)

DPoS staking, epoch management, and slashing for Sentrix Chain.

## Why this crate exists

The BFT engine in [sentrix-bft](../sentrix-bft) needs an active validator set,
the block executor in [sentrix-core](../sentrix-core) needs to settle stake /
delegate / unbond transitions, and the slashing detector needs a place to
record downtime + double-sign evidence. Isolating that surface here keeps the
state machine testable and lets the gRPC layer ([sentrix-grpc](../sentrix-grpc))
expose `GetValidatorSet` without dragging in the EVM stack.

Depends only on [sentrix-primitives](../sentrix-primitives), `serde`, `sha2`,
and `tracing`. No I/O — callers persist the registry via
[sentrix-storage](../sentrix-storage).

## Usage

```toml
[dependencies]
sentrix-staking = { path = "../sentrix-staking" }
```

```rust
use sentrix_staking::{StakeRegistry, EpochManager, SlashingEngine, MIN_SELF_STAKE, MIN_BFT_VALIDATORS};

// StakeRegistry owns validators + delegations + active_set. The block
// executor mutates it during staking ops; the BFT engine reads active_set
// + voting power; epoch boundaries rotate the set.
let mut registry = StakeRegistry::default();
let epoch = EpochManager::epoch_for_height(123_456);

// Slashing decisions go through SlashingEngine, which records evidence
// and decrements the offender's stake at the next epoch boundary.
```

Key re-exports: `StakeRegistry`, `EpochManager`, `SlashingEngine`,
`MIN_SELF_STAKE`, `MIN_BFT_VALIDATORS`.

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
