// blockchain.rs - Sentrix

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use crate::core::block::Block;
use crate::core::transaction::{Transaction, TokenOp};
use crate::core::account::AccountDB;
use crate::core::authority::AuthorityManager;
use crate::core::merkle::merkle_root;
use crate::core::vm::ContractRegistry;
use crate::types::error::{SentrixError, SentrixResult};

// ── Chain constants ──────────────────────────────────────
pub const MAX_SUPPLY: u64         = 210_000_000 * 100_000_000; // in sentri
pub const BLOCK_REWARD: u64       = 100_000_000;               // 1 SRX in sentri
pub const HALVING_INTERVAL: u64   = 42_000_000;                 // blocks
pub const BLOCK_TIME_SECS: u64    = 3;
pub const MAX_TX_PER_BLOCK: usize = 100;
pub const CHAIN_ID: u64           = 7119;

// ── Genesis addresses (from genesis_wallets.json — private keys secured) ──
pub const FOUNDER_ADDRESS:         &str = "0x4f3319a747fd564136209cd5d9e7d1a1e4d142be";
pub const ECOSYSTEM_FUND_ADDRESS:  &str = "0xeb70fdefd00fdb768dec06c478f450c351499f14";
pub const EARLY_VALIDATOR_ADDRESS: &str = "0xa7fc67af1ba0c664d859f4c1bcd2eb1f7211f112";
pub const RESERVE_ADDRESS:         &str = "0x2578cad17e3e56c2970a5b5eab45952439f5ba97";

pub const GENESIS_ALLOCATIONS: &[(&str, u64)] = &[
    (FOUNDER_ADDRESS,         21_000_000 * 100_000_000),
    (ECOSYSTEM_FUND_ADDRESS,  21_000_000 * 100_000_000),
    (EARLY_VALIDATOR_ADDRESS, 10_500_000 * 100_000_000),
    (RESERVE_ADDRESS,         10_500_000 * 100_000_000),
];

pub const TOTAL_PREMINE: u64 = 63_000_000 * 100_000_000;

// ── Blockchain struct ────────────────────────────────────
// M-04 FIX: skip chain in serialization — blocks saved individually in storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blockchain {
    #[serde(skip, default)]
    pub chain: Vec<Block>,
    pub accounts: AccountDB,
    pub authority: AuthorityManager,
    pub contracts: ContractRegistry,
    pub mempool: VecDeque<Transaction>,
    pub total_minted: u64,
    pub chain_id: u64,
}

impl Blockchain {
    pub fn new(admin_address: String) -> Self {
        let mut bc = Self {
            chain: Vec::new(),
            accounts: AccountDB::new(),
            authority: AuthorityManager::new(admin_address),
            contracts: ContractRegistry::new(),
            mempool: VecDeque::new(),
            total_minted: 0,
            chain_id: CHAIN_ID,
        };
        bc.initialize_genesis();
        bc
    }

    fn initialize_genesis(&mut self) {
        // Apply genesis premine allocations
        for (address, amount) in GENESIS_ALLOCATIONS {
            let _ = self.accounts.credit(address, *amount);
        }
        self.total_minted = TOTAL_PREMINE;

        // Create genesis block
        let genesis = Block::genesis();
        self.chain.push(genesis);
    }

    // ── Chain state queries ──────────────────────────────
    pub fn height(&self) -> u64 {
        self.chain.len() as u64 - 1
    }

    // L-02 FIX: return Result instead of panicking on empty chain
    pub fn latest_block(&self) -> SentrixResult<&Block> {
        self.chain.last()
            .ok_or_else(|| SentrixError::NotFound("chain is empty".to_string()))
    }

    pub fn get_block(&self, index: u64) -> Option<&Block> {
        self.chain.get(index as usize)
    }

    pub fn get_block_by_hash(&self, hash: &str) -> Option<&Block> {
        self.chain.iter().find(|b| b.hash == hash)
    }

    // ── Supply & reward ──────────────────────────────────
    pub fn get_block_reward(&self) -> u64 {
        let remaining = MAX_SUPPLY.saturating_sub(self.total_minted);
        if remaining == 0 {
            return 0;
        }

        let halvings = self.height() / HALVING_INTERVAL;
        let reward = BLOCK_REWARD >> halvings; // divide by 2^halvings

        if reward == 0 {
            return 0;
        }

        reward.min(remaining)
    }

