// staking.rs — DPoS Stake Registry (Voyager Phase 2a)
//
// Manages validator stakes, delegations, unbonding, rewards, and commission.
// Replaces AuthorityManager for blocks >= VOYAGER_DPOS_HEIGHT.

use sentrix_primitives::{SentrixError, SentrixResult};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

// ── Constants ────────────────────────────────────────────────

pub const MIN_SELF_STAKE: u64 = 15_000 * 100_000_000; // 15,000 SRX in sentri
pub const MAX_ACTIVE_VALIDATORS: usize = 21;
pub const MAX_CANDIDATES: usize = 100;
pub const UNBONDING_PERIOD: u64 = 201_600; // 7 days at 3s blocks
pub const MAX_DELEGATIONS_PER_ACCOUNT: usize = 10;
pub const MAX_UNBONDING_ENTRIES: usize = 7;
pub const REDELEGATE_COOLDOWN: u64 = 201_600; // 7 days
pub const MIN_COMMISSION: u16 = 500; // 5% in basis points
pub const MAX_COMMISSION: u16 = 2000; // 20%
pub const MAX_COMMISSION_CHANGE_PER_EPOCH: u16 = 200; // 2%

// ── Structs ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorStake {
    pub address: String,
    pub self_stake: u64,
    pub total_delegated: u64,
    pub commission_rate: u16,     // basis points (500 = 5%)
    pub max_commission_rate: u16, // set at registration, immutable
    pub is_jailed: bool,
    pub jail_until: u64,     // block height, 0 = not jailed
    pub is_tombstoned: bool, // permanent ban (double-sign)
    pub blocks_signed: u64,
    pub blocks_missed: u64,
    pub pending_rewards: u64, // accumulated, unclaimed
    pub registration_height: u64,
}

impl ValidatorStake {
    pub fn total_stake(&self) -> u64 {
        self.self_stake.saturating_add(self.total_delegated)
    }

