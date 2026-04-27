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

use crate::staking::StakeRegistry;
use sentrix_primitives::transaction::JailEvidence;
use sentrix_primitives::{SentrixError, SentrixResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Constants ────────────────────────────────────────────────

/// Rolling window for liveness tracking, in blocks.
///
/// At 1s block time = ~4 hours. Long enough to absorb normal operational
/// downtime (a weekly 10-minute deploy is 0.07% of the window; even a
/// 30-minute emergency recovery is 12.5%). Short enough that a
/// persistently offline validator still gets jailed within a half-day.
///
/// Comparable chains:
///   - Tendermint default: 100 (≈100s — demo-tight)
///   - Cosmos Hub:         10_000 (≈16.7h @ 6s block time)
///   - Osmosis:            30_000 (≈41.7h @ 5s)
///   - Sei:                10_000 (≈1.1h @ 400ms)
///   - Sentrix (here):     14_400 (≈4h @ 1s)
///
/// Sentrix lands between Sei's tight-on-fast-blocks approach and Cosmos's
/// generous-long-window approach, scaled for our 1s block cadence.
pub const LIVENESS_WINDOW: u64 = 14_400;

/// Minimum signed blocks required per window for a validator to stay out
/// of jail. Expressed as an absolute block count, not a fraction, so the
/// math stays integer-friendly.
///
/// 4_320 / 14_400 = 30% — validator must sign at least 30% of blocks in
/// any rolling 4-hour window. Translated to downtime tolerance: up to
/// ~70% of the window (≈2.8 hours) can be missed before jailing. That
/// covers:
///   - Weekly 10-minute deploy    →  ~0.07% downtime (absorbed)
///   - Emergency 30-min recovery  →  12.5% downtime (absorbed)
///   - Extended 2-hour debugging  →  50% downtime (absorbed)
///   - Full 3-hour outage in 4h   →  75% downtime (jailed)
///
/// Cosmos Hub uses 5% (generous, built for massive validator sets).
/// We go stricter because Sentrix's 21-validator target means each
/// individual validator carries proportionally more responsibility —
/// one flapping validator on a 21-node network is ~5% of producing
/// capacity lost, which is significant.
pub const MIN_SIGNED_PER_WINDOW: u64 = 4_320;

/// Stake slashed on a liveness-downtime jail, in basis points.
///
/// 10 BP = 0.1% of stake. Gentle-but-not-zero: operators notice (a
/// self-stake of 15_000 SRX becomes 14_985 SRX) without losing a life-
/// changing amount. Cosmos Hub uses 1 BP (0.01%) which is symbolic;
/// we go 10× stricter because individual reliability matters more at
/// Sentrix's smaller validator count.
///
/// Compare to `DOUBLE_SIGN_SLASH_BP` (2000 BP / 20%) for equivocation —
/// malicious behavior is punished 200× harder than negligence.
pub const DOWNTIME_SLASH_BP: u16 = 10;

/// Blocks jailed after a liveness failure.
///
/// 600 blocks = 10 minutes @ 1s block time. Matches Cosmos Hub's
/// `downtime_jail_duration`. Long enough that the operator has to
/// actively notice + investigate + file an unjail tx (can't just
/// hot-reset and pretend nothing happened). Short enough that a
/// legitimately-flapping validator recovers quickly after the root
/// cause is fixed.
pub const DOWNTIME_JAIL_BLOCKS: u64 = 600;

/// Stake slashed on a proven equivocation (double-sign), in basis points.
///
/// 2000 BP = 20%. Unchanged from v2.1.6. Double-signing is provably
/// malicious (not accidental), so punishment is deliberately harsh.
/// Matches Cosmos Hub, Osmosis, Sei, and most BFT chains' standard.
/// Usually followed by tombstone (permanent ban) so the validator
/// can't re-enter the active set.
pub const DOUBLE_SIGN_SLASH_BP: u16 = 2000;

// ── Liveness Tracker ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LivenessTracker {
    /// Per-validator sliding window: height → signed (true/false)
    /// We store only the last LIVENESS_WINDOW entries per validator.
    records: HashMap<String, Vec<LivenessRecord>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LivenessRecord {
    height: u64,
    signed: bool,
}

impl LivenessTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that a validator signed (or missed) a block at this height
    pub fn record(&mut self, validator: &str, height: u64, signed: bool) {
        let entries = self.records.entry(validator.to_string()).or_default();
        entries.push(LivenessRecord { height, signed });

        // Trim to window size
        if entries.len() > LIVENESS_WINDOW as usize {
            let excess = entries.len() - LIVENESS_WINDOW as usize;
            entries.drain(..excess);
        }
    }

    /// Check if validator has fallen below the minimum signed threshold
    pub fn is_downtime(&self, validator: &str) -> bool {
        let entries = match self.records.get(validator) {
            Some(e) => e,
            None => return false,
        };

        // Only check once we have a full window
        if entries.len() < LIVENESS_WINDOW as usize {
            return false;
        }

        let signed_count = entries.iter().filter(|r| r.signed).count() as u64;
        signed_count < MIN_SIGNED_PER_WINDOW
    }

    /// Get signed/missed counts for a validator
    pub fn get_stats(&self, validator: &str) -> (u64, u64) {
        let entries = match self.records.get(validator) {
            Some(e) => e,
            None => return (0, 0),
        };
        let signed = entries.iter().filter(|r| r.signed).count() as u64;
        let missed = entries.iter().filter(|r| !r.signed).count() as u64;
        (signed, missed)
    }

    /// Clear records for a validator (used after slashing to reset window)
    pub fn reset(&mut self, validator: &str) {
        self.records.remove(validator);
    }
}

