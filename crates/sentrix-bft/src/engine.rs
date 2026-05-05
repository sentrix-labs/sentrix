// bft.rs — BFT consensus state machine (Voyager Phase 2a)
//
// Tendermint-style 3-phase: Propose → Prevote → Precommit → Finalize.
// Proposer selected by weighted round-robin from active DPoS validator set.
// Finality at 2/3+1 stake weight.
//
// The round-state struct, vote tally, and phase timeouts each got
// their own file — this one was past 2k lines and review surface was
// getting painful. Everything is re-exported below so downstream code
// (and the lib.rs umbrella) doesn't have to care where each piece
// physically lives.

mod state;
mod timeouts;
mod vote_collector;

pub use state::{BftPhase, BftRoundState};
pub use timeouts::{
    MAX_ROUND, PRECOMMIT_TIMEOUT_MS, PREVOTE_TIMEOUT_MS, PROPOSE_TIMEOUT_MS, precommit_timeout,
    prevote_timeout, propose_timeout,
};
pub use vote_collector::VoteCollector;

use std::collections::HashMap;
use std::time::{Duration, Instant};
// errors used by integration callers, not directly in this module
use crate::messages::{
    BlockJustification, Precommit, Prevote, RoundStatus, supermajority_threshold,
};
use sentrix_staking::StakeRegistry;

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

        // V3 defense-in-depth: refuse proposals from a jailed or
        // tombstoned validator even if `weighted_proposer` returned
        // their address. This is the original #236 intent.
        //
        // #247 follow-up (2026-04-25 bisect): the ORIGINAL #236 patch
        // ALSO rejected when the proposer was missing from
        // `stake_registry` entirely (the `else` branch). Testnet
        // bisect showed that branch was the v2.1.12 livelock trigger:
        // registry-vs-active_set drift happens in real operational
        // state, and the cure is worse than the disease.
        // `weighted_proposer` ALREADY gated the proposer address
        // against `active_set` at line 377, so trusting that gate is
        // safe. A genuinely-rogue proposer can't slip through because
        // their address won't match `weighted_proposer(height, round)`
        // in the first place.
        //
        // Invariant preserved: jailed/tombstoned validators cannot
        // drive consensus.
        // Invariant dropped: proposer must be in stake_registry
        //   (replaced by: proposer must match weighted_proposer,
        //   which is already enforced via active_set membership).
        if let Some(v) = stake_registry.get_validator(proposer)
            && (v.is_jailed || v.is_tombstoned)
        {
            return BftAction::Wait;
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

    /// Stale-lock relaxation threshold — see `accept_proposal` notes.
    /// 16 was chosen empirically: it is far past the propose+prevote+
    /// precommit timeouts at any tolerable backoff, so by the time a
    /// validator has spent 16 rounds locked without anyone re-proving
    /// the lock, the network has manifestly moved on.
    const STALE_LOCK_ROUND_GAP: u32 = 16;

    fn accept_proposal(&mut self, block_hash: &str) -> BftAction {
        // 2026-04-30 fix for the eager-lock livelock pinned in the
        // 2026-04-28 validator block-773012 divergence runbook. The
        // engine locks on `prevote_supermajority(h)` but doesn't always
        // stash the block bytes (gap 3 documented below) — and once
        // locked-without-bytes, it prevotes nil on every subsequent
        // round's proposal because `locked_hash != block_hash`. With
        // a small validator set the conflicting prevote never reaches
        // the unlock supermajority either, so the chain spins skip
        // rounds forever (live mainnet 2026-04-29 stall at h=921606).
        //
        // The relaxation: if we have a stale lock (no bytes AND many
        // rounds have elapsed since we acquired it), drop the lock so
        // we can prevote the current proposal. "No bytes" means the
        // network never re-presented the locked block to us, so any
        // commitment we hold is a phantom — no peer can verify the
        // signed prevote we'd cast for it. Discarding it is safer than
        // the perpetual liveness loss.
        //
        // Safety reasoning for a 4-validator chain: a real prevote-
        // supermajority for the locked hash would need 3/4. If 3
        // validators actually committed the locked block they'd be at
        // height h+1 by now, and they don't vote at height h's later
        // rounds — so a STALE_LOCK_ROUND_GAP-old lock with no bytes
        // and no observable progress means at most 2 peers ever saw
        // that hash, well under quorum. Releasing the lock cannot fork.
        if let (Some(locked_hash), Some(locked_round)) =
            (&self.state.locked_hash, self.state.locked_round)
            && locked_hash != block_hash
            && self.state.locked_block.is_none()
            && self
                .state
                .round
                .saturating_sub(locked_round)
                >= Self::STALE_LOCK_ROUND_GAP
        {
            tracing::warn!(
                "BFT stale-lock relax: dropping lock on {} acquired at round {} \
                 (current round {} at height {}, no cached bytes) — prevote will \
                 follow the current proposal {}",
                &locked_hash[..locked_hash.len().min(12)],
                locked_round,
                self.state.round,
                self.state.height,
                &block_hash[..block_hash.len().min(12)],
            );
            self.state.locked_hash = None;
            self.state.locked_round = None;
            // locked_block is already None by the guard above; clear
            // staging too so a stale staged-but-never-promoted entry
            // can't bleed into the relaxed prevote.
            self.state.staging_block = None;
        }
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
        // M-15 status:
        //
        //   1. Locked-block re-propose IS implemented (PR #258 engine
        //      cache + PR #259 main.rs wiring). When a validator is
        //      locked on hash H and rotates into proposer slot at a
        //      later round, `build_or_reuse_proposal` in main.rs
        //      consults `locked_proposal_bytes()` and re-broadcasts
        //      cached B(H) instead of building a fresh block. The
        //      "prevote nil on lock conflict" guard below remains
        //      load-bearing — it's how locked validators reject a
        //      fresh-proposal attempt from a peer that lost its cache.
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
        //   3. Open liveness gap: a validator whose libp2p dropped
        //      the round-N Propose message will lock via peer
        //      prevotes but never stash the bytes, so
        //      `locked_proposal_bytes()` returns None for them. If
        //      that validator becomes round-(N+1) proposer they fall
        //      through to `create_block_voyager` and build a
        //      fresh-hash block, which peers reject by lock — round
        //      times out via skip-round. Pinned by
        //      `tests/m15_repropose.rs::test_m15_locked_without_bytes_returns_none_from_accessor`.
        //      Fix candidate: when locked + no cached bytes + we are
        //      proposer, gossip a `RoundStatus` "I need bytes for H"
        //      and prevote nil rather than proposing fresh. Belongs
        //      in main.rs / network layer, not the engine.
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
                // Invariant for `locked_proposal_bytes`: when
                // `locked_block` is Some, its bytes MUST hash to
                // `locked_hash`. PoLC moves the lock to a new hash —
                // any cached bytes from the previous lock are now
                // stale and must be cleared before the new hash is
                // pinned. Without this, a PoLC where staging mismatches
                // the new winner returns Some((new_hash, old_bytes))
                // from `locked_proposal_bytes`, which would corrupt
                // the re-propose path.
                if self.state.locked_hash.as_deref() != Some(h.as_str()) {
                    self.state.locked_block = None;
                }
                self.state.locked_hash = Some(h.clone());
                self.state.locked_round = Some(self.state.round);
                // V2 M-15 Step 3: promote staging → locked if its hash
                // matches what just crossed 2/3+ prevote. Callers that
                // stashed block bytes via `stash_proposal_bytes()` get
                // the cache populated here; callers that didn't stash
                // leave `locked_block` None and fall back to
                // build-new-block behaviour in the validator loop.
                // Staging for a different hash is discarded (take() leaves
                // None regardless); promote only if hashes match.
                if let Some((staged_hash, staged_bytes)) = self.state.staging_block.take()
                    && &staged_hash == h
                {
                    self.state.locked_block = Some(staged_bytes);
                }
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
            (
                precommit.block_hash.clone(),
                precommit.signature.clone(),
                stake,
            ),
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
                    // V1 (re-applied 2026-04-25 on v2.1.16 base): emit REAL
                    // signatures. Filter to precommits that voted for the
                    // finalized hash — nil precommits and precommits for
                    // other hashes don't belong in this block's justification.
                    // With V2 locked-block re-propose now shipped in v2.1.16,
                    // the locked-validator-prevote-nil scenario self-unsticks
                    // via re-propose instead of livelocking, so this emission
                    // path no longer has the timing vulnerability that
                    // surfaced in the v2.1.12 bake.
                    for (val, (vote_hash, sig, w)) in &self.state.precommits {
                        if vote_hash.as_deref() == Some(block_hash.as_str()) {
                            justification.add_precommit(val.clone(), sig.clone(), *w);
                        }
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
                    // Backlog #1d + 2026-05-05 h=2575800 investigation: log
                    // BOTH tallies so we can distinguish:
                    //   - Healthy timeout: prevote_tally thin + precommit nil
                    //     → genuine network silence, recovery via round skip.
                    //   - Network partition: prevote_tally thin OR split-hash
                    //     → upstream peer mesh issue, address libp2p.
                    //   - Silent-thread pattern (h=2575800 case): prevote
                    //     UNANIMOUS yes on hash X, precommit splits hash X /
                    //     nil → some validators flipped between phases. The
                    //     diff IS the diagnostic: validators that prevoted
                    //     yes but didn't precommit yes are the ones whose
                    //     gossip-handler / engine task wedged silently.
                    let prevote_summary: Vec<String> = self
                        .collector
                        .prevote_tally_snapshot()
                        .into_iter()
                        .map(|(label, w)| format!("{label}={w}"))
                        .collect();
                    let tally_summary: Vec<String> = self
                        .collector
                        .precommit_tally_snapshot()
                        .into_iter()
                        .map(|(label, w)| format!("{label}={w}"))
                        .collect();
                    tracing::warn!(
                        "BFT #1d: precommit nil-majority skip at height={} round={} \
                         threshold={} prevote_tally=[{}] precommit_tally=[{}]",
                        self.state.height,
                        self.state.round,
                        supermajority_threshold(self.state.total_active_stake),
                        prevote_summary.join(", "),
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

    /// True when 2/3+ stake-weighted of peers report being at a higher
    /// round than ours. The cluster has moved on; finalising on our
    /// local view risks split-brain because two disjoint subsets at
    /// different rounds can each reach precommit-supermajority for
    /// different blocks at the same height.
    ///
    /// Validator-count-agnostic by design: uses the same
    /// `supermajority_threshold` math that gates everything else,
    /// so it works identically with N=4 or N=100.
    ///
    /// Returns the round to catch up to (the highest round at which
    /// 2/3+ stake-weight of peers are present). The caller should
    /// abort whatever finalise/precommit they were about to commit
    /// and call `catch_up_round(round)` instead.
    pub fn peer_supermajority_higher_round(&self) -> Option<u32> {
        if self.state.total_active_stake == 0 {
            return None;
        }
        let threshold = supermajority_threshold(self.state.total_active_stake);
        let mut peers: Vec<(u32, u64)> = self
            .peer_rounds
            .values()
            .filter(|(r, _)| *r > self.state.round)
            .copied()
            .collect();
        // Highest round first — we want the largest round at which
        // cumulative stake crosses the supermajority threshold, so
        // scanning from the top guarantees we report the right target.
        peers.sort_by_key(|p| std::cmp::Reverse(p.0));

        let mut accumulated: u64 = 0;
        for (round, stake) in peers {
            accumulated = accumulated.saturating_add(stake);
            if accumulated >= threshold {
                return Some(round);
            }
        }
        None
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

    /// V2 M-15 Step 2: stash the bincode-encoded block bytes for the
    /// hash we're about to consider prevoting on. Called by the
    /// validator loop right before `on_proposal` / `on_own_proposal`
    /// delivers the block's hash. If the hash later crosses the 2/3+
    /// prevote supermajority threshold (Step 3), the bytes get
    /// promoted into `locked_block` and stay available for a
    /// re-propose in a later round when this validator is elected
    /// proposer again.
    ///
    /// Idempotent — re-stashing the same hash overwrites with the
    /// fresh bytes (same content anyway). Different hash overwrites
    /// the staging slot, which is fine because staging is per-round.
    ///
    /// Opaque to the BFT crate — we don't deserialise or inspect the
    /// bytes; we only re-emit them later. Keep the BFT layer
    /// storage-agnostic.
    pub fn stash_proposal_bytes(&mut self, block_hash: &str, bytes: Vec<u8>) {
        self.state.staging_block = Some((block_hash.to_string(), bytes));
    }

    /// V2 M-15 Step 3: return the cached block bytes for the currently
    /// locked hash, or None if we're not locked / the cache is missing.
    /// Validator loop calls this at every "we are proposer of a new
    /// round" decision — if Some, re-broadcast the cached block at
    /// the current round instead of building a fresh one. That gives
    /// the chain a path to unstick at tempo when the locked hash was
    /// the same one the chain is trying to finalise.
    ///
    /// Returns None when:
    /// - Not locked (`locked_hash.is_none()`)
    /// - Locked but no bytes cached (caller never stashed — safe
    ///   fall-back to build-new-block behaviour)
    /// - Cache hash doesn't match locked hash (shouldn't happen under
    ///   invariant but defensive — we'd rather build-fresh than
    ///   re-propose stale bytes)
    pub fn locked_proposal_bytes(&self) -> Option<(String, Vec<u8>)> {
        match (&self.state.locked_hash, &self.state.locked_block) {
            (Some(hash), Some(bytes)) => Some((hash.clone(), bytes.clone())),
            _ => None,
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

    /// V2 M-15 Step 3 regression: stash bytes → drive prevote supermajority
    /// → staging promotes to locked_block → `locked_proposal_bytes` returns
    /// the stashed bytes.
    #[test]
    fn test_v2_staging_promotes_to_locked_on_prevote_supermajority() {
        let (mut engine, _reg) = setup();
        let total = engine.state.total_active_stake;
        let per_val = total / 21;

        // Simulate entering Prevote phase by flipping manually — on_proposal
        // path also flips, but here we isolate the promotion logic.
        engine.state.phase = BftPhase::Prevote;

        // Validator loop stashes the block bytes before routing the
        // prevote traffic into the engine.
        let block_bytes = b"arbitrary opaque block bytes for hash_win".to_vec();
        engine.stash_proposal_bytes("hash_win", block_bytes.clone());
        assert!(engine.state.staging_block.is_some(), "staging slot populated");

        // Drive 2/3+ prevote quorum for hash_win. Threshold at
        // `supermajority_threshold(total)` — with 21 validators at
        // `per_val` each, 14 votes covers ~66.67%, need 15 for 2/3+.
        let threshold = supermajority_threshold(total);
        let needed = ((threshold / per_val) + 1) as usize;
        for i in 0..needed {
            let pv = Prevote {
                height: 100,
                round: 0,
                block_hash: Some("hash_win".into()),
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            let _ = engine.on_prevote_weighted(&pv, per_val);
        }

        assert_eq!(engine.state.locked_hash.as_deref(), Some("hash_win"));
        assert!(
            engine.state.locked_block.is_some(),
            "locked_block must be populated after prevote-supermajority promotion"
        );
        assert!(
            engine.state.staging_block.is_none(),
            "staging_block must be consumed (take())"
        );

        // Accessor returns Some for the locked hash.
        let cached = engine.locked_proposal_bytes();
        assert!(cached.is_some(), "accessor returns cached bytes when locked");
        let (cached_hash, cached_bytes) = cached.unwrap();
        assert_eq!(cached_hash, "hash_win");
        assert_eq!(cached_bytes, block_bytes);
    }

    /// V2 regression: staging cleared on advance_round, locked preserved.
    #[test]
    fn test_v2_advance_round_keeps_locked_clears_staging() {
        let (mut engine, _reg) = setup();
        engine.state.locked_hash = Some("hash_locked".into());
        engine.state.locked_block = Some(b"locked-bytes".to_vec());
        engine.state.staging_block = Some(("hash_next".into(), b"stage-bytes".to_vec()));

        engine.state.advance_round();

        assert_eq!(engine.state.locked_hash.as_deref(), Some("hash_locked"));
        assert_eq!(
            engine.state.locked_block.as_deref(),
            Some(&b"locked-bytes"[..])
        );
        assert!(engine.state.staging_block.is_none(), "staging cleared on advance_round");
    }

    /// V2 regression: new_height clears both locked_block and staging_block.
    #[test]
    fn test_v2_new_height_clears_both_caches() {
        let (mut engine, _reg) = setup();
        engine.state.locked_hash = Some("hash_locked".into());
        engine.state.locked_block = Some(b"locked-bytes".to_vec());
        engine.state.staging_block = Some(("hash_next".into(), b"stage-bytes".to_vec()));

        engine.new_height(101, engine.state.total_active_stake);

        assert!(engine.state.locked_hash.is_none());
        assert!(engine.state.locked_block.is_none(), "locked_block cleared on new_height");
        assert!(engine.state.staging_block.is_none(), "staging_block cleared on new_height");
    }

    /// V2 regression: `locked_proposal_bytes` returns None when not locked.
    #[test]
    fn test_v2_locked_proposal_bytes_none_when_unlocked() {
        let (engine, _reg) = setup();
        assert!(engine.locked_proposal_bytes().is_none());
    }

    /// V2 PoLC happy path: lock on A in round 0, PoLC to B in round 1
    /// with matching staging → `locked_block` is REPLACED with bytes_B,
    /// not preserved as bytes_A. This is the symmetric case to
    /// `test_v2_polc_clears_locked_block_when_staging_mismatch` —
    /// together they pin the rule "locked_block always tracks the
    /// currently-locked hash, never an earlier one."
    #[test]
    fn test_v2_polc_replaces_locked_block_when_staging_matches() {
        let (mut engine, _reg) = setup();
        let total = engine.state.total_active_stake;
        let per_val = total / 21;
        let threshold = supermajority_threshold(total);
        let needed = ((threshold / per_val) + 1) as usize;

        // Round 0: lock on hash_A with bytes_A.
        engine.state.phase = BftPhase::Prevote;
        engine.stash_proposal_bytes("hash_A", b"bytes_A".to_vec());
        for i in 0..needed {
            let pv = Prevote {
                height: 100,
                round: 0,
                block_hash: Some("hash_A".into()),
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            let _ = engine.on_prevote_weighted(&pv, per_val);
        }
        assert_eq!(
            engine.locked_proposal_bytes(),
            Some(("hash_A".into(), b"bytes_A".to_vec()))
        );

        // Round 1: PoLC to hash_B, staging matches.
        engine.advance_round();
        engine.state.phase = BftPhase::Prevote;
        engine.stash_proposal_bytes("hash_B", b"bytes_B".to_vec());
        for i in 0..needed {
            let pv = Prevote {
                height: 100,
                round: 1,
                block_hash: Some("hash_B".into()),
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            let _ = engine.on_prevote_weighted(&pv, per_val);
        }

        // PoLC fired: lock + cache both moved to hash_B.
        assert_eq!(engine.state.locked_hash.as_deref(), Some("hash_B"));
        assert_eq!(
            engine.locked_proposal_bytes(),
            Some(("hash_B".into(), b"bytes_B".to_vec())),
            "PoLC with matching staging must replace locked_block with new bytes"
        );
    }

    /// V2 PoLC regression: when a 2/3+ supermajority forms on a hash
    /// DIFFERENT from the currently-locked hash (proof-of-lock-change),
    /// `locked_block` must be replaced with the new winner's bytes —
    /// or cleared if the new winner's bytes weren't staged.
    ///
    /// This test pins the failure mode where staging.hash != quorum.hash
    /// AND we were already locked on a third (or first) hash: the old
    /// `locked_block` must NOT survive into a state where `locked_hash`
    /// has moved on. If it does, `locked_proposal_bytes()` returns
    /// stale bytes whose hash doesn't match the lock — corrupting the
    /// re-propose path.
    #[test]
    fn test_v2_polc_clears_locked_block_when_staging_mismatch() {
        let (mut engine, _reg) = setup();
        let total = engine.state.total_active_stake;
        let per_val = total / 21;
        let threshold = supermajority_threshold(total);
        let needed = ((threshold / per_val) + 1) as usize;

        // Round 0: lock on hash_A with bytes_A.
        engine.state.phase = BftPhase::Prevote;
        engine.stash_proposal_bytes("hash_A", b"bytes_A".to_vec());
        for i in 0..needed {
            let pv = Prevote {
                height: 100,
                round: 0,
                block_hash: Some("hash_A".into()),
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            let _ = engine.on_prevote_weighted(&pv, per_val);
        }
        assert_eq!(engine.state.locked_hash.as_deref(), Some("hash_A"));
        assert_eq!(
            engine.state.locked_block.as_deref(),
            Some(&b"bytes_A"[..])
        );

        // Round 1: simulate the validator stashing bytes for hash_C
        // (decoy — the wrong hash) but the network forming PoLC quorum
        // on hash_B. Could happen if our peers proposed B but our local
        // gossip delivered C first into the staging slot.
        engine.advance_round();
        engine.state.phase = BftPhase::Prevote;
        engine.stash_proposal_bytes("hash_C", b"bytes_C".to_vec());
        for i in 0..needed {
            let pv = Prevote {
                height: 100,
                round: 1,
                block_hash: Some("hash_B".into()),
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            let _ = engine.on_prevote_weighted(&pv, per_val);
        }

        // PoLC fired: lock has moved to hash_B.
        assert_eq!(engine.state.locked_hash.as_deref(), Some("hash_B"));
        // Staging was for hash_C, didn't match — bytes can't be promoted.
        // locked_block must NOT keep the round-0 bytes_A: they hash to A,
        // but the lock now claims B. Either clear locked_block (None)
        // or the bug surfaces via locked_proposal_bytes returning a
        // hash/bytes mismatch — which corrupts re-propose.
        assert!(
            engine.state.locked_block.is_none(),
            "PoLC to a non-staged hash must clear locked_block, not retain stale bytes from \
             a previous lock — got {:?}",
            engine.state.locked_block.as_ref().map(|b| String::from_utf8_lossy(b).into_owned())
        );
        assert!(
            engine.locked_proposal_bytes().is_none(),
            "locked_proposal_bytes must be None when we cannot honour the lock with cached bytes"
        );
    }

    /// V2 regression: staging for a different hash than the quorum winner
    /// is discarded, not promoted. Protects against a byzantine proposer
    /// stashing junk bytes and leaving us with a stale cache.
    #[test]
    fn test_v2_staging_for_wrong_hash_discarded() {
        let (mut engine, _reg) = setup();
        let total = engine.state.total_active_stake;
        let per_val = total / 21;
        engine.state.phase = BftPhase::Prevote;

        // Stash bytes for hash_A...
        engine.stash_proposal_bytes("hash_A", b"A-bytes".to_vec());

        // ...but quorum forms on hash_B.
        let threshold = supermajority_threshold(total);
        let needed = ((threshold / per_val) + 1) as usize;
        for i in 0..needed {
            let pv = Prevote {
                height: 100,
                round: 0,
                block_hash: Some("hash_B".into()),
                validator: format!("0xval{:03}", i),
                signature: vec![],
            };
            let _ = engine.on_prevote_weighted(&pv, per_val);
        }

        assert_eq!(engine.state.locked_hash.as_deref(), Some("hash_B"));
        assert!(
            engine.state.locked_block.is_none(),
            "wrong-hash staging must be discarded, not promoted"
        );
        assert!(engine.state.staging_block.is_none(), "staging consumed regardless");
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
        // Base 20_000ms + 1_000ms per round, capped at 30_000ms.
        assert_eq!(propose_timeout(0), Duration::from_millis(20_000));
        assert_eq!(propose_timeout(1), Duration::from_millis(21_000));
        assert_eq!(propose_timeout(10), Duration::from_millis(30_000)); // capped
        assert_eq!(propose_timeout(100), Duration::from_millis(30_000)); // capped
    }

    #[test]
    fn test_prevote_timeout_values() {
        // Base 12_000ms + 2_000ms per round, capped at 30_000ms.
        assert_eq!(prevote_timeout(0), Duration::from_millis(12_000));
        assert_eq!(prevote_timeout(1), Duration::from_millis(14_000));
        assert_eq!(prevote_timeout(9), Duration::from_millis(30_000)); // capped
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

    /// V3 regression: `on_proposal` must reject a proposal whose
    /// proposer address is currently jailed, even if `weighted_proposer`
    /// happens to return that address (e.g. stale active_set). Before
    /// the fix, the only gate was "expected == proposer"; nothing
    /// cross-referenced the jail flag, so a jailed validator could
    /// push a block through purely by being in the right rotation slot.
    ///
    /// Race scenario being guarded: slashing applies → jail() sets
    /// is_jailed + calls update_active_set, but a proposal already in
    /// flight from the now-jailed validator arrives before the next
    /// height boundary. The BFT engine's view of active_set may be a
    /// snapshot from before the jail; the defense-in-depth check
    /// ensures the proposal is still refused.
    #[test]
    fn test_jailed_proposer_rejected_at_on_proposal() {
        let (mut engine, mut reg) = setup();
        // Pick the proposer for (height=100, round=0) — that's what
        // setup() initializes the engine with.
        let proposer = reg.weighted_proposer(100, 0).expect("active_set not empty");

        // Simulate the race: active_set still contains the proposer
        // (engine hasn't been told about the jail yet), but the
        // StakeRegistry has is_jailed=true set on them.
        reg.validators
            .get_mut(&proposer)
            .expect("proposer registered")
            .is_jailed = true;
        // Do NOT call reg.update_active_set() — this simulates the
        // stale-view race. The jailed validator is still in active_set.
        assert!(
            reg.active_set.contains(&proposer),
            "precondition: active_set still includes jailed proposer"
        );

        let action = engine.on_proposal("blockhash_abc", &proposer, &reg);
        assert!(
            matches!(action, BftAction::Wait),
            "jailed proposer's proposal must be Wait (rejected), got {:?}",
            action
        );
    }

    /// V1 regression: BlockJustification emitted at finalization must
    /// contain the REAL precommit signature bytes, not `vec![]`
    /// placeholders. Before the fix, the emit loop at the 2/3+ pivot
    /// passed `vec![]` to `add_precommit` regardless of what signature
    /// each peer actually sent, so the justification was cryptographically
    /// meaningless — a silent-reorg surface the moment Voyager activated.
    ///
    /// Also verifies that nil precommits and precommits for a different
    /// hash are NOT included in the finalized block's justification
    /// (only the winning hash's backers should be counted).
    #[test]
    
    fn test_finalize_emits_real_precommit_signatures() {
        let (mut engine, _) = setup();
        let total = engine.state.total_active_stake;
        engine.state.phase = BftPhase::Precommit;

        let per_val = total / 21;
        let threshold = supermajority_threshold(total);
        let needed = ((threshold / per_val) + 1) as usize;

        let winning_hash = "hash_abc".to_string();

        // Throw in one nil-precommit and one wrong-hash precommit FIRST —
        // they should land in state.precommits but NOT in the final
        // justification.
        let nil_pc = Precommit {
            height: 100,
            round: 0,
            block_hash: None,
            validator: "0xval100".into(),
            signature: vec![0xDE, 0xAD],
        };
        // 0xval100 isn't in active_set but that's OK for this unit —
        // we're just stuffing the state map directly via on_precommit.
        // The nil precommit lands because the dedup is per-validator.
        // Use an address the engine doesn't gate on active_set membership for;
        // on_precommit_weighted doesn't check membership.
        let _ = engine.on_precommit_weighted(&nil_pc, per_val);

        let wrong_pc = Precommit {
            height: 100,
            round: 0,
            block_hash: Some("hash_xyz_wrong".into()),
            validator: "0xval101".into(),
            signature: vec![0xBE, 0xEF],
        };
        let _ = engine.on_precommit_weighted(&wrong_pc, per_val);

        // Now the winning hash precommits — each with a DIFFERENT
        // signature so we can spot preservation.
        let mut final_justification = None;
        for i in 0..needed {
            let sig: Vec<u8> = vec![0xA0 + i as u8, 0xB0 + i as u8, 0xC0 + i as u8];
            let pc = Precommit {
                height: 100,
                round: 0,
                block_hash: Some(winning_hash.clone()),
                validator: format!("0xval{:03}", i),
                signature: sig.clone(),
            };
            let action = engine.on_precommit_weighted(&pc, per_val);
            if let BftAction::FinalizeBlock { justification, .. } = action {
                final_justification = Some(justification);
                break;
            }
        }

        let just = final_justification.expect("supermajority should finalize");

        // Every included precommit MUST have a non-empty signature.
        for sp in &just.precommits {
            assert!(
                !sp.signature.is_empty(),
                "justification must not contain vec![] placeholder sigs (V1)"
            );
            // And each signature must start with the 0xA0+i / 0xB0+i / 0xC0+i
            // pattern we sent — confirming the bytes were preserved, not
            // regenerated or zeroed.
            assert_eq!(sp.signature.len(), 3);
            assert_eq!(sp.signature[0] & 0xF0, 0xA0);
        }

        // Nil precommit and wrong-hash precommit MUST NOT appear.
        let validators_in_just: Vec<&str> =
            just.precommits.iter().map(|p| p.validator.as_str()).collect();
        assert!(!validators_in_just.contains(&"0xval100"), "nil precommit leaked into justification");
        assert!(!validators_in_just.contains(&"0xval101"), "wrong-hash precommit leaked into justification");

        // And the reported block_hash matches the winning hash.
        assert_eq!(just.block_hash, winning_hash);
    }

    /// V3 regression: proposer address not in the stake registry at all
    /// (e.g. a removed validator replaying an old gossip message) is
    /// post-#247 bisect: if a validator sits in active_set but is
    /// missing from `stake_registry.validators`, the proposal is
    /// now ACCEPTED (not rejected). Rationale: `weighted_proposer`
    /// already gated the proposer address against active_set at
    /// line 377. active_set mutation requires privileged admin
    /// access — a wire-level attacker cannot inject a rogue
    /// proposer here. The old behaviour (reject on registry-miss)
    /// caused testnet livelock when registry lagged active_set
    /// (the 2026-04-25 v2.1.12 regression tracked in issue #247).
    #[test]
    fn test_active_set_proposer_accepted_when_missing_from_registry_validators() {
        let (mut engine, reg) = setup();
        engine.state.phase = BftPhase::Propose;
        let mut reg = reg;
        // Force weighted_proposer(100, 0) to return "0xdeadbeef" by
        // putting it at the slot the round-robin picks, then call
        // on_proposal with that address. With the old #236 behaviour
        // this would return Wait; with the #247 bisect fix it must
        // accept (weighted_proposer already gated the rotation).
        reg.active_set.clear();
        reg.active_set.push("0xdeadbeef".to_string());
        let action = engine.on_proposal("blockhash_abc", "0xdeadbeef", &reg);
        assert!(
            !matches!(action, BftAction::Wait),
            "active-set proposer should be accepted even if not in registry.validators (post-#247 bisect); got {:?}",
            action
        );
    }

    /// 2026-04-30 regression for the eager-lock livelock pinned in
    /// the 2026-04-28 validator block-773012 divergence runbook.
    ///
    /// Setup: validator is locked on hash A acquired at round 0 with
    /// no cached block bytes. Round advances 16+ times (the threshold)
    /// without anyone re-presenting A. A fresh proposal for hash B
    /// arrives. Without the relax the validator prevotes nil forever;
    /// with the relax it drops the stale lock and prevotes B so the
    /// chain can finalize.
    #[test]
    fn test_stale_lock_with_missing_bytes_relaxes_to_current_proposal() {
        let (mut engine, reg) = setup();
        engine.state.locked_hash = Some("hash_a".to_string());
        engine.state.locked_round = Some(0);
        engine.state.locked_block = None; // the gap-3 condition
        // Advance to round 17 (one past STALE_LOCK_ROUND_GAP=16).
        engine.state.round = 17;
        engine.state.phase = BftPhase::Propose;
        engine.state.our_prevote_cast = false;

        let proposer = reg
            .weighted_proposer(engine.state.height, engine.state.round)
            .expect("weighted_proposer must resolve for the test height/round");

        let action = engine.on_proposal("hash_b", &proposer, &reg);

        match action {
            BftAction::BroadcastPrevote(pv) => {
                assert_eq!(
                    pv.block_hash.as_deref(),
                    Some("hash_b"),
                    "stale-lock relax must prevote the current proposal, not nil"
                );
            }
            other => panic!("expected BroadcastPrevote(hash_b); got {:?}", other),
        }
        assert!(
            engine.state.locked_hash.is_none(),
            "stale lock must be cleared so a fresh polka can re-acquire it"
        );
    }

    /// Inverse of the above — a FRESH lock (within the gap) must keep
    /// prevoting nil on a conflicting proposal. Pins the safety side
    /// of the relax: we only drop the lock when it has been stale for
    /// the full `STALE_LOCK_ROUND_GAP` window.
    #[test]
    fn test_fresh_lock_still_prevotes_nil_on_conflict() {
        let (mut engine, reg) = setup();
        engine.state.locked_hash = Some("hash_a".to_string());
        engine.state.locked_round = Some(5);
        engine.state.locked_block = None;
        // Round 6 is well within the 16-round freshness window.
        engine.state.round = 6;
        engine.state.phase = BftPhase::Propose;
        engine.state.our_prevote_cast = false;

        let proposer = reg
            .weighted_proposer(engine.state.height, engine.state.round)
            .expect("weighted_proposer must resolve for the test height/round");

        let action = engine.on_proposal("hash_b", &proposer, &reg);

        match action {
            BftAction::BroadcastPrevote(pv) => {
                assert_eq!(
                    pv.block_hash, None,
                    "fresh lock on a different hash must still prevote nil"
                );
            }
            other => panic!("expected BroadcastPrevote(nil); got {:?}", other),
        }
        assert_eq!(
            engine.state.locked_hash.as_deref(),
            Some("hash_a"),
            "fresh lock must NOT be cleared by the stale-lock relax"
        );
    }

    /// 2026-04-30 regression for `peer_supermajority_higher_round()` — the
    /// validator-count-agnostic split-brain guard. Pins both directions:
    ///   - empty peer_rounds → None (engine alone can't be split)
    ///   - 2/3+ stake at higher round → Some(target_round)
    ///   - exactly 1/3+ stake at higher round (NOT 2/3+) → None (below
    ///     the supermajority threshold; we should not abort our finalise)
    #[test]
    fn test_peer_supermajority_higher_round_returns_target_when_majority_moved_on() {
        let (mut engine, _reg) = setup();
        // 4 equal-stake validators. total=4_000_000. supermajority threshold
        // = 4_000_000 * 2/3 + 1 = 2_666_667. Three peers at one stake unit
        // each is 3_000_000 — well above the threshold.
        engine.state.total_active_stake = 4_000_000;
        engine.state.round = 5;
        engine.peer_rounds.insert("peer_a".into(), (10, 1_000_000));
        engine.peer_rounds.insert("peer_b".into(), (10, 1_000_000));
        engine.peer_rounds.insert("peer_c".into(), (10, 1_000_000));

        let target = engine.peer_supermajority_higher_round();
        assert_eq!(
            target,
            Some(10),
            "3-of-4 stake at round 10 must trip the guard; got {:?}",
            target
        );
    }

    #[test]
    fn test_peer_supermajority_higher_round_silent_below_threshold() {
        let (mut engine, _reg) = setup();
        engine.state.total_active_stake = 4_000_000;
        engine.state.round = 5;
        // Only 1 peer at higher round → 1_000_000 stake → 25% → below 2/3.
        engine.peer_rounds.insert("peer_a".into(), (10, 1_000_000));

        assert_eq!(
            engine.peer_supermajority_higher_round(),
            None,
            "single peer below supermajority threshold must NOT trip the guard",
        );
    }

    #[test]
    fn test_peer_supermajority_higher_round_silent_when_peers_at_our_round() {
        let (mut engine, _reg) = setup();
        engine.state.total_active_stake = 4_000_000;
        engine.state.round = 5;
        // Three peers at the SAME round as us — not "ahead", so the guard
        // must stay silent. We're not stale.
        engine.peer_rounds.insert("peer_a".into(), (5, 1_000_000));
        engine.peer_rounds.insert("peer_b".into(), (5, 1_000_000));
        engine.peer_rounds.insert("peer_c".into(), (5, 1_000_000));

        assert_eq!(
            engine.peer_supermajority_higher_round(),
            None,
            "peers at our own round must NOT trip the guard",
        );
    }
}
