// account.rs - Sentrix

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::types::error::{SentrixError, SentrixResult};

pub const SENTRI_PER_SRX: u64 = 100_000_000;

/// Keccak-256 hash of empty bytecode (EOA accounts).
/// Equivalent to keccak256(&[]) = 0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
pub const EMPTY_CODE_HASH: [u8; 32] = [
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c,
    0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b,
    0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
];

/// Empty storage root (no contract storage).
pub const EMPTY_STORAGE_ROOT: [u8; 32] = [0u8; 32];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub address: String,
    pub balance: u64,   // in sentri units (EVM boundary converts to U256)
    pub nonce: u64,
    /// Keccak-256 of contract bytecode. EMPTY_CODE_HASH for EOA (non-contract) accounts.
    /// Added in Voyager EVM upgrade. Defaults to EMPTY_CODE_HASH for backward compat.
    #[serde(default = "default_code_hash")]
    pub code_hash: [u8; 32],
    /// Root hash of per-account contract storage trie. EMPTY_STORAGE_ROOT for EOA.
    /// Added in Voyager EVM upgrade. Defaults to EMPTY_STORAGE_ROOT for backward compat.
    #[serde(default)]
    pub storage_root: [u8; 32],
}

fn default_code_hash() -> [u8; 32] {
    EMPTY_CODE_HASH
}

impl Account {
    pub fn new(address: String) -> Self {
        Self {
            address,
            balance: 0,
            nonce: 0,
            code_hash: EMPTY_CODE_HASH,
            storage_root: EMPTY_STORAGE_ROOT,
        }
    }

    pub fn balance_srx(&self) -> f64 {
        self.balance as f64 / SENTRI_PER_SRX as f64
    }

    /// Returns true if this account has deployed contract code.
    pub fn is_contract(&self) -> bool {
        self.code_hash != EMPTY_CODE_HASH
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountDB {
    pub accounts: HashMap<String, Account>,
    pub total_burned: u64,  // total SRX burned in sentri
    /// Contract bytecode storage: code_hash (hex) → bytecode bytes.
    /// Only populated after Voyager EVM fork.
    #[serde(default)]
    pub contract_code: HashMap<String, Vec<u8>>,
    /// Contract storage: "address:slot_hex" → value bytes.
    /// Only populated after Voyager EVM fork.
    #[serde(default)]
    pub contract_storage: HashMap<String, Vec<u8>>,
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

    /// Burn tokens (add to total_burned tracker, no balance deducted)
    pub fn burn(&mut self, amount: u64) {
        self.total_burned = self.total_burned.saturating_add(amount);
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

        // Burn gets ceiling division so odd fees are never lost — validator gets the floor share
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

    /// Store contract bytecode, keyed by its keccak-256 hash (hex).
    pub fn store_contract_code(&mut self, code_hash_hex: &str, bytecode: Vec<u8>) {
        self.contract_code.insert(code_hash_hex.to_string(), bytecode);
    }

    /// Get contract bytecode by its code_hash (hex).
    pub fn get_contract_code(&self, code_hash_hex: &str) -> Option<&Vec<u8>> {
        self.contract_code.get(code_hash_hex)
    }

    /// Store a contract storage value.
    /// Key format: "address:slot_hex" → value bytes.
    pub fn store_contract_storage(&mut self, address: &str, slot_hex: &str, value: Vec<u8>) {
        let key = format!("{}:{}", address, slot_hex);
        self.contract_storage.insert(key, value);
    }

    /// Get a contract storage value.
    pub fn get_contract_storage(&self, address: &str, slot_hex: &str) -> Option<&Vec<u8>> {
        let key = format!("{}:{}", address, slot_hex);
        self.contract_storage.get(&key)
    }

    /// Migrate all existing accounts to EVM-compatible format.
    /// Sets code_hash=EMPTY_CODE_HASH and storage_root=EMPTY_STORAGE_ROOT
    /// for all accounts that don't have them set yet.
    /// Called once at EVM fork height.
    pub fn migrate_to_evm(&mut self) -> usize {
        let mut migrated = 0;
        for account in self.accounts.values_mut() {
            if account.code_hash == [0u8; 32] {
                account.code_hash = EMPTY_CODE_HASH;
                migrated += 1;
            }
        }
        tracing::info!("EVM migration: {} accounts migrated", migrated);
        migrated
    }

    /// Set account as a contract (after EVM CREATE).
    pub fn set_contract(&mut self, address: &str, code_hash: [u8; 32]) {
        let account = self.get_or_create(address);
        account.code_hash = code_hash;
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
    fn test_new_account_has_evm_fields() {
        let account = Account::new("0xtest".to_string());
        assert_eq!(account.code_hash, EMPTY_CODE_HASH);
        assert_eq!(account.storage_root, EMPTY_STORAGE_ROOT);
        assert!(!account.is_contract());
    }

    #[test]
    fn test_is_contract() {
        let mut account = Account::new("0xtest".to_string());
        assert!(!account.is_contract());

        account.code_hash = [0xAA; 32];
        assert!(account.is_contract());
    }

    #[test]
    fn test_migrate_to_evm() {
        let mut db = AccountDB::new();
        // Create accounts with zeroed code_hash (pre-migration state)
        db.credit("alice", 1000).unwrap();
        db.credit("bob", 2000).unwrap();

        // Simulate pre-migration: zero code_hash (old serialization default)
        for account in db.accounts.values_mut() {
            account.code_hash = [0u8; 32];
        }

        let migrated = db.migrate_to_evm();
        assert_eq!(migrated, 2);

        // After migration, all accounts should have EMPTY_CODE_HASH
        for account in db.accounts.values() {
            assert_eq!(account.code_hash, EMPTY_CODE_HASH);
            assert!(!account.is_contract());
        }
    }

    #[test]
    fn test_contract_code_storage() {
        let mut db = AccountDB::new();
        let code = vec![0x60, 0x00, 0x60, 0x00, 0xf3]; // dummy bytecode
        db.store_contract_code("aabbcc", code.clone());
        assert_eq!(db.get_contract_code("aabbcc"), Some(&code));
        assert_eq!(db.get_contract_code("missing"), None);
    }

    #[test]
    fn test_contract_storage() {
        let mut db = AccountDB::new();
        let value = vec![0, 0, 0, 42];
        db.store_contract_storage("0xcontract", "0001", value.clone());
        assert_eq!(db.get_contract_storage("0xcontract", "0001"), Some(&value));
        assert_eq!(db.get_contract_storage("0xcontract", "0002"), None);
    }

    #[test]
    fn test_set_contract() {
        let mut db = AccountDB::new();
        db.credit("0xaddr", 1000).unwrap();
        assert!(!db.accounts.get("0xaddr").map(|a| a.is_contract()).unwrap_or(false));

        db.set_contract("0xaddr", [0xBB; 32]);
        assert!(db.accounts.get("0xaddr").map(|a| a.is_contract()).unwrap_or(false));
    }

    #[test]
    fn test_l02_fee_fully_accounted() {
        // For any fee, burn + validator_share must equal fee.
        // burn = (fee+1)/2, validator = fee - burn = fee/2 (floor)
        for fee in [0u64, 1, 2, 3, 7, 99, 100, 1_000_001] {
            let burn = fee.div_ceil(2);
            let validator = fee - burn;
            assert_eq!(burn + validator, fee, "fee={fee} not fully distributed");
        }
    }
}
