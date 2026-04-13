// vm.rs - Sentrix — SRX-20 Token Standard

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use sha2::{Sha256, Digest};
use crate::types::error::{SentrixError, SentrixResult};

// ── Contract address generation ──────────────────────────
// V6-C-01 FIX: Use deterministic seed (txid or deployer+nonce) instead of SystemTime::now().
// Every node must produce the same contract address when applying the same block.
fn compute_contract_address(deployer: &str, seed: &str) -> String {
    let payload = format!("{}|{}", deployer, seed);
    let hash = Sha256::digest(payload.as_bytes());
    format!("SRX20_{}", hex::encode(&hash[..20]))
}

// ── SRX-20 Contract ──────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SRX20Contract {
    pub contract_address: String,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub owner: String,
    pub total_supply: u64,
    // V5-02: max_supply=0 means unlimited. #[serde(default)] for backward compat with old contracts.
    #[serde(default)]
    pub max_supply: u64,
    pub balances: HashMap<String, u64>,
    pub allowances: HashMap<String, HashMap<String, u64>>,
}

impl SRX20Contract {
    pub fn new(
        contract_address: String,
        name: String,
        symbol: String,
        decimals: u8,
        owner: String,
        max_supply: u64,
    ) -> Self {
        Self {
            contract_address,
            name,
            symbol,
            decimals,
            owner,
            total_supply: 0,
            max_supply,
            balances: HashMap::new(),
            allowances: HashMap::new(),
        }
    }

    // ── Core methods ─────────────────────────────────────

    /// M-05 FIX: mint() now enforces owner check directly.
    /// Previously only callers (execute_mint, call dispatcher) checked ownership —
    /// any future code path calling mint() directly would silently bypass the guard.
    pub fn mint(&mut self, caller: &str, to: &str, amount: u64) -> SentrixResult<()> {
        if caller != self.owner {
            return Err(SentrixError::UnauthorizedValidator(
                "only the contract owner can mint tokens".to_string()
            ));
        }
        if to.is_empty() || amount == 0 {
            return Err(SentrixError::InvalidTransaction("invalid mint params".to_string()));
        }
        // V5-02: enforce max_supply cap (0 = unlimited)
        if self.max_supply > 0 {
            let new_supply = self.total_supply.checked_add(amount)
                .ok_or_else(|| SentrixError::Internal("token supply overflow".to_string()))?;
            if new_supply > self.max_supply {
                return Err(SentrixError::InvalidTransaction(
                    format!("mint would exceed max_supply: {} + {} > {}", self.total_supply, amount, self.max_supply)
                ));
            }
        }
        let entry = self.balances.entry(to.to_string()).or_insert(0);
        *entry = entry.checked_add(amount)
            .ok_or_else(|| SentrixError::Internal("token balance overflow".to_string()))?;
        self.total_supply = self.total_supply.checked_add(amount)
            .ok_or_else(|| SentrixError::Internal("token supply overflow".to_string()))?;
        Ok(())
    }

    pub fn burn(&mut self, from: &str, amount: u64) -> SentrixResult<()> {
        if from.is_empty() || amount == 0 {
            return Err(SentrixError::InvalidTransaction("invalid burn params".to_string()));
        }
        let balance = self.balance_of(from);
        if balance < amount {
            return Err(SentrixError::InsufficientBalance { have: balance, need: amount });
        }
        let entry = self.balances.entry(from.to_string()).or_insert(0);
        *entry = entry.checked_sub(amount)
            .ok_or_else(|| SentrixError::Internal("token burn underflow".to_string()))?;
        self.total_supply = self.total_supply.checked_sub(amount)
            .ok_or_else(|| SentrixError::Internal("supply underflow".to_string()))?;
        Ok(())
    }

