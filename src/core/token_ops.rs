// token_ops.rs - Sentrix — SRX-20 token operations

use crate::core::blockchain::{Blockchain, ECOSYSTEM_FUND_ADDRESS};
use crate::types::error::{SentrixError, SentrixResult};

impl Blockchain {
    // ── SRX-20 Token Operations ────────────────────────────

    pub fn deploy_token(
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

            let burn_share = deploy_fee.div_ceil(2); // L-02 FIX: burn rounds up
            let eco_share = deploy_fee - burn_share;
            self.accounts.total_burned += burn_share;
            self.accounts.credit(ECOSYSTEM_FUND_ADDRESS, eco_share)?;
        }

        // Deploy contract
        let contract_address = self.contracts.deploy(deployer, &name, &symbol, decimals, total_supply, max_supply)?;
        Ok(contract_address)
    }

    pub fn token_transfer(
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

            let burn_share = gas_fee.div_ceil(2); // L-02 FIX: burn rounds up
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

    pub fn token_burn(
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

            let burn_share = gas_fee.div_ceil(2); // L-02 FIX: burn rounds up
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
