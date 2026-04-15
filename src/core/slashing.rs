// slashing.rs — Liveness tracking + double-sign detection (Voyager Phase 2a)
//
// Downtime: missed >50% in 100-block sliding window → 1% slash + 200 blocks jail
// Double-sign: two blocks same height from same validator → 20% slash + permaban

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::types::error::{SentrixError, SentrixResult};
use crate::core::staking::StakeRegistry;

// ── Constants ────────────────────────────────────────────────

pub const LIVENESS_WINDOW: u64 = 100;
pub const MIN_SIGNED_PER_WINDOW: u64 = 50; // 50%
pub const DOWNTIME_SLASH_BP: u16 = 100;     // 1% in basis points
pub const DOWNTIME_JAIL_BLOCKS: u64 = 200;  // ~10 minutes
pub const DOUBLE_SIGN_SLASH_BP: u16 = 2000; // 20%

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
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::staking::MIN_SELF_STAKE;

    fn setup_registry() -> StakeRegistry {
        let mut reg = StakeRegistry::new();
        reg.register_validator("0xval1", MIN_SELF_STAKE, 1000, 0).unwrap();
        reg.register_validator("0xval2", MIN_SELF_STAKE, 1000, 0).unwrap();
        reg.register_validator("0xval3", MIN_SELF_STAKE, 1000, 0).unwrap();
        reg.update_active_set();
        reg
    }

    // ── LivenessTracker tests ────────────────────────────────

    #[test]
    fn test_liveness_no_downtime() {
        let mut tracker = LivenessTracker::new();
        for h in 0..100 {
            tracker.record("0xval1", h, true);
        }
        assert!(!tracker.is_downtime("0xval1"));
    }

    #[test]
    fn test_liveness_downtime_detected() {
        let mut tracker = LivenessTracker::new();
        // Sign 49 out of 100 (below 50% threshold)
        for h in 0..100 {
            tracker.record("0xval1", h, h < 49);
        }
        assert!(tracker.is_downtime("0xval1"));
    }

    #[test]
    fn test_liveness_exactly_threshold() {
        let mut tracker = LivenessTracker::new();
        // Sign exactly 50 out of 100 (at threshold, should NOT be downtime)
        for h in 0..100 {
            tracker.record("0xval1", h, h < 50);
        }
        assert!(!tracker.is_downtime("0xval1"));
    }

    #[test]
    fn test_liveness_window_not_full() {
        let mut tracker = LivenessTracker::new();
        // Only 50 entries, window not full — no downtime even if all missed
        for h in 0..50 {
            tracker.record("0xval1", h, false);
        }
        assert!(!tracker.is_downtime("0xval1"));
    }

    #[test]
    fn test_liveness_sliding_window() {
        let mut tracker = LivenessTracker::new();
        // First 100 blocks: all missed
        for h in 0..100 {
            tracker.record("0xval1", h, false);
        }
        assert!(tracker.is_downtime("0xval1"));

        // Next 100 blocks: all signed (window slides)
        for h in 100..200 {
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
        for h in 0..100 {
            tracker.record("0xval1", h, false);
        }
        assert!(tracker.is_downtime("0xval1"));
        tracker.reset("0xval1");
        assert!(!tracker.is_downtime("0xval1"));
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

        // val1 misses everything
        for h in 0..100 {
            engine.liveness.record("0xval1", h, false);
            engine.liveness.record("0xval2", h, true);
            engine.liveness.record("0xval3", h, true);
        }

        let active = vec!["0xval1".into(), "0xval2".into(), "0xval3".into()];
        let slashed = engine.check_liveness(&mut reg, &active, 100);

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

        for h in 0..100 {
            engine.liveness.record("0xval1", h, false);
        }

        let active = vec!["0xval1".into()];
        let slashed = engine.check_liveness(&mut reg, &active, 100);
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

    #[test]
    fn test_total_slashed_accumulates() {
        let mut reg = setup_registry();
        let mut engine = SlashingEngine::new();

        // Slash via liveness
        for h in 0..100 {
            engine.liveness.record("0xval1", h, false);
        }
        let active = vec!["0xval1".into()];
        engine.check_liveness(&mut reg, &active, 100);

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
}