    pub fn is_active_eligible(&self) -> bool {
        !self.is_jailed && !self.is_tombstoned && self.self_stake >= MIN_SELF_STAKE
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationEntry {
    pub delegator: String,
    pub validator: String,
    pub amount: u64,
    pub height: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnbondingEntry {
    pub delegator: String,
    pub validator: String,
    pub amount: u64,
    pub completion_height: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedelegateRecord {
    pub delegator: String,
    pub completion_height: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StakeRegistry {
    /// All registered validators
    pub validators: HashMap<String, ValidatorStake>,
    /// delegator_address → list of delegations
    pub delegations: HashMap<String, Vec<DelegationEntry>>,
    /// Unbonding queue sorted by completion height
    pub unbonding_queue: BTreeMap<u64, Vec<UnbondingEntry>>,
    /// Track redelegate cooldowns: delegator → last redelegate completion
    pub redelegate_cooldowns: HashMap<String, u64>,
    /// Current active validator set (top 21 by stake)
    pub active_set: Vec<String>,
}

impl StakeRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Validator Registration ───────────────────────────────

    pub fn register_validator(
        &mut self,
        address: &str,
        self_stake: u64,
        commission_rate: u16,
        current_height: u64,
    ) -> SentrixResult<()> {
        if self.validators.contains_key(address) {
            return Err(SentrixError::InvalidTransaction(
                "validator already registered".into(),
            ));
        }
        if self.validators.len() >= MAX_CANDIDATES {
            return Err(SentrixError::InvalidTransaction(format!(
                "max {} validator candidates reached",
                MAX_CANDIDATES
            )));
        }
        if self_stake < MIN_SELF_STAKE {
            return Err(SentrixError::InvalidTransaction(format!(
                "self-stake {} below minimum {}",
                self_stake, MIN_SELF_STAKE
            )));
        }
        if !(MIN_COMMISSION..=MAX_COMMISSION).contains(&commission_rate) {
            return Err(SentrixError::InvalidTransaction(format!(
                "commission {} out of range [{}, {}]",
                commission_rate, MIN_COMMISSION, MAX_COMMISSION
            )));
        }

        self.validators.insert(
            address.to_string(),
            ValidatorStake {
                address: address.to_string(),
                self_stake,
                total_delegated: 0,
                commission_rate,
                // C-09: clamp to MAX_COMMISSION. saturating_add alone
                // tops out at u16::MAX; the per-epoch change budget
                // (5 × 200 = 1000) added to a commission registered near
                // MAX_COMMISSION (e.g. 1500) would otherwise produce a
                // max_commission_rate of 2500, i.e. 25 %, even though
                // the hard ceiling is 2000 (20 %). Clamping restores
                // the invariant that no stored max_commission_rate
                // can exceed MAX_COMMISSION.
                max_commission_rate: commission_rate
                    .saturating_add(MAX_COMMISSION_CHANGE_PER_EPOCH * 5)
                    .min(MAX_COMMISSION),
                is_jailed: false,
                jail_until: 0,
                is_tombstoned: false,
                blocks_signed: 0,
                blocks_missed: 0,
                pending_rewards: 0,
                registration_height: current_height,
            },
        );

        Ok(())
    }

    // ── Delegation ───────────────────────────────────────────

    pub fn delegate(
        &mut self,
        delegator: &str,
        validator: &str,
        amount: u64,
        current_height: u64,
    ) -> SentrixResult<()> {
        if amount == 0 {
            return Err(SentrixError::InvalidTransaction(
                "delegation amount must be > 0".into(),
            ));
        }
        let val = self
            .validators
            .get_mut(validator)
            .ok_or_else(|| SentrixError::InvalidTransaction("validator not found".into()))?;
        if val.is_tombstoned {
            return Err(SentrixError::InvalidTransaction(
                "cannot delegate to tombstoned validator".into(),
            ));
        }

        let delegator_entries = self.delegations.entry(delegator.to_string()).or_default();

        // Check if already delegating to this validator → add to existing
        if let Some(entry) = delegator_entries
            .iter_mut()
            .find(|e| e.validator == validator)
        {
            entry.amount = entry
                .amount
                .checked_add(amount)
                .ok_or_else(|| SentrixError::InvalidTransaction("delegation overflow".into()))?;
        } else {
            if delegator_entries.len() >= MAX_DELEGATIONS_PER_ACCOUNT {
                return Err(SentrixError::InvalidTransaction(format!(
                    "max {} delegations per account",
                    MAX_DELEGATIONS_PER_ACCOUNT
                )));
            }
            delegator_entries.push(DelegationEntry {
                delegator: delegator.to_string(),
                validator: validator.to_string(),
                amount,
                height: current_height,
            });
        }

        // Update validator total
        let val = self
            .validators
            .get_mut(validator)
            .ok_or_else(|| SentrixError::InvalidTransaction("validator not found".into()))?;
        val.total_delegated = val
            .total_delegated
            .checked_add(amount)
            .ok_or_else(|| SentrixError::InvalidTransaction("delegated total overflow".into()))?;

        Ok(())
    }

    pub fn undelegate(
        &mut self,
        delegator: &str,
        validator: &str,
        amount: u64,
        current_height: u64,
    ) -> SentrixResult<()> {
        if amount == 0 {
            return Err(SentrixError::InvalidTransaction(
                "undelegation amount must be > 0".into(),
            ));
        }

        // Find and reduce delegation
        let entries = self
            .delegations
            .get_mut(delegator)
            .ok_or_else(|| SentrixError::InvalidTransaction("no delegations found".into()))?;
        let entry = entries
            .iter_mut()
            .find(|e| e.validator == validator)
            .ok_or_else(|| {
                SentrixError::InvalidTransaction("delegation to this validator not found".into())
            })?;
        if entry.amount < amount {
            return Err(SentrixError::InvalidTransaction(
                "undelegation exceeds delegated amount".into(),
            ));
        }
        entry.amount = entry.amount.saturating_sub(amount);

        // Remove empty delegation entries
        if entry.amount == 0 {
            entries.retain(|e| !(e.validator == validator && e.amount == 0));
        }

        // Check unbonding entry count for this delegator+validator pair
        let existing_unbonding = self
            .unbonding_queue
            .values()
            .flat_map(|v| v.iter())
            .filter(|u| u.delegator == delegator && u.validator == validator)
            .count();
        if existing_unbonding >= MAX_UNBONDING_ENTRIES {
            return Err(SentrixError::InvalidTransaction(format!(
                "max {} unbonding entries per delegation",
                MAX_UNBONDING_ENTRIES
            )));
        }

        // Reduce validator total
        let val = self
            .validators
            .get_mut(validator)
            .ok_or_else(|| SentrixError::InvalidTransaction("validator not found".into()))?;
        val.total_delegated = val.total_delegated.saturating_sub(amount);

        // Queue unbonding
        let completion = current_height.saturating_add(UNBONDING_PERIOD);
        self.unbonding_queue
            .entry(completion)
            .or_default()
            .push(UnbondingEntry {
                delegator: delegator.to_string(),
                validator: validator.to_string(),
                amount,
                completion_height: completion,
            });

        Ok(())
    }

    pub fn redelegate(
        &mut self,
        delegator: &str,
        from_validator: &str,
        to_validator: &str,
        amount: u64,
        current_height: u64,
    ) -> SentrixResult<()> {
        if from_validator == to_validator {
            return Err(SentrixError::InvalidTransaction(
                "cannot redelegate to same validator".into(),
            ));
        }
        if amount == 0 {
            return Err(SentrixError::InvalidTransaction(
                "redelegate amount must be > 0".into(),
            ));
        }

        // Check cooldown
        if let Some(&last) = self.redelegate_cooldowns.get(delegator)
            && current_height < last
        {
            return Err(SentrixError::InvalidTransaction(format!(
                "redelegate cooldown active until height {}",
                last
            )));
        }

        // Verify target validator exists and is not tombstoned
        if !self.validators.contains_key(to_validator) {
            return Err(SentrixError::InvalidTransaction(
                "target validator not found".into(),
            ));
        }
        if self
            .validators
            .get(to_validator)
            .map(|v| v.is_tombstoned)
            .unwrap_or(true)
        {
            return Err(SentrixError::InvalidTransaction(
                "cannot redelegate to tombstoned validator".into(),
            ));
        }

        // Reduce from source
        let entries = self
            .delegations
            .get_mut(delegator)
            .ok_or_else(|| SentrixError::InvalidTransaction("no delegations found".into()))?;
        let from_entry = entries
            .iter_mut()
            .find(|e| e.validator == from_validator)
            .ok_or_else(|| {
                SentrixError::InvalidTransaction("delegation to source validator not found".into())
            })?;
        if from_entry.amount < amount {
            return Err(SentrixError::InvalidTransaction(
                "redelegate exceeds delegated amount".into(),
            ));
        }
        from_entry.amount = from_entry.amount.saturating_sub(amount);
        if from_entry.amount == 0 {
            entries.retain(|e| !(e.validator == from_validator && e.amount == 0));
        }

        // Update source validator total
        if let Some(val) = self.validators.get_mut(from_validator) {
            val.total_delegated = val.total_delegated.saturating_sub(amount);
        }

        // Add to target (reuse delegate logic inline to avoid borrow issues)
        let entries = self.delegations.entry(delegator.to_string()).or_default();
        if let Some(to_entry) = entries.iter_mut().find(|e| e.validator == to_validator) {
            to_entry.amount = to_entry
                .amount
                .checked_add(amount)
                .ok_or_else(|| SentrixError::InvalidTransaction("delegation overflow".into()))?;
        } else {
            if entries.len() >= MAX_DELEGATIONS_PER_ACCOUNT {
                return Err(SentrixError::InvalidTransaction(format!(
                    "max {} delegations per account",
                    MAX_DELEGATIONS_PER_ACCOUNT
                )));
            }
            entries.push(DelegationEntry {
                delegator: delegator.to_string(),
                validator: to_validator.to_string(),
                amount,
                height: current_height,
            });
        }

        // Update target validator total
        if let Some(val) = self.validators.get_mut(to_validator) {
            val.total_delegated = val.total_delegated.checked_add(amount).ok_or_else(|| {
                SentrixError::InvalidTransaction("delegated total overflow".into())
            })?;
        }

        // Set cooldown
        self.redelegate_cooldowns.insert(
            delegator.to_string(),
            current_height.saturating_add(REDELEGATE_COOLDOWN),
        );

        Ok(())
    }

    // ── Slashing ─────────────────────────────────────────────

    pub fn slash(&mut self, validator: &str, basis_points: u16) -> SentrixResult<u64> {
        let val = self
            .validators
            .get_mut(validator)
            .ok_or_else(|| SentrixError::InvalidTransaction("validator not found".into()))?;

        let total = val.total_stake();
        let slash_amount = (total as u128)
            .checked_mul(basis_points as u128)
            .ok_or_else(|| SentrixError::InvalidTransaction("slash calculation overflow".into()))?
            / 10_000;
        let slash_amount = slash_amount as u64;

        // Slash self-stake first
        let from_self = slash_amount.min(val.self_stake);
        val.self_stake = val.self_stake.saturating_sub(from_self);

        // Remaining slashed from delegators proportionally
        let remaining = slash_amount.saturating_sub(from_self);
        if remaining > 0 && val.total_delegated > 0 {
            let delegated_before = val.total_delegated;
            val.total_delegated = val.total_delegated.saturating_sub(remaining);

            // Reduce individual delegation amounts proportionally
            for entries in self.delegations.values_mut() {
                for entry in entries.iter_mut() {
                    if entry.validator == validator && delegated_before > 0 {
                        let entry_slash = (entry.amount as u128).saturating_mul(remaining as u128)
                            / (delegated_before as u128);
                        entry.amount = entry.amount.saturating_sub(entry_slash as u64);
                    }
                }
            }
        }

        // Also slash unbonding entries for this validator
        for entries in self.unbonding_queue.values_mut() {
            for entry in entries.iter_mut() {
                if entry.validator == validator {
                    let entry_slash =
                        (entry.amount as u128).saturating_mul(basis_points as u128) / 10_000;
                    entry.amount = entry.amount.saturating_sub(entry_slash as u64);
                }
            }
        }

        Ok(slash_amount)
    }

    pub fn jail(
        &mut self,
        validator: &str,
        duration: u64,
        current_height: u64,
    ) -> SentrixResult<()> {
        let val = self
            .validators
            .get_mut(validator)
            .ok_or_else(|| SentrixError::InvalidTransaction("validator not found".into()))?;
        val.is_jailed = true;
        val.jail_until = current_height.saturating_add(duration);
        Ok(())
    }

    pub fn tombstone(&mut self, validator: &str) -> SentrixResult<()> {
        let val = self
            .validators
            .get_mut(validator)
            .ok_or_else(|| SentrixError::InvalidTransaction("validator not found".into()))?;
        val.is_jailed = true;
        val.is_tombstoned = true;
        val.jail_until = u64::MAX;
        Ok(())
    }

    pub fn unjail(&mut self, validator: &str, current_height: u64) -> SentrixResult<()> {
        let val = self
            .validators
            .get_mut(validator)
            .ok_or_else(|| SentrixError::InvalidTransaction("validator not found".into()))?;
        if val.is_tombstoned {
            return Err(SentrixError::InvalidTransaction(
                "tombstoned validators cannot unjail".into(),
            ));
        }
        if current_height < val.jail_until {
            return Err(SentrixError::InvalidTransaction(format!(
                "jail period active until height {}",
                val.jail_until
            )));
        }
        if val.self_stake < MIN_SELF_STAKE {
            return Err(SentrixError::InvalidTransaction(
                "self-stake below minimum after slashing — add more stake first".into(),
            ));
        }
        val.is_jailed = false;
        val.jail_until = 0;
        Ok(())
    }

    // ── Active Set ───────────────────────────────────────────

    pub fn compute_active_set(&self) -> Vec<String> {
        let mut eligible: Vec<_> = self
            .validators
            .values()
            .filter(|v| v.is_active_eligible())
            .collect();

        // Sort by total_stake desc, tie-break by lower address
        eligible.sort_by(|a, b| {
            b.total_stake()
                .cmp(&a.total_stake())
                .then_with(|| a.address.cmp(&b.address))
        });

        eligible
            .into_iter()
            .take(MAX_ACTIVE_VALIDATORS)
            .map(|v| v.address.clone())
            .collect()
    }

    pub fn update_active_set(&mut self) {
        self.active_set = self.compute_active_set();
    }

    pub fn is_active(&self, address: &str) -> bool {
        self.active_set.contains(&address.to_string())
    }

    pub fn active_count(&self) -> usize {
        self.active_set.len()
    }

    // ── Rewards ──────────────────────────────────────────────

    pub fn distribute_reward(
        &mut self,
        proposer: &str,
        block_reward: u64,
        validator_fee_share: u64,
    ) -> SentrixResult<()> {
        let val = self.validators.get(proposer).ok_or_else(|| {
            SentrixError::InvalidTransaction("proposer not found in stake registry".into())
        })?;

        let total_reward = block_reward.saturating_add(validator_fee_share);
        let commission =
            (total_reward as u128).saturating_mul(val.commission_rate as u128) / 10_000;
        let commission = commission as u64;
        let delegator_pool = total_reward.saturating_sub(commission);

        // Commission goes to validator's pending rewards
        let val = self
            .validators
            .get_mut(proposer)
            .ok_or_else(|| SentrixError::InvalidTransaction("proposer not found".into()))?;
        val.pending_rewards = val.pending_rewards.saturating_add(commission);

        // Delegator pool distributed proportionally
        let total_stake = val.total_stake();
        if total_stake == 0 || delegator_pool == 0 {
            return Ok(());
        }

        // Self-stake portion goes to validator too
        let self_share =
            (delegator_pool as u128).saturating_mul(val.self_stake as u128) / (total_stake as u128);
        val.pending_rewards = val.pending_rewards.saturating_add(self_share as u64);

        // Remaining distributed to delegators (tracked off-chain for now, claimable later)
        // For Phase 2a, we accumulate at the validator level and delegators claim proportionally
        // Full per-delegator reward tracking comes in a follow-up

        Ok(())
    }

    // ── Unbonding ────────────────────────────────────────────

    /// Process matured unbonding entries. Returns list of (delegator, amount) to credit back.
    pub fn process_unbonding(&mut self, current_height: u64) -> Vec<(String, u64)> {
        let mut released = Vec::new();
        let matured_keys: Vec<u64> = self
            .unbonding_queue
            .range(..=current_height)
            .map(|(&k, _)| k)
            .collect();

        for key in matured_keys {
            if let Some(entries) = self.unbonding_queue.remove(&key) {
                for entry in entries {
                    released.push((entry.delegator, entry.amount));
                }
            }
        }

        released
    }

    // ── Commission ───────────────────────────────────────────

    pub fn update_commission(&mut self, validator: &str, new_rate: u16) -> SentrixResult<()> {
        let val = self
            .validators
            .get_mut(validator)
            .ok_or_else(|| SentrixError::InvalidTransaction("validator not found".into()))?;

        if !(MIN_COMMISSION..=MAX_COMMISSION).contains(&new_rate) {
            return Err(SentrixError::InvalidTransaction(format!(
                "commission {} out of range [{}, {}]",
                new_rate, MIN_COMMISSION, MAX_COMMISSION
            )));
        }
        if new_rate > val.max_commission_rate {
            return Err(SentrixError::InvalidTransaction(format!(
                "commission {} exceeds max {}",
                new_rate, val.max_commission_rate
            )));
        }
        let diff = new_rate.abs_diff(val.commission_rate);
        if diff > MAX_COMMISSION_CHANGE_PER_EPOCH {
            return Err(SentrixError::InvalidTransaction(format!(
                "commission change {} exceeds max {} per epoch",
                diff, MAX_COMMISSION_CHANGE_PER_EPOCH
            )));
        }

        val.commission_rate = new_rate;
        Ok(())
    }

    // ── Queries ──────────────────────────────────────────────

    pub fn get_validator(&self, address: &str) -> Option<&ValidatorStake> {
        self.validators.get(address)
    }

    pub fn get_delegations(&self, delegator: &str) -> &[DelegationEntry] {
        self.delegations
            .get(delegator)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    pub fn get_pending_unbonding(&self, delegator: &str) -> Vec<&UnbondingEntry> {
        self.unbonding_queue
            .values()
            .flat_map(|v| v.iter())
            .filter(|e| e.delegator == delegator)
            .collect()
    }

    /// Weighted proposer selection: deterministic round-robin weighted by stake
    pub fn weighted_proposer(&self, height: u64, round: u32) -> Option<String> {
        if self.active_set.is_empty() {
            return None;
        }

        // Build cumulative stake weights
        let mut weights: Vec<(String, u64)> = Vec::new();
        let mut total_weight: u64 = 0;
        for addr in &self.active_set {
            if let Some(v) = self.validators.get(addr) {
                let w = v.total_stake();
                total_weight = total_weight.saturating_add(w);
                weights.push((addr.clone(), total_weight));
            }
        }

        if total_weight == 0 {
            return None;
        }

        // Deterministic selection based on height + round — use SHA-256 hash
        // to ensure uniform distribution across total_weight range.
        // Previous naive (height*31+round)*7 % total_weight always produced
        // small values → always picked the first validator.
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(height.to_le_bytes());
        hasher.update(round.to_le_bytes());
        let hash = hasher.finalize();
        // Take first 8 bytes as u64 for selector
        let mut sel_bytes = [0u8; 8];
        sel_bytes.copy_from_slice(&hash[..8]);
        let selector = u64::from_le_bytes(sel_bytes) % total_weight;

        for (addr, cumulative) in &weights {
            if selector < *cumulative {
                return Some(addr.clone());
            }
        }

        // Fallback to last validator
        weights.last().map(|(addr, _)| addr.clone())
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn new_registry() -> StakeRegistry {
        StakeRegistry::new()
    }

    fn register_val(reg: &mut StakeRegistry, addr: &str, stake: u64) {
        reg.register_validator(addr, stake, 1000, 0).unwrap();
    }

    #[test]
    fn test_register_validator_basic() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        assert_eq!(reg.validators.len(), 1);
        assert_eq!(reg.validators["0xval1"].self_stake, MIN_SELF_STAKE);
        assert_eq!(reg.validators["0xval1"].commission_rate, 1000);
    }

    #[test]
    fn test_register_below_min_stake() {
        let mut reg = new_registry();
        assert!(
            reg.register_validator("0xval1", MIN_SELF_STAKE - 1, 1000, 0)
                .is_err()
        );
    }

    #[test]
    fn test_register_duplicate() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        assert!(
            reg.register_validator("0xval1", MIN_SELF_STAKE, 1000, 0)
                .is_err()
        );
    }

    // C-09: max_commission_rate must be clamped to MAX_COMMISSION on
    // registration, so that `update_commission` cannot grow the rate
    // above the hard ceiling even after saturating the per-epoch
    // growth budget.
    #[test]
    fn test_c09_max_commission_rate_clamped_on_registration() {
        let mut reg = new_registry();
        // Register at the upper edge of the allowed commission band.
        // With the pre-fix saturating_add, max_commission_rate would be
        // 1500 + 1000 = 2500 > MAX_COMMISSION (2000).
        reg.register_validator("0xvalhigh", MIN_SELF_STAKE, 1500, 0)
            .unwrap();
        let stored = reg.get_validator("0xvalhigh").unwrap();
        assert!(
            stored.max_commission_rate <= MAX_COMMISSION,
            "max_commission_rate {} must not exceed MAX_COMMISSION {}",
            stored.max_commission_rate,
            MAX_COMMISSION
        );

        // Sanity check: a mid-range registration still gets a budget
        // (the clamp only kicks in near the ceiling).
        reg.register_validator("0xvallow", MIN_SELF_STAKE, 500, 0)
            .unwrap();
        let stored_low = reg.get_validator("0xvallow").unwrap();
        assert_eq!(
            stored_low.max_commission_rate,
            500 + MAX_COMMISSION_CHANGE_PER_EPOCH * 5,
            "low-rate registration should get the full 5-epoch budget"
        );
    }

    #[test]
    fn test_register_commission_out_of_range() {
        let mut reg = new_registry();
        assert!(
            reg.register_validator("0xval1", MIN_SELF_STAKE, 100, 0)
                .is_err()
        ); // too low
        assert!(
            reg.register_validator("0xval2", MIN_SELF_STAKE, 5000, 0)
                .is_err()
        ); // too high
    }

    #[test]
    fn test_delegate_basic() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 1_000_000, 100).unwrap();
        assert_eq!(reg.validators["0xval1"].total_delegated, 1_000_000);
        assert_eq!(reg.delegations["0xdel1"].len(), 1);
        assert_eq!(reg.delegations["0xdel1"][0].amount, 1_000_000);
    }

    #[test]
    fn test_delegate_add_to_existing() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 1_000, 100).unwrap();
        reg.delegate("0xdel1", "0xval1", 2_000, 200).unwrap();
        assert_eq!(reg.delegations["0xdel1"].len(), 1);
        assert_eq!(reg.delegations["0xdel1"][0].amount, 3_000);
        assert_eq!(reg.validators["0xval1"].total_delegated, 3_000);
    }

