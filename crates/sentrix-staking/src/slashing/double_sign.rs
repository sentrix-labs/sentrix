// The equivocation half of the slashing story: spotting validators
// who signed two different block hashes at the same height. The
// detector keeps a sliding map of (validator, height) → block_hash
// and yells if a fresh signature arrives for a height it's already
// seen with a different hash.
//
// `DoubleSignEvidence` is the on-the-wire shape — same struct that
// `StakingOp::SubmitEvidence` carries. Anyone watching the gossip
// stream can construct one from two conflicting signatures and
// submit it; the chain re-runs the detector check, slashes 20% of
// stake, and tombstones the offender (permanent ban, no unjail).
// That's intentionally harsher than the downtime path — equivocation
// is provably malicious, not negligent.

use super::liveness::LIVENESS_WINDOW;
use sentrix_primitives::{SentrixError, SentrixResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Stake slashed on a proven equivocation (double-sign), in basis points.
///
/// 2000 BP = 20%. Unchanged from v2.1.6. Double-signing is provably
/// malicious (not accidental), so punishment is deliberately harsh.
/// Matches Cosmos Hub, Osmosis, Sei, and most BFT chains' standard.
/// Usually followed by tombstone (permanent ban) so the validator
/// can't re-enter the active set.
pub const DOUBLE_SIGN_SLASH_BP: u16 = 2000;

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
