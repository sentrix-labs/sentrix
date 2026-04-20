// bft.rs — BFT consensus state machine (Voyager Phase 2a)
//
// Tendermint-style 3-phase: Propose → Prevote → Precommit → Finalize.
// Proposer selected by weighted round-robin from active DPoS validator set.
// Finality at 2/3+1 stake weight.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
// errors used by integration callers, not directly in this module
use crate::messages::{
    BlockJustification, Precommit, Prevote, RoundStatus, supermajority_threshold,
};
use sentrix_staking::StakeRegistry;

// ── Timeouts ─────────────────────────────────────────────────

// Timeouts tuned for 4-validator testnet with ~100ms localhost latency.
// Round 0 must give enough time for all validators to start up + exchange
// their first proposal. Previous 3s propose caused premature nil-prevotes
// when validators started at slightly different times.
pub const PROPOSE_TIMEOUT_MS: u64 = 10_000;
pub const PREVOTE_TIMEOUT_MS: u64 = 10_000;
pub const PRECOMMIT_TIMEOUT_MS: u64 = 10_000;
pub const TIMEOUT_INCREMENT_MS: u64 = 1_000; // +1s per round for propose
pub const VOTE_TIMEOUT_INCREMENT_MS: u64 = 2_000; // +2s per round for votes
pub const MAX_TIMEOUT_MS: u64 = 30_000;
pub const MAX_ROUND: u32 = 100;

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

    /// Snapshot of the current precommit tally, for diagnostic logging.
    /// Returns `(short_hash_or_nil, weight)` pairs sorted by weight desc
    /// so split-vote livelocks are easy to spot in journalctl.
    ///
    /// Added for backlog #1d — the nil-skip branch previously lost this
    /// information, making unanimous-nil-timeout indistinguishable from
    /// split-vote livelock in the logs.
    pub fn precommit_tally_snapshot(&self) -> Vec<(String, u64)> {
        let mut entries: Vec<(String, u64)> = self
            .precommit_tally
            .iter()
            .map(|(h, w)| {
                let label = match h {
                    Some(hash) => {
                        let trimmed = &hash[..hash.len().min(12)];
                        format!("{trimmed}…")
                    }
                    None => "nil".to_string(),
                };
                (label, *w)
            })
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.1));
        entries
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
    /// Peer is at a higher height — we need to sync blocks first
    SyncNeeded { peer_height: u64 },
}

#[derive(Debug)]
pub struct BftEngine {
    pub state: BftRoundState,
    pub collector: VoteCollector,
    pub our_address: String,
    phase_start: Instant,
    /// Per-peer highest observed round at current height (validator → (round, stake)).
    /// Used for f+1 stake-weighted round skipping to close the persistent
    /// 1-round-drift livelock (issue #143) that the legacy single-peer
    /// "2+ rounds ahead" trigger could not resolve.
    peer_rounds: HashMap<String, (u32, u64)>,
}

impl BftEngine {
    pub fn new(height: u64, our_address: String, total_active_stake: u64) -> Self {
        Self {
            state: BftRoundState::new(height, 0, total_active_stake),
            collector: VoteCollector::new(),
            our_address,
            phase_start: Instant::now(),
            peer_rounds: HashMap::new(),
        }
    }

    /// Reset for a new height
    pub fn new_height(&mut self, height: u64, total_active_stake: u64) {
        self.state.advance_height(height, total_active_stake);
        self.collector.reset();
        self.phase_start = Instant::now();
        self.peer_rounds.clear();
    }

    /// Advance to next round (timeout or nil)
    pub fn advance_round(&mut self) {
        self.state.advance_round();
        self.collector.reset();
        self.phase_start = Instant::now();
    }