    pub fn transfer(&mut self, from: &str, to: &str, amount: u64) -> SentrixResult<()> {
        if from.is_empty() || to.is_empty() || amount == 0 {
            return Err(SentrixError::InvalidTransaction("invalid transfer params".to_string()));
        }
        if from == to {
            return Err(SentrixError::InvalidTransaction("cannot transfer to self".to_string()));
        }
        let balance = self.balance_of(from);
        if balance < amount {
            return Err(SentrixError::InsufficientBalance { have: balance, need: amount });
        }
        // Safe: checked balance >= amount above
        let from_entry = self.balances.entry(from.to_string()).or_insert(0);
        *from_entry = from_entry.checked_sub(amount)
            .ok_or_else(|| SentrixError::Internal("transfer underflow".to_string()))?;
        let to_entry = self.balances.entry(to.to_string()).or_insert(0);
        *to_entry = to_entry.checked_add(amount)
            .ok_or_else(|| SentrixError::Internal("transfer overflow".to_string()))?;
        Ok(())
    }

    // M-01 FIX: require allowance reset to 0 before setting new non-zero value
    pub fn approve(&mut self, owner: &str, spender: &str, amount: u64) -> SentrixResult<()> {
        if owner.is_empty() || spender.is_empty() {
            return Err(SentrixError::InvalidTransaction("invalid approve params".to_string()));
        }
        let current = self.allowance(owner, spender);
        if current != 0 && amount != 0 {
            return Err(SentrixError::InvalidTransaction(
                "must set allowance to 0 before changing to non-zero value".to_string()
            ));
        }
        self.allowances
            .entry(owner.to_string())
            .or_default()
            .insert(spender.to_string(), amount);
        Ok(())
    }

    pub fn increase_allowance(&mut self, owner: &str, spender: &str, delta: u64) -> SentrixResult<()> {
        if owner.is_empty() || spender.is_empty() || delta == 0 {
            return Err(SentrixError::InvalidTransaction("invalid increase_allowance params".to_string()));
        }
        let entry = self.allowances
            .entry(owner.to_string())
            .or_default()
            .entry(spender.to_string())
            .or_insert(0);
        *entry = entry.checked_add(delta)
            .ok_or_else(|| SentrixError::Internal("allowance overflow".to_string()))?;
        Ok(())
    }

    pub fn decrease_allowance(&mut self, owner: &str, spender: &str, delta: u64) -> SentrixResult<()> {
        if owner.is_empty() || spender.is_empty() || delta == 0 {
            return Err(SentrixError::InvalidTransaction("invalid decrease_allowance params".to_string()));
        }
        let entry = self.allowances
            .entry(owner.to_string())
            .or_default()
            .entry(spender.to_string())
            .or_insert(0);
        *entry = entry.checked_sub(delta)
            .ok_or_else(|| SentrixError::InvalidTransaction("allowance underflow".to_string()))?;
        Ok(())
    }

    pub fn transfer_from(
        &mut self,
        spender: &str,
        from: &str,
        to: &str,
        amount: u64,
    ) -> SentrixResult<()> {
        if spender.is_empty() || from.is_empty() || to.is_empty() || amount == 0 {
            return Err(SentrixError::InvalidTransaction("invalid transfer_from params".to_string()));
        }
        let allowed = self.allowance(from, spender);
        if allowed < amount {
            return Err(SentrixError::InvalidTransaction(
                format!("allowance {} < amount {}", allowed, amount)
            ));
        }
        let balance = self.balance_of(from);
        if balance < amount {
            return Err(SentrixError::InsufficientBalance { have: balance, need: amount });
        }
        // Safe deductions (all pre-checked above)
        let new_allowance = allowed.checked_sub(amount)
            .ok_or_else(|| SentrixError::Internal("allowance underflow".to_string()))?;
        self.allowances
            .entry(from.to_string()).or_default()
            .insert(spender.to_string(), new_allowance);
        let from_entry = self.balances.entry(from.to_string()).or_insert(0);
        *from_entry = from_entry.checked_sub(amount)
            .ok_or_else(|| SentrixError::Internal("transfer_from underflow".to_string()))?;
        let to_entry = self.balances.entry(to.to_string()).or_insert(0);
        *to_entry = to_entry.checked_add(amount)
            .ok_or_else(|| SentrixError::Internal("transfer_from overflow".to_string()))?;
        Ok(())
    }

