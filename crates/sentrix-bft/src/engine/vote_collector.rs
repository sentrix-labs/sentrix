// The supermajority check needs a fast "has any single block hash
// crossed the 2/3+ stake threshold yet?" answer on every incoming
// vote. The per-round state already keeps the per-validator vote map,
// but iterating that map on every vote got expensive once we pushed
// past 4 validators and started thinking about onboarding more — so
// the tally lives here, kept in sync as votes arrive, and the
// supermajority check is O(distinct hashes voted) instead of
// O(validators).

use crate::messages::supermajority_threshold;
use std::collections::HashMap;

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
