// token_ops.rs - Sentrix — SRX-20 token operations

use crate::core::blockchain::{Blockchain, ECOSYSTEM_FUND_ADDRESS};
use crate::types::error::{SentrixError, SentrixResult};

impl Blockchain {
    // ── SRX-20 Token Operations ────────────────────────────

    // pub(crate) — not callable from routes.rs; canonical on-chain path uses the mempool/add_block pipeline.
    // Direct state mutation bypasses transaction lifecycle; restrict to internal/testing use only.
    // #[allow(dead_code)] — kept for potential future internal use (migration, genesis tooling)
    #[allow(dead_code)]
    pub(crate) fn deploy_token(
        &mut self,
        deployer: &str,
        name: String,
        symbol: String,
        decimals: u8,
        total_supply: u64,
        max_supply: u64,
        deploy_fee: u64,
    ) -> SentrixResult<String> {
        // Check deployer has enough for fee
        let balance = self.accounts.get_balance(deployer);
        if balance < deploy_fee {
            return Err(SentrixError::InsufficientBalance {
                have: balance,
                need: deploy_fee,
            });
        }

        // Deduct fee: 50% burned, 50% to ecosystem fund
        if deploy_fee > 0 {
            let deployer_acc = self.accounts.get_or_create(deployer);
            deployer_acc.balance = deployer_acc.balance.checked_sub(deploy_fee)
                .ok_or_else(|| SentrixError::Internal("deploy fee underflow".to_string()))?;

            let burn_share = deploy_fee.div_ceil(2); // burn gets ceiling so no fee is lost to rounding
            let eco_share = deploy_fee - burn_share;
            self.accounts.total_burned += burn_share;
            self.accounts.credit(ECOSYSTEM_FUND_ADDRESS, eco_share)?;
        }

        // Deploy contract — use deployer+nonce as seed (internal path; no txid available here)
        // Internal only — canonical on-chain path passes tx.txid through add_block for deterministic addressing
        let nonce = self.accounts.get_nonce(deployer);
        let seed = format!("{}|{}|{}", deployer, nonce, name);
        let contract_address = self.contracts.deploy(deployer, &name, &symbol, decimals, total_supply, max_supply, &seed)?;
        Ok(contract_address)
    }

    // pub(crate) — internal/testing use only; routes submit through the mempool path
    #[allow(dead_code)]
    pub(crate) fn token_transfer(
        &mut self,
        contract_address: &str,
        caller: &str,
        to: &str,
        amount: u64,
        gas_fee: u64,
    ) -> SentrixResult<()> {
        // Check caller has enough SRX for gas
        let balance = self.accounts.get_balance(caller);
        if balance < gas_fee {
            return Err(SentrixError::InsufficientBalance {
                have: balance,
                need: gas_fee,
            });
        }

        // Deduct gas: 50% burned, 50% to current validator (or ecosystem fund)
        if gas_fee > 0 {
            let caller_acc = self.accounts.get_or_create(caller);
            caller_acc.balance = caller_acc.balance.checked_sub(gas_fee)
                .ok_or_else(|| SentrixError::Internal("gas fee underflow".to_string()))?;

            let burn_share = gas_fee.div_ceil(2); // burn gets ceiling so no fee is lost to rounding
            let val_share = gas_fee - burn_share;
            self.accounts.total_burned += burn_share;

            let validator_addr = self.authority
                .expected_validator(self.height() + 1)
                .map(|v| v.address.clone())
                .unwrap_or_else(|_| ECOSYSTEM_FUND_ADDRESS.to_string());
            self.accounts.credit(&validator_addr, val_share)?;
        }

        // Execute token transfer
        let contract = self.contracts.get_contract_mut(contract_address)
            .ok_or_else(|| SentrixError::NotFound(format!("contract {}", contract_address)))?;
        contract.transfer(caller, to, amount)?;
        Ok(())
    }

