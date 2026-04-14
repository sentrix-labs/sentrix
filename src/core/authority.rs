// authority.rs - Sentrix

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use secp256k1::PublicKey;
use crate::types::error::{SentrixError, SentrixResult};

// Minimum active validators for collusion resistance (PoA N/2+1 design)
pub const MIN_ACTIVE_VALIDATORS: usize = 3;
// Admin log size is bounded to prevent unbounded memory growth
pub const MAX_ADMIN_LOG_SIZE: usize = 10_000;

// Append-only admin audit trail — every privileged operation is logged immutably.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminEvent {
    pub operation: String,
    pub caller: String,
    pub target_address: String,
    pub target_name: String,
    pub timestamp: u64,
}

impl AdminEvent {
    fn now(operation: &str, caller: &str, target_address: &str, target_name: &str) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            operation: operation.to_string(),
            caller: caller.to_string(),
            target_address: target_address.to_string(),
            target_name: target_name.to_string(),
            timestamp,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Validator {
    pub address: String,
    pub name: String,
    pub public_key: String,
    pub registered_at: u64,
    pub blocks_produced: u64,
    pub is_active: bool,
    pub last_block_time: u64,
}

impl Validator {
    pub fn new(address: String, name: String, public_key: String) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            address,
            name,
            public_key,
            registered_at: now,
            blocks_produced: 0,
            is_active: true,
            last_block_time: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorityManager {
    pub validators: HashMap<String, Validator>,
    pub admin_address: String,
    // Append new log entry — admin log is write-only; existing entries are never modified
    #[serde(default)]
    pub admin_log: Vec<AdminEvent>,
}

impl AuthorityManager {
    pub fn new(admin_address: String) -> Self {
        Self {
            validators: HashMap::new(),
            admin_address,
            admin_log: Vec::new(),
        }
    }

    // Get active validators sorted by address (deterministic order)
    pub fn active_validators(&self) -> Vec<&Validator> {
        let mut active: Vec<&Validator> = self.validators
            .values()
            .filter(|v| v.is_active)
            .collect();
        active.sort_by(|a, b| a.address.cmp(&b.address));
        active
    }

    // Round-robin: which validator should produce block at height h?
    pub fn expected_validator(&self, block_height: u64) -> SentrixResult<&Validator> {
        let active = self.active_validators();
        if active.is_empty() {
            return Err(SentrixError::NoActiveValidators);
        }
        let idx = (block_height as usize) % active.len();
        Ok(active[idx])
    }

    // Is this address authorized to produce the block at this height?
    pub fn is_authorized(&self, address: &str, block_height: u64) -> SentrixResult<bool> {
        let expected = self.expected_validator(block_height)?;
        Ok(expected.address == address)
    }

    // Admin operations
    pub fn add_validator(
        &mut self,
        caller: &str,
        address: String,
        name: String,
        public_key: String,
    ) -> SentrixResult<()> {
        if caller != self.admin_address {
            return Err(SentrixError::UnauthorizedValidator(
                format!("{} is not admin", caller)
            ));
        }
        if self.validators.contains_key(&address) {
            return Err(SentrixError::InvalidBlock(
                format!("validator {} already exists", address)
            ));
        }

        // Validate address format before the expensive secp256k1 point check
        if !crate::core::blockchain::is_valid_sentrix_address(&address) {
            return Err(SentrixError::InvalidTransaction(
                format!("invalid validator address format: {}", address)
            ));
        }

        // Verify public_key is a valid secp256k1 point that derives to the given address
        let pk_bytes = hex::decode(&public_key)
            .map_err(|_| SentrixError::InvalidTransaction(
                "public_key: invalid hex encoding".to_string()
            ))?;
        let pk = PublicKey::from_slice(&pk_bytes)
            .map_err(|_| SentrixError::InvalidTransaction(
                "public_key: not a valid secp256k1 public key".to_string()
            ))?;
        let derived = crate::wallet::wallet::Wallet::derive_address(&pk);
        if derived != address {
            return Err(SentrixError::InvalidTransaction(
                format!(
                    "public_key does not correspond to address — derived {}, expected {}",
                    derived, address
                )
            ));
        }

        self.validators.insert(
            address.clone(),
            Validator::new(address.clone(), name.clone(), public_key),
        );
        // Log the privileged operation for the admin audit trail
        tracing::warn!("ADMIN_OP: {} called add_validator for {} ('{}') at {}",
            caller, address, name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_secs()
        );
        self.admin_log.push(AdminEvent::now("add_validator", caller, &address, &name));
        self.trim_admin_log();
        Ok(())
    }

