//! sentrix-staking — DPoS staking, epoch management, and slashing.
//!
//! Provides:
//! - `StakeRegistry` — validator registration, delegation, unbonding
//! - `EpochManager` — epoch boundaries, validator set rotation
//! - `SlashingEngine` — downtime + double-sign detection

#![allow(missing_docs)]

pub mod epoch;
pub mod slashing;
pub mod staking;

pub use epoch::EpochManager;
pub use slashing::SlashingEngine;
pub use staking::{StakeRegistry, MIN_SELF_STAKE};
