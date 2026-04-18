// state_export.rs — State snapshot export/import for backup, bootstrap, and migration.

use crate::blockchain::Blockchain;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use serde::{Deserialize, Serialize};

/// Complete snapshot of chain state at a specific height. Includes account
/// balances, contract code, contract storage, validator set, and staking
/// state. Designed for:
/// - Backup before risky upgrades
/// - Bootstrap a new node without full history sync
/// - Fork the chain for testing
/// - Archive historical state
/// - Migrate between versions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Snapshot format version (bump on breaking changes).
    pub version: u32,
    /// Metadata at the point of export.
    pub metadata: SnapshotMetadata,
    /// All accounts with non-zero balance or non-zero nonce.
    pub accounts: Vec<AccountEntry>,
    /// Deployed contract bytecode (code_hash → hex-encoded bytes).
    pub contract_code: Vec<ContractCodeEntry>,
    /// Per-contract storage slots.
    pub contract_storage: Vec<StorageEntry>,
    /// Active + inactive validators.
    pub validators: Vec<ValidatorEntry>,
    /// Admin address (authority manager).
    pub admin_address: String,
    /// Chain-wide counters.
    pub total_minted: u64,
    pub total_burned: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    pub chain_id: u64,
    pub height: u64,
    pub block_hash: String,
    pub timestamp: u64,
    pub exported_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountEntry {
    pub address: String,
    pub balance: u64,
    pub nonce: u64,
    pub code_hash: String,
    pub storage_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractCodeEntry {
    pub code_hash: String,
    pub bytecode_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageEntry {
    pub key: String,
    pub value_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorEntry {
    pub address: String,
    pub name: String,
    pub public_key: String,
    pub is_active: bool,
    pub blocks_produced: u64,
}

const SNAPSHOT_VERSION: u32 = 1;

impl Blockchain {
    /// Export the current chain state as a `StateSnapshot`.
    /// Must be called while the node is STOPPED (no concurrent writes to sled).
    pub fn export_state(&self) -> SentrixResult<StateSnapshot> {
        let height = self.height();
        let latest = self.latest_block()?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let metadata = SnapshotMetadata {
            chain_id: self.chain_id,
            height,
            block_hash: latest.hash.clone(),
            timestamp: latest.timestamp,
            exported_at: now,
        };

        // Accounts: export all with balance > 0 or nonce > 0 or is_contract.
        let mut accounts: Vec<AccountEntry> = self
            .accounts
            .accounts
            .values()
            .filter(|a| a.balance > 0 || a.nonce > 0 || a.is_contract())
            .map(|a| AccountEntry {
                address: a.address.clone(),
                balance: a.balance,
                nonce: a.nonce,
                code_hash: hex::encode(a.code_hash),
                storage_root: hex::encode(a.storage_root),
            })
            .collect();
        accounts.sort_by(|a, b| a.address.cmp(&b.address));

        // Contract code
        let mut contract_code: Vec<ContractCodeEntry> = self
            .accounts
            .contract_code
            .iter()
            .map(|(hash, bytes)| ContractCodeEntry {
                code_hash: hash.clone(),
                bytecode_hex: hex::encode(bytes),
            })
            .collect();
        contract_code.sort_by(|a, b| a.code_hash.cmp(&b.code_hash));

        // Contract storage
        let mut contract_storage: Vec<StorageEntry> = self
            .accounts
            .contract_storage
            .iter()
            .map(|(key, value)| StorageEntry {
                key: key.clone(),
                value_hex: hex::encode(value),
            })
            .collect();
        contract_storage.sort_by(|a, b| a.key.cmp(&b.key));

        // Validators
        let mut validators: Vec<ValidatorEntry> = self
            .authority
            .validators
            .values()
            .map(|v| ValidatorEntry {
                address: v.address.clone(),
                name: v.name.clone(),
                public_key: v.public_key.clone(),
                is_active: v.is_active,
                blocks_produced: v.blocks_produced,
            })
            .collect();
        validators.sort_by(|a, b| a.address.cmp(&b.address));

        Ok(StateSnapshot {
            version: SNAPSHOT_VERSION,
            metadata,
            accounts,
            contract_code,
            contract_storage,
            validators,
            admin_address: self.authority.admin_address.clone(),
            total_minted: self.total_minted,
            total_burned: self.accounts.total_burned,
        })
    }

    /// Import state from a snapshot, REPLACING the current accounts,
    /// contracts, storage, and validator set. Chain history (blocks) is
    /// NOT imported — only the latest state. After import the node should
    /// be restarted to rebuild its in-memory trie.
    ///
    /// Returns the number of accounts imported.
    pub fn import_state(&mut self, snapshot: &StateSnapshot) -> SentrixResult<usize> {
        if snapshot.version != SNAPSHOT_VERSION {
            return Err(SentrixError::Internal(format!(
                "unsupported snapshot version: {} (expected {})",
                snapshot.version, SNAPSHOT_VERSION
            )));
        }

        // Clear current state
        self.accounts.accounts.clear();
        self.accounts.contract_code.clear();
        self.accounts.contract_storage.clear();
        self.accounts.total_burned = snapshot.total_burned;
        self.total_minted = snapshot.total_minted;

        // Import accounts
        for entry in &snapshot.accounts {
            let account = self.accounts.get_or_create(&entry.address);
            account.balance = entry.balance;
            account.nonce = entry.nonce;
            if let Ok(bytes) = hex::decode(&entry.code_hash)
                && bytes.len() == 32
            {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                account.code_hash = arr;
            }
            if let Ok(bytes) = hex::decode(&entry.storage_root)
                && bytes.len() == 32
            {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                account.storage_root = arr;
            }
        }

        // Import contract code
        for entry in &snapshot.contract_code {
            if let Ok(bytes) = hex::decode(&entry.bytecode_hex) {
                self.accounts.store_contract_code(&entry.code_hash, bytes);
            }
        }

        // Import contract storage
        for entry in &snapshot.contract_storage {
            if let Ok(bytes) = hex::decode(&entry.value_hex) {
                // Key format: "address:slot_hex"
                let parts: Vec<&str> = entry.key.splitn(2, ':').collect();
                if parts.len() == 2 {
                    self.accounts
                        .store_contract_storage(parts[0], parts[1], bytes);
                }
            }
        }

        // Import validators
        self.authority.validators.clear();
        self.authority.admin_address = snapshot.admin_address.clone();
        for v in &snapshot.validators {
            use crate::authority::Validator;
            let mut val = Validator::new(v.address.clone(), v.name.clone(), v.public_key.clone());
            val.is_active = v.is_active;
            val.blocks_produced = v.blocks_produced;
            self.authority.validators.insert(v.address.clone(), val);
        }

        Ok(snapshot.accounts.len())
    }

    /// Verify a snapshot's internal consistency.
    /// Returns Ok(summary_string) or Err if corrupt.
    pub fn verify_snapshot(snapshot: &StateSnapshot) -> SentrixResult<String> {
        if snapshot.version != SNAPSHOT_VERSION {
            return Err(SentrixError::Internal(format!(
                "unsupported snapshot version: {}",
                snapshot.version
            )));
        }

        // Check account addresses are valid format
        for a in &snapshot.accounts {
            if !crate::blockchain::is_valid_sentrix_address(&a.address) {
                return Err(SentrixError::Internal(format!(
                    "invalid account address: {}",
                    a.address
                )));
            }
        }

        // Check validators
        for v in &snapshot.validators {
            if !crate::blockchain::is_valid_sentrix_address(&v.address) {
                return Err(SentrixError::Internal(format!(
                    "invalid validator address: {}",
                    v.address
                )));
            }
        }

        // Check no duplicate addresses
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for a in &snapshot.accounts {
            if !seen.insert(&a.address) {
                return Err(SentrixError::Internal(format!(
                    "duplicate account address: {}",
                    a.address
                )));
            }
        }

        let total_balance: u64 = snapshot.accounts.iter().map(|a| a.balance).sum();

        Ok(format!(
            "Snapshot v{} OK: chain_id={} height={} accounts={} validators={} total_balance={:.4} SRX contracts={} storage_entries={}",
            snapshot.version,
            snapshot.metadata.chain_id,
            snapshot.metadata.height,
            snapshot.accounts.len(),
            snapshot.validators.len(),
            total_balance as f64 / 100_000_000.0,
            snapshot.contract_code.len(),
            snapshot.contract_storage.len(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockchain::Blockchain;

    fn setup() -> Blockchain {
        // Use valid Sentrix-format addresses so verify_snapshot passes.
        let admin = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let val = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mut bc = Blockchain::new(admin.to_string());
        bc.authority.add_validator_unchecked(
            val.to_string(),
            "Validator 1".to_string(),
            "pk1".to_string(),
        );
        bc
    }

    #[test]
    fn test_export_import_roundtrip() {
        let mut bc = setup();
        bc.accounts
            .credit("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef", 1_000_000)
            .unwrap();

        let snapshot = bc.export_state().unwrap();
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        assert_eq!(snapshot.metadata.chain_id, bc.chain_id);
        assert!(snapshot.accounts.len() >= 5); // 4 genesis + 1 test

        // Import into a fresh blockchain
        let mut bc2 = Blockchain::new("admin2".to_string());
        let count = bc2.import_state(&snapshot).unwrap();
        assert_eq!(count, snapshot.accounts.len());

        // Verify same balances
        assert_eq!(
            bc2.accounts
                .get_balance("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
            1_000_000
        );
        assert_eq!(bc2.total_minted, bc.total_minted);
        assert_eq!(bc2.accounts.total_burned, bc.accounts.total_burned);
    }

    #[test]
    fn test_verify_valid_snapshot() {
        let bc = setup();
        let snapshot = bc.export_state().unwrap();
        let result = Blockchain::verify_snapshot(&snapshot);
        assert!(result.is_ok());
        let summary = result.unwrap();
        assert!(summary.contains("OK"));
    }

    #[test]
    fn test_verify_bad_version() {
        let bc = setup();
        let mut snapshot = bc.export_state().unwrap();
        snapshot.version = 999;
        let result = Blockchain::verify_snapshot(&snapshot);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_duplicate_address() {
        let bc = setup();
        let mut snapshot = bc.export_state().unwrap();
        if let Some(first) = snapshot.accounts.first().cloned() {
            snapshot.accounts.push(first);
        }
        let result = Blockchain::verify_snapshot(&snapshot);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("duplicate"));
    }
}