    // ── Mempool ──────────────────────────────────────────
    pub fn add_to_mempool(&mut self, tx: Transaction) -> SentrixResult<()> {
        if tx.is_coinbase() {
            return Err(SentrixError::InvalidTransaction(
                "cannot manually add coinbase to mempool".to_string()
            ));
        }

        // Basic validation
        let expected_nonce = self.accounts.get_nonce(&tx.from_address)
            + self.mempool_pending_count(&tx.from_address);
        tx.validate(expected_nonce, self.chain_id)?;

        // Check balance including pending mempool spends
        let pending_spend = self.mempool_pending_spend(&tx.from_address);
        let available = self.accounts.get_balance(&tx.from_address)
            .saturating_sub(pending_spend);
        // H-02 FIX: checked addition to prevent overflow
        let needed = tx.amount.checked_add(tx.fee)
            .ok_or_else(|| SentrixError::InvalidTransaction(
                "amount + fee overflow".to_string()
            ))?;

        if available < needed {
            return Err(SentrixError::InsufficientBalance {
                have: available,
                need: needed,
            });
        }

        // Insert sorted by fee descending (highest fee = front of queue)
        let pos = self.mempool.iter()
            .position(|existing| existing.fee < tx.fee)
            .unwrap_or(self.mempool.len());
        self.mempool.insert(pos, tx);
        Ok(())
    }

    fn mempool_pending_count(&self, address: &str) -> u64 {
        self.mempool.iter()
            .filter(|tx| tx.from_address == address)
            .count() as u64
    }

    fn mempool_pending_spend(&self, address: &str) -> u64 {
        self.mempool.iter()
            .filter(|tx| tx.from_address == address)
            .map(|tx| tx.amount.saturating_add(tx.fee))
            .fold(0u64, |acc, v| acc.saturating_add(v))
    }

    pub fn mempool_size(&self) -> usize {
        self.mempool.len()
    }

    // ── Chain validation ─────────────────────────────────
    pub fn is_valid_chain(&self) -> bool {
        for i in 1..self.chain.len() {
            let block = &self.chain[i];
            let prev = &self.chain[i - 1];

            if block.previous_hash != prev.hash {
                return false;
            }
            if !block.is_valid_hash() {
                return false;
            }
            // Verify merkle root matches transaction content
            let txids: Vec<String> = block.transactions.iter().map(|tx| tx.txid.clone()).collect();
            if merkle_root(&txids) != block.merkle_root {
                return false;
            }
        }
        true
    }

    // ── Block creation (validator calls this) ────────────
    pub fn create_block(&mut self, validator_address: &str) -> SentrixResult<Block> {
        let next_height = self.height() + 1;

        // Check authorization
        if !self.authority.is_authorized(validator_address, next_height)? {
            return Err(SentrixError::NotYourTurn);
        }

        // Build transaction list — coinbase first
        let reward = self.get_block_reward();
        let coinbase = Transaction::new_coinbase(
            validator_address.to_string(),
            reward,
            next_height,
        );

        let mut transactions = vec![coinbase];

        // Take up to MAX_TX_PER_BLOCK from mempool
        let take = self.mempool.len().min(MAX_TX_PER_BLOCK - 1);
        let mempool_txs: Vec<Transaction> = self.mempool.drain(..take).collect();
        transactions.extend(mempool_txs);

        let block = Block::new(
            next_height,
            self.latest_block()?.hash.clone(),
            transactions,
            validator_address.to_string(),
        );

        Ok(block)
    }

    // L-04: Memory estimate for chain in RAM
    pub fn get_memory_estimate(&self) -> String {
        let block_count = self.chain.len();
        let estimate_mb = (block_count * 2) / 1024; // ~2KB per block estimate
        format!("~{}MB for {} blocks", estimate_mb, block_count)
    }

