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
/// P1: minimum active validator count for BFT safety. A BFT round
/// requires ⌈2N/3⌉+1 stake-weighted votes for finality, and byzantine
/// tolerance `f = ⌊(N-1)/3⌋` is only non-zero at N ≥ 4. Below four
/// validators the network still produces blocks under PoA round-robin
/// but cannot mathematically tolerate a single byzantine actor under
/// BFT, so the validator loop must refuse to start Voyager mode until
/// the active set meets this size.
pub const MIN_BFT_VALIDATORS: usize = 4;
pub const MAX_CANDIDATES: usize = 100;
pub const UNBONDING_PERIOD: u64 = 201_600; // 7 days at 3s blocks
pub const MAX_DELEGATIONS_PER_ACCOUNT: usize = 10;
pub const MAX_UNBONDING_ENTRIES: usize = 7;
/// P1: hard cap on total unbonding entries for a single validator across
/// all delegators. Prevents pathological memory growth from many
/// small-stake delegators issuing partial unbondings against the same
/// validator; the per-(delegator, validator) cap alone (= 7) multiplied
/// by MAX_CANDIDATES delegators × MAX_DELEGATIONS_PER_ACCOUNT would
/// otherwise bound total entries in the thousands per validator. 10 000
/// is generous for realistic networks and far below the point where
/// the BTreeMap iteration becomes a block-time concern.
pub const MAX_UNBONDING_PER_VALIDATOR: usize = 10_000;
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
    /// Block height of the last successful `update_commission` call.
    /// 0 = never changed. Used to rate-limit commission churn to at
    /// most one change per epoch — defends against the N-call stepping
    /// attack where an operator calls `update_commission(+2%)` many
    /// times within one block to inflate commission unboundedly while
    /// each individual call stays inside `MAX_COMMISSION_CHANGE_PER_EPOCH`.
    #[serde(default)]
    pub last_commission_change_height: u64,
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
    /// V4 Step 1: per-delegator accumulated rewards, pending claim.
    /// delegator_address → total reward across all validators they delegate to.
    /// Persists across blocks. Cleared when the delegator successfully calls
    /// `claim_rewards` (Step 3 in the V4 rollout — not wired yet).
    ///
    /// `#[serde(default)]` so chains with older chain.db can upgrade without
    /// a migration event. Field populated at epoch boundary (Step 2 wiring
    /// in `distribute_reward` v2) once that lands.
    ///
    /// Design: `audits/reward-distribution-fix-design.md`
    #[serde(default)]
    pub delegator_rewards: HashMap<String, u64>,
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
                last_commission_change_height: 0,
            },
        );

        Ok(())
    }

    // ── Self-stake top-up ────────────────────────────────────

    /// Add real SRX to an existing validator's `self_stake`. Caller is
    /// the validator itself (authorization enforced by the dispatch
    /// site at `block_executor.rs`, which checks `tx.from_address ==
    /// validator_address`). The corresponding SRX has already been
    /// transferred from the validator's balance into PROTOCOL_TREASURY
    /// by the outer `accounts.transfer` in apply-Pass-2; this fn only
    /// updates the staking registry.
    ///
    /// Supply-invariant preserving — no mint, no phantom. SRX moves
    /// from circulating balance into bonded stake. Designed as the
    /// proper recovery path for slashed validators whose `self_stake
    /// < MIN_SELF_STAKE`, replacing the break-glass `force_unjail`
    /// (which mints phantom SRX). After this call brings self_stake
    /// back ≥ MIN_SELF_STAKE, the validator can run the standard
    /// `unjail` op without supply damage.
    pub fn add_self_stake(&mut self, validator: &str, amount: u64) -> SentrixResult<()> {
        if amount == 0 {
            return Err(SentrixError::InvalidTransaction(
                "AddSelfStake: amount must be > 0".into(),
            ));
        }
        let val = self
            .validators
            .get_mut(validator)
            .ok_or_else(|| SentrixError::InvalidTransaction("validator not found".into()))?;
        if val.is_tombstoned {
            return Err(SentrixError::InvalidTransaction(
                "AddSelfStake: tombstoned validators cannot add self_stake".into(),
            ));
        }
        val.self_stake = val.self_stake.checked_add(amount).ok_or_else(|| {
            SentrixError::InvalidTransaction("AddSelfStake: self_stake overflow".into())
        })?;
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
        // AND the global per-validator cap (P1). Iterate the queue once and
        // count both buckets in the same pass.
        let (existing_unbonding, per_validator_total) = self
            .unbonding_queue
            .values()
            .flat_map(|v| v.iter())
            .filter(|u| u.validator == validator)
            .fold((0usize, 0usize), |(per_pair, total), u| {
                (per_pair + (u.delegator == delegator) as usize, total + 1)
            });
        if existing_unbonding >= MAX_UNBONDING_ENTRIES {
            return Err(SentrixError::InvalidTransaction(format!(
                "max {} unbonding entries per delegation",
                MAX_UNBONDING_ENTRIES
            )));
        }
        if per_validator_total >= MAX_UNBONDING_PER_VALIDATOR {
            return Err(SentrixError::InvalidTransaction(format!(
                "max {} unbonding entries per validator reached",
                MAX_UNBONDING_PER_VALIDATOR
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

            // Reduce individual delegation amounts proportionally.
            //
            // P1: use ceiling division so the slashed total is at least
            // the validator-level `remaining` amount, never below. Floor
            // division (previous behaviour) loses fractions — e.g. at a
            // 10 % slash rate across three 99-token delegations, each
            // delegator loses 9 tokens (floor of 9.9) instead of 10,
            // under-slashing the network. Ceiling may over-slash a
            // single delegator by at most 1 sentri per rounding step
            // (imperceptible) but keeps the protocol-wide slash invariant
            // ≥ stated rate.
            for entries in self.delegations.values_mut() {
                for entry in entries.iter_mut() {
                    if entry.validator == validator && delegated_before > 0 {
                        let num = (entry.amount as u128).saturating_mul(remaining as u128);
                        let den = delegated_before as u128;
                        let entry_slash = num.div_ceil(den);
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

        // V3: slashing may have pushed self_stake below MIN_SELF_STAKE,
        // which makes `is_active_eligible()` false. Refresh active_set so
        // the now-ineligible validator is evicted immediately.
        self.update_active_set();

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
        // V3: refresh active_set so the jailed validator is evicted from
        // proposer rotation immediately. Without this, a validator jailed
        // mid-epoch stays eligible to propose until the next epoch tick.
        self.update_active_set();
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
        // V3: same reasoning as jail() — immediate active_set eviction.
        self.update_active_set();
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
        // V3: refresh active_set so the unjailed validator re-enters
        // proposer rotation immediately rather than waiting for the
        // next epoch tick.
        self.update_active_set();
        Ok(())
    }

    /// Operator-only recovery path for validators that have been jailed
    /// AND slashed below `MIN_SELF_STAKE`, leaving them unable to clear
    /// the jail via the normal `unjail()` path and unable to stake up
    /// because the chain is stuck (BFT refuses to start while
    /// `active_set` is empty). This is the chicken-and-egg case observed
    /// on testnet after the pre-#147 BFT livelock auto-slashed all 4
    /// validators.
    ///
    /// Semantics differ from `unjail()` in two places: the jail-period
    /// cooldown (`jail_until`) is skipped, and if the validator's
    /// `self_stake` is below `MIN_SELF_STAKE` it is bumped back up to
    /// exactly `MIN_SELF_STAKE` so they clear the eligibility check.
    /// Tombstoned validators are still rejected — tombstone is
    /// permanent by design.
    ///
    /// Callers are responsible for running this only on an operator-
    /// owned chain DB (it bypasses consensus) and then propagating the
    /// same edit to every peer's DB before consensus resumes, otherwise
    /// peers will disagree on the stake_registry state.
    pub fn force_unjail(&mut self, validator: &str) -> SentrixResult<()> {
        let val = self
            .validators
            .get_mut(validator)
            .ok_or_else(|| SentrixError::InvalidTransaction("validator not found".into()))?;
        if val.is_tombstoned {
            return Err(SentrixError::InvalidTransaction(
                "tombstoned validators cannot unjail".into(),
            ));
        }
        if val.self_stake < MIN_SELF_STAKE {
            val.self_stake = MIN_SELF_STAKE;
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

    /// V4 Step 2: reward distribution v2 — pay every signer in the
    /// justification pro-rata by stake, split each signer's share into
    /// commission + self-stake + delegator pool, and accumulate per-
    /// delegator share into `delegator_rewards` (which Step 3's claim
    /// CLI drains into SRX balances).
    ///
    /// Back-compat: if `signers` is empty (e.g. Pioneer PoA path where
    /// blocks have no justification), fall through to the legacy
    /// single-proposer payout so this function stays usable on
    /// pre-Voyager chains.
    ///
    /// Design: `audits/reward-distribution-fix-design.md`.
    pub fn distribute_reward(
        &mut self,
        proposer: &str,
        signers: &[(String, u64)],
        block_reward: u64,
        validator_fee_share: u64,
    ) -> SentrixResult<()> {
        let total_reward = block_reward.saturating_add(validator_fee_share);

        // ── Pioneer/legacy path ───────────────────────────────────
        if signers.is_empty() {
            return self.distribute_to_single_validator(proposer, total_reward);
        }

        // ── Voyager v2 path: multi-signer pro-rata ────────────────
        let total_signer_stake: u64 = signers.iter().map(|(_, s)| *s).sum();
        if total_signer_stake == 0 {
            // Degenerate — no stake to weight on; fall back.
            return self.distribute_to_single_validator(proposer, total_reward);
        }

        for (addr, signer_stake) in signers {
            if *signer_stake == 0 {
                continue;
            }
            let signer_share = (total_reward as u128)
                .saturating_mul(*signer_stake as u128)
                / total_signer_stake as u128;
            let signer_share = signer_share as u64;
            if signer_share == 0 {
                continue;
            }
            self.pay_one_signer(addr, signer_share)?;
        }
        Ok(())
    }

    /// Legacy single-validator payout (commission + self-stake to val,
    /// delegator pool dropped). Used by the Pioneer fallback above.
    fn distribute_to_single_validator(
        &mut self,
        validator_addr: &str,
        total_reward: u64,
    ) -> SentrixResult<()> {
        let val = self.validators.get(validator_addr).ok_or_else(|| {
            SentrixError::InvalidTransaction(
                "proposer not found in stake registry".into(),
            )
        })?;
        let commission =
            (total_reward as u128).saturating_mul(val.commission_rate as u128) / 10_000;
        let commission = commission as u64;
        let delegator_pool = total_reward.saturating_sub(commission);
        let total_stake = val.total_stake();
        let self_stake = val.self_stake;

        let val_mut = self
            .validators
            .get_mut(validator_addr)
            .ok_or_else(|| SentrixError::InvalidTransaction("proposer not found".into()))?;
        val_mut.pending_rewards = val_mut.pending_rewards.saturating_add(commission);

        if total_stake == 0 || delegator_pool == 0 {
            return Ok(());
        }
        let self_share =
            (delegator_pool as u128).saturating_mul(self_stake as u128) / total_stake as u128;
        val_mut.pending_rewards = val_mut.pending_rewards.saturating_add(self_share as u64);
        Ok(())
    }

    /// V4 Step 2: split one signer's pro-rata share into commission +
    /// self-stake + per-delegator accumulator.
    fn pay_one_signer(
        &mut self,
        validator_addr: &str,
        signer_share: u64,
    ) -> SentrixResult<()> {
        // Fetch validator state (pre-mutation so we can read then write).
        let (commission_rate, self_stake, total_stake) = {
            let val = match self.validators.get(validator_addr) {
                Some(v) => v,
                None => {
                    // Signer not in our registry (stale justification?) — drop
                    // this share rather than panic. Won't happen on a healthy
                    // chain because justification signers are filtered to
                    // active_set at emit time.
                    return Ok(());
                }
            };
            (val.commission_rate, val.self_stake, val.total_stake())
        };

        // 1. Commission off the top.
        let commission =
            (signer_share as u128).saturating_mul(commission_rate as u128) / 10_000;
        let commission = commission as u64;
        let pool = signer_share.saturating_sub(commission);

        // 2. Commission credited to validator.
        {
            let val = self
                .validators
                .get_mut(validator_addr)
                .expect("validator present, just read above");
            val.pending_rewards = val.pending_rewards.saturating_add(commission);
        }

        if total_stake == 0 || pool == 0 {
            return Ok(());
        }

        // 3. Self-stake portion credited to validator.
        let self_share =
            (pool as u128).saturating_mul(self_stake as u128) / total_stake as u128;
        let self_share = self_share as u64;
        {
            let val = self
                .validators
                .get_mut(validator_addr)
                .expect("validator present");
            val.pending_rewards = val.pending_rewards.saturating_add(self_share);
        }

        // 4. Delegator pool distributed per-delegator into accumulator.
        let delegator_pool = pool.saturating_sub(self_share);
        if delegator_pool == 0 {
            return Ok(());
        }

        // Sum of delegations to this validator (denominator for pro-rata).
        let total_delegated: u64 = self
            .delegations
            .values()
            .flatten()
            .filter(|e| e.validator == validator_addr)
            .map(|e| e.amount)
            .sum();
        if total_delegated == 0 {
            return Ok(());
        }

        // Collect (delegator, amount) pairs first to avoid double-borrow
        // of self when writing into delegator_rewards.
        let shares: Vec<(String, u64)> = self
            .delegations
            .values()
            .flatten()
            .filter(|e| e.validator == validator_addr)
            .map(|e| {
                let share = (delegator_pool as u128).saturating_mul(e.amount as u128)
                    / total_delegated as u128;
                (e.delegator.clone(), share as u64)
            })
            .collect();

        for (delegator, share) in shares {
            if share == 0 {
                continue;
            }
            let entry = self.delegator_rewards.entry(delegator).or_insert(0);
            *entry = entry.saturating_add(share);
        }

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

    /// Change `validator`'s commission rate. Enforces:
    ///   1. `new_rate ∈ [MIN_COMMISSION, MAX_COMMISSION]` (invariant)
    ///   2. `new_rate ≤ max_commission_rate` (operator-declared ceiling)
    ///   3. |new − old| ≤ `MAX_COMMISSION_CHANGE_PER_EPOCH` (single-step cap)
    ///   4. At most one successful call per epoch per validator — closes
    ///      the N-call stepping attack where each step stayed inside (3)
    ///      but cumulative drift exceeded the per-epoch intent.
    ///
    /// `current_height` is the block height at which the transaction
    /// carrying this commission change is being applied. The caller
    /// (block executor or test fixture) is responsible for passing the
    /// authoritative height — this function is stateless w.r.t. the
    /// global chain head.
    pub fn update_commission(
        &mut self,
        validator: &str,
        new_rate: u16,
        current_height: u64,
    ) -> SentrixResult<()> {
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

        // Rate-limit: reject if already changed within the current epoch.
        // `last_commission_change_height == 0` is the "never changed"
        // sentinel and must pass through (no previous change to compare).
        // `current_height == 0` is genesis and registration-time only —
        // no user-initiated commission change can land at genesis so this
        // is effectively unreachable in production, but kept defensive.
        if val.last_commission_change_height > 0 {
            let last_epoch = val.last_commission_change_height / crate::epoch::EPOCH_LENGTH;
            let current_epoch = current_height / crate::epoch::EPOCH_LENGTH;
            if last_epoch == current_epoch {
                return Err(SentrixError::InvalidTransaction(format!(
                    "commission already changed in epoch {} (at height {}); at most one change per epoch",
                    current_epoch, val.last_commission_change_height
                )));
            }
        }

        val.commission_rate = new_rate;
        val.last_commission_change_height = current_height;
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

    /// Proposer selection: deterministic round-robin over the active set.
    ///
    /// Picks `active_set[(height + round) % len]`. Same validator at the
    /// same `(height, round)` for all peers — required for BFT consensus
    /// to agree on who's allowed to propose. Function name kept as
    /// `weighted_proposer` for call-site compatibility; the *voting*
    /// weight is still stake-weighted (see `BftEngine::on_*_weighted`).
    /// Only the proposer slot itself is now stake-blind.
    ///
    /// Backlog #consensus-audit Section 5(4): the previous SHA-256 over
    /// cumulative stake weights gave the largest-stake validator a much
    /// bigger share of proposer slots, which is a centralisation risk
    /// once mainnet is on Voyager (a single big delegator could see its
    /// validator proposing >50% of blocks). Pure round-robin keeps the
    /// proposer rotation fair across the active set; stake still matters
    /// in two places: who is *in* the active set (top-N by stake), and
    /// vote weight at quorum time.
    ///
    /// `active_set` order is deterministic (sorted by stake desc with
    /// address-asc tie-break — see `compute_active_set`), so all nodes
    /// pick the same proposer for any given `(height, round)`.
    /// V4 Step 1 helper: list all delegations TO a specific validator.
    /// Existing `self.delegations` is delegator-keyed, so finding all
    /// delegators of a validator requires scanning every delegator's
    /// entries. This helper encapsulates that scan so call sites in
    /// `distribute_reward` v2 (Step 2) stay readable.
    ///
    /// O(total delegation entries). Sufficient at current scale; a
    /// validator-keyed secondary index would drop it to O(per-validator)
    /// if profiling shows it matters.
    pub fn delegations_to(&self, validator: &str) -> Vec<&DelegationEntry> {
        self.delegations
            .values()
            .flatten()
            .filter(|e| e.validator == validator)
            .collect()
    }

    /// V4 Step 1 helper: claim pending rewards for a delegator.
    /// Returns amount claimed (cleared from `delegator_rewards`).
    /// Wiring into balance-credit (so the claimed amount lands in
    /// the delegator's SRX balance) happens in Step 3 via the CLI
    /// transaction + blockchain::apply side — this helper just
    /// consumes the accumulator.
    pub fn take_delegator_rewards(&mut self, delegator: &str) -> u64 {
        self.delegator_rewards.remove(delegator).unwrap_or(0)
    }

    pub fn weighted_proposer(&self, height: u64, round: u32) -> Option<String> {
        if self.active_set.is_empty() {
            return None;
        }
        let n = self.active_set.len() as u64;
        let idx = (height.wrapping_add(round as u64) % n) as usize;
        Some(self.active_set[idx].clone())
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

    /// V4 Step 1 regression: `delegations_to` returns every delegation
    /// entry whose validator matches, across all delegators.
    #[test]
    fn test_v4_delegations_to_aggregates_across_delegators() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval2", MIN_SELF_STAKE);
        reg.delegate("0xdelA", "0xval1", 1_000, 100).unwrap();
        reg.delegate("0xdelB", "0xval1", 2_000, 101).unwrap();
        reg.delegate("0xdelC", "0xval2", 5_000, 102).unwrap();

        let to_val1 = reg.delegations_to("0xval1");
        assert_eq!(to_val1.len(), 2);
        let total_to_val1: u64 = to_val1.iter().map(|e| e.amount).sum();
        assert_eq!(total_to_val1, 3_000);

        let to_val2 = reg.delegations_to("0xval2");
        assert_eq!(to_val2.len(), 1);
        assert_eq!(to_val2[0].amount, 5_000);

        let to_unknown = reg.delegations_to("0xnobody");
        assert!(to_unknown.is_empty());
    }

    /// V4 Step 1 regression: `take_delegator_rewards` drains the
    /// accumulator and returns the amount (for later balance-credit
    /// in Step 3's claim flow).
    #[test]
    fn test_v4_take_delegator_rewards_drains_accumulator() {
        let mut reg = new_registry();
        reg.delegator_rewards.insert("0xdelA".to_string(), 12_345);
        reg.delegator_rewards.insert("0xdelB".to_string(), 7_890);

        let claimed_a = reg.take_delegator_rewards("0xdelA");
        assert_eq!(claimed_a, 12_345);
        assert!(!reg.delegator_rewards.contains_key("0xdelA"));

        // re-claim returns 0
        assert_eq!(reg.take_delegator_rewards("0xdelA"), 0);

        // unaffected entry still there
        assert_eq!(reg.delegator_rewards["0xdelB"], 7_890);

        // unknown delegator returns 0
        assert_eq!(reg.take_delegator_rewards("0xunknown"), 0);
    }

    /// V4 Step 1 backward-compat: StakeRegistry::default() produces
    /// empty delegator_rewards; existing tests building registries via
    /// default() continue to work without migration.
    #[test]
    fn test_v4_delegator_rewards_default_empty() {
        let reg = StakeRegistry::default();
        assert!(reg.delegator_rewards.is_empty());
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

        // Legacy Pioneer-path (empty signers) — 10% commission, reward = 1 SRX
        reg.distribute_reward("0xval1", &[], 100_000_000, 0).unwrap();

        let val = &reg.validators["0xval1"];
        // Commission = 10% of 100M = 10M
        // Self-stake share of delegator pool: 50% of 90M = 45M
        // Total pending = 10M + 45M = 55M
        assert_eq!(val.pending_rewards, 55_000_000);
    }

    /// V4 Step 2 regression: multi-signer reward distribution v2.
    /// 4-validator chain, all 4 signers with equal stake, 100M reward.
    /// Each signer gets 25M share; with 10% commission + 50% self-stake
    /// split (validator's own self_stake vs delegations) and per-validator
    /// delegator accumulation.
    #[test]
    fn test_v4_distribute_reward_multi_signer_equal_stakes() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval2", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval3", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval4", MIN_SELF_STAKE);
        // Each validator has one delegator with MIN_SELF_STAKE delegated
        reg.delegate("0xdelA", "0xval1", MIN_SELF_STAKE, 100).unwrap();
        reg.delegate("0xdelB", "0xval2", MIN_SELF_STAKE, 100).unwrap();
        reg.delegate("0xdelC", "0xval3", MIN_SELF_STAKE, 100).unwrap();
        reg.delegate("0xdelD", "0xval4", MIN_SELF_STAKE, 100).unwrap();
        reg.update_active_set();

        // All 4 signers with equal stake weight.
        let signers = vec![
            ("0xval1".to_string(), MIN_SELF_STAKE * 2),
            ("0xval2".to_string(), MIN_SELF_STAKE * 2),
            ("0xval3".to_string(), MIN_SELF_STAKE * 2),
            ("0xval4".to_string(), MIN_SELF_STAKE * 2),
        ];
        reg.distribute_reward("0xval1", &signers, 100_000_000, 0).unwrap();

        // Each signer's share = 100M / 4 = 25M
        // Commission (10%) = 2.5M → validator pending
        // Pool = 22.5M → split 50/50 self vs delegator
        // Self share = 11.25M → validator pending (so validator total = 2.5 + 11.25 = 13.75M)
        // Delegator pool = 11.25M → delegator accumulator
        for v in ["0xval1", "0xval2", "0xval3", "0xval4"] {
            assert_eq!(
                reg.validators[v].pending_rewards, 13_750_000,
                "validator {} must get commission+self share",
                v
            );
        }
        for d in ["0xdelA", "0xdelB", "0xdelC", "0xdelD"] {
            assert_eq!(
                reg.delegator_rewards[d], 11_250_000,
                "delegator {} must get pro-rata share",
                d
            );
        }
    }

    /// V4 Step 2 regression: signers outside the stake_registry are
    /// silently skipped (defensive — won't happen on a healthy chain
    /// because justification signers are filtered to active_set at
    /// emit time, but we don't want a panic if it does slip through).
    #[test]
    fn test_v4_distribute_reward_unknown_signer_skipped() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.update_active_set();

        let signers = vec![
            ("0xval1".to_string(), MIN_SELF_STAKE),
            ("0xrogue".to_string(), MIN_SELF_STAKE), // not registered
        ];
        // Should not error, should silently skip rogue.
        reg.distribute_reward("0xval1", &signers, 100_000_000, 0).unwrap();

        // val1 got 50M share; rogue got 0 (skipped).
        // Commission 10% = 5M; pool 45M; self-share = 45M × SELF/SELF = 45M (no delegators).
        // Total = 5 + 45 = 50M.
        assert_eq!(reg.validators["0xval1"].pending_rewards, 50_000_000);
        assert!(!reg.delegator_rewards.contains_key("0xrogue"));
    }

    /// V4 Step 2 regression: empty signers falls back to legacy
    /// single-validator path (so Pioneer chains keep working).
    #[test]
    fn test_v4_distribute_reward_empty_signers_legacy_fallback() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.update_active_set();

        // Empty signers vec = Pioneer fallback = pay proposer only (legacy).
        reg.distribute_reward("0xval1", &[], 100_000_000, 0).unwrap();
        // Same as test_distribute_reward with no delegators:
        // commission 10M + self-share (all of pool since no delegators share) = 100M.
        assert_eq!(reg.validators["0xval1"].pending_rewards, 100_000_000);
    }

    /// V4 Step 3 regression: claim flow drains accumulator.
    #[test]
    fn test_v4_claim_rewards_after_distribute() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.delegate("0xdelA", "0xval1", MIN_SELF_STAKE, 100).unwrap();
        reg.update_active_set();

        // One signer, full reward to them.
        let signers = vec![("0xval1".to_string(), MIN_SELF_STAKE * 2)];
        reg.distribute_reward("0xval1", &signers, 100_000_000, 0).unwrap();

        // Delegator should have accumulated a share.
        let accumulated = reg.delegator_rewards.get("0xdelA").copied().unwrap_or(0);
        assert!(accumulated > 0, "delegator should have pending rewards");

        // Claim drains.
        let claimed = reg.take_delegator_rewards("0xdelA");
        assert_eq!(claimed, accumulated);
        assert!(!reg.delegator_rewards.contains_key("0xdelA"));

        // Re-claim returns 0.
        assert_eq!(reg.take_delegator_rewards("0xdelA"), 0);
    }

    #[test]
    fn test_commission_update() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);

        // Within bounds — single call in epoch 0 succeeds
        reg.update_commission("0xval1", 1200, 100).unwrap(); // +2% at h=100
        assert_eq!(reg.validators["0xval1"].commission_rate, 1200);

        // Too large a change — but also would fail same-epoch rule;
        // use a fresh epoch to isolate the size-cap assertion.
        let h = crate::epoch::EPOCH_LENGTH; // epoch 1
        assert!(reg.update_commission("0xval1", 1500, h).is_err()); // +3%, max is 2%
    }

    /// Regression test for V5 commission-stepping attack. Before the
    /// per-epoch throttle, an operator could call `update_commission`
    /// repeatedly within one block, each call clearing the per-step
    /// 2% diff check while cumulatively inflating the commission far
    /// beyond the per-epoch intent. After the fix: only the first
    /// call per epoch lands; subsequent calls in the same epoch are
    /// rejected regardless of size.
    ///
    /// This test MUST FAIL on main (before the fix) because the 2nd
    /// and 3rd in-epoch calls would succeed, raising commission from
    /// 1000 → 1600 within one epoch. After the fix, only call #1 lands.
    #[test]
    fn test_commission_stepping_attack_rejected_same_epoch() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        // Start at 10% commission (set by register_val / default).
        let start = reg.validators["0xval1"].commission_rate;

        // First call in epoch 0 — lands.
        reg.update_commission("0xval1", start + 200, 50).unwrap();
        let after_first = reg.validators["0xval1"].commission_rate;
        assert_eq!(after_first, start + 200);

        // Second call, still epoch 0, small step — must be REJECTED.
        let r2 = reg.update_commission("0xval1", start + 400, 100);
        assert!(r2.is_err(), "2nd call in same epoch must fail");
        let msg2 = format!("{:?}", r2.unwrap_err());
        assert!(
            msg2.contains("epoch") && msg2.contains("one change"),
            "expected per-epoch rate-limit error, got: {}",
            msg2
        );
        assert_eq!(
            reg.validators["0xval1"].commission_rate,
            after_first,
            "rate must not have advanced past first call"
        );

        // Third call, still epoch 0 — also rejected.
        let r3 = reg.update_commission("0xval1", start + 600, 150);
        assert!(r3.is_err(), "3rd call in same epoch must fail");

        // Advance to epoch 1 — next small step should now succeed.
        let h_next_epoch = crate::epoch::EPOCH_LENGTH + 10;
        reg.update_commission("0xval1", start + 400, h_next_epoch)
            .unwrap();
        assert_eq!(reg.validators["0xval1"].commission_rate, start + 400);

        // Another call in epoch 1 — rejected again (rate-limit applies
        // every epoch, not just epoch 0).
        let r5 = reg.update_commission("0xval1", start + 600, h_next_epoch + 1);
        assert!(r5.is_err(), "2nd call in epoch 1 must also fail");
    }

    /// V3 regression: jail() must immediately evict the validator from
    /// active_set so the proposer rotation skips them. Before the fix,
    /// active_set only updated on explicit `update_active_set()` calls
    /// (typically at epoch boundaries), leaving a jailed validator
    /// eligible to propose for up to EPOCH_LENGTH blocks.
    #[test]
    fn test_jail_evicts_from_active_set_immediately() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval2", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval3", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval4", MIN_SELF_STAKE);
        reg.update_active_set();
        assert_eq!(reg.active_count(), 4);
        assert!(reg.is_active("0xval2"));

        // Jail val2 mid-epoch.
        reg.jail("0xval2", 100, 50).unwrap();

        // active_set must have dropped val2 without an explicit
        // update_active_set() call from the caller.
        assert!(
            !reg.is_active("0xval2"),
            "jailed validator must be evicted immediately"
        );
        assert_eq!(reg.active_count(), 3);

        // weighted_proposer must now never return val2.
        for h in 0..100 {
            for r in 0..4 {
                let p = reg.weighted_proposer(h, r);
                assert_ne!(p.as_deref(), Some("0xval2"));
            }
        }
    }

    /// V3 regression: tombstone() must also evict immediately AND must
    /// not be reversible by unjail() (covered elsewhere but asserted
    /// here for completeness).
    #[test]
    fn test_tombstone_evicts_permanently() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval2", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval3", MIN_SELF_STAKE);
        register_val(&mut reg, "0xval4", MIN_SELF_STAKE);
        reg.update_active_set();

        reg.tombstone("0xval3").unwrap();
        assert!(!reg.is_active("0xval3"));

        // Unjail must refuse tombstoned validators.
        let r = reg.unjail("0xval3", u64::MAX - 1);
        assert!(r.is_err(), "tombstoned validator cannot be unjailed");
        assert!(!reg.is_active("0xval3"));
    }

    /// V3 regression: unjail() re-admits the validator to active_set
    /// immediately (subject to the normal eligibility checks).
    #[test]
    fn test_unjail_readmits_to_active_set() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE * 2);
        register_val(&mut reg, "0xval2", MIN_SELF_STAKE);
        reg.update_active_set();
        reg.jail("0xval2", 50, 100).unwrap();
        assert!(!reg.is_active("0xval2"));

        // Jail period expired — unjail at height > jail_until.
        reg.unjail("0xval2", 151).unwrap();
        assert!(
            reg.is_active("0xval2"),
            "unjailed + eligible validator must rejoin active_set immediately"
        );
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
    fn test_force_unjail_restores_stake_and_clears_flags() {
        // Operator-only recovery for the chicken-and-egg state the
        // testnet hit on 2026-04-19: pre-#147 livelock auto-slashed
        // every validator below MIN_SELF_STAKE, so the normal
        // `unjail()` path refused and BFT could not restart.
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.jail("0xval1", 100, 1000).unwrap();
        reg.slash("0xval1", 5000).unwrap(); // 50%, drops below min

        assert!(reg.validators["0xval1"].is_jailed);
        assert!(reg.validators["0xval1"].self_stake < MIN_SELF_STAKE);
        // Sanity: the normal path can't recover this one.
        assert!(reg.unjail("0xval1", 200).is_err());

        reg.force_unjail("0xval1").unwrap();

        let v = &reg.validators["0xval1"];
        assert!(!v.is_jailed, "force_unjail must clear is_jailed");
        assert_eq!(v.jail_until, 0, "force_unjail must clear jail_until");
        assert_eq!(
            v.self_stake, MIN_SELF_STAKE,
            "force_unjail must restore self_stake to MIN_SELF_STAKE",
        );
    }

    #[test]
    fn test_force_unjail_preserves_stake_when_already_above_min() {
        // If stake is already ≥ MIN_SELF_STAKE, force_unjail must not
        // overwrite it — only jail flags are cleared.
        let mut reg = new_registry();
        let bigger = MIN_SELF_STAKE.saturating_add(12_345);
        register_val(&mut reg, "0xval1", bigger);
        reg.jail("0xval1", 100, 9999).unwrap();

        reg.force_unjail("0xval1").unwrap();

        let v = &reg.validators["0xval1"];
        assert!(!v.is_jailed);
        assert_eq!(v.jail_until, 0);
        assert_eq!(v.self_stake, bigger, "stake should not be touched");
    }

    // ── add_self_stake ────────────────────────────────────────

    #[test]
    fn test_add_self_stake_bumps_existing_validator() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        let initial = reg.validators["0xval1"].self_stake;

        reg.add_self_stake("0xval1", 50_000).unwrap();

        let after = &reg.validators["0xval1"];
        assert_eq!(after.self_stake, initial + 50_000);
        // total_delegated must NOT move — that's the whole point vs
        // ordinary Delegate from the validator's own wallet.
        assert_eq!(after.total_delegated, 0);
    }

    #[test]
    fn test_add_self_stake_can_lift_below_min_to_above_min() {
        // The 2026-04-27 self-stake-shortfall scenario: slashing
        // dropped self_stake below MIN_SELF_STAKE; AddSelfStake is
        // the supply-invariant-preserving recovery path that gets
        // it back over the floor so plain `unjail` becomes viable.
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.slash("0xval1", 100).unwrap(); // 1% slash drops below min
        assert!(reg.validators["0xval1"].self_stake < MIN_SELF_STAKE);
        let shortfall = MIN_SELF_STAKE - reg.validators["0xval1"].self_stake;

        reg.add_self_stake("0xval1", shortfall).unwrap();

        assert_eq!(reg.validators["0xval1"].self_stake, MIN_SELF_STAKE);
        // Now the standard unjail path works (after jail cooldown).
    }

    #[test]
    fn test_add_self_stake_rejects_zero_amount() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        let result = reg.add_self_stake("0xval1", 0);
        assert!(matches!(
            result,
            Err(SentrixError::InvalidTransaction(ref m)) if m.contains("must be > 0")
        ));
    }

    #[test]
    fn test_add_self_stake_rejects_unknown_validator() {
        let mut reg = new_registry();
        let result = reg.add_self_stake("0xnobody", 1_000);
        assert!(matches!(
            result,
            Err(SentrixError::InvalidTransaction(ref m)) if m.contains("validator not found")
        ));
    }

    #[test]
    fn test_add_self_stake_rejects_tombstoned() {
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.validators.get_mut("0xval1").unwrap().is_tombstoned = true;
        let result = reg.add_self_stake("0xval1", 50_000);
        assert!(matches!(
            result,
            Err(SentrixError::InvalidTransaction(ref m)) if m.contains("tombstoned")
        ));
    }

    #[test]
    fn test_add_self_stake_does_not_overflow() {
        // Defensive: u64 self_stake + amount must not silently wrap.
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        // Force self_stake near u64 max so the next add overflows.
        reg.validators.get_mut("0xval1").unwrap().self_stake = u64::MAX - 100;
        let result = reg.add_self_stake("0xval1", 1_000);
        assert!(matches!(
            result,
            Err(SentrixError::InvalidTransaction(ref m)) if m.contains("overflow")
        ));
        // State unchanged on rejection.
        assert_eq!(
            reg.validators["0xval1"].self_stake,
            u64::MAX - 100,
            "overflow path must not partially mutate"
        );
    }

    #[test]
    fn test_force_unjail_rejects_tombstoned() {
        // Tombstone is permanent by design — force_unjail must still
        // refuse, same as `unjail`.
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.validators.get_mut("0xval1").unwrap().is_tombstoned = true;

        assert!(reg.force_unjail("0xval1").is_err());
    }

    #[test]
    fn test_force_unjail_skips_cooldown() {
        // Unlike `unjail`, `force_unjail` does not honor the
        // jail_until cooldown — operator override is the whole point.
        let mut reg = new_registry();
        register_val(&mut reg, "0xval1", MIN_SELF_STAKE);
        reg.jail("0xval1", 100, 50_000).unwrap();

        reg.force_unjail("0xval1").unwrap();
        let v = &reg.validators["0xval1"];
        assert!(!v.is_jailed);
        assert_eq!(v.jail_until, 0);
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

    /// Invariant: for every validator V, the sum of live delegation-entry
    /// amounts to V across all delegators equals `validators[V].total_delegated`.
    ///
    /// `delegate` / `undelegate` / `redelegate` / `slash` each maintain this by
    /// construction (checked_add on both sides in delegate, saturating_sub on
    /// both sides in undelegate, atomic pair in redelegate, proportional in
    /// slash). This test hammers a random mix of those ops and asserts the
    /// invariant after every step, so a future refactor that breaks the
    /// coupling is caught at test time instead of at a mainnet fork.
    fn assert_delegation_sum_invariant(reg: &StakeRegistry) {
        use std::collections::HashMap;
        let mut sum_per_val: HashMap<&str, u128> = HashMap::new();
        for entries in reg.delegations.values() {
            for e in entries {
                *sum_per_val.entry(e.validator.as_str()).or_insert(0) += e.amount as u128;
            }
        }
        for (addr, val) in &reg.validators {
            let expected = val.total_delegated as u128;
            let actual = sum_per_val.get(addr.as_str()).copied().unwrap_or(0);
            assert_eq!(
                expected, actual,
                "delegation sum invariant broken for validator {addr}: \
                 total_delegated = {expected}, actual Σ entries = {actual}",
            );
        }
    }

    #[test]
    fn test_delegation_sum_invariant_under_random_ops() {
        // Seed is fixed so the test is reproducible. If this fails in CI the
        // seed + op-trace can be replayed locally.
        //
        // Using a tiny self-rolled LCG instead of pulling in `rand` as a
        // dev-dep. Quality of randomness doesn't matter here — we just need
        // cheap reproducible "whichever op next".
        struct Lcg(u64);
        impl Lcg {
            fn next(&mut self) -> u64 {
                self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                self.0 >> 33
            }
            fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
                &xs[self.next() as usize % xs.len()]
            }
        }

        let mut rng = Lcg(0xdeadbeef_cafef00d);
        let mut reg = new_registry();

        // Seed 4 validators + 6 candidate delegators
        let vals = ["0xv1", "0xv2", "0xv3", "0xv4"];
        let dels = ["0xd1", "0xd2", "0xd3", "0xd4", "0xd5", "0xd6"];
        for v in &vals {
            register_val(&mut reg, v, MIN_SELF_STAKE);
        }
        assert_delegation_sum_invariant(&reg);

        for height in 100u64..600u64 {
            let op = rng.next() % 4;
            let del = *rng.pick(&dels);
            let val = *rng.pick(&vals);
            // Bounded amounts so we don't overflow u64::MAX in sums but big
            // enough to hit the non-trivial bookkeeping paths.
            let amount: u64 = (rng.next() % 10_000 + 1) * 1_000;

            let _ = match op {
                0 => reg.delegate(del, val, amount, height),
                1 => reg.undelegate(del, val, amount, height),
                2 => {
                    let val_to = *rng.pick(&vals);
                    reg.redelegate(del, val, val_to, amount, height)
                }
                _ => {
                    let bp: u16 = (rng.next() % 1001) as u16; // 0..=1000 bp
                    reg.slash(val, bp).map(|_| ())
                }
            };

            // Invariant must hold whether the op succeeded (state changed
            // consistently) or failed (state unchanged).
            assert_delegation_sum_invariant(&reg);
        }
    }
}