    pub fn remove_validator(&mut self, caller: &str, address: &str) -> SentrixResult<()> {
        if caller != self.admin_address {
            return Err(SentrixError::UnauthorizedValidator(
                format!("{} is not admin", caller)
            ));
        }
        if address == self.admin_address {
            return Err(SentrixError::InvalidBlock(
                "admin cannot remove itself".to_string()
            ));
        }
        // Ensure at least MIN_ACTIVE_VALIDATORS remain after removal to maintain BFT quorum
        let active_after = self.active_validators().iter()
            .filter(|v| v.address != address)
            .count();
        if active_after < MIN_ACTIVE_VALIDATORS {
            return Err(SentrixError::InvalidBlock(
                format!("cannot remove: at least {} active validators required", MIN_ACTIVE_VALIDATORS)
            ));
        }
        let name = self.validators.get(address)
            .map(|v| v.name.clone())
            .unwrap_or_default();
        self.validators.remove(address)
            .ok_or_else(|| SentrixError::NotFound(format!("validator {}", address)))?;
        // Log the privileged operation for the admin audit trail
        tracing::warn!("ADMIN_OP: {} called remove_validator for {} ('{}') at {}",
            caller, address, name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_secs()
        );
        self.admin_log.push(AdminEvent::now("remove_validator", caller, address, &name));
        self.trim_admin_log();
        Ok(())
    }

    pub fn toggle_validator(&mut self, caller: &str, address: &str) -> SentrixResult<bool> {
        if caller != self.admin_address {
            return Err(SentrixError::UnauthorizedValidator(
                format!("{} is not admin", caller)
            ));
        }
        let validator = self.validators.get(address)
            .ok_or_else(|| SentrixError::NotFound(format!("validator {}", address)))?;

        // Prevent deactivating below MIN_ACTIVE_VALIDATORS to maintain BFT quorum
        if validator.is_active {
            let active_after = self.active_validators().iter()
                .filter(|v| v.address != address)
                .count();
            if active_after < MIN_ACTIVE_VALIDATORS {
                return Err(SentrixError::InvalidBlock(
                    format!("cannot deactivate: at least {} active validators required", MIN_ACTIVE_VALIDATORS)
                ));
            }
        }

        let validator = self.validators.get_mut(address)
            .ok_or_else(|| SentrixError::NotFound(format!("validator {}", address)))?;
        validator.is_active = !validator.is_active;
        let new_state = validator.is_active;
        let name = validator.name.clone();
        let op = if new_state { "activate_validator" } else { "deactivate_validator" };
        // Log the privileged operation for the admin audit trail
        tracing::warn!("ADMIN_OP: {} called {} for {} ('{}') at {}",
            caller, op, address, name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_secs()
        );
        self.admin_log.push(AdminEvent::now(op, caller, address, &name));
        self.trim_admin_log();
        Ok(new_state)
    }

    pub fn rename_validator(&mut self, caller: &str, address: &str, new_name: String) -> SentrixResult<()> {
        if caller != self.admin_address {
            return Err(SentrixError::UnauthorizedValidator(
                format!("{} is not admin", caller)
            ));
        }
        let validator = self.validators.get_mut(address)
            .ok_or_else(|| SentrixError::NotFound(format!("validator {}", address)))?;
        let old_name = validator.name.clone();
        validator.name = new_name.clone();
        // Log the privileged operation; target_name records the new name for auditability
        let log_name = format!("{} -> {}", old_name, new_name);
        tracing::warn!("ADMIN_OP: {} called rename_validator for {} ('{}') at {}",
            caller, address, log_name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default().as_secs()
        );
        self.admin_log.push(AdminEvent::now("rename_validator", caller, address, &log_name));
        self.trim_admin_log();
        Ok(())
    }