    // ── Block application (two-pass atomic) ─────────────
    pub fn add_block(&mut self, block: Block) -> SentrixResult<()> {
        // L-04 FIX: warn on large chain
        if !self.chain.is_empty() && self.chain.len().is_multiple_of(10_000) {
            tracing::warn!(
                "chain length is {} blocks ({}) — consider archival strategy",
                self.chain.len(), self.get_memory_estimate()
            );
        }

        let expected_index = self.height() + 1;
        let expected_prev = self.latest_block()?.hash.clone();

        // ── Pass 1: dry-run validation ───────────────────
        block.validate_structure(expected_index, &expected_prev)?;

        // C-02 FIX: Verify validator authorization for this block height
        if !self.authority.is_authorized(&block.validator, expected_index)? {
            return Err(SentrixError::UnauthorizedValidator(
                format!("validator {} not authorized for block {}", block.validator, expected_index)
            ));
        }

        // H-06 FIX: Validate block timestamp
        let prev_timestamp = self.latest_block()?.timestamp;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if block.timestamp < prev_timestamp {
            return Err(SentrixError::InvalidBlock(
                "block timestamp is before previous block".to_string()
            ));
        }
        if block.timestamp > now + 15 {
            return Err(SentrixError::InvalidBlock(
                "block timestamp too far in the future".to_string()
            ));
        }

        // Validate coinbase amount
        let reward = self.get_block_reward();
        let coinbase = block.coinbase()
            .ok_or_else(|| SentrixError::InvalidBlock("missing coinbase".to_string()))?;
        if coinbase.amount > reward {
            return Err(SentrixError::InvalidBlock(
                format!("coinbase {} exceeds reward {}", coinbase.amount, reward)
            ));
        }

        // Validate all non-coinbase transactions on working state copy
        let mut working_balances: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        let mut working_nonces: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

        for tx in block.transactions.iter().skip(1) {
            // Get working balance (fall back to real balance)
            let balance = working_balances
                .get(&tx.from_address)
                .copied()
                .unwrap_or_else(|| self.accounts.get_balance(&tx.from_address));

            // Get working nonce
            let nonce = working_nonces
                .get(&tx.from_address)
                .copied()
                .unwrap_or_else(|| self.accounts.get_nonce(&tx.from_address));

            // Validate
            tx.validate(nonce, self.chain_id)?;

            // H-02 FIX: checked addition to prevent overflow
            let needed = tx.amount.checked_add(tx.fee)
                .ok_or_else(|| SentrixError::InvalidTransaction(
                    "amount + fee overflow".to_string()
                ))?;
            if balance < needed {
                return Err(SentrixError::InsufficientBalance {
                    have: balance,
                    need: needed,
                });
            }

            // Validate token operation if present
            if let Some(token_op) = TokenOp::decode(&tx.data) {
                match &token_op {
                    TokenOp::Transfer { contract, amount, .. } => {
                        if !self.contracts.exists(contract) {
                            return Err(SentrixError::InvalidTransaction(
                                format!("token contract {} not found", contract)
                            ));
                        }
                        let token_bal = self.contracts.get_token_balance(contract, &tx.from_address);
                        if token_bal < *amount {
                            return Err(SentrixError::InsufficientBalance { have: token_bal, need: *amount });
                        }
                    }
                    TokenOp::Burn { contract, amount } => {
                        if !self.contracts.exists(contract) {
                            return Err(SentrixError::InvalidTransaction(
                                format!("token contract {} not found", contract)
                            ));
                        }
                        let token_bal = self.contracts.get_token_balance(contract, &tx.from_address);
                        if token_bal < *amount {
                            return Err(SentrixError::InsufficientBalance { have: token_bal, need: *amount });
                        }
                    }
                    TokenOp::Mint { contract, .. } => {
                        if !self.contracts.exists(contract) {
                            return Err(SentrixError::InvalidTransaction(
                                format!("token contract {} not found", contract)
                            ));
                        }
                    }
                    TokenOp::Approve { contract, .. } => {
                        if !self.contracts.exists(contract) {
                            return Err(SentrixError::InvalidTransaction(
                                format!("token contract {} not found", contract)
                            ));
                        }
                    }
                    TokenOp::Deploy { .. } => {} // deploy creates new contract, no pre-check needed
                }
            }

            // Update working state
            *working_balances.entry(tx.from_address.clone()).or_insert(balance) -= needed;
            *working_nonces.entry(tx.from_address.clone()).or_insert(nonce) += 1;
        }

        // ── Pass 2: commit ───────────────────────────────
        // Apply coinbase reward
        self.accounts.credit(&block.validator, coinbase.amount)?;
        self.total_minted += coinbase.amount;

        // Apply all transactions
        let mut total_fee: u64 = 0;
        for tx in block.transactions.iter().skip(1) {
            self.accounts.transfer(
                &tx.from_address,
                &tx.to_address,
                tx.amount,
                tx.fee,
            )?;
            total_fee += tx.fee;

            // Execute token operation if present in data field
            if let Some(token_op) = TokenOp::decode(&tx.data) {
                match token_op {
                    TokenOp::Deploy { name, symbol, decimals, supply } => {
                        self.contracts.deploy(&tx.from_address, &name, &symbol, decimals, supply)?;
                    }
                    TokenOp::Transfer { contract, to, amount } => {
                        self.contracts.execute_transfer(&contract, &tx.from_address, &to, amount)?;
                    }
                    TokenOp::Burn { contract, amount } => {
                        self.contracts.execute_burn(&contract, &tx.from_address, amount)?;
                    }
                    TokenOp::Mint { contract, to, amount } => {
                        self.contracts.execute_mint(&contract, &tx.from_address, &to, amount)?;
                    }
                    TokenOp::Approve { contract, spender, amount } => {
                        self.contracts.execute_approve(&contract, &tx.from_address, &spender, amount)?;
                    }
                }
            }
        }

        // Validator gets 50% of fees (other 50% already burned in transfer)
        let validator_fee_share = total_fee / 2;
        if validator_fee_share > 0 {
            self.accounts.credit(&block.validator, validator_fee_share)?;
        }

        // Record validator stats
        self.authority.record_block_produced(&block.validator, block.timestamp);

        // Remove mined transactions from mempool
        let mined_txids: std::collections::HashSet<String> = block.transactions
            .iter()
            .map(|tx| tx.txid.clone())
            .collect();
        self.mempool.retain(|tx| !mined_txids.contains(&tx.txid));

        // Append block to chain
        self.chain.push(block);

        Ok(())
    }

