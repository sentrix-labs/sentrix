// justification.rs — Block justification types for BFT consensus.
//
// These live in primitives (not sentrix-bft) because Block needs to
// contain an optional justification, and Block is in primitives.

use serde::{Deserialize, Serialize};

/// A signed precommit from a validator, included in a block justification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPrecommit {
    pub validator: String,
    pub block_hash: String,
    pub signature: Vec<u8>,
    pub stake_weight: u64,
}

/// Calculate 2/3+1 threshold for a given total stake.
pub fn supermajority_threshold(total_stake: u64) -> u64 {
    (total_stake as u128 * 2 / 3 + 1) as u64
}

/// Proof that a block was finalized by BFT consensus. Contains the signed
/// precommits from 2/3+1 of the validator set that committed to this block.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BlockJustification {
    pub height: u64,
    pub round: u32,
    pub block_hash: String,
    pub precommits: Vec<SignedPrecommit>,
}

impl BlockJustification {
    pub fn new(height: u64, round: u32, block_hash: String) -> Self {
        Self {
            height,
            round,
            block_hash,
            precommits: Vec::new(),
        }
    }

    pub fn add_precommit(&mut self, validator: String, signature: Vec<u8>, stake_weight: u64) {
        self.precommits.push(SignedPrecommit {
            validator,
            block_hash: self.block_hash.clone(),
            signature,
            stake_weight,
        });
    }

    pub fn total_stake(&self) -> u64 {
        self.precommits.iter().map(|p| p.stake_weight).sum()
    }

    /// Alias for total_stake (used by BFT engine).
    pub fn total_weight(&self) -> u64 {
        self.total_stake()
    }

    /// Check if we have supermajority (2/3+1 by stake weight).
    pub fn has_supermajority(&self, total_stake: u64) -> bool {
        if total_stake == 0 {
            return false;
        }
        self.total_weight() >= supermajority_threshold(total_stake)
    }

    pub fn signer_count(&self) -> usize {
        self.precommits.len()
    }
}
