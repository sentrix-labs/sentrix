// bft.rs — BFT consensus state machine (Voyager Phase 2a)
//
// Tendermint-style 3-phase: Propose → Prevote → Precommit → Finalize.
// Proposer selected by weighted round-robin from active DPoS validator set.
// Finality at 2/3+1 stake weight.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
// errors used by integration callers, not directly in this module
use crate::core::bft_messages::{
    Prevote, Precommit, BlockJustification, supermajority_threshold,
};
use crate::core::staking::StakeRegistry;

// ── Timeouts ─────────────────────────────────────────────────

pub const PROPOSE_TIMEOUT_MS: u64 = 3_000;
pub const PREVOTE_TIMEOUT_MS: u64 = 6_000;
pub const PRECOMMIT_TIMEOUT_MS: u64 = 6_000;
pub const TIMEOUT_INCREMENT_MS: u64 = 1_000;  // +1s per round for propose
pub const VOTE_TIMEOUT_INCREMENT_MS: u64 = 2_000; // +2s per round for votes
pub const MAX_TIMEOUT_MS: u64 = 30_000;
pub const MAX_ROUND: u32 = 20;

// ── BFT State ────────────────────────────────────────────────

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
    /// Precommits collected: validator → (block_hash option, stake_weight)
    pub precommits: HashMap<String, (Option<String>, u64)>,
    /// Our own vote cast this round (prevent double-voting)
    pub our_prevote_cast: bool,
    pub our_precommit_cast: bool,
    /// Locked value: if we precommitted for a hash, we're locked on it
    pub locked_hash: Option<String>,
    pub locked_round: Option<u32>,
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
        // locked_hash and locked_round persist across rounds
    }

    pub fn advance_height(&mut self, new_height: u64, total_active_stake: u64) {
        *self = Self::new(new_height, 0, total_active_stake);
    }
}

// ── Vote Collector ───────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct VoteCollector {
    /// Track which block_hash has how much stake in prevotes
    prevote_tally: HashMap<Option<String>, u64>,
    /// Same for precommits
    precommit_tally: HashMap<Option<String>, u64>,
}

impl VoteCollector {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_prevote(&mut self, block_hash: Option<String>, stake: u64) {
        *self.prevote_tally.entry(block_hash).or_insert(0) += stake;
    }

    pub fn add_precommit(&mut self, block_hash: Option<String>, stake: u64) {
        *self.precommit_tally.entry(block_hash).or_insert(0) += stake;
    }

    /// Check if any single block_hash has supermajority prevotes
    pub fn prevote_supermajority(&self, total_stake: u64) -> Option<Option<String>> {
        let threshold = supermajority_threshold(total_stake);
        for (hash, &weight) in &self.prevote_tally {
            if weight >= threshold {
                return Some(hash.clone());
            }
        }
        None
    }

    /// Check if any single block_hash has supermajority precommits
    pub fn precommit_supermajority(&self, total_stake: u64) -> Option<Option<String>> {
        let threshold = supermajority_threshold(total_stake);
        for (hash, &weight) in &self.precommit_tally {
            if weight >= threshold {
                return Some(hash.clone());
            }
        }
        None
    }

    /// Total prevote weight collected
    pub fn total_prevote_weight(&self) -> u64 {
        self.prevote_tally.values().sum()
    }

    /// Total precommit weight collected
    pub fn total_precommit_weight(&self) -> u64 {
        self.precommit_tally.values().sum()
    }

    pub fn reset(&mut self) {
        self.prevote_tally.clear();
        self.precommit_tally.clear();
    }
}

// ── Timeout Calculator ───────────────────────────────────────

pub fn propose_timeout(round: u32) -> Duration {
    let ms = PROPOSE_TIMEOUT_MS
        .saturating_add((round as u64).saturating_mul(TIMEOUT_INCREMENT_MS))
        .min(MAX_TIMEOUT_MS);
    Duration::from_millis(ms)
}