    // ── SRX-20 Token Operations ────────────────────────────

    pub fn deploy_token(
        &mut self,
        deployer: &str,
        name: String,
        symbol: String,
        decimals: u8,
        total_supply: u64,
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

            let burn_share = deploy_fee / 2;
            let eco_share = deploy_fee - burn_share;
            self.accounts.total_burned += burn_share;
            self.accounts.credit(ECOSYSTEM_FUND_ADDRESS, eco_share)?;
        }

        // Deploy contract
        let contract_address = self.contracts.deploy(deployer, &name, &symbol, decimals, total_supply)?;
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

            let burn_share = gas_fee / 2;
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

            let burn_share = gas_fee / 2;
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

    // ── Transaction queries ──────────────────────────────

    pub fn get_transaction(&self, txid: &str) -> Option<serde_json::Value> {
        for block in self.chain.iter().rev() {
            for tx in &block.transactions {
                if tx.txid == txid {
                    return Some(serde_json::json!({
                        "transaction": tx,
                        "block_index": block.index,
                        "block_hash": block.hash,
                        "block_timestamp": block.timestamp,
                    }));
                }
            }
        }
        None
    }

    // L-03 FIX: paginated address history (limit + offset)
    pub fn get_address_history(&self, address: &str, limit: usize, offset: usize) -> Vec<serde_json::Value> {
        let mut history = Vec::new();
        let mut skipped = 0usize;
        for block in &self.chain {
            for tx in &block.transactions {
                if tx.from_address == address || tx.to_address == address {
                    if skipped < offset {
                        skipped += 1;
                        continue;
                    }
                    if history.len() >= limit {
                        return history;
                    }
                    let direction = if tx.from_address == address {
                        if tx.is_coinbase() { "reward" } else { "out" }
                    } else {
                        "in"
                    };
                    history.push(serde_json::json!({
                        "txid": tx.txid,
                        "direction": direction,
                        "from": tx.from_address,
                        "to": tx.to_address,
                        "amount": tx.amount,
                        "fee": tx.fee,
                        "block_index": block.index,
                        "block_timestamp": block.timestamp,
                    }));
                }
            }
        }
        history
    }

    pub fn get_address_tx_count(&self, address: &str) -> usize {
        self.chain.iter()
            .flat_map(|b| &b.transactions)
            .filter(|tx| tx.from_address == address || tx.to_address == address)
            .count()
    }

    pub fn get_latest_transactions(&self, limit: usize, offset: usize) -> Vec<serde_json::Value> {
        let mut result = Vec::new();
        let mut skipped = 0usize;
        for block in self.chain.iter().rev() {
            for tx in block.transactions.iter().rev() {
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if result.len() >= limit {
                    return result;
                }
                result.push(serde_json::json!({
                    "txid": tx.txid,
                    "from": tx.from_address,
                    "to": tx.to_address,
                    "amount": tx.amount,
                    "fee": tx.fee,
                    "is_coinbase": tx.is_coinbase(),
                    "block_index": block.index,
                    "block_timestamp": block.timestamp,
                }));
            }
        }
        result
    }

    pub fn get_token_holders(&self, contract: &str) -> Option<Vec<serde_json::Value>> {
        self.contracts.get_holders_list(contract)
    }