    pub fn balance_of(&self, address: &str) -> u64 {
        self.balances.get(address).copied().unwrap_or(0)
    }

    pub fn allowance(&self, owner: &str, spender: &str) -> u64 {
        self.allowances
            .get(owner)
            .and_then(|m| m.get(spender))
            .copied()
            .unwrap_or(0)
    }

    pub fn holders(&self) -> usize {
        self.balances.values().filter(|&&b| b > 0).count()
    }

    pub fn get_info(&self) -> serde_json::Value {
        serde_json::json!({
            "contract_address": self.contract_address,
            "name": self.name,
            "symbol": self.symbol,
            "decimals": self.decimals,
            "total_supply": self.total_supply,
            "max_supply": self.max_supply, // V5-02: 0 = unlimited
            "owner": self.owner,
            "holders": self.holders(),
        })
    }

    pub fn list_holders(&self) -> Vec<serde_json::Value> {
        let mut holders: Vec<(&String, u64)> = self.balances.iter()
            .filter(|(_, b)| **b > 0)
            .map(|(addr, bal)| (addr, *bal))
            .collect();
        holders.sort_by(|a, b| b.1.cmp(&a.1));
        holders.iter().map(|(addr, bal)| serde_json::json!({
            "address": addr,
            "balance": bal,
        })).collect()
    }
}

// ── Contract Registry ────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ContractRegistry {
    pub contracts: HashMap<String, SRX20Contract>,
}