pub fn prevote_timeout(round: u32) -> Duration {
    let ms = PREVOTE_TIMEOUT_MS
        .saturating_add((round as u64).saturating_mul(VOTE_TIMEOUT_INCREMENT_MS))
        .min(MAX_TIMEOUT_MS);
    Duration::from_millis(ms)
}

pub fn precommit_timeout(round: u32) -> Duration {
    let ms = PRECOMMIT_TIMEOUT_MS
        .saturating_add((round as u64).saturating_mul(VOTE_TIMEOUT_INCREMENT_MS))
        .min(MAX_TIMEOUT_MS);
    Duration::from_millis(ms)
}

// ── BFT Engine ───────────────────────────────────────────────

/// Result of processing a BFT event
#[derive(Debug, Clone)]
pub enum BftAction {
    /// We should propose a block (we are the proposer)
    ProposeBlock,
    /// We should broadcast our prevote
    BroadcastPrevote(Prevote),
    /// We should broadcast our precommit
    BroadcastPrecommit(Precommit),
    /// Block is finalized — apply to state
    FinalizeBlock {
        height: u64,
        round: u32,
        block_hash: String,
        justification: BlockJustification,
    },
    /// No block this round — nil precommits, advance round
    SkipRound,
    /// Timeout — advance round with backoff
    TimeoutAdvanceRound,
    /// Nothing to do yet — waiting for more votes
    Wait,
}

#[derive(Debug)]
pub struct BftEngine {
    pub state: BftRoundState,
    pub collector: VoteCollector,
    pub our_address: String,
    phase_start: Instant,
}

impl BftEngine {
    pub fn new(height: u64, our_address: String, total_active_stake: u64) -> Self {
        Self {
            state: BftRoundState::new(height, 0, total_active_stake),
            collector: VoteCollector::new(),
            our_address,
            phase_start: Instant::now(),
        }
    }

    /// Reset for a new height
    pub fn new_height(&mut self, height: u64, total_active_stake: u64) {
        self.state.advance_height(height, total_active_stake);
        self.collector.reset();
        self.phase_start = Instant::now();
    }

    /// Advance to next round (timeout or nil)
    pub fn advance_round(&mut self) {
        self.state.advance_round();
        self.collector.reset();
        self.phase_start = Instant::now();
    }

    /// Are we the proposer for current height+round?
    pub fn is_proposer(&self, stake_registry: &StakeRegistry) -> bool {
        stake_registry
            .weighted_proposer(self.state.height, self.state.round)
            .as_deref() == Some(self.our_address.as_str())
    }

    /// Get the expected proposer for current height+round
    pub fn expected_proposer(&self, stake_registry: &StakeRegistry) -> Option<String> {
        stake_registry.weighted_proposer(self.state.height, self.state.round)
    }

    /// Check if phase has timed out
    pub fn is_timed_out(&self) -> bool {
        let elapsed = self.phase_start.elapsed();
        let timeout = match self.state.phase {
            BftPhase::Propose => propose_timeout(self.state.round),
            BftPhase::Prevote => prevote_timeout(self.state.round),
            BftPhase::Precommit => precommit_timeout(self.state.round),
            BftPhase::Finalize => Duration::from_secs(0), // no timeout
        };
        elapsed >= timeout
    }

    /// Handle receiving a proposal from another validator
    pub fn on_proposal(
        &mut self,
        block_hash: &str,
        proposer: &str,
        stake_registry: &StakeRegistry,
    ) -> BftAction {
        if self.state.phase != BftPhase::Propose {
            return BftAction::Wait;
        }

        // Verify proposer is correct for this height+round
        let expected = stake_registry.weighted_proposer(self.state.height, self.state.round);
        if expected.as_deref() != Some(proposer) {
            return BftAction::Wait; // wrong proposer, ignore
        }

        self.accept_proposal(block_hash)
    }

    /// Handle our own proposal — skip proposer validation (we created this block).
    /// Used in the validator loop where we already know we're the block producer.
    pub fn on_own_proposal(&mut self, block_hash: &str) -> BftAction {
        if self.state.phase != BftPhase::Propose {
            return BftAction::Wait;
        }
        self.accept_proposal(block_hash)
    }

