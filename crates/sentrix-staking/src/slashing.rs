// slashing.rs — Liveness tracking + double-sign detection (Voyager Phase 2a)
//
// Downtime: missed >70% in 14_400-block sliding window → 0.1% slash + 600 blocks jail
// Double-sign: two blocks same height from same validator → 20% slash + permaban
//
// 2026-04-22 tuning — Sentrix-style liveness (replaced Tendermint defaults).
// The previous 100-block / 50% configuration was Tendermint's reference demo
// default. At Sentrix's 1s block time that gave operators a ~50-second
// downtime budget before auto-jail — far too tight for realistic ops
// (kernel upgrades, VPS provider maintenance, fast-deploy rolling
// restarts all routinely exceed it). Observed symptom: every `fast-deploy
// testnet` restart rolled the 4 validators through their 3-5s startup,
// which tripped the liveness threshold within a single deploy and
// auto-jailed the whole set. Happened 3× today alone.
//
// New values are tuned for Sentrix's actual operating profile:
//  - 1-second block time (faster than Cosmos's 6s, so we need a longer
//    window to tolerate the same real-time downtime)
//  - Solo-operator scale (not datacenter HA — occasional human-in-the-loop
//    outages are normal)
//  - Target validator count 21 (individual reliability matters)
//  - Weekly deploy cadence (not quarterly)
//
// Rationale for each constant inline below.

// The two pieces of state this engine keeps — the sliding-window
// liveness tracker and the double-sign detector — live in their own
// files (everything got too cramped in here once the consensus-jail
// dispatch landed). Re-export so the rest of the codebase still does
// `use sentrix_staking::slashing::LivenessTracker` etc.
mod double_sign;
mod liveness;

pub use double_sign::{DOUBLE_SIGN_SLASH_BP, DoubleSignDetector, DoubleSignEvidence};
pub use liveness::{
    DOWNTIME_JAIL_BLOCKS, DOWNTIME_SLASH_BP, LIVENESS_WINDOW, LivenessTracker, MIN_SIGNED_PER_WINDOW,
};

use crate::staking::StakeRegistry;
use sentrix_primitives::transaction::JailEvidence;
use sentrix_primitives::{SentrixError, SentrixResult};
use serde::{Deserialize, Serialize};


// ── Slashing Engine ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SlashingEngine {
    pub liveness: LivenessTracker,
    pub double_sign: DoubleSignDetector,
    /// Total tokens slashed (burned) since genesis
    pub total_slashed: u64,
}

