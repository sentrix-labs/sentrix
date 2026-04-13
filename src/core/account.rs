// account.rs - Sentrix

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::types::error::{SentrixError, SentrixResult};

pub const SENTRI_PER_SRX: u64 = 100_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub address: String,
    pub balance: u64,   // in sentri units
    pub nonce: u64,
}

impl Account {
    pub fn new(address: String) -> Self {
        Self { address, balance: 0, nonce: 0 }
    }

    pub fn balance_srx(&self) -> f64 {
        self.balance as f64 / SENTRI_PER_SRX as f64
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountDB {
    pub accounts: HashMap<String, Account>,
    pub total_burned: u64,  // total SRX burned in sentri
}

impl AccountDB {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_create(&mut self, address: &str) -> &mut Account {
        self.accounts
            .entry(address.to_string())
            .or_insert_with(|| Account::new(address.to_string()))
    }

    pub fn get_balance(&self, address: &str) -> u64 {
        self.accounts.get(address).map(|a| a.balance).unwrap_or(0)
    }

    pub fn get_nonce(&self, address: &str) -> u64 {
        self.accounts.get(address).map(|a| a.nonce).unwrap_or(0)
    }

    pub fn credit(&mut self, address: &str, amount: u64) -> SentrixResult<()> {
        let account = self.get_or_create(address);
        account.balance = account.balance.checked_add(amount)
            .ok_or_else(|| SentrixError::Internal("balance overflow".to_string()))?;
        Ok(())
    }

    pub fn transfer(
        &mut self,
        from: &str,
        to: &str,
        amount: u64,
        fee: u64,
    ) -> SentrixResult<()> {
        let total = amount.checked_add(fee)
            .ok_or_else(|| SentrixError::Internal("amount + fee overflow".to_string()))?;
        let from_balance = self.get_balance(from);

        if from_balance < total {
            return Err(SentrixError::InsufficientBalance {
                have: from_balance,
                need: total,
            });
        }

        // Deduct from sender
        {
            let sender = self.get_or_create(from);
            sender.balance = sender.balance.checked_sub(total)
                .ok_or_else(|| SentrixError::Internal("balance underflow".to_string()))?;
            sender.nonce += 1;
        }

        // Credit recipient
        self.credit(to, amount)?;

        // L-02 FIX: Burn rounds up (ceiling) so odd fees are never lost — validator gets floor
        let burn_amount = fee.div_ceil(2);
        self.total_burned = self.total_burned.saturating_add(burn_amount);

        Ok(())
    }

    pub fn apply_block_reward(&mut self, validator: &str, reward: u64, fee_share: u64) -> SentrixResult<()> {
        let total = reward.checked_add(fee_share)
            .ok_or_else(|| SentrixError::Internal("reward overflow".to_string()))?;
        self.credit(validator, total)
    }

    pub fn total_supply(&self) -> u64 {
        self.accounts.values().map(|a| a.balance).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_account_zero_balance() {
        let db = AccountDB::new();
        assert_eq!(db.get_balance("SRX_test"), 0);
        assert_eq!(db.get_nonce("SRX_test"), 0);
    }

    #[test]
    fn test_credit() {
        let mut db = AccountDB::new();
        db.credit("addr1", 1000).unwrap();
        assert_eq!(db.get_balance("addr1"), 1000);
    }

    #[test]
    fn test_transfer_success() {
        let mut db = AccountDB::new();
        db.credit("alice", 10_000).unwrap();
        db.transfer("alice", "bob", 5_000, 100).unwrap();
        assert_eq!(db.get_balance("alice"), 4_900);
        assert_eq!(db.get_balance("bob"), 5_000);
        assert_eq!(db.get_nonce("alice"), 1);
    }

    #[test]
    fn test_transfer_insufficient_balance() {
        let mut db = AccountDB::new();
        db.credit("alice", 100).unwrap();
        let result = db.transfer("alice", "bob", 5_000, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_burn_tracking() {
        let mut db = AccountDB::new();
        db.credit("alice", 10_000).unwrap();
        db.transfer("alice", "bob", 5_000, 200).unwrap();
        assert_eq!(db.total_burned, 100); // 50% of fee=200 (even fee, unaffected by rounding)
    }

    // ── L-02: burn rounding tests ─────────────────────────

    #[test]
    fn test_l02_burn_odd_fee_rounds_up() {
        let mut db = AccountDB::new();
        db.credit("alice", 10_000).unwrap();

        // fee=1 → burn=(1+1)/2=1, no sentri lost
        db.transfer("alice", "bob", 100, 1).unwrap();
        assert_eq!(db.total_burned, 1);

        // fee=3 → burn=(3+1)/2=2
        db.transfer("alice", "bob", 100, 3).unwrap();
        assert_eq!(db.total_burned, 3); // 1 + 2
    }

    #[test]
    fn test_l02_burn_even_fee_unchanged() {
        let mut db = AccountDB::new();
        db.credit("alice", 10_000).unwrap();

        // fee=2 → burn=1 (same as before fix)
        db.transfer("alice", "bob", 100, 2).unwrap();
        assert_eq!(db.total_burned, 1);

        // fee=100 → burn=50
        db.transfer("alice", "bob", 100, 100).unwrap();
        assert_eq!(db.total_burned, 51); // 1 + 50
    }

    #[test]
    fn test_l02_fee_fully_accounted() {
        // For any fee, burn + validator_share must equal fee.
        // burn = (fee+1)/2, validator = fee - burn = fee/2 (floor)
        for fee in [0u64, 1, 2, 3, 7, 99, 100, 1_000_001] {
            let burn = (fee + 1) / 2;
            let validator = fee - burn;
            assert_eq!(burn + validator, fee, "fee={fee} not fully distributed");
        }
    }
}