    fn accept_proposal(&mut self, block_hash: &str) -> BftAction {
        self.state.proposed_hash = Some(block_hash.to_string());
        self.state.phase = BftPhase::Prevote;
        self.phase_start = Instant::now();

        // If we haven't voted yet, cast our prevote
        if !self.state.our_prevote_cast {
            self.state.our_prevote_cast = true;

            // If we're locked on a different hash, prevote nil
            let vote_hash = if let Some(ref locked) = self.state.locked_hash {
                if locked != block_hash {
                    None // locked on different hash
                } else {
                    Some(block_hash.to_string())
                }
            } else {
                Some(block_hash.to_string())
            };

            return BftAction::BroadcastPrevote(Prevote {
                height: self.state.height,
                round: self.state.round,
                block_hash: vote_hash,
                validator: self.our_address.clone(),
                signature: vec![], // filled by caller with actual signature
            });
        }

        BftAction::Wait
    }

    /// Handle receiving a prevote
    pub fn on_prevote(&mut self, prevote: &Prevote) -> BftAction {
        if prevote.height != self.state.height || prevote.round != self.state.round {
            return BftAction::Wait;
        }

        // Prevent duplicate prevotes from same validator
        if self.state.prevotes.contains_key(&prevote.validator) {
            return BftAction::Wait;
        }

        // Look up validator's stake
        let stake = 1; // default; caller should provide actual stake
        self.state.prevotes.insert(
            prevote.validator.clone(),
            (prevote.block_hash.clone(), stake),
        );
        self.collector.add_prevote(prevote.block_hash.clone(), stake);

        // Check for supermajority
        if let Some(hash) = self.collector.prevote_supermajority(self.state.total_active_stake)
            && self.state.phase == BftPhase::Prevote
        {
                self.state.phase = BftPhase::Precommit;
                self.phase_start = Instant::now();

                // Lock on hash if non-nil
                if let Some(ref h) = hash {
                    self.state.locked_hash = Some(h.clone());
                    self.state.locked_round = Some(self.state.round);
                }

                if !self.state.our_precommit_cast {
                    self.state.our_precommit_cast = true;
                    return BftAction::BroadcastPrecommit(Precommit {
                        height: self.state.height,
                        round: self.state.round,
                        block_hash: hash,
                        validator: self.our_address.clone(),
                        signature: vec![],
                    });
                }
            }

        BftAction::Wait
    }

    /// Handle receiving a prevote with known stake weight
    pub fn on_prevote_weighted(&mut self, prevote: &Prevote, stake: u64) -> BftAction {
        if prevote.height != self.state.height || prevote.round != self.state.round {
            return BftAction::Wait;
        }
        if self.state.prevotes.contains_key(&prevote.validator) {
            return BftAction::Wait;
        }

        self.state.prevotes.insert(
            prevote.validator.clone(),
            (prevote.block_hash.clone(), stake),
        );
        self.collector.add_prevote(prevote.block_hash.clone(), stake);

        if let Some(hash) = self.collector.prevote_supermajority(self.state.total_active_stake)
            && self.state.phase == BftPhase::Prevote
        {
                self.state.phase = BftPhase::Precommit;
                self.phase_start = Instant::now();

                if let Some(ref h) = hash {
                    self.state.locked_hash = Some(h.clone());
                    self.state.locked_round = Some(self.state.round);
                }

                if !self.state.our_precommit_cast {
                    self.state.our_precommit_cast = true;
                    return BftAction::BroadcastPrecommit(Precommit {
                        height: self.state.height,
                        round: self.state.round,
                        block_hash: hash,
                        validator: self.our_address.clone(),
                        signature: vec![],
                    });
                }
            }

        BftAction::Wait
    }