impl ContractRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    // V6-C-01 FIX: `seed` must be deterministic on-chain data.
    // For blocks: pass `tx.txid`. For internal/testing: pass deployer+nonce combo.
    // Never pass SystemTime::now() — it causes consensus divergence across nodes.
    pub fn deploy(
        &mut self,
        deployer: &str,
        name: &str,
        symbol: &str,
        decimals: u8,
        total_supply: u64,
        max_supply: u64, // V5-02: 0 = unlimited
        seed: &str,      // V6-C-01: deterministic seed (txid for on-chain deploys)
    ) -> SentrixResult<String> {
        // L-03 FIX: Validate token name and symbol lengths/format
        if name.is_empty() || name.len() > 64 {
            return Err(SentrixError::InvalidTransaction(
                "token name must be 1–64 characters".to_string(),
            ));
        }
        if symbol.is_empty() || symbol.len() > 10 || !symbol.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(SentrixError::InvalidTransaction(
                "token symbol must be 1–10 ASCII alphanumeric characters".to_string(),
            ));
        }

        // V6-C-01 FIX: address derived from deterministic seed — no SystemTime::now()
        let mut addr = compute_contract_address(deployer, seed);
        // Handle collision by appending a counter (same seed + same state = same collision resolution)
        let mut counter = 0u64;
        while self.contracts.contains_key(&addr) {
            counter += 1;
            addr = compute_contract_address(deployer, &format!("{}|{}", seed, counter));
        }

        let mut contract = SRX20Contract::new(
            addr.clone(),
            name.to_string(),
            symbol.to_string(),
            decimals,
            deployer.to_string(),
            max_supply, // V5-02: 0 = unlimited
        );
        // Mint total supply to deployer (deployer is the owner, so caller == owner)
        contract.mint(deployer, deployer, total_supply)?;
        self.contracts.insert(addr.clone(), contract);
        Ok(addr)
    }

    pub fn get_contract(&self, address: &str) -> Option<&SRX20Contract> {
        self.contracts.get(address)
    }

    pub fn get_contract_mut(&mut self, address: &str) -> Option<&mut SRX20Contract> {
        self.contracts.get_mut(address)
    }

    pub fn list_contracts(&self) -> Vec<serde_json::Value> {
        self.contracts.values().map(|c| c.get_info()).collect()
    }

    pub fn contract_count(&self) -> usize {
        self.contracts.len()
    }

    pub fn exists(&self, address: &str) -> bool {
        self.contracts.contains_key(address)
    }

    pub fn get_token_balance(&self, contract: &str, address: &str) -> u64 {
        self.contracts.get(contract)
            .map(|c| c.balance_of(address))
            .unwrap_or(0)
    }

    pub fn get_holders_list(&self, contract: &str) -> Option<Vec<serde_json::Value>> {
        self.contracts.get(contract).map(|c| c.list_holders())
    }

    // ── On-chain token op helpers (called from add_block) ──

    pub fn execute_transfer(&mut self, contract: &str, from: &str, to: &str, amount: u64) -> SentrixResult<()> {
        let c = self.contracts.get_mut(contract)
            .ok_or_else(|| SentrixError::NotFound(format!("contract {}", contract)))?;
        c.transfer(from, to, amount)
    }

    pub fn execute_burn(&mut self, contract: &str, from: &str, amount: u64) -> SentrixResult<()> {
        let c = self.contracts.get_mut(contract)
            .ok_or_else(|| SentrixError::NotFound(format!("contract {}", contract)))?;
        c.burn(from, amount)
    }

    pub fn execute_mint(&mut self, contract: &str, caller: &str, to: &str, amount: u64) -> SentrixResult<()> {
        let c = self.contracts.get_mut(contract)
            .ok_or_else(|| SentrixError::NotFound(format!("contract {}", contract)))?;
        // mint() now enforces owner check internally (M-05), caller passed through
        c.mint(caller, to, amount)
    }

    pub fn execute_approve(&mut self, contract: &str, owner: &str, spender: &str, amount: u64) -> SentrixResult<()> {
        let c = self.contracts.get_mut(contract)
            .ok_or_else(|| SentrixError::NotFound(format!("contract {}", contract)))?;
        c.approve(owner, spender, amount)
    }

    // ── Dispatch ─────────────────────────────────────────
    pub fn call(
        &mut self,
        contract_address: &str,
        method: &str,
        caller: &str,
        params: &serde_json::Value,
    ) -> SentrixResult<serde_json::Value> {
        let contract = self.contracts.get_mut(contract_address)
            .ok_or_else(|| SentrixError::NotFound(
                format!("contract {}", contract_address)
            ))?;

        match method {
            "mint" => {
                let to = params["to"].as_str().unwrap_or("");
                let amount = params["amount"].as_u64().unwrap_or(0);
                // mint() enforces owner check internally (M-05 fix)
                contract.mint(caller, to, amount)?;
                Ok(serde_json::json!({"status": "ok"}))
            }
            "burn" => {
                let amount = params["amount"].as_u64().unwrap_or(0);
                contract.burn(caller, amount)?;
                Ok(serde_json::json!({"status": "ok", "burned": amount}))
            }
            "transfer" => {
                let to = params["to"].as_str().unwrap_or("");
                let amount = params["amount"].as_u64().unwrap_or(0);
                contract.transfer(caller, to, amount)?;
                Ok(serde_json::json!({"status": "ok"}))
            }
            "approve" => {
                let spender = params["spender"].as_str().unwrap_or("");
                let amount = params["amount"].as_u64().unwrap_or(0);
                contract.approve(caller, spender, amount)?;
                Ok(serde_json::json!({"status": "ok"}))
            }
            "increase_allowance" => {
                let spender = params["spender"].as_str().unwrap_or("");
                let delta = params["amount"].as_u64().unwrap_or(0);
                contract.increase_allowance(caller, spender, delta)?;
                Ok(serde_json::json!({"status": "ok"}))
            }
            "decrease_allowance" => {
                let spender = params["spender"].as_str().unwrap_or("");
                let delta = params["amount"].as_u64().unwrap_or(0);
                contract.decrease_allowance(caller, spender, delta)?;
                Ok(serde_json::json!({"status": "ok"}))
            }
            "transfer_from" => {
                let from = params["from"].as_str().unwrap_or("");
                let to = params["to"].as_str().unwrap_or("");
                let amount = params["amount"].as_u64().unwrap_or(0);
                contract.transfer_from(caller, from, to, amount)?;
                Ok(serde_json::json!({"status": "ok"}))
            }
            "balance_of" => {
                let address = params["address"].as_str().unwrap_or("");
                Ok(serde_json::json!({"balance": contract.balance_of(address)}))
            }
            "allowance" => {
                let owner = params["owner"].as_str().unwrap_or("");
                let spender = params["spender"].as_str().unwrap_or("");
                Ok(serde_json::json!({"allowance": contract.allowance(owner, spender)}))
            }
            "get_info" => {
                Ok(contract.get_info())
            }
            _ => Err(SentrixError::NotFound(format!("method {}", method))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_registry() -> (ContractRegistry, String) {
        let mut reg = ContractRegistry::new();
        let addr = reg.deploy("owner", "FastPoint Token", "FPT", 18, 1_000_000_000, 0, "seed_fpt").unwrap();
        (reg, addr)
    }

    #[test]
    fn test_deploy_contract() {
        let (reg, addr) = setup_registry();
        assert!(addr.starts_with("SRX20_"));
        assert_eq!(reg.contract_count(), 1);
        let c = reg.get_contract(&addr).unwrap();
        assert_eq!(c.name, "FastPoint Token");
        assert_eq!(c.symbol, "FPT");
        assert_eq!(c.total_supply, 1_000_000_000);
        assert_eq!(c.balance_of("owner"), 1_000_000_000);
    }

    #[test]
    fn test_transfer() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();
        c.transfer("owner", "alice", 1_000).unwrap();
        assert_eq!(c.balance_of("owner"), 999_999_000);
        assert_eq!(c.balance_of("alice"), 1_000);
    }

    #[test]
    fn test_transfer_insufficient() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();
        let result = c.transfer("alice", "bob", 1_000); // alice has 0
        assert!(result.is_err());
    }

    #[test]
    fn test_transfer_to_self() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();
        let result = c.transfer("owner", "owner", 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_approve_and_transfer_from() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();

        // Owner approves spender for 500
        c.approve("owner", "spender", 500).unwrap();
        assert_eq!(c.allowance("owner", "spender"), 500);

        // Spender transfers 200 from owner to alice
        c.transfer_from("spender", "owner", "alice", 200).unwrap();
        assert_eq!(c.balance_of("alice"), 200);
        assert_eq!(c.allowance("owner", "spender"), 300);
    }

    #[test]
    fn test_transfer_from_exceeds_allowance() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();
        c.approve("owner", "spender", 100).unwrap();
        let result = c.transfer_from("spender", "owner", "alice", 200);
        assert!(result.is_err());
    }

    #[test]
    fn test_mint_only_owner() {
        let (mut reg, addr) = setup_registry();
        let result = reg.call(
            &addr, "mint", "not_owner",
            &serde_json::json!({"to": "someone", "amount": 1000}),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_mint_by_owner() {
        let (mut reg, addr) = setup_registry();
        reg.call(
            &addr, "mint", "owner",
            &serde_json::json!({"to": "alice", "amount": 5000}),
        ).unwrap();
        let c = reg.get_contract(&addr).unwrap();
        assert_eq!(c.balance_of("alice"), 5000);
        assert_eq!(c.total_supply, 1_000_005_000);
    }

    #[test]
    fn test_dispatch_transfer() {
        let (mut reg, addr) = setup_registry();
        reg.call(
            &addr, "transfer", "owner",
            &serde_json::json!({"to": "bob", "amount": 100}),
        ).unwrap();
        let c = reg.get_contract(&addr).unwrap();
        assert_eq!(c.balance_of("bob"), 100);
    }

    #[test]
    fn test_dispatch_balance_of() {
        let (mut reg, addr) = setup_registry();
        let result = reg.call(
            &addr, "balance_of", "anyone",
            &serde_json::json!({"address": "owner"}),
        ).unwrap();
        assert_eq!(result["balance"], 1_000_000_000);
    }

    #[test]
    fn test_dispatch_get_info() {
        let (mut reg, addr) = setup_registry();
        let result = reg.call(&addr, "get_info", "anyone", &serde_json::json!({})).unwrap();
        assert_eq!(result["symbol"], "FPT");
        assert_eq!(result["holders"], 1);
    }

    #[test]
    fn test_holders_count() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();
        assert_eq!(c.holders(), 1); // only owner
        c.transfer("owner", "alice", 100).unwrap();
        assert_eq!(c.holders(), 2);
        c.transfer("owner", "bob", 100).unwrap();
        assert_eq!(c.holders(), 3);
    }

    #[test]
    fn test_unknown_contract() {
        let mut reg = ContractRegistry::new();
        let result = reg.call("SRX20_fake", "transfer", "caller", &serde_json::json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn test_unknown_method() {
        let (mut reg, addr) = setup_registry();
        let result = reg.call(&addr, "destroy", "owner", &serde_json::json!({}));
        assert!(result.is_err());
    }

    #[test]
    fn test_list_contracts() {
        let (reg, _addr) = setup_registry();
        let list = reg.list_contracts();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["symbol"], "FPT");
    }

    #[test]
    fn test_burn() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();
        let supply_before = c.total_supply;
        c.burn("owner", 1_000).unwrap();
        assert_eq!(c.balance_of("owner"), supply_before - 1_000);
        assert_eq!(c.total_supply, supply_before - 1_000);
    }

    #[test]
    fn test_burn_insufficient() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();
        let result = c.burn("alice", 1_000); // alice has 0
        assert!(result.is_err());
    }

    #[test]
    fn test_burn_via_dispatch() {
        let (mut reg, addr) = setup_registry();
        reg.call(
            &addr, "burn", "owner",
            &serde_json::json!({"amount": 500}),
        ).unwrap();
        let c = reg.get_contract(&addr).unwrap();
        assert_eq!(c.total_supply, 999_999_500);
    }

    #[test]
    fn test_burn_reduces_supply() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();
        c.transfer("owner", "alice", 1_000).unwrap();
        c.burn("alice", 500).unwrap();
        assert_eq!(c.balance_of("alice"), 500);
        assert_eq!(c.total_supply, 999_999_500); // 1B - 500 burned
    }

    #[test]
    fn test_m01_approve_race_condition_prevented() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();

        // First approve from 0 → 500: should succeed
        c.approve("owner", "spender", 500).unwrap();
        assert_eq!(c.allowance("owner", "spender"), 500);

        // Change from 500 → 200 directly: should FAIL (race condition prevention)
        let result = c.approve("owner", "spender", 200);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must set allowance to 0"));

        // Reset to 0 first, then set new value: should succeed
        c.approve("owner", "spender", 0).unwrap();
        assert_eq!(c.allowance("owner", "spender"), 0);
        c.approve("owner", "spender", 200).unwrap();
        assert_eq!(c.allowance("owner", "spender"), 200);
    }

    // ── M-05: mint() Owner Check ────────────────────────

    #[test]
    fn test_m05_mint_direct_non_owner_fails() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();

        // Direct call to SRX20Contract::mint() with non-owner must be rejected
        let result = c.mint("not_owner", "alice", 1_000);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("owner"), "Expected owner error, got: {}", err_str);
        // Balance unchanged
        assert_eq!(c.balance_of("alice"), 0);
    }

    #[test]
    fn test_m05_mint_direct_owner_succeeds() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();
        let supply_before = c.total_supply;

        // Direct call with the actual owner must succeed
        c.mint("owner", "alice", 5_000).unwrap();
        assert_eq!(c.balance_of("alice"), 5_000);
        assert_eq!(c.total_supply, supply_before + 5_000);
    }

    #[test]
    fn test_m05_execute_mint_validates_owner() {
        let (mut reg, addr) = setup_registry();

        // execute_mint with non-owner should fail (enforced by mint() itself)
        let result = reg.execute_mint(&addr, "not_owner", "alice", 100);
        assert!(result.is_err());

        // execute_mint with owner should succeed
        reg.execute_mint(&addr, "owner", "alice", 100).unwrap();
        assert_eq!(reg.get_token_balance(&addr, "alice"), 100);
    }

    #[test]
    fn test_m05_deploy_mints_to_deployer_as_owner() {
        // Verify that ContractRegistry::deploy() still works correctly after M-05 fix:
        // deploy calls mint(deployer, deployer, supply) — deployer is owner so it passes.
        let mut reg = ContractRegistry::new();
        let addr = reg.deploy("alice", "AliceCoin", "ALC", 18, 500_000, 0, "seed_alc").unwrap();
        let c = reg.get_contract(&addr).unwrap();
        assert_eq!(c.owner, "alice");
        assert_eq!(c.balance_of("alice"), 500_000);
        assert_eq!(c.total_supply, 500_000);
    }

    #[test]
    fn test_m01_increase_decrease_allowance() {
        let (mut reg, addr) = setup_registry();
        let c = reg.get_contract_mut(&addr).unwrap();

        // Start at 0, increase by 100
        c.increase_allowance("owner", "spender", 100).unwrap();
        assert_eq!(c.allowance("owner", "spender"), 100);

        // Increase by another 50
        c.increase_allowance("owner", "spender", 50).unwrap();
        assert_eq!(c.allowance("owner", "spender"), 150);

        // Decrease by 30
        c.decrease_allowance("owner", "spender", 30).unwrap();
        assert_eq!(c.allowance("owner", "spender"), 120);

        // Decrease below 0 should fail
        let result = c.decrease_allowance("owner", "spender", 999);
        assert!(result.is_err());
        // Allowance unchanged
        assert_eq!(c.allowance("owner", "spender"), 120);
    }

    // ── L-03: token name/symbol validation tests ──────────

    #[test]
    fn test_l03_deploy_name_too_long() {
        let mut reg = ContractRegistry::new();
        let long_name = "A".repeat(65); // 65 chars — exceeds 64 limit
        let result = reg.deploy("deployer", &long_name, "TKN", 8, 1_000_000, 0, "seed_long_name");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("name"), "error should mention 'name': {err}");
    }

    #[test]
    fn test_l03_deploy_symbol_too_long() {
        let mut reg = ContractRegistry::new();
        let long_sym = "ABCDEFGHIJK"; // 11 chars — exceeds 10 limit
        let result = reg.deploy("deployer", "ValidName", long_sym, 8, 1_000_000, 0, "seed_long_sym");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("symbol"), "error should mention 'symbol': {err}");
    }

    #[test]
    fn test_l03_deploy_symbol_non_alphanumeric_rejected() {
        let mut reg = ContractRegistry::new();
        // Spaces and special chars not allowed
        assert!(reg.deploy("d", "Name", "T K N", 8, 0, 0, "seed1").is_err());
        assert!(reg.deploy("d", "Name", "TK$N", 8, 0, 0, "seed2").is_err());
        assert!(reg.deploy("d", "Name", "", 8, 0, 0, "seed3").is_err());
        // Valid symbol must succeed
        assert!(reg.deploy("d", "Name", "TKN", 8, 1_000_000, 0, "seed4").is_ok());
    }

    // ── V5-02: Token max_supply cap tests ────────────────

    #[test]
    fn test_v502_mint_within_cap_succeeds() {
        let mut reg = ContractRegistry::new();
        // Deploy with max_supply=2_000_000 and initial supply=1_000_000
        let addr = reg.deploy("owner", "CappedToken", "CAP", 18, 1_000_000, 2_000_000, "seed_cap1").unwrap();
        let c = reg.get_contract(&addr).unwrap();
        assert_eq!(c.max_supply, 2_000_000);
        assert_eq!(c.total_supply, 1_000_000);

        // Minting 500_000 more (→1_500_000 ≤ 2_000_000) must succeed
        reg.execute_mint(&addr, "owner", "owner", 500_000).unwrap();
        let c = reg.get_contract(&addr).unwrap();
        assert_eq!(c.total_supply, 1_500_000);
    }

    #[test]
    fn test_v502_mint_exceeds_cap_rejected() {
        let mut reg = ContractRegistry::new();
        // max_supply=1_000_000, initial supply=1_000_000 (already at cap)
        let addr = reg.deploy("owner", "CappedToken", "CAP", 18, 1_000_000, 1_000_000, "seed_cap2").unwrap();

        // Minting even 1 more must fail
        let result = reg.execute_mint(&addr, "owner", "owner", 1);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("max_supply"), "error should mention max_supply: {err}");
    }

    #[test]
    fn test_v502_unlimited_cap_allows_unbounded_mint() {
        let mut reg = ContractRegistry::new();
        // max_supply=0 means unlimited
        let addr = reg.deploy("owner", "UnlimitedToken", "UNL", 18, 1_000_000, 0, "seed_unl").unwrap();

        // Should mint any amount without restriction
        reg.execute_mint(&addr, "owner", "owner", 1_000_000_000).unwrap();
        let c = reg.get_contract(&addr).unwrap();
        assert_eq!(c.total_supply, 1_001_000_000);
    }

    #[test]
    fn test_v502_max_supply_in_get_info() {
        let mut reg = ContractRegistry::new();
        let addr = reg.deploy("owner", "TestToken", "TST", 8, 500_000, 1_000_000, "seed_tst").unwrap();
        let info = reg.get_contract(&addr).unwrap().get_info();
        assert_eq!(info["max_supply"], 1_000_000_u64);
        assert_eq!(info["total_supply"], 500_000_u64);
    }

    #[test]
    fn test_v502_deploy_supply_exceeding_max_supply_rejected() {
        let mut reg = ContractRegistry::new();
        // initial supply > max_supply should fail
        let result = reg.deploy("owner", "Invalid", "INV", 18, 2_000_000, 1_000_000, "seed_inv");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("supply") || err.contains("max"),
            "Expected supply cap error, got: {}", err
        );
    }

    #[test]
    fn test_v502_mint_exactly_at_cap_succeeds() {
        let mut reg = ContractRegistry::new();
        let addr = reg.deploy("owner", "ExactCap", "EXC", 18, 900_000, 1_000_000, "seed_exc").unwrap();
        // Mint exactly to the cap (900k + 100k = 1M)
        assert!(reg.execute_mint(&addr, "owner", "owner", 100_000).is_ok());
        let c = reg.get_contract(&addr).unwrap();
        assert_eq!(c.total_supply, 1_000_000);

        // One more mint should fail
        let result = reg.execute_mint(&addr, "owner", "owner", 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_v502_burn_reduces_supply_allowing_more_mint() {
        let mut reg = ContractRegistry::new();
        let addr = reg.deploy("owner", "Burnable", "BRN", 18, 1_000_000, 1_000_000, "seed_brn").unwrap();
        // At cap — can't mint more
        assert!(reg.execute_mint(&addr, "owner", "owner", 1).is_err());
        // Burn some
        reg.execute_burn(&addr, "owner", 500_000).unwrap();
        // Now can mint again
        assert!(reg.execute_mint(&addr, "owner", "owner", 500_000).is_ok());
    }
}
