# sentrix-bft

[![crates.io](https://img.shields.io/crates/v/sentrix-bft.svg)](https://crates.io/crates/sentrix-bft)
[![docs.rs](https://docs.rs/sentrix-bft/badge.svg)](https://docs.rs/sentrix-bft)

Tendermint-style BFT consensus engine for Sentrix Chain.

## Why this crate exists

The validator main loop in [`sentrix-labs/sentrix`](https://github.com/sentrix-labs/sentrix) drives a 3-phase state machine — Propose, Prevote, Precommit — and needs a self-contained module that owns vote collection, round timeouts, and the last-signed-vote guard. Splitting the engine out of the binary lets `sentrix-wire` reference the message types without pulling the libp2p stack, and lets tests run the state machine deterministically without touching the network.

The crate depends on [sentrix-primitives](../sentrix-primitives) for block/transaction types, [sentrix-staking](../sentrix-staking) for active-set lookups, and [sentrix-wallet](../sentrix-wallet) for secp256k1 signing.

## Usage

```toml
[dependencies]
sentrix-bft = { path = "../sentrix-bft" }
```

```rust
use sentrix_bft::{BftEngine, BftAction, BftPhase};

// Build the engine for a given (height, round) and feed it messages.
// The driver calls `BftEngine::tick()` on the propose/prevote/precommit
// timeouts and dispatches the resulting `BftAction`s to the network layer.
let mut engine = BftEngine::new(/* validators, our_address, ... */);
match engine.phase() {
    BftPhase::Propose => { /* proposer broadcasts a Proposal */ }
    BftPhase::Prevote => { /* collect 2f+1 prevotes */ }
    BftPhase::Precommit => { /* collect 2f+1 precommits → finalize */ }
    _ => {}
}
```

Key re-exports: `BftEngine`, `BftAction`, `BftPhase`, `BftRoundState`, `VoteCollector`,
`Proposal`, `Prevote`, `Precommit`, `RoundStatus`, `BlockJustification`,
`supermajority_threshold`.

## Round advancement

Round advancement is timeout-only — votes and `RoundStatus` messages never trigger an
eager round jump. This is the 2026-04-17 fix for the validator-leapfrog stall where
vote-triggered catch-up would clear collected votes on every round bump. Constants:
`PROPOSE_TIMEOUT_MS = 10000`, `MAX_ROUND = 100`.

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