    // Minimum validators needed to collude and control the chain (N/2+1 of active set)
    pub fn collusion_risk(&self) -> usize {
        let n = self.active_count();
        if n == 0 { return 0; }
        n / 2 + 1
    }

    // Trim oldest entries when admin_log reaches its cap to stay bounded
    fn trim_admin_log(&mut self) {
        if self.admin_log.len() > MAX_ADMIN_LOG_SIZE {
            self.admin_log.drain(..self.admin_log.len() - MAX_ADMIN_LOG_SIZE);
        }
    }

    pub fn record_block_produced(&mut self, address: &str, timestamp: u64) {
        if let Some(v) = self.validators.get_mut(address) {
            v.blocks_produced += 1;
            v.last_block_time = timestamp;
        }
    }

    pub fn validator_count(&self) -> usize {
        self.validators.len()
    }

    pub fn active_count(&self) -> usize {
        self.active_validators().len()
    }

    /// Test-only helper: add a validator without public key crypto validation.
    /// Use this in unit tests where you want to control the address string directly.
    #[cfg(test)]
    pub fn add_validator_unchecked(&mut self, address: String, name: String, public_key: String) {
        self.validators.insert(address.clone(), Validator::new(address, name, public_key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate a (address, public_key_hex) pair for testing using a real secp256k1 wallet.
    fn gen_validator_keypair() -> (String, String) {
        let wallet = crate::wallet::wallet::Wallet::generate(); // returns Wallet, not Result
        let pk = wallet.get_public_key().unwrap();
        let pk_hex = hex::encode(pk.serialize_uncompressed());
        (wallet.address.clone(), pk_hex) // clone address since Wallet implements Drop (zeroize)
    }

    fn setup() -> AuthorityManager {
        let mut mgr = AuthorityManager::new("admin".to_string());
        let (addr1, pk1) = gen_validator_keypair();
        let (addr2, pk2) = gen_validator_keypair();
        let (addr3, pk3) = gen_validator_keypair();
        mgr.add_validator("admin", addr1, "Validator 1".to_string(), pk1)
            .expect("add_validator with valid keys should succeed");
        mgr.add_validator("admin", addr2, "Validator 2".to_string(), pk2)
            .expect("add_validator with valid keys should succeed");
        mgr.add_validator("admin", addr3, "Validator 3".to_string(), pk3)
            .expect("add_validator with valid keys should succeed");
        mgr
    }

    // 4-validator setup so tests can toggle one and still satisfy MIN_ACTIVE_VALIDATORS
    fn setup_4() -> AuthorityManager {
        let mut mgr = AuthorityManager::new("admin".to_string());
        let (addr1, pk1) = gen_validator_keypair();
        let (addr2, pk2) = gen_validator_keypair();
        let (addr3, pk3) = gen_validator_keypair();
        let (addr4, pk4) = gen_validator_keypair();
        mgr.add_validator("admin", addr1, "Validator 1".to_string(), pk1).unwrap();
        mgr.add_validator("admin", addr2, "Validator 2".to_string(), pk2).unwrap();
        mgr.add_validator("admin", addr3, "Validator 3".to_string(), pk3).unwrap();
        mgr.add_validator("admin", addr4, "Validator 4".to_string(), pk4).unwrap();
        mgr
    }

    #[test]
    fn test_add_validator() {
        let mgr = setup();
        assert_eq!(mgr.validator_count(), 3);
        assert_eq!(mgr.active_count(), 3);
    }

    #[test]
    fn test_round_robin_scheduling() {
        let mgr = setup();
        let active = mgr.active_validators();
        assert_eq!(active.len(), 3);

        // Round robin: 0,1,2,0,1,2,...
        let v0 = mgr.expected_validator(0).unwrap().address.clone();
        let v1 = mgr.expected_validator(1).unwrap().address.clone();
        let v2 = mgr.expected_validator(2).unwrap().address.clone();
        let v3 = mgr.expected_validator(3).unwrap().address.clone();

        assert_eq!(v0, v3); // wraps around
        assert_ne!(v0, v1);
        assert_ne!(v1, v2);
    }

    #[test]
    fn test_is_authorized() {
        let mgr = setup();
        let expected = mgr.expected_validator(0).unwrap().address.clone();
        assert!(mgr.is_authorized(&expected, 0).unwrap());
        assert!(!mgr.is_authorized("wrong_address", 0).unwrap());
    }

    #[test]
    fn test_non_admin_cannot_add() {
        let mut mgr = setup();
        let (addr, pk) = gen_validator_keypair();
        let result = mgr.add_validator("not_admin", addr, "Val 4".to_string(), pk);
        assert!(result.is_err());
    }

    #[test]
    fn test_toggle_validator() {
        let mut mgr = setup_4(); // 4 validators: toggling one leaves 3 ≥ MIN_ACTIVE_VALIDATORS
        let addr = mgr.active_validators()[0].address.clone();

        mgr.toggle_validator("admin", &addr).unwrap();
        assert_eq!(mgr.active_count(), 3);

        mgr.toggle_validator("admin", &addr).unwrap();
        assert_eq!(mgr.active_count(), 4);
    }

    #[test]
    fn test_no_active_validators_error() {
        let mgr = AuthorityManager::new("admin".to_string());
        let result = mgr.expected_validator(0);
        assert!(result.is_err());
    }

    #[test]
    fn test_admin_cannot_remove_itself() {
        let mut mgr = AuthorityManager::new("admin".to_string());
        let result = mgr.remove_validator("admin", "admin");
        assert!(result.is_err());
    }

    #[test]
    fn test_h03_toggle_cannot_deactivate_last_validator() {
        let mut mgr = AuthorityManager::new("admin".to_string());
        let (addr1, pk1) = gen_validator_keypair();
        mgr.add_validator("admin", addr1.clone(), "V1".to_string(), pk1).unwrap();
        assert_eq!(mgr.active_count(), 1);

        // Trying to deactivate below MIN_ACTIVE_VALIDATORS should fail
        let result = mgr.toggle_validator("admin", &addr1);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("at least 3"), "Expected min validator error, got: {}", err_str);

        // Validator should still be active
        assert_eq!(mgr.active_count(), 1);
    }

    #[test]
    fn test_rename_validator() {
        let mut mgr = setup();
        let addr = mgr.active_validators()[0].address.clone();
        let blocks_before = mgr.validators[&addr].blocks_produced;

        mgr.rename_validator("admin", &addr, "New Name".to_string()).unwrap();
        assert_eq!(mgr.validators[&addr].name, "New Name");
        assert_eq!(mgr.validators[&addr].blocks_produced, blocks_before); // counter preserved
    }

    #[test]
    fn test_rename_non_admin_fails() {
        let mut mgr = setup();
        let addr = mgr.active_validators()[0].address.clone();
        let result = mgr.rename_validator("not_admin", &addr, "X".to_string());
        assert!(result.is_err());
    }

    // ── I-03: Admin audit log tests ──────────────────────

    #[test]
    fn test_i03_add_validator_logs_event() {
        let mut mgr = AuthorityManager::new("admin".to_string());
        assert_eq!(mgr.admin_log.len(), 0);

        let (addr, pk) = gen_validator_keypair();
        mgr.add_validator("admin", addr.clone(), "Test Val".to_string(), pk).unwrap();

        assert_eq!(mgr.admin_log.len(), 1);
        let event = &mgr.admin_log[0];
        assert_eq!(event.operation, "add_validator");
        assert_eq!(event.caller, "admin");
        assert_eq!(event.target_address, addr);
        assert_eq!(event.target_name, "Test Val");
        assert!(event.timestamp > 0);
    }

    #[test]
    fn test_i03_remove_validator_logs_event() {
        let mut mgr = setup_4(); // need 4 so removing one leaves 3 ≥ MIN_ACTIVE_VALIDATORS
        let addr = mgr.active_validators()[0].address.clone();
        let name = mgr.validators[&addr].name.clone();
        let log_len_before = mgr.admin_log.len();

        // Need 2 validators to remove one
        mgr.remove_validator("admin", &addr).unwrap();

        assert_eq!(mgr.admin_log.len(), log_len_before + 1);
        let event = mgr.admin_log.last().unwrap();
        assert_eq!(event.operation, "remove_validator");
        assert_eq!(event.caller, "admin");
        assert_eq!(event.target_address, addr);
        assert_eq!(event.target_name, name);
    }

    #[test]
    fn test_i03_toggle_validator_logs_deactivate_and_activate() {
        let mut mgr = setup_4(); // need 4 so toggling one leaves 3 ≥ MIN_ACTIVE_VALIDATORS
        let addr = mgr.active_validators()[0].address.clone();
        let log_len_before = mgr.admin_log.len();

        // Toggle off
        mgr.toggle_validator("admin", &addr).unwrap();
        let deact_event = mgr.admin_log.last().unwrap();
        assert_eq!(deact_event.operation, "deactivate_validator");
        assert_eq!(deact_event.caller, "admin");
        assert_eq!(deact_event.target_address, addr);

        // Toggle on
        mgr.toggle_validator("admin", &addr).unwrap();
        let act_event = mgr.admin_log.last().unwrap();
        assert_eq!(act_event.operation, "activate_validator");
        assert_eq!(mgr.admin_log.len(), log_len_before + 2);
    }

    #[test]
    fn test_i03_rename_validator_logs_event() {
        let mut mgr = setup();
        let addr = mgr.active_validators()[0].address.clone();
        let old_name = mgr.validators[&addr].name.clone();
        let log_len_before = mgr.admin_log.len();

        mgr.rename_validator("admin", &addr, "Brand New Name".to_string()).unwrap();

        assert_eq!(mgr.admin_log.len(), log_len_before + 1);
        let event = mgr.admin_log.last().unwrap();
        assert_eq!(event.operation, "rename_validator");
        assert_eq!(event.caller, "admin");
        assert_eq!(event.target_address, addr);
        // target_name records old → new
        assert!(event.target_name.contains(&old_name));
        assert!(event.target_name.contains("Brand New Name"));
    }

    #[test]
    fn test_i03_failed_admin_ops_not_logged() {
        let mut mgr = setup();
        let log_len_before = mgr.admin_log.len();

        // Non-admin attempting to add a validator — must fail and not log
        let (addr, pk) = gen_validator_keypair();
        let result = mgr.add_validator("hacker", addr, "Evil".to_string(), pk);
        assert!(result.is_err());
        assert_eq!(mgr.admin_log.len(), log_len_before, "failed op must not log");
    }

    #[test]
    fn test_i03_admin_log_serde_roundtrip() {
        // admin_log must survive serialize/deserialize (used in blockchain state persistence)
        let mut mgr = AuthorityManager::new("admin".to_string());
        let (addr, pk) = gen_validator_keypair();
        mgr.add_validator("admin", addr, "Val".to_string(), pk).unwrap();

        let json = serde_json::to_string(&mgr).unwrap();
        let loaded: AuthorityManager = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.admin_log.len(), 1);
        assert_eq!(loaded.admin_log[0].operation, "add_validator");
    }

    // ── V5-01: Minimum active validator enforcement ────────

    #[test]
    fn test_v501_remove_enforces_min_active_validators() {
        let mut mgr = setup(); // exactly 3 validators = MIN_ACTIVE_VALIDATORS
        let addr = mgr.active_validators()[0].address.clone();
        // Removing one would leave 2 < MIN_ACTIVE_VALIDATORS
        let result = mgr.remove_validator("admin", &addr);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("at least 3"), "Expected min 3 error, got: {}", err);
    }