    // pub(crate) — internal/testing use only; routes submit through the mempool path
    #[allow(dead_code)]
    pub(crate) fn token_burn(
        &mut self,
        contract_address: &str,
        caller: &str,
        amount: u64,
        gas_fee: u64,
    ) -> SentrixResult<()> {
        // Check caller has enough SRX for gas
        let balance = self.accounts.get_balance(caller);
        if balance < gas_fee {
            return Err(SentrixError::InsufficientBalance {
                have: balance,
                need: gas_fee,
            });
        }

        // Deduct gas: 50% burned, 50% to validator
        if gas_fee > 0 {
            let caller_acc = self.accounts.get_or_create(caller);
            caller_acc.balance = caller_acc.balance.checked_sub(gas_fee)
                .ok_or_else(|| SentrixError::Internal("gas fee underflow".to_string()))?;

            let burn_share = gas_fee.div_ceil(2); // burn gets ceiling so no fee is lost to rounding
            let val_share = gas_fee - burn_share;
            self.accounts.total_burned += burn_share;

            let validator_addr = self.authority
                .expected_validator(self.height() + 1)
                .map(|v| v.address.clone())
                .unwrap_or_else(|_| ECOSYSTEM_FUND_ADDRESS.to_string());
            self.accounts.credit(&validator_addr, val_share)?;
        }

        // Execute token burn
        let contract = self.contracts.get_contract_mut(contract_address)
            .ok_or_else(|| SentrixError::NotFound(format!("contract {}", contract_address)))?;
        contract.burn(caller, amount)?;
        Ok(())
    }

    pub fn token_balance(&self, contract_address: &str, address: &str) -> u64 {
        self.contracts.get_contract(contract_address)
            .map(|c| c.balance_of(address))
            .unwrap_or(0)
    }

    pub fn token_info(&self, contract_address: &str) -> SentrixResult<serde_json::Value> {
        let contract = self.contracts.get_contract(contract_address)
            .ok_or_else(|| SentrixError::NotFound(format!("contract {}", contract_address)))?;
        Ok(contract.get_info())
    }

    pub fn list_tokens(&self) -> Vec<serde_json::Value> {
        self.contracts.list_contracts()
    }
}

// ── Tests ─────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use crate::core::blockchain::Blockchain;

    fn setup() -> Blockchain {
        let mut bc = Blockchain::new("admin".to_string());
        bc.authority.add_validator_unchecked("v1".to_string(), "V1".to_string(), "pk1".to_string());
        bc
    }

    // deploy_token fee correctly split 50/50 burn vs ecosystem fund
    #[test]
    fn test_deploy_token_fee_split() {
        let mut bc = setup();
        let deployer = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let deploy_fee = 1_000_000u64;
        bc.accounts.credit(deployer, deploy_fee).unwrap();

        let burned_before = bc.accounts.total_burned;
        let eco_before = bc.accounts.get_balance(crate::core::blockchain::ECOSYSTEM_FUND_ADDRESS);

        bc.deploy_token(deployer, "Foo".to_string(), "FOO".to_string(), 8, 1_000, 0, deploy_fee).unwrap();

        let burned_after = bc.accounts.total_burned;
        let eco_after = bc.accounts.get_balance(crate::core::blockchain::ECOSYSTEM_FUND_ADDRESS);

        // burn_share = ceil(fee/2), eco_share = fee - burn_share
        let expected_burn = deploy_fee.div_ceil(2);
        let expected_eco = deploy_fee - expected_burn;
        assert_eq!(burned_after - burned_before, expected_burn);
        assert_eq!(eco_after - eco_before, expected_eco);
    }

    // token_info returns error for unknown contract
    #[test]
    fn test_token_info_unknown_contract() {
        let bc = setup();
        let result = bc.token_info("SRX20_nonexistent_contract");
        assert!(result.is_err());
    }

    // token_balance returns 0 for unknown contract (not an error)
    #[test]
    fn test_token_balance_unknown_contract_returns_zero() {
        let bc = setup();
        let balance = bc.token_balance("SRX20_nonexistent", "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
        assert_eq!(balance, 0);
    }
}