impl SlashingEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Check liveness for all active validators and slash if needed.
    /// Returns list of (validator, slash_amount).
    pub fn check_liveness(
        &mut self,
        stake_registry: &mut StakeRegistry,
        active_set: &[String],
        current_height: u64,
    ) -> Vec<(String, u64)> {
        let mut slashed = Vec::new();

        for validator in active_set {
            if !self.liveness.is_downtime(validator) {
                continue;
            }

            // Already jailed? Skip
            if let Some(v) = stake_registry.get_validator(validator)
                && v.is_jailed
            {
                continue;
            }

            // Slash + jail
            if let Ok(amount) = stake_registry.slash(validator, DOWNTIME_SLASH_BP) {
                let _ = stake_registry.jail(validator, DOWNTIME_JAIL_BLOCKS, current_height);
                self.liveness.reset(validator);
                self.total_slashed = self.total_slashed.saturating_add(amount);
                slashed.push((validator.clone(), amount));
            }
        }

        slashed
    }

    /// Process double-sign evidence. Returns slash amount if valid.
    pub fn process_double_sign(
        &mut self,
        stake_registry: &mut StakeRegistry,
        evidence: &DoubleSignEvidence,
    ) -> SentrixResult<u64> {
        // Already tombstoned?
        if let Some(v) = stake_registry.get_validator(&evidence.validator)
            && v.is_tombstoned
        {
            return Err(SentrixError::InvalidTransaction(
                "validator already tombstoned".into(),
            ));
        }

        let is_new = self.double_sign.process_evidence(evidence)?;
        if !is_new {
            return Ok(0); // already processed
        }

        let amount = stake_registry.slash(&evidence.validator, DOUBLE_SIGN_SLASH_BP)?;
        stake_registry.tombstone(&evidence.validator)?;
        self.total_slashed = self.total_slashed.saturating_add(amount);

        Ok(amount)
    }

    /// Record liveness from a finalized block's signers.
    ///
    /// 2026-04-29: rewrote this to ONLY add signed=true entries for the
    /// addresses in `signers`. Previously we iterated `active_set` and
    /// derived a signed bool by checking membership — but each node's
    /// `active_set` view differed in catchup / post-jail / mid-restart
    /// scenarios, so the LivenessTracker drifted across validators and
    /// `compute_jail_evidence` produced different bundles → mainnet
    /// halt at h=892799 on 2026-04-29 with `local=0 vs claimed=1`.
    /// Recording from `signers` only (which IS canonical — every node
    /// sees the same `block.justification.precommits`) keeps trackers
    /// in lockstep across the fleet.
    ///
    /// `_active_set` is kept in the signature for compat with the
    /// existing call sites in main.rs / libp2p_node.rs and only used
    /// for the periodic snapshot logging below.
    ///
    /// Per-validator participation snapshot every 1000 blocks
    /// (RUST_LOG=info). The point is fleet-wide divergence detection —
    /// pre-2026-04-29 this metric WAS the smoking gun for jail-counter
    /// drift; post-fix it should stay monotone across all four
    /// validators. See `audits/jail-cascade-root-cause-analysis.md`.
    pub fn record_block_signatures(
        &mut self,
        active_set: &[String],
        signers: &[String],
        height: u64,
    ) {
        for signer in signers {
            self.liveness.record_signed(signer, height);
        }

        // Periodic per-validator participation snapshot (every 1000 blocks)
        // for fleet-wide correlation. INFO-level so operators see it by
        // default without enabling DEBUG logs — that's the whole point of
        // the metric (divergence detection requires fleet-wide visibility,
        // can't ask operator to remember to enable extra logging).
        // Volume: ~4 lines / 1000 blocks / validator = ~16 lines/hr per
        // validator at 1s blocks. Low enough to not be noise.
        if height.is_multiple_of(1000) {
            for validator in active_set {
                let (signed_count, missed_count) = self.liveness.get_stats(validator);
                tracing::info!(
                    target: "sentrix_staking::slashing",
                    height,
                    validator = %validator,
                    signed = signed_count,
                    missed = missed_count,
                    "jail counter snapshot"
                );
            }
        }
    }

    /// Compute the deterministic JailEvidence list for the consensus-jail
    /// dispatch.
    ///
    /// Caller is the epoch-boundary block proposer. Output goes into a
    /// `StakingOp::JailEvidenceBundle` transaction; verifiers re-run this
    /// function and reject the block if the recomputed list doesn't match
    /// what the proposer claimed (byte-equal). That's the consensus rule
    /// that lets the chain agree on a single jail decision instead of
    /// each validator deciding locally.
    ///
    /// 2026-04-29: rewrote to take `current_height` and call
    /// `is_downtime_at` instead of `is_downtime`. The old form depended
    /// on entry counts in the LivenessTracker, which drifted across
    /// validators because the recording path used per-node `active_set`
    /// (see `record_block_signatures` for the matching fix). With both
    /// halves canonical — recording from `signers`, downtime keyed by
    /// height — peers produce identical evidence given the same chain
    /// history.
    ///
    /// `justification_hashes` is still empty here (Phase B initial
    /// shape); count-based verification is enough for the deterministic
    /// equality check.
    pub fn compute_jail_evidence(
        &self,
        active_set: &[String],
        current_height: u64,
    ) -> Vec<JailEvidence> {
        let mut evidence = Vec::new();
        for validator in active_set {
            if !self.liveness.is_downtime_at(validator, current_height) {
                continue;
            }
            let (signed_count, missed_count) = self.liveness.get_stats(validator);
            evidence.push(JailEvidence {
                validator: validator.clone(),
                signed_count,
                missed_count,
                // Phase B initial: count-based verification only.
                // Phase C will populate with actual missed-block hashes.
                justification_hashes: Vec::new(),
            });
        }
        evidence
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::staking::MIN_SELF_STAKE;

    fn setup_registry() -> StakeRegistry {
        let mut reg = StakeRegistry::new();
        reg.register_validator("0xval1", MIN_SELF_STAKE, 1000, 0)
            .unwrap();
        reg.register_validator("0xval2", MIN_SELF_STAKE, 1000, 0)
            .unwrap();
        reg.register_validator("0xval3", MIN_SELF_STAKE, 1000, 0)
            .unwrap();
        reg.update_active_set();
        reg
    }

    // ── LivenessTracker tests ────────────────────────────────

    // ── LivenessTracker tests ────────────────────────────────
    // Tests use the LIVENESS_WINDOW / MIN_SIGNED_PER_WINDOW constants
    // so they stay correct if those values are re-tuned. Iteration
    // count is LIVENESS_WINDOW (= 14_400 currently); each record() is
    // a ~nanosecond operation, so even the full-window tests finish
    // under a millisecond.

    #[test]
    fn test_liveness_no_downtime() {
        let mut tracker = LivenessTracker::new();
        for h in 0..LIVENESS_WINDOW {
            tracker.record("0xval1", h, true);
        }
        assert!(!tracker.is_downtime("0xval1"));
    }

    #[test]
    fn test_liveness_downtime_detected() {
        let mut tracker = LivenessTracker::new();
        // Sign one below the minimum threshold — should trip downtime.
        let signed_count = MIN_SIGNED_PER_WINDOW - 1;
        for h in 0..LIVENESS_WINDOW {
            tracker.record("0xval1", h, h < signed_count);
        }
        assert!(tracker.is_downtime("0xval1"));
    }

    #[test]
    fn test_liveness_exactly_threshold() {
        let mut tracker = LivenessTracker::new();
        // Sign exactly MIN_SIGNED_PER_WINDOW (at threshold, NOT downtime).
        for h in 0..LIVENESS_WINDOW {
            tracker.record("0xval1", h, h < MIN_SIGNED_PER_WINDOW);
        }
        assert!(!tracker.is_downtime("0xval1"));
    }

    #[test]
    fn test_liveness_window_not_full() {
        let mut tracker = LivenessTracker::new();
        // Half-window of all-missed — not full yet, no downtime even though every entry is a miss.
        let half = LIVENESS_WINDOW / 2;
        for h in 0..half {
            tracker.record("0xval1", h, false);
        }
        assert!(!tracker.is_downtime("0xval1"));
    }

    #[test]
    fn test_liveness_sliding_window() {
        let mut tracker = LivenessTracker::new();
        // First full window: all missed — downtime fires.
        for h in 0..LIVENESS_WINDOW {
            tracker.record("0xval1", h, false);
        }
        assert!(tracker.is_downtime("0xval1"));

        // Next full window: all signed — sliding window replaces old entries, no more downtime.
        for h in LIVENESS_WINDOW..(LIVENESS_WINDOW * 2) {
            tracker.record("0xval1", h, true);
        }
        assert!(!tracker.is_downtime("0xval1"));
    }

    #[test]
    fn test_liveness_get_stats() {
        let mut tracker = LivenessTracker::new();
        for h in 0..10 {
            tracker.record("0xval1", h, h % 2 == 0); // 5 signed, 5 missed
        }
        let (signed, missed) = tracker.get_stats("0xval1");
        assert_eq!(signed, 5);
        assert_eq!(missed, 5);
    }

    #[test]
    fn test_liveness_reset() {
        let mut tracker = LivenessTracker::new();
        for h in 0..LIVENESS_WINDOW {
            tracker.record("0xval1", h, false);
        }
        assert!(tracker.is_downtime("0xval1"));
        tracker.reset("0xval1");
        assert!(!tracker.is_downtime("0xval1"));
    }

    /// 2026-04-22 Sentrix-style tuning regression test — 30-min outage
    /// (1_800 blocks missed at end of window) is within tolerance.
    #[test]
    fn test_liveness_tolerates_30min_outage() {
        let mut tracker = LivenessTracker::new();
        // First 14_400 - 1_800 = 12_600 blocks signed, last 1_800 missed (30 min offline).
        let signed_cutoff = LIVENESS_WINDOW - 1_800;
        for h in 0..LIVENESS_WINDOW {
            tracker.record("0xval1", h, h < signed_cutoff);
        }
        assert!(
            !tracker.is_downtime("0xval1"),
            "30-min outage in a 4-hour window must not auto-jail — \
             that's the realistic recovery window for solo-dev ops"
        );
    }

    #[test]
    fn test_liveness_unknown_validator() {
        let tracker = LivenessTracker::new();
        assert!(!tracker.is_downtime("0xunknown"));
        assert_eq!(tracker.get_stats("0xunknown"), (0, 0));
    }

    // ── DoubleSignDetector tests ─────────────────────────────

    #[test]
    fn test_double_sign_detected() {
        let mut detector = DoubleSignDetector::new();
        // First block at height 100
        let result = detector.record_block("0xval1", 100, "hash_a", "sig_a");
        assert!(result.is_none());

        // Different block at same height = double-sign
        let result = detector.record_block("0xval1", 100, "hash_b", "sig_b");
        assert!(result.is_some());
        let evidence = result.unwrap();
        assert_eq!(evidence.validator, "0xval1");
        assert_eq!(evidence.height, 100);
        assert_eq!(evidence.block_hash_a, "hash_a");
        assert_eq!(evidence.block_hash_b, "hash_b");
    }

    #[test]
    fn test_double_sign_same_hash_ok() {
        let mut detector = DoubleSignDetector::new();
        detector.record_block("0xval1", 100, "hash_a", "sig_a");
        let result = detector.record_block("0xval1", 100, "hash_a", "sig_a");
        assert!(result.is_none()); // same hash, not a double-sign
    }

    #[test]
    fn test_double_sign_different_heights_ok() {
        let mut detector = DoubleSignDetector::new();
        detector.record_block("0xval1", 100, "hash_a", "sig_a");
        let result = detector.record_block("0xval1", 101, "hash_b", "sig_b");
        assert!(result.is_none()); // different heights, not a double-sign
    }

    #[test]
    fn test_double_sign_different_validators_ok() {
        let mut detector = DoubleSignDetector::new();
        detector.record_block("0xval1", 100, "hash_a", "sig_a");
        let result = detector.record_block("0xval2", 100, "hash_b", "sig_b");
        assert!(result.is_none()); // different validators
    }

    #[test]
    fn test_evidence_processing() {
        let mut detector = DoubleSignDetector::new();
        let evidence = DoubleSignEvidence {
            validator: "0xval1".into(),
            height: 100,
            block_hash_a: "hash_a".into(),
            block_hash_b: "hash_b".into(),
            signature_a: "sig_a".into(),
            signature_b: "sig_b".into(),
        };

        // First time: processed
        assert!(detector.process_evidence(&evidence).unwrap());

        // Second time: already processed
        assert!(!detector.process_evidence(&evidence).unwrap());
    }

    #[test]
    fn test_evidence_same_hash_rejected() {
        let mut detector = DoubleSignDetector::new();
        let evidence = DoubleSignEvidence {
            validator: "0xval1".into(),
            height: 100,
            block_hash_a: "same_hash".into(),
            block_hash_b: "same_hash".into(),
            signature_a: "sig_a".into(),
            signature_b: "sig_b".into(),
        };
        assert!(detector.process_evidence(&evidence).is_err());
    }

    // ── SlashingEngine tests ─────────────────────────────────

    #[test]
    fn test_check_liveness_slashes() {
        let mut reg = setup_registry();
        let mut engine = SlashingEngine::new();

        // val1 misses everything in a FULL window. val2/val3 sign everything.
        // Need LIVENESS_WINDOW entries to trip the "window-full" guard.
        for h in 0..LIVENESS_WINDOW {
            engine.liveness.record("0xval1", h, false);
            engine.liveness.record("0xval2", h, true);
            engine.liveness.record("0xval3", h, true);
        }

        let active = vec!["0xval1".into(), "0xval2".into(), "0xval3".into()];
        let slashed = engine.check_liveness(&mut reg, &active, LIVENESS_WINDOW);

        assert_eq!(slashed.len(), 1);
        assert_eq!(slashed[0].0, "0xval1");
        assert!(slashed[0].1 > 0);
        assert!(reg.get_validator("0xval1").unwrap().is_jailed);
    }

    #[test]
    fn test_check_liveness_skips_jailed() {
        let mut reg = setup_registry();
        let mut engine = SlashingEngine::new();

        // val1 already jailed
        reg.jail("0xval1", 500, 0).unwrap();

        for h in 0..LIVENESS_WINDOW {
            engine.liveness.record("0xval1", h, false);
        }

        let active = vec!["0xval1".into()];
        let slashed = engine.check_liveness(&mut reg, &active, LIVENESS_WINDOW);
        assert!(slashed.is_empty()); // skipped because already jailed
    }

    #[test]
    fn test_process_double_sign() {
        let mut reg = setup_registry();
        let mut engine = SlashingEngine::new();

        let evidence = DoubleSignEvidence {
            validator: "0xval1".into(),
            height: 50,
            block_hash_a: "hash_a".into(),
            block_hash_b: "hash_b".into(),
            signature_a: "sig_a".into(),
            signature_b: "sig_b".into(),
        };

        let amount = engine.process_double_sign(&mut reg, &evidence).unwrap();
        assert!(amount > 0);
        let val = reg.get_validator("0xval1").unwrap();
        assert!(val.is_tombstoned);
        assert!(val.is_jailed);
    }

    #[test]
    fn test_double_sign_already_tombstoned() {
        let mut reg = setup_registry();
        let mut engine = SlashingEngine::new();

        reg.tombstone("0xval1").unwrap();

        let evidence = DoubleSignEvidence {
            validator: "0xval1".into(),
            height: 50,
            block_hash_a: "hash_a".into(),
            block_hash_b: "hash_b".into(),
            signature_a: "sig_a".into(),
            signature_b: "sig_b".into(),
        };

        assert!(engine.process_double_sign(&mut reg, &evidence).is_err());
    }

    #[test]
    fn test_record_block_signatures() {
        let mut engine = SlashingEngine::new();
        let active = vec!["0xval1".into(), "0xval2".into(), "0xval3".into()];
        let signers = vec!["0xval1".into(), "0xval3".into()]; // val2 didn't sign

        engine.record_block_signatures(&active, &signers, 100);

        // 2026-04-29 fix: we only record canonical "signed=true" events
        // for addresses in `signers`. Validators who didn't sign get
        // no entry — the absence of recent records is what
        // is_downtime_at uses to flag downtime, not an explicit "missed"
        // counter.
        let (s1, m1) = engine.liveness.get_stats("0xval1");
        assert_eq!(s1, 1);
        assert_eq!(m1, 0);

        let (s2, m2) = engine.liveness.get_stats("0xval2");
        assert_eq!(s2, 0, "val2 didn't sign → no signed entries recorded");
        assert_eq!(m2, 0, "val2 has no entries at all (not signers, not missed)");

        let (s3, _) = engine.liveness.get_stats("0xval3");
        assert_eq!(s3, 1);
    }

    /// #253 regression: a 4-validator BFT chain where every block's
    /// justification carries all four precommits should NOT trigger
    /// downtime jail on any validator, even after a full LIVENESS_WINDOW.
    ///
    /// Before #253's fix, `main.rs` called `record_block_signatures`
    /// with `signers = vec![proposer]`, so each non-proposer validator
    /// was counted as MISSED every block. On a 4-validator chain, that
    /// put each validator at 25% signed vs the 30% MIN_SIGNED_PER_WINDOW
    /// threshold → deterministic cascade-jail every 14400 blocks
    /// (~80min at 3 blocks/sec). This test pins the correct semantics:
    /// every precommit signer in the justification counts as "signed".
    #[test]
    fn test_full_justification_no_cascade_jail() {
        let mut engine = SlashingEngine::new();
        let active = vec![
            "0xval1".into(),
            "0xval2".into(),
            "0xval3".into(),
            "0xval4".into(),
        ];

        // Simulate a full LIVENESS_WINDOW of blocks where every
        // validator signs every block (healthy 4/4 justification).
        for h in 0..LIVENESS_WINDOW {
            engine.record_block_signatures(&active, &active, h);
        }

        for v in &active {
            assert!(
                !engine.liveness.is_downtime(v),
                "#253: validator {} must not be downtime when all 4 precommits are recorded \
                 every block — got downtime after LIVENESS_WINDOW of full participation",
                v
            );
            let (signed, missed) = engine.liveness.get_stats(v);
            assert_eq!(
                signed, LIVENESS_WINDOW,
                "#253: {} should show full signed_count, got signed={} missed={}",
                v, signed, missed
            );
        }
    }

    /// #253 regression: the BROKEN pre-fix model (`signers = vec![proposer]`
    /// where proposer rotates round-robin) deterministically cascade-jails
    /// every validator. This test pins that the broken model WAS indeed
    /// below threshold so the fix is load-bearing, not cosmetic.
    #[test]
    fn test_proposer_only_signers_triggers_cascade_jail() {
        let mut engine = SlashingEngine::new();
        let active: Vec<String> =
            (0..4).map(|i| format!("0xval{}", i + 1)).collect();

        // Simulate LIVENESS_WINDOW blocks with the BROKEN model:
        // only the rotating proposer goes into `signers`.
        for h in 0..LIVENESS_WINDOW {
            let proposer_idx = (h as usize) % active.len();
            let signers = vec![active[proposer_idx].clone()];
            engine.record_block_signatures(&active, &signers, h);
        }

        // Each validator signed exactly 1/4 of blocks = 25% < 30% threshold.
        // Under the deterministic is_downtime_at path (which the
        // consensus-jail dispatch uses), each validator is downtime
        // because their signed_in_window count falls below the threshold.
        // current_height = LIVENESS_WINDOW + 4 so the val that first
        // signed at h=3 is also past the grace boundary.
        let current_height = LIVENESS_WINDOW + 4;
        for v in &active {
            let (signed, _) = engine.liveness.get_stats(v);
            let expected = LIVENESS_WINDOW / 4;
            // Allow ±1 for integer division of LIVENESS_WINDOW / 4.
            assert!(
                signed.abs_diff(expected) <= 1,
                "#253 broken-model pin: {} signed {} blocks; expected ≈{}",
                v,
                signed,
                expected
            );
            assert!(
                engine.liveness.is_downtime_at(v, current_height),
                "#253 broken-model pin: {} must be flagged downtime under the old \
                 signers=vec![proposer] scheme (25% signed < 30% threshold)",
                v
            );
        }
    }

    #[test]
    fn test_total_slashed_accumulates() {
        let mut reg = setup_registry();
        let mut engine = SlashingEngine::new();

        // Slash via liveness — need full window to trip the window-full guard.
        for h in 0..LIVENESS_WINDOW {
            engine.liveness.record("0xval1", h, false);
        }
        let active = vec!["0xval1".into()];
        engine.check_liveness(&mut reg, &active, LIVENESS_WINDOW);

        assert!(engine.total_slashed > 0);
        let first_slash = engine.total_slashed;

        // Slash via double-sign on another validator
        let evidence = DoubleSignEvidence {
            validator: "0xval2".into(),
            height: 50,
            block_hash_a: "hash_a".into(),
            block_hash_b: "hash_b".into(),
            signature_a: "sig_a".into(),
            signature_b: "sig_b".into(),
        };
        engine.process_double_sign(&mut reg, &evidence).unwrap();
        assert!(engine.total_slashed > first_slash);
    }

    // ── Phase B (consensus-jail): compute_jail_evidence tests ──

    /// Empty active_set: returns empty evidence (no validators to evaluate).
    #[test]
    fn test_compute_jail_evidence_empty_active_set() {
        let engine = SlashingEngine::new();
        let evidence = engine.compute_jail_evidence(&[], LIVENESS_WINDOW * 2);
        assert!(evidence.is_empty());
    }

    /// Healthy validators (signed every block in window): empty evidence.
    #[test]
    fn test_compute_jail_evidence_healthy_validators_returns_empty() {
        let mut engine = SlashingEngine::new();
        // Sign every block from 0 → LIVENESS_WINDOW. At current_height
        // == LIVENESS_WINDOW, val1's full window is filled with signed
        // entries → not downtime.
        for h in 0..LIVENESS_WINDOW {
            engine.liveness.record_signed("0xval1", h);
        }
        let evidence = engine
            .compute_jail_evidence(&["0xval1".into()], LIVENESS_WINDOW);
        assert!(
            evidence.is_empty(),
            "healthy validator must NOT appear in jail evidence"
        );
    }

    /// Validator that signed once a long time ago, then went silent:
    /// the old anchor entry counts as "we've been watching them"
    /// (passes the grace gate), but they're past the window with zero
    /// signed entries inside it → downtime.
    #[test]
    fn test_compute_jail_evidence_downtime_validator_included() {
        let mut engine = SlashingEngine::new();
        // Anchor: one sign at h=0 so we know they were ever observed.
        engine.liveness.record_signed("0xval1", 0);
        // No signatures since. At current_height = LIVENESS_WINDOW + 100,
        // window is (100, LIVENESS_WINDOW + 100] — anchor at h=0 is
        // outside it, so signed_in_window = 0 < MIN_SIGNED_PER_WINDOW.
        let current_height = LIVENESS_WINDOW + 100;
        let evidence = engine
            .compute_jail_evidence(&["0xval1".into()], current_height);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].validator, "0xval1");
        // Phase B initial impl: justification_hashes empty
        assert!(evidence[0].justification_hashes.is_empty());
    }

    /// Determinism: same record sequence → same evidence on both engines.
    /// This is the property the consensus-jail dispatch depends on.
    #[test]
    fn test_compute_jail_evidence_deterministic() {
        let mut engine_a = SlashingEngine::new();
        let mut engine_b = SlashingEngine::new();

        // val1: signs occasionally (will trip downtime). val2: signs
        // every block (healthy). Apply the same record sequence to both
        // engines and confirm they produce byte-identical evidence.
        for h in 0..LIVENESS_WINDOW {
            if h.is_multiple_of(10) {
                engine_a.liveness.record_signed("0xval1", h);
                engine_b.liveness.record_signed("0xval1", h);
            }
            engine_a.liveness.record_signed("0xval2", h);
            engine_b.liveness.record_signed("0xval2", h);
        }

        let active_set: Vec<String> = vec!["0xval1".into(), "0xval2".into()];
        let current_height = LIVENESS_WINDOW * 2;
        let evidence_a = engine_a.compute_jail_evidence(&active_set, current_height);
        let evidence_b = engine_b.compute_jail_evidence(&active_set, current_height);
        assert_eq!(
            evidence_a, evidence_b,
            "evidence must be byte-deterministic across engines with identical state"
        );
    }

    /// Mixed active_set: only validators below threshold appear.
    #[test]
    fn test_compute_jail_evidence_partial_active_set() {
        let mut engine = SlashingEngine::new();

        // val1: anchor at h=0, no signs since → downtime once we're past window
        engine.liveness.record_signed("0xval1", 0);
        // val2: signs every block → healthy
        // val3: signs every other block (50%) → above 30% threshold
        for h in 0..LIVENESS_WINDOW {
            engine.liveness.record_signed("0xval2", h);
            if h.is_multiple_of(2) {
                engine.liveness.record_signed("0xval3", h);
            }
        }

        let current_height = LIVENESS_WINDOW + 100;
        let evidence = engine.compute_jail_evidence(
            &["0xval1".into(), "0xval2".into(), "0xval3".into()],
            current_height,
        );
        assert_eq!(evidence.len(), 1, "only val1 should be in evidence");
        assert_eq!(evidence[0].validator, "0xval1");
    }

    /// Validator first observed too recently → grace period → no downtime
    /// even if they only signed once.
    #[test]
    fn test_compute_jail_evidence_partial_window_excluded() {
        let mut engine = SlashingEngine::new();
        // First observed at h=LIVENESS_WINDOW/2. At current_height ==
        // LIVENESS_WINDOW, first_seen > current_height - LIVENESS_WINDOW
        // (grace period still active) → no downtime.
        engine.liveness.record_signed("0xval1", LIVENESS_WINDOW / 2);
        let evidence = engine
            .compute_jail_evidence(&["0xval1".into()], LIVENESS_WINDOW);
        assert!(
            evidence.is_empty(),
            "validator inside grace period must not produce evidence"
        );
    }
}
