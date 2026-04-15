// evm/database.rs — SentrixEvmDb: revm::Database adapter for SentrixTrie
//
// Maps Sentrix account state to revm's Database trait, allowing the EVM
// to read balances, nonces, contract code, and storage from our trie.

use alloy_primitives::{Address, B256, U256};
use revm::database_interface::{Database, DBErrorMarker};
use revm::state::{AccountInfo, Bytecode};
use std::collections::HashMap;

/// Minimal EVM database error type that implements DBErrorMarker.
/// Wraps string error messages for simplicity.
#[derive(Debug, Clone)]
pub struct EvmDbError(pub String);

impl std::fmt::Display for EvmDbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EvmDbError: {}", self.0)
    }
}

impl std::error::Error for EvmDbError {}
impl DBErrorMarker for EvmDbError {}

/// EVM database backed by Sentrix's account state and sled storage.
///
/// For each EVM execution context, a fresh SentrixEvmDb is created from
/// the current chain state. After execution, the resulting state changes
/// are applied back to the trie.
pub struct SentrixEvmDb {
    /// Account balances and nonces from AccountDB
    accounts: HashMap<Address, AccountInfo>,
    /// Contract bytecode: code_hash → bytecode
    code: HashMap<B256, Bytecode>,
    /// Contract storage: (address, slot) → value
    storage: HashMap<(Address, U256), U256>,
    /// Block hashes: number → hash (recent window)
    block_hashes: HashMap<u64, B256>,
}

impl Default for SentrixEvmDb {
    fn default() -> Self {
        Self::new()
    }
}

impl SentrixEvmDb {
    pub fn new() -> Self {
        Self {
            accounts: HashMap::new(),
            code: HashMap::new(),
            storage: HashMap::new(),
            block_hashes: HashMap::new(),
        }
    }

    /// Load an account from Sentrix's AccountDB into the EVM db.
    pub fn insert_account(&mut self, address: Address, info: AccountInfo) {
        self.accounts.insert(address, info);
    }

    /// Store contract bytecode.
    pub fn insert_code(&mut self, hash: B256, code: Bytecode) {
        self.code.insert(hash, code);
    }

    /// Store a storage slot value.
    pub fn insert_storage(&mut self, address: Address, slot: U256, value: U256) {
        self.storage.insert((address, slot), value);
    }

    /// Store a block hash for the BLOCKHASH opcode.
    pub fn insert_block_hash(&mut self, number: u64, hash: B256) {
        self.block_hashes.insert(number, hash);
    }
}

impl Database for SentrixEvmDb {
    type Error = EvmDbError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        Ok(self.accounts.get(&address).cloned())
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.code
            .get(&code_hash)
            .cloned()
            .ok_or_else(|| EvmDbError(format!("code hash {} not found", code_hash)))
    }

    fn storage(&mut self, address: Address, index: U256) -> Result<U256, Self::Error> {
        Ok(self.storage.get(&(address, index)).copied().unwrap_or(U256::ZERO))
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        Ok(self.block_hashes.get(&number).copied().unwrap_or(B256::ZERO))
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_db_returns_none() {
        let mut db = SentrixEvmDb::new();
        let addr = Address::ZERO;
        let result = db.basic(addr);
        assert!(result.is_ok());
        assert!(result.ok().flatten().is_none());
    }

    #[test]
    fn test_insert_and_read_account() {
        let mut db = SentrixEvmDb::new();
        let addr = Address::from([0x42u8; 20]);
        let info = AccountInfo {
            balance: U256::from(1_000_000u64),
            nonce: 5,
            code_hash: B256::ZERO,
            account_id: None,
            code: None,
        };
        db.insert_account(addr, info.clone());

        let result = db.basic(addr).ok().flatten();
        assert!(result.is_some());
        let loaded = result.unwrap();
        assert_eq!(loaded.balance, U256::from(1_000_000u64));
        assert_eq!(loaded.nonce, 5);
    }

    #[test]
    fn test_storage_default_zero() {
        let mut db = SentrixEvmDb::new();
        let addr = Address::from([0x01u8; 20]);
        let slot = U256::from(42u64);
        let val = db.storage(addr, slot);
        assert!(val.is_ok());
        assert_eq!(val.ok(), Some(U256::ZERO));
    }

    #[test]
    fn test_insert_and_read_storage() {
        let mut db = SentrixEvmDb::new();
        let addr = Address::from([0x01u8; 20]);
        let slot = U256::from(1u64);
        let value = U256::from(999u64);
        db.insert_storage(addr, slot, value);

        let result = db.storage(addr, slot);
        assert_eq!(result.ok(), Some(U256::from(999u64)));
    }

    #[test]
    fn test_block_hash_default_zero() {
        let mut db = SentrixEvmDb::new();
        let hash = db.block_hash(100);
        assert_eq!(hash.ok(), Some(B256::ZERO));
    }

    #[test]
    fn test_insert_and_read_block_hash() {
        let mut db = SentrixEvmDb::new();
        let hash = B256::from([0xABu8; 32]);
        db.insert_block_hash(42, hash);
        let result = db.block_hash(42);
        assert_eq!(result.ok(), Some(hash));
    }

    #[test]
    fn test_code_by_hash_missing() {
        let mut db = SentrixEvmDb::new();
        let hash = B256::from([0xFFu8; 32]);
        let result = db.code_by_hash(hash);
        assert!(result.is_err());
    }

    #[test]
    fn test_insert_and_read_code() {
        let mut db = SentrixEvmDb::new();
        let hash = B256::from([0xCCu8; 32]);
        let code = Bytecode::new_raw(alloy_primitives::Bytes::from(vec![0x60, 0x00, 0x60, 0x00]));
        db.insert_code(hash, code);

        let result = db.code_by_hash(hash);
        assert!(result.is_ok());
    }
}