    #[test]
    fn test_v501_toggle_enforces_min_active_validators() {
        let mut mgr = setup(); // exactly 3 validators = MIN_ACTIVE_VALIDATORS
        let addr = mgr.active_validators()[0].address.clone();
        // Deactivating one would leave 2 < MIN_ACTIVE_VALIDATORS
        let result = mgr.toggle_validator("admin", &addr);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("at least 3"), "Expected min 3 error, got: {}", err);
    }

    #[test]
    fn test_v501_collusion_risk() {
        let mgr3 = setup(); // 3 validators
        assert_eq!(mgr3.collusion_risk(), 2); // floor(3/2)+1 = 2
        let mgr4 = setup_4(); // 4 validators
        assert_eq!(mgr4.collusion_risk(), 3); // floor(4/2)+1 = 3
        let empty = AuthorityManager::new("admin".to_string());
        assert_eq!(empty.collusion_risk(), 0);
    }

    // ── V5-05: Validator address format validation ─────────

    #[test]
    fn test_v505_add_validator_rejects_invalid_address_format() {
        let mut mgr = AuthorityManager::new("admin".to_string());
        let (_, pk) = gen_validator_keypair();
        // Use syntactically invalid address (no 0x prefix)
        let result = mgr.add_validator("admin", "not_a_valid_address".to_string(), "Bad Val".to_string(), pk);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("invalid") || err.contains("address"),
            "Expected address format error, got: {}", err
        );
    }

