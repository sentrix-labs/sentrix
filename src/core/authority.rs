// authority.rs - Sentrix

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::types::error::{SentrixError, SentrixResult};

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
}

impl AuthorityManager {
    pub fn new(admin_address: String) -> Self {
        Self {
            validators: HashMap::new(),
            admin_address,
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
        self.validators.insert(
            address.clone(),
            Validator::new(address, name, public_key),
        );
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
        // Ensure at least 1 active validator remains
        let active_after = self.active_validators().iter()
            .filter(|v| v.address != address)
            .count();
        if active_after < 1 {
            return Err(SentrixError::InvalidBlock(
                "cannot remove: at least 1 active validator required".to_string()
            ));
        }
        self.validators.remove(address)
            .ok_or_else(|| SentrixError::NotFound(format!("validator {}", address)))?;
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

        // H-03 FIX: prevent deactivating the last active validator
        if validator.is_active {
            let active_after = self.active_validators().iter()
                .filter(|v| v.address != address)
                .count();
            if active_after < 1 {
                return Err(SentrixError::InvalidBlock(
                    "cannot deactivate: at least 1 active validator required".to_string()
                ));
            }
        }

        let validator = self.validators.get_mut(address).unwrap();
        validator.is_active = !validator.is_active;
        Ok(validator.is_active)
    }

    pub fn rename_validator(&mut self, caller: &str, address: &str, new_name: String) -> SentrixResult<()> {
        if caller != self.admin_address {
            return Err(SentrixError::UnauthorizedValidator(
                format!("{} is not admin", caller)
            ));
        }
        let validator = self.validators.get_mut(address)
            .ok_or_else(|| SentrixError::NotFound(format!("validator {}", address)))?;
        validator.name = new_name;
        Ok(())
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> AuthorityManager {
        let mut mgr = AuthorityManager::new("admin".to_string());
        mgr.add_validator("admin", "val1".to_string(), "Validator 1".to_string(), "pk1".to_string()).unwrap();
        mgr.add_validator("admin", "val2".to_string(), "Validator 2".to_string(), "pk2".to_string()).unwrap();
        mgr.add_validator("admin", "val3".to_string(), "Validator 3".to_string(), "pk3".to_string()).unwrap();
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
        let result = mgr.add_validator(
            "not_admin",
            "val4".to_string(),
            "Val 4".to_string(),
            "pk4".to_string(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_toggle_validator() {
        let mut mgr = setup();
        let addr = mgr.active_validators()[0].address.clone();

        mgr.toggle_validator("admin", &addr).unwrap();
        assert_eq!(mgr.active_count(), 2);

        mgr.toggle_validator("admin", &addr).unwrap();
        assert_eq!(mgr.active_count(), 3);
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
        mgr.add_validator("admin", "val1".to_string(), "V1".to_string(), "pk1".to_string()).unwrap();
        assert_eq!(mgr.active_count(), 1);

        // Trying to deactivate the only active validator should fail
        let result = mgr.toggle_validator("admin", "val1");
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("at least 1 active validator"), "Expected min validator error, got: {}", err_str);

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

    #[test]
    fn test_h03_toggle_allows_deactivate_with_others() {
        let mut mgr = AuthorityManager::new("admin".to_string());
        mgr.add_validator("admin", "val1".to_string(), "V1".to_string(), "pk1".to_string()).unwrap();
        mgr.add_validator("admin", "val2".to_string(), "V2".to_string(), "pk2".to_string()).unwrap();
        assert_eq!(mgr.active_count(), 2);

        // With 2 validators, deactivating one should succeed
        let result = mgr.toggle_validator("admin", "val1");
        assert!(result.is_ok());
        assert_eq!(mgr.active_count(), 1);

        // But deactivating the last one should fail
        let result = mgr.toggle_validator("admin", "val2");
        assert!(result.is_err());
    }
}