    pub fn get_token_trades(&self, contract_addr: &str, limit: usize, offset: usize) -> Vec<serde_json::Value> {
        let mut result = Vec::new();
        let mut skipped = 0usize;
        for block in self.chain.iter().rev() {
            for tx in block.transactions.iter() {
                let entry = match TokenOp::decode(&tx.data) {
                    Some(TokenOp::Transfer { contract, to, amount }) if contract == contract_addr => {
                        Some(serde_json::json!({
                            "type": "transfer",
                            "from": tx.from_address,
                            "to": to,
                            "amount": amount,
                            "txid": tx.txid,
                            "block_index": block.index,
                            "block_timestamp": block.timestamp,
                        }))
                    }
                    Some(TokenOp::Burn { contract, amount }) if contract == contract_addr => {
                        Some(serde_json::json!({
                            "type": "burn",
                            "from": tx.from_address,
                            "to": serde_json::Value::Null,
                            "amount": amount,
                            "txid": tx.txid,
                            "block_index": block.index,
                            "block_timestamp": block.timestamp,
                        }))
                    }
                    Some(TokenOp::Mint { contract, to, amount }) if contract == contract_addr => {
                        Some(serde_json::json!({
                            "type": "mint",
                            "from": tx.from_address,
                            "to": to,
                            "amount": amount,
                            "txid": tx.txid,
                            "block_index": block.index,
                            "block_timestamp": block.timestamp,
                        }))
                    }
                    _ => None,
                };
                if let Some(e) = entry {
                    if skipped < offset {
                        skipped += 1;
                    } else {
                        result.push(e);
                        if result.len() >= limit {
                            return result;
                        }
                    }
                }
            }
        }
        result
    }

    // ── Stats ────────────────────────────────────────────
    pub fn chain_stats(&self) -> serde_json::Value {
        serde_json::json!({
            "height": self.height(),
            "total_blocks": self.chain.len(),
            "total_minted_srx": self.total_minted as f64 / 100_000_000.0,
            "max_supply_srx": MAX_SUPPLY as f64 / 100_000_000.0,
            "total_burned_srx": self.accounts.total_burned as f64 / 100_000_000.0,
            "mempool_size": self.mempool.len(),
            "active_validators": self.authority.active_count(),
            "deployed_tokens": self.contracts.contract_count(),
            "chain_id": self.chain_id,
            "next_block_reward_srx": self.get_block_reward() as f64 / 100_000_000.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::{Secp256k1, SecretKey, PublicKey};
    use secp256k1::rand::rngs::OsRng;
    use crate::core::transaction::{Transaction, MIN_TX_FEE};

    fn make_keypair() -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        secp.generate_keypair(&mut OsRng)
    }

    fn derive_addr(pk: &PublicKey) -> String {
        crate::wallet::wallet::Wallet::derive_address(pk)
    }

    fn setup_chain() -> Blockchain {
        let mut bc = Blockchain::new("admin".to_string());
        bc.authority.add_validator(
            "admin",
            "validator1".to_string(),
            "Validator 1".to_string(),
            "pk1".to_string(),
        ).unwrap();
        bc
    }

    #[test]
    fn test_genesis_initialized() {
        let bc = setup_chain();
        assert_eq!(bc.height(), 0);
        assert_eq!(bc.total_minted, TOTAL_PREMINE);
        assert!(bc.is_valid_chain());
    }

    #[test]
    fn test_block_reward_era0() {
        let bc = setup_chain();
        assert_eq!(bc.get_block_reward(), BLOCK_REWARD);
    }

    #[test]
    fn test_create_and_add_block() {
        let mut bc = setup_chain();
        let block = bc.create_block("validator1").unwrap();
        assert_eq!(block.index, 1);
        bc.add_block(block).unwrap();
        assert_eq!(bc.height(), 1);
        assert!(bc.is_valid_chain());
    }

    #[test]
    fn test_unauthorized_validator_rejected() {
        let mut bc = setup_chain();
        let result = bc.create_block("not_a_validator");
        assert!(result.is_err());
    }

    #[test]
    fn test_mempool_and_block_inclusion() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);

        // Fund the real derived address
        bc.accounts.credit(&sender, 10_000_000).unwrap();

        let tx = Transaction::new(
            sender,
            "receiver".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        ).unwrap();

        bc.add_to_mempool(tx).unwrap();
        assert_eq!(bc.mempool_size(), 1);

        let block = bc.create_block("validator1").unwrap();
        assert_eq!(block.tx_count(), 2); // coinbase + 1 tx
        bc.add_block(block).unwrap();
        assert_eq!(bc.mempool_size(), 0);
    }

    #[test]
    fn test_chain_tamper_detected() {
        let mut bc = setup_chain();
        bc.create_block("validator1").map(|b| bc.add_block(b)).unwrap().unwrap();

        // Tamper with txid — breaks merkle root integrity
        bc.chain[1].transactions[0].txid = "tampered".to_string();
        assert!(!bc.is_valid_chain());
    }