// ── Double-Sign Evidence ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoubleSignEvidence {
    pub validator: String,
    pub height: u64,
    pub block_hash_a: String,
    pub block_hash_b: String,
    pub signature_a: String,
    pub signature_b: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DoubleSignDetector {
    /// Recent block signatures: (validator, height) → block_hash
    /// We keep a sliding window of recent blocks for detection
    recent_blocks: HashMap<(String, u64), String>,
    /// Max entries before cleanup
    max_entries: usize,
    /// Processed evidence hashes (prevent double-processing)
    processed: Vec<String>,
}

impl DoubleSignDetector {
    pub fn new() -> Self {
        Self {
            recent_blocks: HashMap::new(),
            max_entries: 10_000,
            processed: Vec::new(),
        }
    }

    /// Record a block signature. Returns evidence if double-sign detected.
    pub fn record_block(
        &mut self,
        validator: &str,
        height: u64,
        block_hash: &str,
        signature: &str,
    ) -> Option<DoubleSignEvidence> {
        let key = (validator.to_string(), height);

        if let Some(existing_hash) = self.recent_blocks.get(&key) {
            if existing_hash != block_hash {
                return Some(DoubleSignEvidence {
                    validator: validator.to_string(),
                    height,
                    block_hash_a: existing_hash.clone(),
                    block_hash_b: block_hash.to_string(),
                    signature_a: String::new(), // filled by caller if available
                    signature_b: signature.to_string(),
                });
            }
            return None; // same hash, not a double-sign
        }

        self.recent_blocks.insert(key, block_hash.to_string());

        // Cleanup old entries
        if self.recent_blocks.len() > self.max_entries {
            let cutoff_height = height.saturating_sub(LIVENESS_WINDOW * 10);
            self.recent_blocks.retain(|(_v, h), _| *h > cutoff_height);
        }

        None
    }

