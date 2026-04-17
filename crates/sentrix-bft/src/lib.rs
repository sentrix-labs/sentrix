//! sentrix-bft — BFT consensus engine (Tendermint-style) for Sentrix.
//!
//! Provides:
//! - `BftEngine` — 3-phase state machine (Propose → Prevote → Precommit → Finalize)
//! - `BftAction` — action results (ProposeBlock, BroadcastPrevote, FinalizeBlock, etc.)
//! - Message types: `Proposal`, `Prevote`, `Precommit`, `RoundStatus`, `BlockJustification`
//! - Vote signing + verification (secp256k1 ECDSA)
//!
//! # BFT Fix Notes (2026-04-17)
//!
//! Round advancement is TIMEOUT-ONLY. No vote-triggered or RoundStatus-triggered
//! catch-up. This prevents the "validator leapfrog" stall where validators
//! clear collected votes on every round jump. See commit 3a588bb.
//!
//! Constants: PROPOSE_TIMEOUT_MS=10000, MAX_ROUND=100.

#![allow(missing_docs)]

pub mod engine;
pub mod messages;

pub use engine::{BftEngine, BftAction, BftPhase, BftRoundState, VoteCollector};
pub use engine::{propose_timeout, prevote_timeout, precommit_timeout};
pub use engine::{PROPOSE_TIMEOUT_MS, PREVOTE_TIMEOUT_MS, PRECOMMIT_TIMEOUT_MS, MAX_ROUND};
pub use messages::{
    BftMessage, BlockJustification, Precommit, Prevote, Proposal, RoundStatus,
    supermajority_threshold,
};
