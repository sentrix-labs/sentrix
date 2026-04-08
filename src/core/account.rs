// account.rs - Sentrix Chain

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

    pub fn credit(&mut self, address: &str, amount: u64) {
        let account = self.get_or_create(address);
        account.balance += amount;
    }

    pub fn transfer(
        &mut self,
        from: &str,
        to: &str,
        amount: u64,
        fee: u64,
    ) -> SentrixResult<()> {
        let total = amount + fee;
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
            sender.balance -= total;
            sender.nonce += 1;
        }

        // Credit recipient
        self.credit(to, amount);

        // Burn 50% of fee, credit 50% to validator (handled by caller)
        let burn_amount = fee / 2;
        self.total_burned += burn_amount;

        Ok(())
    }

    pub fn apply_block_reward(&mut self, validator: &str, reward: u64, fee_share: u64) {
        self.credit(validator, reward + fee_share);
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
        db.credit("addr1", 1000);
        assert_eq!(db.get_balance("addr1"), 1000);
    }

    #[test]
    fn test_transfer_success() {
        let mut db = AccountDB::new();
        db.credit("alice", 10_000);
        db.transfer("alice", "bob", 5_000, 100).unwrap();
        assert_eq!(db.get_balance("alice"), 4_900);
        assert_eq!(db.get_balance("bob"), 5_000);
        assert_eq!(db.get_nonce("alice"), 1);
    }

    #[test]
    fn test_transfer_insufficient_balance() {
        let mut db = AccountDB::new();
        db.credit("alice", 100);
        let result = db.transfer("alice", "bob", 5_000, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_burn_tracking() {
        let mut db = AccountDB::new();
        db.credit("alice", 10_000);
        db.transfer("alice", "bob", 5_000, 200).unwrap();
        assert_eq!(db.total_burned, 100); // 50% of fee=200
    }
}
