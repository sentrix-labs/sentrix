// What "where am I in the BFT round" looks like in memory. The phase
// enum walks through Propose → Prevote → Precommit → Finalize like any
// Tendermint-derived engine, and `BftRoundState` is the per-(height,
// round) scratchpad — collected prevotes, collected precommits, who
// signed what, plus the locked-block bytes that let a previously-locked
// validator re-propose its cached block in a later round (the PoLC
// re-propose path that used to deadlock the engine before V2 M-15
// landed it).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BftPhase {
    Propose,
    Prevote,
    Precommit,
    Finalize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BftRoundState {
    pub height: u64,
    pub round: u32,
    pub phase: BftPhase,
    /// Block hash proposed in this round (if any)
    pub proposed_hash: Option<String>,
    /// Prevotes collected: validator → (block_hash option, stake_weight)
    pub prevotes: HashMap<String, (Option<String>, u64)>,
    /// Precommits collected: validator → (block_hash option, signature bytes, stake_weight).
    /// V1 (re-applied 2026-04-25 on v2.1.16 base): signature bytes kept
    /// alongside vote so `BlockJustification` emits REAL signatures at
    /// finalize, not `vec![]` placeholders. Closes V1 Voyager blocker.
    ///
    /// Prior v2.1.14 revert was the wrong diagnosis — the real v2.1.12
    /// livelock trigger was PR #244's hot-path fsync (closed v2.1.15).
    /// With V2 M-15 locked-block re-propose now shipped in v2.1.16 too,
    /// the locked-validator-prevote-nil scenario self-unsticks via
    /// re-propose instead of livelocking.
    pub precommits: HashMap<String, (Option<String>, Vec<u8>, u64)>,
    /// Our own vote cast this round (prevent double-voting)
    pub our_prevote_cast: bool,
    pub our_precommit_cast: bool,
    /// Locked value: if we precommitted for a hash, we're locked on it
    pub locked_hash: Option<String>,
    pub locked_round: Option<u32>,
    /// V2 M-15 Step 1: block bytes cached alongside `locked_hash` so a
    /// locked validator elected proposer in a later round can RE-PROPOSE
    /// the cached block instead of building a fresh one (whose timestamp
    /// → different hash would trigger the lock-nil-prevote livelock).
    /// See `audits/v2-locked-block-repropose-implementation-plan.md`.
    ///
    /// Populated from `staging_block` at the moment prevote-supermajority
    /// fires. Cleared on `new_height`. Preserved across `advance_round`
    /// (same semantics as `locked_hash`). `#[serde(default)]` for
    /// forward-compat — `BftRoundState` is in-memory-only today but the
    /// planned WAL (M-16 follow-up) will persist this field.
    #[serde(default)]
    pub locked_block: Option<Vec<u8>>,
    /// V2 M-15 Step 1: temporary hold for a proposal's block bytes
    /// between `on_proposal` arrival and prevote-supermajority. If the
    /// staged hash matches the hash that crosses 2/3+ prevote threshold,
    /// bytes are promoted to `locked_block`. Cleared on `advance_round`
    /// and `new_height`. Not wired yet — Step 2 adds the on_proposal /
    /// on_own_proposal parameters to stash bytes here.
    #[serde(default)]
    pub staging_block: Option<(String, Vec<u8>)>,
    /// Total stake of current active set (for threshold calculation)
    pub total_active_stake: u64,
}

impl BftRoundState {
    pub fn new(height: u64, round: u32, total_active_stake: u64) -> Self {
        Self {
            height,
            round,
            phase: BftPhase::Propose,
            proposed_hash: None,
            prevotes: HashMap::new(),
            precommits: HashMap::new(),
            our_prevote_cast: false,
            our_precommit_cast: false,
            locked_hash: None,
            locked_round: None,
            locked_block: None,
            staging_block: None,
            total_active_stake,
        }
    }

    pub fn advance_round(&mut self) {
        self.round = self.round.saturating_add(1);
        self.phase = BftPhase::Propose;
        self.proposed_hash = None;
        self.prevotes.clear();
        self.precommits.clear();
        self.our_prevote_cast = false;
        self.our_precommit_cast = false;
        // locked_hash, locked_round, and locked_block persist across rounds
        // (same PoLC semantics). staging_block is per-round and cleared here —
        // next round's proposal will re-stash.
        self.staging_block = None;
    }

    pub fn advance_height(&mut self, new_height: u64, total_active_stake: u64) {
        *self = Self::new(new_height, 0, total_active_stake);
    }
}