    #[test]
    fn test_validator_earns_reward() {
        let mut bc = setup_chain();
        let balance_before = bc.accounts.get_balance("validator1");

        let block = bc.create_block("validator1").unwrap();
        bc.add_block(block).unwrap();

        let balance_after = bc.accounts.get_balance("validator1");
        assert!(balance_after > balance_before);
        assert_eq!(balance_after - balance_before, BLOCK_REWARD);
    }

    #[test]
    fn test_supply_cap_tracked() {
        let mut bc = setup_chain();
        let block = bc.create_block("validator1").unwrap();
        bc.add_block(block).unwrap();
        assert_eq!(bc.total_minted, TOTAL_PREMINE + BLOCK_REWARD);
    }

    // ── SRX-20 Token Tests ──────────────────────────────

    #[test]
    fn test_deploy_token() {
        let mut bc = setup_chain();
        // Fund deployer
        bc.accounts.credit("deployer", 1_000_000).unwrap();

        let addr = bc.deploy_token(
            "deployer", "TestToken".to_string(), "TT".to_string(),
            18, 1_000_000, 100_000,
        ).unwrap();

        assert!(addr.starts_with("SRX20_"));
        assert_eq!(bc.token_balance(&addr, "deployer"), 1_000_000);
        assert_eq!(bc.list_tokens().len(), 1);
        // Fee deducted: 100k fee, deployer had 1M
        assert_eq!(bc.accounts.get_balance("deployer"), 900_000);
    }

    #[test]
    fn test_deploy_token_insufficient_fee() {
        let mut bc = setup_chain();
        bc.accounts.credit("deployer", 100).unwrap(); // not enough for fee
        let result = bc.deploy_token(
            "deployer", "Token".to_string(), "TK".to_string(),
            18, 1_000, 1_000,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_token_transfer() {
        let mut bc = setup_chain();
        bc.accounts.credit("alice", 1_000_000).unwrap();

        let addr = bc.deploy_token(
            "alice", "Coin".to_string(), "CN".to_string(),
            18, 500_000, 10_000,
        ).unwrap();

        bc.token_transfer(&addr, "alice", "bob", 100_000, 1_000).unwrap();
        assert_eq!(bc.token_balance(&addr, "alice"), 400_000);
        assert_eq!(bc.token_balance(&addr, "bob"), 100_000);
    }

    #[test]
    fn test_token_transfer_gas_burned() {
        let mut bc = setup_chain();
        bc.accounts.credit("alice", 1_000_000).unwrap();

        let addr = bc.deploy_token(
            "alice", "Coin".to_string(), "CN".to_string(),
            18, 500_000, 0,
        ).unwrap();

        let burned_before = bc.accounts.total_burned;
        bc.token_transfer(&addr, "alice", "bob", 100, 10_000).unwrap();
        // 50% of 10_000 gas = 5_000 burned
        assert_eq!(bc.accounts.total_burned - burned_before, 5_000);
    }

    #[test]
    fn test_token_info() {
        let mut bc = setup_chain();
        bc.accounts.credit("deployer", 1_000_000).unwrap();

        let addr = bc.deploy_token(
            "deployer", "MyToken".to_string(), "MT".to_string(),
            8, 21_000_000, 0,
        ).unwrap();

        let info = bc.token_info(&addr).unwrap();
        assert_eq!(info["symbol"], "MT");
        assert_eq!(info["name"], "MyToken");
        assert_eq!(info["total_supply"], 21_000_000);
        assert_eq!(info["decimals"], 8);
    }

    #[test]
    fn test_chain_stats_includes_tokens() {
        let mut bc = setup_chain();
        bc.accounts.credit("d", 1_000_000).unwrap();
        bc.deploy_token("d", "A".to_string(), "A".to_string(), 18, 100, 0).unwrap();
        bc.deploy_token("d", "B".to_string(), "B".to_string(), 18, 200, 0).unwrap();
        let stats = bc.chain_stats();
        assert_eq!(stats["deployed_tokens"], 2);
    }

    #[test]
    fn test_mempool_priority_fee_ordering() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);

        bc.accounts.credit(&sender, 100_000_000).unwrap();

        // Add 3 txs with different fees: low, high, medium
        let tx_low = Transaction::new(
            sender.clone(), "recv".to_string(),
            100_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        let tx_high = Transaction::new(
            sender.clone(), "recv".to_string(),
            100_000, MIN_TX_FEE * 100, 1, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        let tx_mid = Transaction::new(
            sender.clone(), "recv".to_string(),
            100_000, MIN_TX_FEE * 10, 2, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();

        bc.add_to_mempool(tx_low).unwrap();
        bc.add_to_mempool(tx_high).unwrap();
        bc.add_to_mempool(tx_mid).unwrap();

        // Mempool should be ordered: high, mid, low
        let fees: Vec<u64> = bc.mempool.iter().map(|tx| tx.fee).collect();
        assert_eq!(fees, vec![MIN_TX_FEE * 100, MIN_TX_FEE * 10, MIN_TX_FEE]);
    }

    #[test]
    fn test_c02_add_block_rejects_unauthorized_validator() {
        let mut bc = setup_chain();
        // Add a second validator
        bc.authority.add_validator(
            "admin", "validator2".to_string(), "Validator 2".to_string(), "pk2".to_string(),
        ).unwrap();

        // Determine who is authorized for block 1
        let expected = bc.authority.expected_validator(1).unwrap().address.clone();
        let unauthorized = if expected == "validator1" { "validator2" } else { "validator1" };

        // Create a valid block with the authorized validator
        let block = bc.create_block(&expected).unwrap();
        // Tamper the validator field to the unauthorized validator
        let mut tampered_block = block.clone();
        tampered_block.validator = unauthorized.to_string();
        // Recalculate hash so structure validation passes
        tampered_block.hash = tampered_block.calculate_hash();

        // Should be rejected because the other validator is not authorized for this height
        let result = bc.add_block(tampered_block);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("not authorized"), "Expected 'not authorized' error, got: {}", err_str);
    }

    #[test]
    fn test_h02_mempool_rejects_overflow_amount_fee() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);

        bc.accounts.credit(&sender, 100_000_000).unwrap();

        // Create tx with amount = u64::MAX and fee = 1 — would overflow
        let tx = Transaction::new(
            sender, "recv".to_string(),
            u64::MAX, 1, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();

        let result = bc.add_to_mempool(tx);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("overflow") || err_str.contains("fee"),
            "Expected overflow error, got: {}", err_str
        );
    }