    /// Verify and process external evidence submission
    pub fn process_evidence(&mut self, evidence: &DoubleSignEvidence) -> SentrixResult<bool> {
        // Basic validation
        if evidence.block_hash_a == evidence.block_hash_b {
            return Err(SentrixError::InvalidTransaction(
                "evidence hashes must differ".into(),
            ));
        }
        if evidence.validator.is_empty() {
            return Err(SentrixError::InvalidTransaction(
                "evidence missing validator".into(),
            ));
        }

        // Check not already processed
        let evidence_id = format!(
            "{}:{}:{}:{}",
            evidence.validator, evidence.height, evidence.block_hash_a, evidence.block_hash_b
        );
        if self.processed.contains(&evidence_id) {
            return Ok(false); // already processed
        }

        self.processed.push(evidence_id);

        // Cap processed list
        if self.processed.len() > 1000 {
            self.processed.drain(..500);
        }

        Ok(true)
    }
}

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

    /// Record block production for liveness tracking.
    /// `proposer` signed, everyone else in active_set should also have signed (voted).
    ///
    /// Per-validator signed/missed counts emitted as DEBUG-level tracing every
    /// EPOCH_LENGTH boundary. Use with `RUST_LOG=sentrix_staking::slashing=debug`
    /// to detect jail-counter divergence across the fleet (each validator's log
    /// should show identical signed/missed for any given height — divergence is
    /// the smoking-gun signature of the 2026-04-26 jail-cascade pattern).
    /// See `audits/jail-cascade-root-cause-analysis.md`.
    pub fn record_block_signatures(
        &mut self,
        active_set: &[String],
        signers: &[String],
        height: u64,
    ) {
        for validator in active_set {
            let signed = signers.contains(validator);
            self.liveness.record(validator, height, signed);
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

    /// Phase B (consensus-jail): compute deterministic JailEvidence list
    /// from the current LivenessTracker state for the active set.
    ///
    /// Each entry in the returned Vec carries (validator, signed_count,
    /// missed_count, justification_hashes). Caller (epoch-boundary proposer)
    /// includes this in `StakingOp::JailEvidenceBundle` for consensus-applied
    /// jail decisions.
    ///
    /// Determinism: peers MUST produce identical evidence given identical
    /// LivenessTracker state — that's the whole point of the design (consensus
    /// applies the same jail decision uniformly across all validators).
    /// This holds because `LivenessTracker.get_stats` is purely a function of
    /// the records HashMap, no local-only state.
    ///
    /// `justification_hashes`: Phase B initial impl returns empty vec. Phase C
    /// will populate with actual missed-block hashes for selective verification.
    /// Empty list still allows count-based verification (peer recomputes signed
    /// + missed count, compares to claim).
    ///
    /// Only validators that have FULL window AND fall below MIN_SIGNED_PER_WINDOW
    /// are included (matches legacy `is_downtime` predicate).
    pub fn compute_jail_evidence(&self, active_set: &[String]) -> Vec<JailEvidence> {
        let mut evidence = Vec::new();
        for validator in active_set {
            if !self.liveness.is_downtime(validator) {
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
        let signers = vec!["0xval1".into(), "0xval3".into()]; // val2 missed

        engine.record_block_signatures(&active, &signers, 100);

        let (s1, m1) = engine.liveness.get_stats("0xval1");
        assert_eq!(s1, 1);
        assert_eq!(m1, 0);

        let (s2, m2) = engine.liveness.get_stats("0xval2");
        assert_eq!(s2, 0);
        assert_eq!(m2, 1);
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
                engine.liveness.is_downtime(v),
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
        let evidence = engine.compute_jail_evidence(&[]);
        assert!(evidence.is_empty());
    }

    /// Healthy validators (no downtime): returns empty evidence.
    #[test]
    fn test_compute_jail_evidence_healthy_validators_returns_empty() {
        let mut engine = SlashingEngine::new();
        // Fill window for val1 with all signed (no downtime).
        for h in 0..LIVENESS_WINDOW {
            engine.liveness.record("0xval1", h, true);
        }
        let evidence = engine.compute_jail_evidence(&["0xval1".into()]);
        assert!(
            evidence.is_empty(),
            "healthy validator must NOT appear in jail evidence"
        );
    }

    /// Validator below threshold: returns evidence entry with signed/missed counts.
    #[test]
    fn test_compute_jail_evidence_downtime_validator_included() {
        let mut engine = SlashingEngine::new();
        // Fill window with all missed → triggers downtime.
        for h in 0..LIVENESS_WINDOW {
            engine.liveness.record("0xval1", h, false);
        }
        let evidence = engine.compute_jail_evidence(&["0xval1".into()]);
        assert_eq!(evidence.len(), 1);
        assert_eq!(evidence[0].validator, "0xval1");
        assert_eq!(evidence[0].signed_count, 0);
        assert_eq!(evidence[0].missed_count, LIVENESS_WINDOW);
        // Phase B initial impl: justification_hashes empty
        assert!(evidence[0].justification_hashes.is_empty());
    }

    /// Determinism: same LivenessTracker state → same evidence list.
    /// This is the CRITICAL property — peers MUST agree on evidence to
    /// reach consensus on jail decision.
    #[test]
    fn test_compute_jail_evidence_deterministic() {
        let mut engine_a = SlashingEngine::new();
        let mut engine_b = SlashingEngine::new();

        // Apply identical record sequence to both.
        for h in 0..LIVENESS_WINDOW {
            // val1 mostly missed (will trigger jail)
            let val1_signed = h % 10 == 0;
            engine_a.liveness.record("0xval1", h, val1_signed);
            engine_b.liveness.record("0xval1", h, val1_signed);
            // val2 always signed
            engine_a.liveness.record("0xval2", h, true);
            engine_b.liveness.record("0xval2", h, true);
        }

        let active_set: Vec<String> = vec!["0xval1".into(), "0xval2".into()];
        let evidence_a = engine_a.compute_jail_evidence(&active_set);
        let evidence_b = engine_b.compute_jail_evidence(&active_set);
        assert_eq!(
            evidence_a, evidence_b,
            "evidence must be byte-deterministic across engines with identical state"
        );
    }

    /// Mixed active_set: only validators below threshold appear in evidence.
    #[test]
    fn test_compute_jail_evidence_partial_active_set() {
        let mut engine = SlashingEngine::new();

        // val1: full window all missed → downtime
        for h in 0..LIVENESS_WINDOW {
            engine.liveness.record("0xval1", h, false);
        }
        // val2: full window all signed → healthy
        for h in 0..LIVENESS_WINDOW {
            engine.liveness.record("0xval2", h, true);
        }
        // val3: full window with 50% signed → above threshold (50% > 30%)
        for h in 0..LIVENESS_WINDOW {
            engine.liveness.record("0xval3", h, h % 2 == 0);
        }

        let evidence = engine.compute_jail_evidence(&[
            "0xval1".into(),
            "0xval2".into(),
            "0xval3".into(),
        ]);
        assert_eq!(evidence.len(), 1, "only val1 should be in evidence");
        assert_eq!(evidence[0].validator, "0xval1");
    }

    /// Window not full → not downtime → not in evidence.
    #[test]
    fn test_compute_jail_evidence_partial_window_excluded() {
        let mut engine = SlashingEngine::new();
        // Half-window all missed — not yet at downtime threshold (window not full)
        for h in 0..(LIVENESS_WINDOW / 2) {
            engine.liveness.record("0xval1", h, false);
        }
        let evidence = engine.compute_jail_evidence(&["0xval1".into()]);
        assert!(
            evidence.is_empty(),
            "partial window must not produce evidence"
        );
    }
}
