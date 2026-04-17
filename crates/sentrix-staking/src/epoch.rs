// epoch.rs — Epoch system for DPoS validator rotation (Voyager Phase 2a)
//
// Epoch length: 28,800 blocks (~24 hours at 3s).
// At each epoch boundary: recalculate active set, process unbonding, distribute rewards.

use crate::staking::StakeRegistry;
use sentrix_primitives::SentrixResult;
use serde::{Deserialize, Serialize};

pub const EPOCH_LENGTH: u64 = 28_800; // blocks per epoch

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EpochInfo {
    pub epoch_number: u64,
    pub start_height: u64,
    pub end_height: u64,
    /// Validator addresses active during this epoch (frozen at boundary)
    pub validator_set: Vec<String>,
    /// Total staked across active set at epoch start
    pub total_staked: u64,
    /// Accumulated rewards during this epoch (for reporting)
    pub total_rewards: u64,
    pub total_blocks_produced: u64,
}

impl EpochInfo {
    pub fn genesis() -> Self {
        Self {
            epoch_number: 0,
            start_height: 0,
            end_height: EPOCH_LENGTH.saturating_sub(1),
            validator_set: Vec::new(),
            total_staked: 0,
            total_rewards: 0,
            total_blocks_produced: 0,
        }
    }

    pub fn contains_height(&self, height: u64) -> bool {
        height >= self.start_height && height <= self.end_height
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochManager {
    pub current_epoch: EpochInfo,
    /// History of past epochs (keep last N for queries)
    pub history: Vec<EpochInfo>,
    /// Max history entries to keep
    pub max_history: usize,
}

impl Default for EpochManager {
    fn default() -> Self {
        Self {
            current_epoch: EpochInfo::genesis(),
            history: Vec::new(),
            max_history: 100,
        }
    }
}

impl EpochManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Which epoch number does this height belong to?
    pub fn epoch_for_height(height: u64) -> u64 {
        height / EPOCH_LENGTH
    }

    /// Is this height an epoch boundary (last block of an epoch)?
    pub fn is_epoch_boundary(height: u64) -> bool {
        height > 0 && (height + 1).is_multiple_of(EPOCH_LENGTH)
    }

    /// Is this height the first block of a new epoch?
    pub fn is_epoch_start(height: u64) -> bool {
        height.is_multiple_of(EPOCH_LENGTH)
    }

    /// Initialize the first epoch with a stake registry (called at fork activation)
    pub fn initialize(&mut self, stake_registry: &StakeRegistry, fork_height: u64) {
        let epoch_num = Self::epoch_for_height(fork_height);
        let start = epoch_num * EPOCH_LENGTH;
        let active = stake_registry.compute_active_set();
        let total_staked: u64 = active
            .iter()
            .filter_map(|a| stake_registry.get_validator(a))
            .map(|v| v.total_stake())
            .sum();

        self.current_epoch = EpochInfo {
            epoch_number: epoch_num,
            start_height: start,
            end_height: start + EPOCH_LENGTH - 1,
            validator_set: active,
            total_staked,
            total_rewards: 0,
            total_blocks_produced: 0,
        };
    }

    /// Process epoch transition. Called when is_epoch_boundary(height) is true.
    /// Returns list of (delegator, amount) from matured unbonding.
    pub fn transition(
        &mut self,
        stake_registry: &mut StakeRegistry,
        current_height: u64,
    ) -> SentrixResult<Vec<(String, u64)>> {
        // Archive current epoch
        let finished = self.current_epoch.clone();
        self.history.push(finished);
        if self.history.len() > self.max_history {
            self.history.remove(0);
        }

        // Process matured unbonding
        let released = stake_registry.process_unbonding(current_height);

        // Compute new active set
        stake_registry.update_active_set();
        let active = stake_registry.active_set.clone();
        let total_staked: u64 = active
            .iter()
            .filter_map(|a| stake_registry.get_validator(a))
            .map(|v| v.total_stake())
            .sum();

        let next_epoch = self.current_epoch.epoch_number + 1;
        let next_start = (next_epoch) * EPOCH_LENGTH;

        self.current_epoch = EpochInfo {
            epoch_number: next_epoch,
            start_height: next_start,
            end_height: next_start + EPOCH_LENGTH - 1,
            validator_set: active,
            total_staked,
            total_rewards: 0,
            total_blocks_produced: 0,
        };

        Ok(released)
    }

    /// Record a block produced in the current epoch
    pub fn record_block(&mut self, reward: u64) {
        self.current_epoch.total_blocks_produced += 1;
        self.current_epoch.total_rewards = self.current_epoch.total_rewards.saturating_add(reward);
    }

    /// Get the proposer for a given height+round from the current epoch's validator set
    pub fn get_proposer(
        &self,
        stake_registry: &StakeRegistry,
        height: u64,
        round: u32,
    ) -> Option<String> {
        if self.current_epoch.validator_set.is_empty() {
            return None;
        }
        stake_registry.weighted_proposer(height, round)
    }

    /// Is this validator in the current epoch's active set?
    pub fn is_current_validator(&self, address: &str) -> bool {
        self.current_epoch
            .validator_set
            .contains(&address.to_string())
    }

    /// Get epoch info for a past epoch number
    pub fn get_epoch(&self, epoch_number: u64) -> Option<&EpochInfo> {
        if epoch_number == self.current_epoch.epoch_number {
            return Some(&self.current_epoch);
        }
        self.history.iter().find(|e| e.epoch_number == epoch_number)
    }

    /// Get the last N epochs (most recent first)
    pub fn recent_epochs(&self, count: usize) -> Vec<&EpochInfo> {
        let mut result: Vec<&EpochInfo> = vec![&self.current_epoch];
        for e in self.history.iter().rev().take(count.saturating_sub(1)) {
            result.push(e);
        }
        result
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::staking::{MIN_SELF_STAKE, StakeRegistry};

    fn setup_registry(count: usize) -> StakeRegistry {
        let mut reg = StakeRegistry::new();
        for i in 0..count {
            let addr = format!("0xval{:03}", i);
            let stake = MIN_SELF_STAKE + (i as u64) * 1_000_000_000;
            reg.register_validator(&addr, stake, 1000, 0).unwrap();
        }
        reg.update_active_set();
        reg
    }

    #[test]
    fn test_epoch_for_height() {
        assert_eq!(EpochManager::epoch_for_height(0), 0);
        assert_eq!(EpochManager::epoch_for_height(28_799), 0);
        assert_eq!(EpochManager::epoch_for_height(28_800), 1);
        assert_eq!(EpochManager::epoch_for_height(57_600), 2);
    }

    #[test]
    fn test_is_epoch_boundary() {
        assert!(!EpochManager::is_epoch_boundary(0));
        assert!(EpochManager::is_epoch_boundary(28_799));
        assert!(!EpochManager::is_epoch_boundary(28_800));
        assert!(EpochManager::is_epoch_boundary(57_599));
    }

    #[test]
    fn test_is_epoch_start() {
        assert!(EpochManager::is_epoch_start(0));
        assert!(!EpochManager::is_epoch_start(1));
        assert!(EpochManager::is_epoch_start(28_800));
        assert!(EpochManager::is_epoch_start(57_600));
    }

    #[test]
    fn test_genesis_epoch() {
        let em = EpochManager::new();
        assert_eq!(em.current_epoch.epoch_number, 0);
        assert_eq!(em.current_epoch.start_height, 0);
        assert_eq!(em.current_epoch.end_height, EPOCH_LENGTH - 1);
    }

    #[test]
    fn test_initialize() {
        let reg = setup_registry(5);
        let mut em = EpochManager::new();
        em.initialize(&reg, 0);

        assert_eq!(em.current_epoch.epoch_number, 0);
        assert_eq!(em.current_epoch.validator_set.len(), 5);
        assert!(em.current_epoch.total_staked > 0);
    }

    #[test]
    fn test_initialize_at_mid_epoch() {
        let reg = setup_registry(3);
        let mut em = EpochManager::new();
        em.initialize(&reg, 50_000); // mid second epoch

        assert_eq!(em.current_epoch.epoch_number, 1); // 50000 / 28800 = 1
        assert_eq!(em.current_epoch.start_height, 28_800);
    }

    #[test]
    fn test_transition() {
        let mut reg = setup_registry(5);
        let mut em = EpochManager::new();
        em.initialize(&reg, 0);

        let released = em.transition(&mut reg, EPOCH_LENGTH - 1).unwrap();
        assert!(released.is_empty()); // no unbonding to release
        assert_eq!(em.current_epoch.epoch_number, 1);
        assert_eq!(em.current_epoch.start_height, EPOCH_LENGTH);
        assert_eq!(em.history.len(), 1);
        assert_eq!(em.history[0].epoch_number, 0);
    }

    #[test]
    fn test_transition_releases_unbonding() {
        let mut reg = setup_registry(3);
        let mut em = EpochManager::new();
        em.initialize(&reg, 0);

        // Delegate and undelegate
        reg.delegate("0xdel1", "0xval000", 1_000_000, 100).unwrap();
        reg.undelegate("0xdel1", "0xval000", 500_000, 100).unwrap();

        // Transition well past unbonding period
        let released = em.transition(&mut reg, 300_000).unwrap();
        assert_eq!(released.len(), 1);
        assert_eq!(released[0].0, "0xdel1");
        assert_eq!(released[0].1, 500_000);
    }

    #[test]
    fn test_record_block() {
        let mut em = EpochManager::new();
        em.record_block(100_000_000);
        em.record_block(100_000_000);
        assert_eq!(em.current_epoch.total_blocks_produced, 2);
        assert_eq!(em.current_epoch.total_rewards, 200_000_000);
    }

    #[test]
    fn test_is_current_validator() {
        let reg = setup_registry(3);
        let mut em = EpochManager::new();
        em.initialize(&reg, 0);
        assert!(em.is_current_validator("0xval000"));
        assert!(!em.is_current_validator("0xunknown"));
    }

    #[test]
    fn test_epoch_history_capped() {
        let mut reg = setup_registry(3);
        let mut em = EpochManager::new();
        em.max_history = 5;
        em.initialize(&reg, 0);

        for i in 0..10 {
            let height = ((i + 1) * EPOCH_LENGTH) - 1;
            em.transition(&mut reg, height).unwrap();
        }

        assert!(em.history.len() <= 5);
        assert_eq!(em.current_epoch.epoch_number, 10);
    }

    #[test]
    fn test_recent_epochs() {
        let mut reg = setup_registry(3);
        let mut em = EpochManager::new();
        em.initialize(&reg, 0);

        em.transition(&mut reg, EPOCH_LENGTH - 1).unwrap();
        em.transition(&mut reg, 2 * EPOCH_LENGTH - 1).unwrap();

        let recent = em.recent_epochs(3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].epoch_number, 2); // current
        assert_eq!(recent[1].epoch_number, 1);
        assert_eq!(recent[2].epoch_number, 0);
    }

    #[test]
    fn test_get_epoch() {
        let mut reg = setup_registry(3);
        let mut em = EpochManager::new();
        em.initialize(&reg, 0);
        em.transition(&mut reg, EPOCH_LENGTH - 1).unwrap();

        assert!(em.get_epoch(0).is_some());
        assert!(em.get_epoch(1).is_some());
        assert!(em.get_epoch(999).is_none());
    }

    #[test]
    fn test_contains_height() {
        let epoch = EpochInfo {
            epoch_number: 1,
            start_height: 28_800,
            end_height: 57_599,
            validator_set: vec![],
            total_staked: 0,
            total_rewards: 0,
            total_blocks_produced: 0,
        };
        assert!(!epoch.contains_height(28_799));
        assert!(epoch.contains_height(28_800));
        assert!(epoch.contains_height(40_000));
        assert!(epoch.contains_height(57_599));
        assert!(!epoch.contains_height(57_600));
    }

    #[test]
    fn test_validator_set_changes_on_transition() {
        let mut reg = setup_registry(3);
        let mut em = EpochManager::new();
        em.initialize(&reg, 0);

        let initial_set = em.current_epoch.validator_set.clone();

        // Jail one validator before transition
        reg.jail("0xval000", 100, 0).unwrap();

        em.transition(&mut reg, EPOCH_LENGTH - 1).unwrap();

        // Jailed validator should be gone from new set
        assert!(
            !em.current_epoch
                .validator_set
                .contains(&"0xval000".to_string())
        );
        assert_ne!(em.current_epoch.validator_set, initial_set);
    }
}