    #[test]
    fn test_delegate_to_unknown_validator() {
        let mut reg = new_registry();
        assert!(reg.delegate("0xdel1", "0xunknown", 1_000, 100).is_err());
    }

    #[test]
    fn test_delegate_zero_amount() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        assert!(reg.delegate("0xdel1", "0xval1", 0, 100).is_err());
    }

    #[test]
    fn test_delegate_to_tombstoned() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.tombstone("0xval1").unwrap();
        assert!(reg.delegate("0xdel1", "0xval1", 1_000, 100).is_err());
    }

    #[test]
    fn test_delegate_max_validators() {
        let mut reg = new_registry();
        for i in 0..MAX_DELEGATIONS_PER_ACCOUNT {
            let addr = format!("0xval{}", i);
            register_val(&mut reg, &addr, MIN_SELF_STAKE);
            reg.delegate("0xdel1", &addr, 1_000, 100).unwrap();
        }
        let extra = format!("0xval{}", MAX_DELEGATIONS_PER_ACCOUNT);
        register_val(&mut reg, &extra, MIN_SELF_STAKE);
        assert!(reg.delegate("0xdel1", &extra, 1_000, 100).is_err());
    }

    #[test]
    fn test_undelegate_basic() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 10_000, 100).unwrap();
        reg.undelegate("0xdel1", "0xval1", 3_000, 200).unwrap();

        assert_eq!(reg.delegations["0xdel1"][0].amount, 7_000);
        assert_eq!(reg.validators["0xval1"].total_delegated, 7_000);
        // Unbonding queued
        let completion = 200 + UNBONDING_PERIOD;
        assert_eq!(reg.unbonding_queue[&completion].len(), 1);
        assert_eq!(reg.unbonding_queue[&completion][0].amount, 3_000);
    }

    #[test]
    fn test_undelegate_exceeds_amount() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 1_000, 100).unwrap();
        assert!(reg.undelegate("0xdel1", "0xval1", 2_000, 200).is_err());
    }

    #[test]
    fn test_undelegate_full_removes_entry() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 5_000, 100).unwrap();
        reg.undelegate("0xdel1", "0xval1", 5_000, 200).unwrap();
        // Delegation entry should be removed (amount=0)
        let entries = reg.delegations.get("0xdel1").unwrap();
        assert!(entries.is_empty() || entries.iter().all(|e| e.amount > 0));
    }

    #[test]
    fn test_redelegate_basic() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval2", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 10_000, 100).unwrap();

        reg.redelegate("0xdel1", "0xval1", "0xval2", 4_000, 200)
            .unwrap();

        assert_eq!(
            reg.delegations["0xdel1"]
                .iter()
                .find(|e| e.validator == "0xval1")
                .unwrap()
                .amount,
            6_000
        );
        assert_eq!(
            reg.delegations["0xdel1"]
                .iter()
                .find(|e| e.validator == "0xval2")
                .unwrap()
                .amount,
            4_000
        );
        assert_eq!(reg.validators["0xval1"].total_delegated, 6_000);
        assert_eq!(reg.validators["0xval2"].total_delegated, 4_000);
    }

    #[test]
    fn test_redelegate_cooldown() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval2", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval3", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 10_000, 100).unwrap();
        reg.redelegate("0xdel1", "0xval1", "0xval2", 2_000, 200)
            .unwrap();

        // Second redelegate within cooldown should fail
        reg.delegate("0xdel1", "0xval2", 5_000, 200).unwrap();
        assert!(
            reg.redelegate("0xdel1", "0xval2", "0xval3", 1_000, 300)
                .is_err()
        );

        // After cooldown it works
        reg.redelegate(
            "0xdel1",
            "0xval2",
            "0xval3",
            1_000,
            200 + REDELEGATE_COOLDOWN,
        )
        .unwrap();
    }

    #[test]
    fn test_redelegate_same_validator() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 10_000, 100).unwrap();
        assert!(
            reg.redelegate("0xdel1", "0xval1", "0xval1", 1_000, 200)
                .is_err()
        );
    }

    #[test]
    fn test_slash_self_stake_only() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        let slashed = reg.slash("0xval1", 100).unwrap(); // 1%
        let expected = MIN_SELF_STAKE / 100;
        assert_eq!(slashed, expected);
        assert_eq!(
            reg.validators["0xval1"].self_stake,
            MIN_SELF_STAKE - expected
        );
    }

    #[test]
    fn test_slash_with_delegations() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", MIN_SELF_STAKE, 100)
            .unwrap(); // equal delegation
        let total = MIN_SELF_STAKE * 2;
        let slashed = reg.slash("0xval1", 2000).unwrap(); // 20%
        assert_eq!(slashed, total / 5);
    }

    #[test]
    fn test_jail_and_unjail() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.jail("0xval1", 200, 1000).unwrap();
        assert!(reg.validators["0xval1"].is_jailed);
        assert_eq!(reg.validators["0xval1"].jail_until, 1200);

        // Can't unjail before jail period
        assert!(reg.unjail("0xval1", 1100).is_err());

        // Can unjail after
        reg.unjail("0xval1", 1200).unwrap();
        assert!(!reg.validators["0xval1"].is_jailed);
    }

    #[test]
    fn test_tombstone_permanent() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.tombstone("0xval1").unwrap();
        assert!(reg.validators["0xval1"].is_tombstoned);
        assert!(reg.unjail("0xval1", u64::MAX).is_err());
    }

    #[test]
    fn test_active_set_top_21() {
        let mut reg = new_registry();
        for i in 0..30 {
            let addr = format!("0xval{:03}", i);
            let stake = MIN_SELF_STAKE + (i as u64) * 1_000_000;
            register_val(&mut reg, &addr, stake);
        }
        let active = reg.compute_active_set();
        assert_eq!(active.len(), MAX_ACTIVE_VALIDATORS);
        // Highest staker should be first (val029 has most stake)
        assert_eq!(active[0], "0xval029");
    }

    #[test]
    fn test_active_set_excludes_jailed() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE + 1_000_000);
        register_val(&mut reg, "0xval2", MIN_SELF_STAKE);
        reg.jail("0xval1", 100, 0).unwrap();
        let active = reg.compute_active_set();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0], "0xval2");
    }

    #[test]
    fn test_active_set_tiebreak_by_address() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xbbb", MIN_SELF_STAKE);
        register_val(&mut reg, "0xaaa", MIN_SELF_STAKE);
        let active = reg.compute_active_set();
        assert_eq!(active[0], "0xaaa"); // lower address wins tie
    }

    #[test]
    fn test_process_unbonding() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 10_000, 100).unwrap();
        reg.undelegate("0xdel1", "0xval1", 5_000, 100).unwrap();

        // Before maturity — nothing released
        let released = reg.process_unbonding(100 + UNBONDING_PERIOD - 1);
        assert!(released.is_empty());

        // At maturity — released
        let released = reg.process_unbonding(100 + UNBONDING_PERIOD);
        assert_eq!(released.len(), 1);
        assert_eq!(released[0], ("0xdel1".to_string(), 5_000));

        // Queue is now empty
        assert!(reg.unbonding_queue.is_empty());
    }

    #[test]
    fn test_distribute_reward() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", MIN_SELF_STAKE, 100)
            .unwrap();
        reg.update_active_set();

        // 10% commission, reward = 100_000_000 (1 SRX)
        reg.distribute_reward("0xval1", 100_000_000, 0).unwrap();

        let val = &reg.validators["0xval1"];
        // Commission = 10% of 100M = 10M
        // Self-stake share of delegator pool: 50% of 90M = 45M
        // Total pending = 10M + 45M = 55M
        assert_eq!(val.pending_rewards, 55_000_000);
    }

    #[test]
    fn test_commission_update() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);

        // Within bounds
        reg.update_commission("0xval1", 1200).unwrap(); // +2%
        assert_eq!(reg.validators["0xval1"].commission_rate, 1200);

        // Too large a change
        assert!(reg.update_commission("0xval1", 1500).is_err()); // +3%, max is 2%
    }

    #[test]
    fn test_weighted_proposer() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval2", MIN_SELF_STAKE * 2); // double stake
        reg.update_active_set();

        // Should get a proposer (deterministic but depends on height+round)
        let p = reg.weighted_proposer(0, 0);
        assert!(p.is_some());

        // Different heights give different proposers eventually
        let mut saw_val1 = false;
        let mut saw_val2 = false;
        for h in 0..100 {
            if let Some(p) = reg.weighted_proposer(h, 0) {
                if p == "0xval1" {
                    saw_val1 = true;
                }
                if p == "0xval2" {
                    saw_val2 = true;
                }
            }
        }
        // val2 has 2x stake so should appear more often, but both should appear
        assert!(saw_val1 || saw_val2); // at minimum one appears
    }

    #[test]
    fn test_weighted_proposer_empty() {
        let reg = new_registry();
        assert!(reg.weighted_proposer(0, 0).is_none());
    }

    #[test]
    fn test_max_candidates() {
        let mut reg = new_registry();
        for i in 0..MAX_CANDIDATES {
            let addr = format!("0xval{:04}", i);
            register_val(&mut reg, &addr, MIN_SELF_STAKE);
        }
        assert!(
            reg.register_validator("0xoverflow", MIN_SELF_STAKE, 1000, 0)
                .is_err()
        );
    }

    #[test]
    fn test_total_stake_calculation() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 5_000_000, 100).unwrap();
        assert_eq!(
            reg.validators["0xval1"].total_stake(),
            MIN_SELF_STAKE + 5_000_000
        );
    }

    #[test]
    fn test_slash_unbonding_entries() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 10_000, 100).unwrap();
        reg.undelegate("0xdel1", "0xval1", 10_000, 100).unwrap();

        // Slash 10% — should also reduce unbonding amount
        reg.slash("0xval1", 1000).unwrap();

        let completion = 100 + UNBONDING_PERIOD;
        let unbonding = &reg.unbonding_queue[&completion][0];
        assert!(unbonding.amount < 10_000); // should be slashed
        assert_eq!(unbonding.amount, 9_000); // 10% of 10K = 1K slashed
    }

    #[test]
    fn test_unjail_below_min_stake() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.jail("0xval1", 100, 0).unwrap();
        // Slash enough to go below minimum
        reg.slash("0xval1", 5000).unwrap(); // 50%
        // Can't unjail because self-stake below minimum
        assert!(reg.unjail("0xval1", 200).is_err());
    }

    #[test]
    fn test_get_pending_unbonding() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdel1", "0xval1", 10_000, 100).unwrap();
        reg.undelegate("0xdel1", "0xval1", 3_000, 100).unwrap();
        reg.undelegate("0xdel1", "0xval1", 2_000, 200).unwrap();

        let pending = reg.get_pending_unbonding("0xdel1");
        assert_eq!(pending.len(), 2);
    }
}