    /// Round catch-up: if peers are at a higher round, fast-forward.
    /// Returns `Some(Prevote)` (nil block_hash) if we advanced — the caller
    /// MUST sign and broadcast this prevote so peers observe our
    /// participation in the caught-up round.
    ///
    /// Issue #133: without the nil-prevote, a validator that restarts
    /// mid-consensus catches up to the peers' round and then sits
    /// silently in `BftPhase::Propose` forever — gossipsub does not
    /// replay the round's original proposal and the catching-up
    /// validator is rarely the proposer for the round it just caught
    /// up to. The chain then runs on 3-of-4 quorum and any timing
    /// drift stalls the height.
    ///
    /// Nil prevote is Tendermint-legal: it represents "I participate in
    /// this round but have no valid proposal to vote for". It cannot be
    /// a double-vote because `advance_round` above cleared
    /// `our_prevote_cast`, and `accept_proposal` respects
    /// `our_prevote_cast`, so a proposal arriving later in the same
    /// round does not cause us to re-vote.
    pub fn catch_up_round(&mut self, target_round: u32) -> Option<Prevote> {
        if target_round <= self.state.round {
            return None;
        }
        tracing::info!(
            "BFT round catch-up: round {} → {} at height {}",
            self.state.round,
            target_round,
            self.state.height,
        );
        while self.state.round < target_round {
            self.state.advance_round();
            self.collector.reset();
        }
        self.phase_start = Instant::now();

        // Enter Prevote phase with a nil vote so we contribute to quorum
        // immediately instead of waiting (forever) for a proposal that
        // gossipsub will not re-deliver.
        self.state.phase = BftPhase::Prevote;
        self.state.our_prevote_cast = true;
        Some(Prevote {
            height: self.state.height,
            round: self.state.round,
            block_hash: None,
            validator: self.our_address.clone(),
            signature: vec![], // caller signs before broadcast
        })
    }