    #[test]
    fn test_h06_add_block_rejects_past_timestamp() {
        let mut bc = setup_chain();

        // Create a valid block
        let mut block = bc.create_block("validator1").unwrap();
        // Set timestamp to before genesis block
        block.timestamp = 0;
        block.hash = block.calculate_hash();

        let result = bc.add_block(block);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("timestamp"), "Expected timestamp error, got: {}", err_str);
    }

    #[test]
    fn test_h06_add_block_rejects_future_timestamp() {
        let mut bc = setup_chain();

        let mut block = bc.create_block("validator1").unwrap();
        // Set timestamp far in the future (1 hour from now)
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() + 3600;
        block.timestamp = future;
        block.hash = block.calculate_hash();

        let result = bc.add_block(block);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("future"), "Expected future timestamp error, got: {}", err_str);
    }

    #[test]
    fn test_l02_latest_block_on_empty_chain_returns_err() {
        let mut bc = Blockchain::new("admin".to_string());
        bc.chain.clear();
        let result = bc.latest_block();
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("empty"), "Expected 'empty' error, got: {}", err_str);
    }

    #[test]
    fn test_l03_address_history_pagination() {
        let mut bc = setup_chain();

        // Produce 5 blocks so validator1 has 5 coinbase rewards
        for _ in 0..5 {
            let block = bc.create_block("validator1").unwrap();
            bc.add_block(block).unwrap();
        }

        // Full history: validator1 has 5 reward txs
        let all = bc.get_address_history("validator1", 100, 0);
        assert_eq!(all.len(), 5);

        // Limit=2, offset=0: first 2
        let page1 = bc.get_address_history("validator1", 2, 0);
        assert_eq!(page1.len(), 2);

        // Limit=2, offset=2: next 2
        let page2 = bc.get_address_history("validator1", 2, 2);
        assert_eq!(page2.len(), 2);

        // Limit=2, offset=4: last 1
        let page3 = bc.get_address_history("validator1", 2, 4);
        assert_eq!(page3.len(), 1);

        // Offset past end: empty
        let empty = bc.get_address_history("validator1", 2, 100);
        assert_eq!(empty.len(), 0);
    }

    // ── CRIT-01 FIX: On-chain token operation tests ─────

    #[test]
    fn test_onchain_token_deploy_via_block() {
        use crate::core::transaction::{TokenOp, TOKEN_OP_ADDRESS};

        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let deployer = derive_addr(&pk);
        bc.accounts.credit(&deployer, 10_000_000).unwrap();

        // Create token deploy transaction
        let token_op = TokenOp::Deploy {
            name: "TestToken".to_string(),
            symbol: "TT".to_string(),
            decimals: 8,
            supply: 1_000_000,
        };

        let tx = Transaction::new(
            deployer.clone(), TOKEN_OP_ADDRESS.to_string(),
            0, MIN_TX_FEE, 0, token_op.encode().unwrap(),
            CHAIN_ID, &sk, &pk,
        ).unwrap();
        bc.add_to_mempool(tx).unwrap();

        // Mine block
        let block = bc.create_block("validator1").unwrap();
        assert_eq!(block.tx_count(), 2); // coinbase + token deploy
        bc.add_block(block).unwrap();

        // Token should now be deployed
        assert_eq!(bc.contracts.contract_count(), 1);
        let tokens = bc.list_tokens();
        assert_eq!(tokens[0]["symbol"], "TT");
        assert_eq!(tokens[0]["total_supply"], 1_000_000);
    }

    #[test]
    fn test_onchain_token_transfer_via_block() {
        use crate::core::transaction::{TokenOp, TOKEN_OP_ADDRESS};

        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let alice = derive_addr(&pk);
        bc.accounts.credit(&alice, 10_000_000).unwrap();

        // Deploy token first (old method for setup)
        let contract = bc.deploy_token(
            &alice, "Coin".to_string(), "CN".to_string(), 8, 500_000, 0,
        ).unwrap();

        // Create transfer transaction
        let token_op = TokenOp::Transfer {
            contract: contract.clone(),
            to: "bob".to_string(),
            amount: 100_000,
        };
        let tx = Transaction::new(
            alice.clone(), TOKEN_OP_ADDRESS.to_string(),
            0, MIN_TX_FEE, 0, token_op.encode().unwrap(),
            CHAIN_ID, &sk, &pk,
        ).unwrap();
        bc.add_to_mempool(tx).unwrap();

        // Mine block
        let block = bc.create_block("validator1").unwrap();
        bc.add_block(block).unwrap();

        // Verify token balances
        assert_eq!(bc.token_balance(&contract, &alice), 400_000);
        assert_eq!(bc.token_balance(&contract, "bob"), 100_000);
    }

    #[test]
    fn test_onchain_token_op_recorded_in_block() {
        use crate::core::transaction::{TokenOp, TOKEN_OP_ADDRESS};

        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let deployer = derive_addr(&pk);
        bc.accounts.credit(&deployer, 10_000_000).unwrap();

        let token_op = TokenOp::Deploy {
            name: "OnChain".to_string(),
            symbol: "OC".to_string(),
            decimals: 8,
            supply: 999,
        };
        let tx = Transaction::new(
            deployer.clone(), TOKEN_OP_ADDRESS.to_string(),
            0, MIN_TX_FEE, 0, token_op.encode().unwrap(),
            CHAIN_ID, &sk, &pk,
        ).unwrap();
        let txid = tx.txid.clone();
        bc.add_to_mempool(tx).unwrap();

        let block = bc.create_block("validator1").unwrap();
        bc.add_block(block).unwrap();

        // Transaction should be findable in chain
        let found = bc.get_transaction(&txid);
        assert!(found.is_some());
        // Data field should contain the token op
        let tx_data = found.unwrap();
        let block_idx = tx_data["block_index"].as_u64().unwrap();
        assert_eq!(block_idx, 1);
    }

    #[test]
    fn test_onchain_token_transfer_insufficient_rejected() {
        use crate::core::transaction::{TokenOp, TOKEN_OP_ADDRESS};

        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let alice = derive_addr(&pk);
        bc.accounts.credit(&alice, 10_000_000).unwrap();

        let contract = bc.deploy_token(
            &alice, "Coin".to_string(), "CN".to_string(), 8, 100, 0,
        ).unwrap();

        // Try to transfer more than token balance
        let token_op = TokenOp::Transfer {
            contract: contract.clone(),
            to: "bob".to_string(),
            amount: 999, // alice only has 100
        };
        let tx = Transaction::new(
            alice.clone(), TOKEN_OP_ADDRESS.to_string(),
            0, MIN_TX_FEE, 0, token_op.encode().unwrap(),
            CHAIN_ID, &sk, &pk,
        ).unwrap();

        // Should be rejected at mempool (or add_block validation)
        // add_to_mempool doesn't validate token ops, but add_block Pass 1 does
        bc.add_to_mempool(tx).unwrap();
        let block = bc.create_block("validator1").unwrap();
        let result = bc.add_block(block);
        assert!(result.is_err());
    }
}