    /// Handle receiving a precommit with known stake weight
    pub fn on_precommit_weighted(&mut self, precommit: &Precommit, stake: u64) -> BftAction {
        if precommit.height != self.state.height || precommit.round != self.state.round {
            return BftAction::Wait;
        }
        if self.state.precommits.contains_key(&precommit.validator) {
            return BftAction::Wait;
        }

        self.state.precommits.insert(
            precommit.validator.clone(),
            (precommit.block_hash.clone(), stake),
        );
        self.collector.add_precommit(precommit.block_hash.clone(), stake);

        if let Some(hash) = self.collector.precommit_supermajority(self.state.total_active_stake) {
            match hash {
                Some(block_hash) => {
                    // Block finalized!
                    let mut justification = BlockJustification::new(
                        self.state.height,
                        self.state.round,
                        block_hash.clone(),
                    );
                    for (val, (_, w)) in &self.state.precommits {
                        justification.add_precommit(val.clone(), vec![], *w);
                    }

                    self.state.phase = BftPhase::Finalize;
                    return BftAction::FinalizeBlock {
                        height: self.state.height,
                        round: self.state.round,
                        block_hash,
                        justification,
                    };
                }
                None => {
                    // Nil supermajority — skip this round
                    return BftAction::SkipRound;
                }
            }
        }

        BftAction::Wait
    }

    /// Handle timeout — called when is_timed_out() returns true
    pub fn on_timeout(&mut self) -> BftAction {
        match self.state.phase {
            BftPhase::Propose => {
                // No proposal received — prevote nil
                self.state.phase = BftPhase::Prevote;
                self.phase_start = Instant::now();

                if !self.state.our_prevote_cast {
                    self.state.our_prevote_cast = true;
                    return BftAction::BroadcastPrevote(Prevote {
                        height: self.state.height,
                        round: self.state.round,
                        block_hash: None, // nil
                        validator: self.our_address.clone(),
                        signature: vec![],
                    });
                }
                BftAction::Wait
            }
            BftPhase::Prevote => {
                // Didn't get supermajority prevotes — precommit nil
                self.state.phase = BftPhase::Precommit;
                self.phase_start = Instant::now();

                if !self.state.our_precommit_cast {
                    self.state.our_precommit_cast = true;
                    return BftAction::BroadcastPrecommit(Precommit {
                        height: self.state.height,
                        round: self.state.round,
                        block_hash: None, // nil
                        validator: self.our_address.clone(),
                        signature: vec![],
                    });
                }
                BftAction::Wait
            }
            BftPhase::Precommit => {
                // Didn't get supermajority precommits — advance round
                if self.state.round >= MAX_ROUND {
                    return BftAction::SkipRound; // give up on this height
                }
                BftAction::TimeoutAdvanceRound
            }
            BftPhase::Finalize => BftAction::Wait,
        }
    }

    pub fn height(&self) -> u64 {
        self.state.height
    }

    pub fn round(&self) -> u32 {
        self.state.round
    }