    /// Are we the proposer for current height+round?
    pub fn is_proposer(&self, stake_registry: &StakeRegistry) -> bool {
        stake_registry
            .weighted_proposer(self.state.height, self.state.round)
            .as_deref()
            == Some(self.our_address.as_str())
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
        // M-15: semantic reference for the lock state machine.
        //
        // Sentrix's BFT lock is set in `on_prevote_weighted` when a
        // 2/3+ stake-weighted prevote supermajority is observed for a
        // concrete (non-nil) hash — that call path also pins
        // `locked_round`. On the next round:
        //
        //   * advance_round() resets prevotes/precommits/our_cast flags
        //     but PERSISTS locked_hash and locked_round. This is
        //     intentional: the lock is a SAFETY commitment — once
        //     we have evidence that 2/3+ might have committed this
        //     block, we must not prevote a conflicting value without
        //     proof that the network has moved on (Proof of Lock
        //     Change, a.k.a. PoLC in Tendermint literature).
        //
        //   * If a new proposal arrives for a different hash and we
        //     observe a new 2/3+ prevote supermajority for THAT hash
        //     in the current round, `on_prevote_weighted` updates
        //     `locked_hash` to the new value — the implicit PoLC.
        //
        // Known gap (tracked as M-15 follow-up):
        //
        //   1. We do not re-propose the locked block when WE become
        //      proposer of a new round while locked — Tendermint says
        //      we must. Current behaviour: build a fresh block, which
        //      peers prevote-nil on because they observe a lock
        //      conflict, and the round times out. Ultimately not
        //      unsafe (eventually someone else proposes the locked
        //      value or the lock shifts via fresh PoLC) but reduces
        //      liveness.
        //
        //   2. We do not persist per-round prevote history, so an
        //      explicit PoLC object carrying the 2/3 signatures is
        //      not produced — the unlock is triggered by observing a
        //      new supermajority at the same node, not by replaying
        //      a signed PoLC from a peer. This is safe under the
        //      assumption that all validators follow the same lock
        //      rules; it would be exploitable if a byzantine leader
        //      could synthesise a phantom supermajority in his own
        //      collector state, which this code does not allow
        //      (collector keys on validator address + stake from
        //      StakeRegistry — no self-inflation path).
        //
        // The guard below implements the "prevote nil on lock
        // conflict" part correctly; a proper fix for (1) above
        // belongs in block_producer.rs (cache the proposed block and
        // re-use when locked).
        self.state.proposed_hash = Some(block_hash.to_string());
        self.state.phase = BftPhase::Prevote;
        self.phase_start = Instant::now();

        // If we haven't voted yet, cast our prevote
        if !self.state.our_prevote_cast {
            self.state.our_prevote_cast = true;

            // If we're locked on a different hash, prevote nil.
            // Proper Tendermint says we can prevote the locked value
            // instead of nil — Sentrix prevotes nil because the locked
            // block is not re-broadcast by us and peers without it
            // cannot verify the prevote. This is correct under our
            // current "proposer re-broadcasts on timeout" flow; see
            // gap (1) in the lock-semantic note above.
            let vote_hash = if let Some(ref locked) = self.state.locked_hash {
                if locked != block_hash {
                    tracing::debug!(
                        target: "bft_lock",
                        "prevote nil: locked on {} at round {:?}, proposal is {}",
                        &locked[..locked.len().min(12)],
                        self.state.locked_round,
                        &block_hash[..block_hash.len().min(12)]
                    );
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

    /// Handle receiving a prevote with assumed unit stake.
    ///
    /// **A4 / test+legacy use only.** Production code MUST call
    /// [`Self::on_prevote_weighted`] with the actual validator stake from
    /// the StakeRegistry. Equal-weight unit stake works for tests with N
    /// validators all assumed equal but is incorrect for any real DPoS
    /// deployment with weighted stake.
    pub fn on_prevote(&mut self, prevote: &Prevote) -> BftAction {
        self.on_prevote_weighted(prevote, 1)
    }

    /// Handle receiving a prevote with known stake weight
    pub fn on_prevote_weighted(&mut self, prevote: &Prevote, stake: u64) -> BftAction {
        if prevote.height != self.state.height {
            return BftAction::Wait;
        }
        // Only process votes for our current round. Votes from higher rounds
        // are silently dropped — we'll advance to that round naturally via
        // timeout or RoundStatus gossip. This prevents the "validator
        // leapfrog" problem where a single future-round vote clears all
        // collected votes for the current round, making quorum unreachable.
        if prevote.round != self.state.round {
            return BftAction::Wait;
        }
        if self.state.prevotes.contains_key(&prevote.validator) {
            return BftAction::Wait;
        }

        self.state.prevotes.insert(
            prevote.validator.clone(),
            (prevote.block_hash.clone(), stake),
        );
        self.collector
            .add_prevote(prevote.block_hash.clone(), stake);

        if let Some(hash) = self
            .collector
            .prevote_supermajority(self.state.total_active_stake)
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
        if precommit.height != self.state.height {
            return BftAction::Wait;
        }
        // Same as prevote: only process votes for current round. Future-round
        // precommits are dropped — round advancement happens via timeout or
        // RoundStatus gossip only.
        if precommit.round != self.state.round {
            return BftAction::Wait;
        }
        if self.state.precommits.contains_key(&precommit.validator) {
            return BftAction::Wait;
        }

        self.state.precommits.insert(
            precommit.validator.clone(),
            (precommit.block_hash.clone(), stake),
        );
        self.collector
            .add_precommit(precommit.block_hash.clone(), stake);

        if let Some(hash) = self
            .collector
            .precommit_supermajority(self.state.total_active_stake)
        {
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
                    // Nil supermajority — skip this round.
                    //
                    // Backlog #1d investigation: log the per-hash tally so we
                    // can tell a unanimous-nil skip (healthy timeout path) from
                    // a split-vote skip (livelock symptom — one subset voted
                    // block_X, another voted nil, neither reached 2f+1).
                    let tally_summary: Vec<String> = self
                        .collector
                        .precommit_tally_snapshot()
                        .into_iter()
                        .map(|(label, w)| format!("{label}={w}"))
                        .collect();
                    tracing::warn!(
                        "BFT #1d: precommit nil-majority skip at height={} round={} \
                         threshold={} tally=[{}]",
                        self.state.height,
                        self.state.round,
                        supermajority_threshold(self.state.total_active_stake),
                        tally_summary.join(", ")
                    );
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

    /// Handle a RoundStatus gossip from a peer (stake-weighted, issue #143).
    ///
    /// Behaviour:
    /// * Peer at higher height → `SyncNeeded` (block sync required first).
    /// * Peer at same height → record `peer_rounds[validator] = (round, stake)`.
    ///   Then, if f+1 stake (>1/3 of `total_active_stake`) is at rounds
    ///   strictly greater than our current round, catch up to the highest
    ///   such round. This is the standard Tendermint round-skip rule.
    ///
    /// The legacy trigger was "single peer 2+ rounds ahead → catch up to
    /// peer_round − 1", which could not close a persistent 1-round drift
    /// between two validator clusters (observed on testnet, issue #143 —
    /// rounds climbed past 140 with 2/4 always lagging). The stake-weighted
    /// f+1 rule resolves this: once the lagging cluster sees f+1 peers at a
    /// higher round, it jumps directly instead of waiting for its own
    /// timeout to fire.
    ///
    /// Catch-up emits a nil prevote in the new round (issue #133 fix, see
    /// `catch_up_round`) so we participate in quorum immediately.
    pub fn on_round_status_weighted(&mut self, status: &RoundStatus, stake: u64) -> BftAction {
        if status.height > self.state.height {
            return BftAction::SyncNeeded {
                peer_height: status.height,
            };
        }
        if status.height < self.state.height {
            return BftAction::Wait;
        }
        // Track peer's highest-seen round. Keep the max round; refresh the
        // stake snapshot on every update (the peer's stake can change across
        // epoch boundaries).
        let entry = self
            .peer_rounds
            .entry(status.validator.clone())
            .or_insert((0, stake));
        if status.round >= entry.0 {
            entry.0 = status.round;
            entry.1 = stake;
        }

        if let Some(target) = self.f_plus_one_round()
            && target > self.state.round
            && let Some(prevote) = self.catch_up_round(target)
        {
            return BftAction::BroadcastPrevote(prevote);
        }
        BftAction::Wait
    }

    /// Back-compat wrapper for the legacy single-peer RoundStatus path. Call
    /// sites that have the peer's stake should prefer `on_round_status_weighted`.
    /// This wrapper preserves the pre-#143 behaviour (trigger only on 2+ rounds
    /// ahead from a single peer, catch up to peer_round − 1) so existing
    /// integrations compile unchanged.
    pub fn on_round_status(&mut self, status: &RoundStatus) -> BftAction {
        if status.height > self.state.height {
            return BftAction::SyncNeeded {
                peer_height: status.height,
            };
        }
        if status.height == self.state.height
            && status.round > self.state.round + 1
            && let Some(prevote) = self.catch_up_round(status.round - 1)
        {
            return BftAction::BroadcastPrevote(prevote);
        }
        BftAction::Wait
    }

    /// Largest round R such that f+1 stake of distinct peers are at round >= R,
    /// where f+1 is the minimum stake strictly exceeding one third of
    /// `total_active_stake`. Returns None if no such R exists or if
    /// `total_active_stake` is zero.
    ///
    /// Only peers with round strictly greater than our own round are counted —
    /// we ourselves don't trigger a skip for our own round.
    fn f_plus_one_round(&self) -> Option<u32> {
        if self.state.total_active_stake == 0 {
            return None;
        }
        // `f` = floor(total / 3); `f+1` stake = any stake total > f.
        // Minimum stake exceeding 1/3 is `total/3 + 1` for integer math.
        let f_plus_one_threshold = self.state.total_active_stake / 3 + 1;

        let mut peers: Vec<(u32, u64)> = self
            .peer_rounds
            .values()
            .filter(|(r, _)| *r > self.state.round)
            .copied()
            .collect();
        // Sort by round descending so we accumulate from the highest round
        // downward. The first round at which cumulative stake crosses the
        // f+1 threshold is the largest round that f+1 peers have reached.
        peers.sort_by_key(|p| std::cmp::Reverse(p.0));

        let mut accumulated: u64 = 0;
        for (round, stake) in peers {
            accumulated = accumulated.saturating_add(stake);
            if accumulated >= f_plus_one_threshold {
                return Some(round);
            }
        }
        None
    }

    /// Build an UNSIGNED RoundStatus for gossiping. Callers must invoke
    /// [`RoundStatus::sign`] before broadcasting — unsigned statuses are
    /// rejected at the network boundary (see C-01 fix).
    pub fn build_round_status(&self) -> RoundStatus {
        RoundStatus {
            height: self.state.height,
            round: self.state.round,
            validator: self.our_address.clone(),
            signature: Vec::new(),
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
    use sentrix_staking::MIN_SELF_STAKE;

    fn setup() -> (BftEngine, StakeRegistry) {
        let mut reg = StakeRegistry::new();
        for i in 0..21 {
            let addr = format!("0xval{:03}", i);
            reg.register_validator(&addr, MIN_SELF_STAKE, 1000, 0)
                .unwrap();
        }
        reg.update_active_set();

        let total_stake: u64 = reg
            .active_set
            .iter()
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
                height: 100,
                round: 0,
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
                height: 100,
                round: 0,
                block_hash: Some("hash_abc".into()),
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            let action = engine.on_precommit_weighted(&pc, per_val);
            if let BftAction::FinalizeBlock {
                height,
                block_hash,
                justification,
                ..
            } = action
            {
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
                height: 100,
                round: 0,
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
            height: 100,
            round: 0,
            block_hash: Some("hash".into()),
            validator: "0xval001".into(),
            signature: vec![],
        };

        let _a1 = engine.on_prevote_weighted(&pv, 100);
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
            height: 999,
            round: 0, // wrong height
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
    fn test_round_status_catch_up_when_2_ahead() {
        // RoundStatus triggers catch-up to peer_round - 1 when peer is 2+ ahead.
        // This prevents the round desync stall without causing leapfrog.
        //
        // Issue #133: after catch_up, the engine now emits a nil prevote
        // (BftAction::BroadcastPrevote with block_hash = None) so peers
        // observe our participation in the caught-up round — otherwise
        // a restarted validator sits silent and 4-of-4 quorum drops to
        // 3-of-4.
        let (mut engine, _) = setup();
        assert_eq!(engine.round(), 0);

        // Peer at round 5 → we catch up to round 4 (peer_round - 1)
        let status = RoundStatus {
            height: 100,
            round: 5,
            validator: "0xval001".into(),
            signature: Vec::new(),
        };
        let action = engine.on_round_status(&status);
        match action {
            BftAction::BroadcastPrevote(p) => {
                assert_eq!(p.round, 4, "prevote must be for caught-up round");
                assert_eq!(
                    p.block_hash, None,
                    "must be nil prevote — we have no proposal"
                );
            }
            other => panic!("expected BroadcastPrevote(nil), got {other:?}"),
        }
        assert_eq!(engine.round(), 4); // caught up to peer - 1
    }

    // Issue #133: after catch_up, our_prevote_cast must be set so a late
    // proposal for this round does NOT trigger a double-vote.
    #[test]
    fn test_133_catch_up_sets_our_prevote_cast_prevents_double_vote() {
        let (mut engine, reg) = setup();
        let status = RoundStatus {
            height: 100,
            round: 5,
            validator: "0xval001".into(),
            signature: Vec::new(),
        };
        let _ = engine.on_round_status(&status);
        assert!(
            engine.state.our_prevote_cast,
            "catch_up must mark our prevote cast"
        );
        assert_eq!(engine.state.phase, BftPhase::Prevote);

        // Late proposal for the same round must NOT cause a second prevote.
        let proposer = reg.weighted_proposer(engine.height(), 4).unwrap();
        let action = engine.on_proposal("late_hash", &proposer, &reg);
        assert!(
            matches!(action, BftAction::Wait),
            "late proposal must not trigger second prevote when our_prevote_cast is set; got {action:?}"
        );
    }

    #[test]
    fn test_round_status_no_catch_up_when_only_1_ahead() {
        // Peer only 1 round ahead → no catch-up (normal timeout will sync)
        let (mut engine, _) = setup();
        assert_eq!(engine.round(), 0);

        let status = RoundStatus {
            height: 100,
            round: 1, // only 1 ahead
            validator: "0xval001".into(),
            signature: Vec::new(),
        };
        let action = engine.on_round_status(&status);
        assert!(matches!(action, BftAction::Wait));
        assert_eq!(engine.round(), 0); // stays at 0
    }

    #[test]
    fn test_round_status_higher_height_triggers_sync() {
        let (mut engine, _) = setup();
        assert_eq!(engine.height(), 100);

        let status = RoundStatus {
            height: 200,
            round: 0,
            validator: "0xval001".into(),
            signature: Vec::new(),
        };
        let action = engine.on_round_status(&status);
        match action {
            BftAction::SyncNeeded { peer_height } => assert_eq!(peer_height, 200),
            _ => panic!("expected SyncNeeded, got {:?}", action),
        }
        assert_eq!(engine.height(), 100); // unchanged
    }

    #[test]
    fn test_round_status_lower_height_ignored() {
        let (mut engine, _) = setup();
        engine.new_height(200, engine.state.total_active_stake);

        let status = RoundStatus {
            height: 50,
            round: 10,
            validator: "0xval001".into(),
            signature: Vec::new(),
        };
        let action = engine.on_round_status(&status);
        assert!(matches!(action, BftAction::Wait));
        assert_eq!(engine.round(), 0); // unchanged
    }

    #[test]
    fn test_round_status_same_round_noop() {
        let (mut engine, _) = setup();
        engine.state.round = 3;

        let status = RoundStatus {
            height: 100,
            round: 3,
            validator: "0xval001".into(),
            signature: Vec::new(),
        };
        let action = engine.on_round_status(&status);
        assert!(matches!(action, BftAction::Wait));
        assert_eq!(engine.round(), 3); // unchanged
    }

    #[test]
    fn test_round_status_lower_round_ignored() {
        let (mut engine, _) = setup();
        engine.state.round = 5;

        let status = RoundStatus {
            height: 100,
            round: 2,
            validator: "0xval001".into(),
            signature: Vec::new(),
        };
        let action = engine.on_round_status(&status);
        assert!(matches!(action, BftAction::Wait));
        assert_eq!(engine.round(), 5); // unchanged
    }

    #[test]
    fn test_build_round_status() {
        let (engine, _) = setup();
        let status = engine.build_round_status();
        assert_eq!(status.height, 100);
        assert_eq!(status.round, 0);
        assert_eq!(status.validator, "0xval000");
    }

    #[test]
    fn test_partition_recovery_catches_up_via_round_status() {
        // RoundStatus from peers 2+ rounds ahead triggers catch-up to
        // peer_round - 1. This fixes the round desync stall.
        let (mut engine, _) = setup();
        assert_eq!(engine.round(), 0);

        // Receive round status from peer at round 3 → catch up to round 2
        let status = RoundStatus {
            height: 100,
            round: 3,
            validator: "0xval001".into(),
            signature: Vec::new(),
        };
        engine.on_round_status(&status);
        assert_eq!(engine.round(), 2); // caught up to peer - 1

        // Second peer also at round 3 → no further catch-up (already at 2)
        let status2 = RoundStatus {
            height: 100,
            round: 3,
            validator: "0xval002".into(),
            signature: Vec::new(),
        };
        engine.on_round_status(&status2);
        assert_eq!(engine.round(), 2); // stays at 2
    }

    #[test]
    fn test_propose_timeout_values() {
        assert_eq!(propose_timeout(0), Duration::from_millis(10_000));
        assert_eq!(propose_timeout(1), Duration::from_millis(11_000));
        assert_eq!(propose_timeout(10), Duration::from_millis(20_000));
        assert_eq!(propose_timeout(100), Duration::from_millis(30_000)); // capped
    }

    #[test]
    fn test_prevote_timeout_values() {
        assert_eq!(prevote_timeout(0), Duration::from_millis(10_000));
        assert_eq!(prevote_timeout(1), Duration::from_millis(12_000));
        assert_eq!(prevote_timeout(10), Duration::from_millis(30_000)); // capped
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
                height: 100,
                round: 0,
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
                height: 100,
                round: 0,
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
            BftAction::FinalizeBlock {
                height,
                block_hash,
                justification,
                ..
            } => {
                assert_eq!(height, 500);
                assert_eq!(block_hash, "solo_hash");
                assert!(justification.has_supermajority(our_stake));
            }
            _ => panic!("expected FinalizeBlock, got {:?}", action),
        }
    }

    // ── Issue #143: stake-weighted round skipping ───────────────

    /// Shared fixture for issue-#143 tests: a 4-validator engine with
    /// equal stake (250 each, total 1000). f+1 threshold is 334 stake
    /// (= 1000/3 + 1), i.e. 2 validators crossing the one-third boundary.
    fn setup_143() -> BftEngine {
        BftEngine::new(100, "0xself".into(), 1000)
    }

    fn status(validator: &str, round: u32) -> RoundStatus {
        RoundStatus {
            height: 100,
            round,
            validator: validator.into(),
            signature: Vec::new(),
        }
    }

    #[test]
    fn test_143_f_plus_one_peers_at_same_round_triggers_catch_up() {
        // 2 of 4 peers at round 1 → f+1 (2 peers × 250 stake = 500 > 334).
        // Our round is 0, so we should catch up to round 1.
        let mut engine = setup_143();
        assert_eq!(engine.round(), 0);

        let _ = engine.on_round_status_weighted(&status("0xa", 1), 250);
        let action = engine.on_round_status_weighted(&status("0xb", 1), 250);

        assert!(
            matches!(action, BftAction::BroadcastPrevote(ref p) if p.round == 1 && p.block_hash.is_none()),
            "expected nil prevote at round 1 after f+1 peers reported, got {:?}",
            action,
        );
        assert_eq!(engine.round(), 1);
        assert!(engine.state.our_prevote_cast);
    }

    #[test]
    fn test_143_single_peer_one_round_ahead_does_not_trigger() {
        // Only 1/4 peers (250/1000 = 25%) at round 1 — below the 1/3 threshold.
        // Our round stays at 0, matching the pre-#143 safety: we don't jump
        // on a single voice.
        let mut engine = setup_143();
        let action = engine.on_round_status_weighted(&status("0xa", 1), 250);
        assert!(matches!(action, BftAction::Wait));
        assert_eq!(engine.round(), 0);
    }

    #[test]
    fn test_143_peers_spread_across_rounds_picks_max_with_quorum() {
        // 2 peers at round 3, 1 peer at round 5. f+1 threshold = 334.
        //
        // We jump to round 3 (the highest round where f+1 stake converges).
        // The lone peer at round 5 does NOT drag us further — that's only
        // f (one voice), and a lying peer could equally well claim round
        // 5 without having reached it.
        let mut engine = setup_143();
        let a1 = engine.on_round_status_weighted(&status("0xa", 3), 250);
        let a2 = engine.on_round_status_weighted(&status("0xb", 3), 250);
        let a3 = engine.on_round_status_weighted(&status("0xc", 5), 250);

        // First report: only 1 peer at round 3 → below threshold, no skip.
        assert!(matches!(a1, BftAction::Wait), "a1 = {:?}", a1);
        // Second report: 2 peers at round 3 → cumulative 500 stake, crosses
        // the 334 threshold, so we jump to round 3.
        assert!(
            matches!(a2, BftAction::BroadcastPrevote(ref p) if p.round == 3),
            "expected catch-up to round 3 at a2, got {:?}",
            a2,
        );
        // Third report: one peer at round 5 is below threshold on its own,
        // so we stay at round 3. This is the anti-single-liar property.
        assert!(matches!(a3, BftAction::Wait), "a3 = {:?}", a3);
        assert_eq!(engine.round(), 3);
    }

    #[test]
    fn test_143_peer_at_same_round_does_not_trigger() {
        // A peer reporting OUR OWN round should never trigger a skip.
        let mut engine = setup_143();
        let action = engine.on_round_status_weighted(&status("0xa", 0), 250);
        assert!(matches!(action, BftAction::Wait));
        assert_eq!(engine.round(), 0);
    }

    #[test]
    fn test_143_repeated_report_from_same_peer_is_idempotent() {
        // Same peer reporting the same round twice must not double-count
        // their stake — otherwise a chatty peer could unilaterally trigger
        // a skip.
        let mut engine = setup_143();
        let a1 = engine.on_round_status_weighted(&status("0xa", 1), 250);
        let a2 = engine.on_round_status_weighted(&status("0xa", 1), 250);
        assert!(matches!(a1, BftAction::Wait));
        assert!(matches!(a2, BftAction::Wait));
        assert_eq!(engine.round(), 0);
    }

    #[test]
    fn test_143_peer_stake_refresh_on_update() {
        // If a peer's stake changes across epoch boundaries, the next
        // RoundStatus from them should overwrite the cached stake.
        let mut engine = setup_143();
        // Peer first reports with very low stake — below threshold alone.
        let _ = engine.on_round_status_weighted(&status("0xa", 1), 1);
        // Second peer confirms at same round.
        let action = engine.on_round_status_weighted(&status("0xb", 1), 250);
        // 1 + 250 = 251 stake, below 334 threshold — no skip yet.
        assert!(matches!(action, BftAction::Wait));

        // Peer 0xa reports again, this time with their actual 250 stake.
        let action = engine.on_round_status_weighted(&status("0xa", 1), 250);
        assert!(
            matches!(action, BftAction::BroadcastPrevote(ref p) if p.round == 1),
            "stake refresh should unlock the skip, got {:?}",
            action,
        );
        assert_eq!(engine.round(), 1);
    }

    #[test]
    fn test_143_higher_height_still_triggers_sync() {
        // Higher-height RoundStatus bypasses the round-skip logic entirely.
        let mut engine = setup_143();
        let action = engine.on_round_status_weighted(&status("0xa", 0), 250);
        assert!(matches!(action, BftAction::Wait));

        let mut s = status("0xa", 0);
        s.height = 101;
        let action = engine.on_round_status_weighted(&s, 250);
        assert!(
            matches!(action, BftAction::SyncNeeded { peer_height: 101 }),
            "higher height must return SyncNeeded, got {:?}",
            action,
        );
    }

    #[test]
    fn test_143_peer_rounds_clear_on_new_height() {
        // After advancing to a new height, the peer_rounds cache must
        // reset — otherwise stale entries from height N could trigger a
        // spurious skip at height N+1.
        let mut engine = setup_143();
        let _ = engine.on_round_status_weighted(&status("0xa", 5), 250);
        let _ = engine.on_round_status_weighted(&status("0xb", 5), 250);
        assert_eq!(engine.round(), 5);

        engine.new_height(101, 1000);
        // At height 101 with fresh state, a single peer at round 1 must
        // NOT trigger a skip (cache was wiped).
        let action = engine.on_round_status_weighted(&status("0xa", 1), 250);
        assert!(matches!(action, BftAction::Wait));
        assert_eq!(engine.round(), 0);
    }

    #[test]
    fn test_143_legacy_on_round_status_still_works() {
        // The back-compat `on_round_status` wrapper preserves the pre-#143
        // single-peer "2+ rounds ahead" trigger for any call site that
        // hasn't migrated to the weighted API yet.
        let mut engine = setup_143();
        let action = engine.on_round_status(&status("0xa", 2));
        assert!(
            matches!(action, BftAction::BroadcastPrevote(ref p) if p.round == 1),
            "legacy path should catch up to peer_round - 1, got {:?}",
            action,
        );
    }
}
