// Sliding-window record of who-signed-what over the last LIVENESS_WINDOW
// blocks. The downtime predicate flips once a validator's window is
// full AND they've signed fewer than MIN_SIGNED_PER_WINDOW of those
// blocks — at our 1s block time that's the operator-side budget for
// kernel reboots, fast-deploy restarts, and the occasional 2-hour
// debugging session before auto-jail kicks in.
//
// 2026-04-29 caveat (P0 known issue): the recording side currently
// uses each node's CURRENT active_set rather than the historical one
// at block.index, which makes is_downtime non-deterministic across
// validators on real-network mainnet. That's why JAIL_CONSENSUS_HEIGHT
// is parked at u64::MAX until the recording path gets fixed. See
// `audits/2026-04-28-vps5-block-773012-divergence.md`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