    pub fn phase(&self) -> BftPhase {
        self.state.phase
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::staking::MIN_SELF_STAKE;

    fn setup() -> (BftEngine, StakeRegistry) {
        let mut reg = StakeRegistry::new();
        for i in 0..21 {
            let addr = format!("0xval{:03}", i);
            reg.register_validator(&addr, MIN_SELF_STAKE, 1000, 0).unwrap();
        }
        reg.update_active_set();

        let total_stake: u64 = reg.active_set.iter()
            .filter_map(|a| reg.get_validator(a))
            .map(|v| v.total_stake())
            .sum();

        let engine = BftEngine::new(100, "0xval000".into(), total_stake);
        (engine, reg)
    }

    #[test]
    fn test_new_engine() {
        let (engine, _) = setup();
        assert_eq!(engine.height(), 100);
        assert_eq!(engine.round(), 0);
        assert_eq!(engine.phase(), BftPhase::Propose);
    }

    #[test]
    fn test_advance_round() {
        let (mut engine, _) = setup();
        engine.advance_round();
        assert_eq!(engine.round(), 1);
        assert_eq!(engine.phase(), BftPhase::Propose);
    }

    #[test]
    fn test_new_height() {
        let (mut engine, _) = setup();
        engine.state.round = 5;
        engine.new_height(200, 1000);
        assert_eq!(engine.height(), 200);
        assert_eq!(engine.round(), 0);
        assert_eq!(engine.phase(), BftPhase::Propose);
    }

    #[test]
    fn test_on_proposal_valid() {
        let (mut engine, reg) = setup();
        let proposer = reg.weighted_proposer(100, 0).unwrap();
        let action = engine.on_proposal("hash_abc", &proposer, &reg);

        match action {
            BftAction::BroadcastPrevote(pv) => {
                assert_eq!(pv.height, 100);
                assert_eq!(pv.round, 0);
                assert_eq!(pv.block_hash, Some("hash_abc".into()));
            }
            _ => panic!("expected BroadcastPrevote, got {:?}", action),
        }
        assert_eq!(engine.phase(), BftPhase::Prevote);
    }

    #[test]
    fn test_on_proposal_wrong_proposer() {
        let (mut engine, reg) = setup();
        let action = engine.on_proposal("hash_abc", "0xwrong", &reg);
        assert!(matches!(action, BftAction::Wait));
        assert_eq!(engine.phase(), BftPhase::Propose); // unchanged
    }

    #[test]
    fn test_prevote_supermajority_triggers_precommit() {
        let (mut engine, reg) = setup();
        let total = engine.state.total_active_stake;

        // Move to prevote phase
        let proposer = reg.weighted_proposer(100, 0).unwrap();
        engine.on_proposal("hash_abc", &proposer, &reg);

        // Add enough prevotes to reach supermajority
        let threshold = supermajority_threshold(total);
        let per_val = total / 21;
        let needed = (threshold / per_val) + 1;

        let mut got_precommit = false;
        for i in 1..=needed {
            let pv = Prevote {
                height: 100, round: 0,
                block_hash: Some("hash_abc".into()),
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            let action = engine.on_prevote_weighted(&pv, per_val);
            if matches!(action, BftAction::BroadcastPrecommit(_)) {
                got_precommit = true;
            }
        }

        assert!(got_precommit);
        assert_eq!(engine.phase(), BftPhase::Precommit);
    }

    #[test]
    fn test_precommit_supermajority_finalizes() {
        let (mut engine, _) = setup();
        let total = engine.state.total_active_stake;
        engine.state.phase = BftPhase::Precommit;

        let per_val = total / 21;
        let threshold = supermajority_threshold(total);
        let needed = (threshold / per_val) + 1;

        let mut finalized = false;
        for i in 0..needed {
            let pc = Precommit {
                height: 100, round: 0,
                block_hash: Some("hash_abc".into()),
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            let action = engine.on_precommit_weighted(&pc, per_val);
            if let BftAction::FinalizeBlock { height, block_hash, justification, .. } = action {
                assert_eq!(height, 100);
                assert_eq!(block_hash, "hash_abc");
                assert!(justification.has_supermajority(total));
                finalized = true;
            }
        }

        assert!(finalized);
    }

    #[test]
    fn test_nil_precommit_supermajority_skips() {
        let (mut engine, _) = setup();
        let total = engine.state.total_active_stake;
        engine.state.phase = BftPhase::Precommit;

        let per_val = total / 21;
        let threshold = supermajority_threshold(total);
        let needed = (threshold / per_val) + 1;

        let mut skipped = false;
        for i in 0..needed {
            let pc = Precommit {
                height: 100, round: 0,
                block_hash: None, // nil
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            let action = engine.on_precommit_weighted(&pc, per_val);
            if matches!(action, BftAction::SkipRound) {
                skipped = true;
            }
        }

        assert!(skipped);
    }

    #[test]
    fn test_timeout_propose_sends_nil_prevote() {
        let (mut engine, _) = setup();
        assert_eq!(engine.phase(), BftPhase::Propose);

        let action = engine.on_timeout();
        match action {
            BftAction::BroadcastPrevote(pv) => {
                assert!(pv.is_nil());
            }
            _ => panic!("expected nil prevote"),
        }
        assert_eq!(engine.phase(), BftPhase::Prevote);
    }

    #[test]
    fn test_timeout_prevote_sends_nil_precommit() {
        let (mut engine, _) = setup();
        engine.state.phase = BftPhase::Prevote;

        let action = engine.on_timeout();
        match action {
            BftAction::BroadcastPrecommit(pc) => {
                assert!(pc.is_nil());
            }
            _ => panic!("expected nil precommit"),
        }
        assert_eq!(engine.phase(), BftPhase::Precommit);
    }

    #[test]
    fn test_timeout_precommit_advances_round() {
        let (mut engine, _) = setup();
        engine.state.phase = BftPhase::Precommit;

        let action = engine.on_timeout();
        assert!(matches!(action, BftAction::TimeoutAdvanceRound));
    }

    #[test]
    fn test_timeout_max_round() {
        let (mut engine, _) = setup();
        engine.state.phase = BftPhase::Precommit;
        engine.state.round = MAX_ROUND;

        let action = engine.on_timeout();
        assert!(matches!(action, BftAction::SkipRound));
    }

    #[test]
    fn test_duplicate_prevote_ignored() {
        let (mut engine, _) = setup();
        engine.state.phase = BftPhase::Prevote;

        let pv = Prevote {
            height: 100, round: 0,
            block_hash: Some("hash".into()),
            validator: "0xval001".into(),
            signature: vec![],
        };

        let a1 = engine.on_prevote_weighted(&pv, 100);
        let a2 = engine.on_prevote_weighted(&pv, 100); // duplicate
        // Second one should be Wait (ignored)
        assert!(matches!(a2, BftAction::Wait));
        // Only counted once
        assert_eq!(engine.state.prevotes.len(), 1);
    }

    #[test]
    fn test_wrong_height_prevote_ignored() {
        let (mut engine, _) = setup();
        engine.state.phase = BftPhase::Prevote;

        let pv = Prevote {
            height: 999, round: 0, // wrong height
            block_hash: Some("hash".into()),
            validator: "0xval001".into(),
            signature: vec![],
        };
        let action = engine.on_prevote_weighted(&pv, 100);
        assert!(matches!(action, BftAction::Wait));
        assert!(engine.state.prevotes.is_empty());
    }

    #[test]
    fn test_locked_hash_prevote() {
        let (mut engine, reg) = setup();
        // Lock on hash_a
        engine.state.locked_hash = Some("hash_a".into());
        engine.state.locked_round = Some(0);

        // Proposal for different hash
        let proposer = reg.weighted_proposer(100, 0).unwrap();
        let action = engine.on_proposal("hash_b", &proposer, &reg);

        match action {
            BftAction::BroadcastPrevote(pv) => {
                // Should prevote nil because locked on different hash
                assert!(pv.is_nil());
            }
            _ => panic!("expected nil prevote due to lock"),
        }
    }

    #[test]
    fn test_locked_hash_same_prevote() {
        let (mut engine, reg) = setup();
        engine.state.locked_hash = Some("hash_a".into());
        engine.state.locked_round = Some(0);

        let proposer = reg.weighted_proposer(100, 0).unwrap();
        let action = engine.on_proposal("hash_a", &proposer, &reg);

        match action {
            BftAction::BroadcastPrevote(pv) => {
                // Same hash as locked — vote for it
                assert_eq!(pv.block_hash, Some("hash_a".into()));
            }
            _ => panic!("expected prevote for locked hash"),
        }
    }

    #[test]
    fn test_propose_timeout_values() {
        assert_eq!(propose_timeout(0), Duration::from_millis(3_000));
        assert_eq!(propose_timeout(1), Duration::from_millis(4_000));
        assert_eq!(propose_timeout(10), Duration::from_millis(13_000));
        assert_eq!(propose_timeout(100), Duration::from_millis(30_000)); // capped
    }

    #[test]
    fn test_prevote_timeout_values() {
        assert_eq!(prevote_timeout(0), Duration::from_millis(6_000));
        assert_eq!(prevote_timeout(1), Duration::from_millis(8_000));
        assert_eq!(prevote_timeout(12), Duration::from_millis(30_000)); // capped
    }

    #[test]
    fn test_vote_collector_prevote() {
        let mut vc = VoteCollector::new();
        vc.add_prevote(Some("hash_a".into()), 10);
        vc.add_prevote(Some("hash_a".into()), 5);
        vc.add_prevote(Some("hash_b".into()), 3);

        assert_eq!(vc.total_prevote_weight(), 18);

        // hash_a has 15, threshold for 21 total is 15
        let result = vc.prevote_supermajority(21);
        assert_eq!(result, Some(Some("hash_a".into())));
    }

    #[test]
    fn test_vote_collector_no_supermajority() {
        let mut vc = VoteCollector::new();
        vc.add_prevote(Some("hash_a".into()), 7);
        vc.add_prevote(Some("hash_b".into()), 7);
        vc.add_prevote(Some("hash_c".into()), 7);

        assert!(vc.prevote_supermajority(21).is_none()); // split vote
    }

    #[test]
    fn test_vote_collector_nil_supermajority() {
        let mut vc = VoteCollector::new();
        vc.add_prevote(None, 15);
        vc.add_prevote(Some("hash_a".into()), 6);

        let result = vc.prevote_supermajority(21);
        assert_eq!(result, Some(None)); // nil supermajority
    }

    #[test]
    fn test_full_round_happy_path() {
        // Simulate a complete BFT round: propose → prevote → precommit → finalize
        let (mut engine, reg) = setup();
        let total = engine.state.total_active_stake;
        let per_val = total / 21;

        // 1. Proposal
        let proposer = reg.weighted_proposer(100, 0).unwrap();
        engine.on_proposal("block_hash", &proposer, &reg);
        assert_eq!(engine.phase(), BftPhase::Prevote);

        // 2. Collect prevotes (15+ needed for 21 validators)
        for i in 1..=16 {
            let pv = Prevote {
                height: 100, round: 0,
                block_hash: Some("block_hash".into()),
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            engine.on_prevote_weighted(&pv, per_val);
        }
        assert_eq!(engine.phase(), BftPhase::Precommit);

        // 3. Collect precommits
        let mut finalized = false;
        for i in 0..=16 {
            let pc = Precommit {
                height: 100, round: 0,
                block_hash: Some("block_hash".into()),
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            let action = engine.on_precommit_weighted(&pc, per_val);
            if let BftAction::FinalizeBlock { block_hash, .. } = action {
                assert_eq!(block_hash, "block_hash");
                finalized = true;
            }
        }
        assert!(finalized);
    }

    #[test]
    fn test_single_validator_self_finalize() {
        // Single-validator BFT: on_own_proposal → self-prevote → self-precommit → finalize
        let our_stake = MIN_SELF_STAKE;
        let mut engine = BftEngine::new(500, "0xsolo".into(), our_stake);

        // 1. Own proposal (no proposer validation)
        let action = engine.on_own_proposal("solo_hash");
        let prevote = match action {
            BftAction::BroadcastPrevote(pv) => {
                assert_eq!(pv.block_hash, Some("solo_hash".into()));
                pv
            }
            _ => panic!("expected BroadcastPrevote, got {:?}", action),
        };
        assert_eq!(engine.phase(), BftPhase::Prevote);

        // 2. Self-prevote (100% stake = instant supermajority)
        let action = engine.on_prevote_weighted(&prevote, our_stake);
        let precommit = match action {
            BftAction::BroadcastPrecommit(pc) => {
                assert_eq!(pc.block_hash, Some("solo_hash".into()));
                pc
            }
            _ => panic!("expected BroadcastPrecommit, got {:?}", action),
        };
        assert_eq!(engine.phase(), BftPhase::Precommit);

        // 3. Self-precommit → finalize
        let action = engine.on_precommit_weighted(&precommit, our_stake);
        match action {
            BftAction::FinalizeBlock { height, block_hash, justification, .. } => {
                assert_eq!(height, 500);
                assert_eq!(block_hash, "solo_hash");
                assert!(justification.has_supermajority(our_stake));
            }
            _ => panic!("expected FinalizeBlock, got {:?}", action),
        }
    }
}