    #[test]
    fn test_v505_add_validator_accepts_valid_address_format() {
        let mut mgr = AuthorityManager::new("admin".to_string());
        let (addr, pk) = gen_validator_keypair();
        // Real derived address should pass both format check and H-02 crypto check
        assert!(mgr.add_validator("admin", addr, "Valid Val".to_string(), pk).is_ok());
    }

    // ── V5-11: Bounded admin_log ────────────────────────────

    #[test]
    fn test_v511_admin_log_bounded_at_max_size() {
        let mut mgr = AuthorityManager::new("admin".to_string());
        let (addr, pk) = gen_validator_keypair();
        mgr.add_validator("admin", addr.clone(), "Val".to_string(), pk).unwrap();

        // Rename MAX_ADMIN_LOG_SIZE + 10 times to overflow
        for i in 0..MAX_ADMIN_LOG_SIZE + 10 {
            mgr.rename_validator("admin", &addr, format!("Name {}", i)).unwrap();
        }

        assert!(
            mgr.admin_log.len() <= MAX_ADMIN_LOG_SIZE,
            "admin_log exceeded MAX: {} > {}", mgr.admin_log.len(), MAX_ADMIN_LOG_SIZE
        );
    }

    #[test]
    fn test_v501_remove_succeeds_with_4_validators() {
        let mut mgr = setup_4(); // 4 validators
        let addr = mgr.active_validators()[0].address.clone();
        // Removing one leaves 3 = MIN_ACTIVE_VALIDATORS — should succeed
        assert!(mgr.remove_validator("admin", &addr).is_ok());
        assert_eq!(mgr.active_count(), 3);
    }

