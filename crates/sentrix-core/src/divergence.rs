//! State-root-divergence rate alarm.
//!
//! Extracted from `crate::blockchain` so the rejection-rate tracker
//! lives next to its tests, not buried in the middle of `Blockchain`.

use std::collections::VecDeque;
use std::time::Instant;

/// Rate-threshold detector for "this validator has diverged from peers".
///
/// Why not just rely on the per-event ERROR log:
///   The existing `CRITICAL #1e: state_root mismatch at block N` line is
///   correct but emits once PER rejected block. During a real divergence
///   that's ~1 line/s. Journald rotation evicts the first occurrences
///   within hours, so by the time an operator checks, they see only the
///   tail end and don't realize the validator has been rejecting
///   everything from peers for the entire time.
///
/// What this adds:
///   One ERROR-level alarm when the rolling rejection rate exceeds the
///   threshold, rate-limited so subsequent rejections within the
///   cooldown don't spam. The alarm message names the recovery playbook
///   explicitly (rsync chain.db from a healthy peer) so the operator
///   can act without having to look anything up.
#[derive(Debug, Default, Clone)]
pub(crate) struct DivergenceTracker {
    /// Timestamps of recent state_root-mismatch rejections.
    recent_rejections: VecDeque<Instant>,
    /// Total rejections observed since process boot (monotonic).
    total_rejections: u64,
    /// Last alarm emission timestamp (for rate-limiting).
    pub(crate) last_alarm_at: Option<Instant>,
}

impl DivergenceTracker {
    /// Rolling window for rate calculation.
    pub(crate) const WINDOW_SECS: u64 = 300; // 5 minutes
    /// Alarm fires when recent_rejections.len() reaches this within the window.
    pub(crate) const ALARM_THRESHOLD: usize = 100;
    /// Minimum seconds between alarm emissions (prevents spam).
    pub(crate) const ALARM_COOLDOWN_SECS: u64 = 60;

    /// Record one state_root-mismatch rejection and maybe emit an alarm.
    /// Call this from the rejection path in `apply_block_pass2` (or
    /// wherever state_root mismatch is detected).
    pub fn record_rejection(&mut self, block_index: u64) {
        let now = Instant::now();

        // Evict entries older than the window.
        while let Some(front) = self.recent_rejections.front() {
            if now.duration_since(*front).as_secs() > Self::WINDOW_SECS {
                self.recent_rejections.pop_front();
            } else {
                break;
            }
        }

        self.recent_rejections.push_back(now);
        self.total_rejections = self.total_rejections.saturating_add(1);

        // Check alarm threshold + cooldown.
        if self.recent_rejections.len() >= Self::ALARM_THRESHOLD {
            let should_alarm = self
                .last_alarm_at
                .map(|t| now.duration_since(t).as_secs() >= Self::ALARM_COOLDOWN_SECS)
                .unwrap_or(true);
            if should_alarm {
                tracing::error!(
                    "🚨 DIVERGENCE ALERT: this validator has rejected {} peer blocks with \
                     state_root mismatch in the last {}s (total since boot: {}, current block {}). \
                     This strongly suggests the local chain.db has diverged from the network. \
                     RECOVERY: stop this validator, rsync /opt/sentrix/data/chain.db from a \
                     healthy peer with all validators briefly stopped, then restart. See \
                     `internal design doc` for the full \
                     playbook. If ≥2 validators are flagging this, the OTHER validator(s) may \
                     be canonical — investigate before rsync'ing the wrong direction.",
                    self.recent_rejections.len(),
                    Self::WINDOW_SECS,
                    self.total_rejections,
                    block_index,
                );
                self.last_alarm_at = Some(now);
            }
        }
    }

    /// Read-only view of counters, for RPC exposure (future) and tests.
    #[allow(dead_code)] // surfaced via RPC in a follow-up PR (`/chain/divergence`)
    pub fn stats(&self) -> (usize, u64) {
        (self.recent_rejections.len(), self.total_rejections)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counter monotonicity + threshold behavior. We can't test the
    /// rolling eviction in real-time without waiting 5 minutes, so we
    /// test the linear count path which is what actually fires the
    /// alarm under real divergence conditions.
    #[test]
    fn test_divergence_tracker_counts() {
        let mut t = DivergenceTracker::default();
        assert_eq!(t.stats(), (0, 0));

        for i in 0..50u64 {
            t.record_rejection(1_000 + i);
        }
        let (recent, total) = t.stats();
        assert_eq!(
            recent, 50,
            "all 50 rejections should be within the 5-min window"
        );
        assert_eq!(total, 50);
    }

    /// Alarm cooldown — after the threshold is crossed, subsequent
    /// rejections within the cooldown must not re-emit. We can't
    /// assert the log output directly without an observer; we assert
    /// the internal `last_alarm_at` state instead.
    #[test]
    fn test_divergence_alarm_cooldown() {
        let mut t = DivergenceTracker::default();
        // Push enough rejections to cross the threshold.
        for i in 0..DivergenceTracker::ALARM_THRESHOLD as u64 {
            t.record_rejection(2_000 + i);
        }
        assert!(
            t.last_alarm_at.is_some(),
            "alarm must fire once threshold crossed"
        );

        // Subsequent rejection within the cooldown window doesn't
        // update `last_alarm_at` (since the current alarm is still
        // within cooldown). Hard to assert without mocking time;
        // just verify no panic + tracker continues accepting records.
        t.record_rejection(2_999);
        let (_recent, total) = t.stats();
        assert_eq!(total, DivergenceTracker::ALARM_THRESHOLD as u64 + 1);
    }
}