    #[test]
    fn test_v505_add_validator_rejects_short_hex_address() {
        let mut mgr = AuthorityManager::new("admin".to_string());
        let (_, pk) = gen_validator_keypair();
        // Too short — only 20 chars after 0x instead of 40
        let result = mgr.add_validator("admin", "0xdeadbeef".to_string(), "Short".to_string(), pk);
        assert!(result.is_err());
    }

    #[test]
    fn test_v511_oldest_entries_trimmed_not_newest() {
        let mut mgr = AuthorityManager::new("admin".to_string());
        let (addr, pk) = gen_validator_keypair();
        mgr.add_validator("admin", addr.clone(), "Val".to_string(), pk).unwrap();

        // Fill to MAX_ADMIN_LOG_SIZE + 5 renames
        for i in 0..MAX_ADMIN_LOG_SIZE + 5 {
            mgr.rename_validator("admin", &addr, format!("Name {}", i)).unwrap();
        }

        // Newest entry must still be present (oldest were trimmed)
        let last = mgr.admin_log.last().unwrap();
        assert_eq!(last.operation, "rename_validator");
        assert!(mgr.admin_log.len() <= MAX_ADMIN_LOG_SIZE);
    }

    #[test]
    fn test_h03_toggle_allows_deactivate_with_others() {
        // 4 validators so toggling one leaves 3 ≥ MIN_ACTIVE_VALIDATORS
        let mut mgr = setup_4();
        let addr1 = mgr.active_validators()[0].address.clone();
        let addr2 = mgr.active_validators()[1].address.clone();
        assert_eq!(mgr.active_count(), 4);

        // Deactivating one leaves 3 — should succeed
        let result = mgr.toggle_validator("admin", &addr1);
        assert!(result.is_ok());
        assert_eq!(mgr.active_count(), 3);

        // Deactivating another would leave 2 < 3 — should fail
        let result = mgr.toggle_validator("admin", &addr2);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("at least 3"), "Expected min 3 error, got: {}", err_str);
    }
}
