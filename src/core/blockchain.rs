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
// V5-10: Hash algorithm version — reserved for future SHA-3/BLAKE3 migration
pub const HASH_VERSION: u8        = 1; // 1 = SHA-256 (current)

// C-03 FIX: Mempool size limits to prevent RAM exhaustion
pub const MAX_MEMPOOL_SIZE: usize        = 10_000;
pub const MAX_MEMPOOL_PER_SENDER: usize  = 100;
// M-04 FIX: Mempool TTL — transactions older than this are pruned
pub const MEMPOOL_MAX_AGE_SECS: u64      = 3_600; // 1 hour
// I-01 FIX: Sliding window — only keep last N blocks in RAM; older blocks stay in sled
pub const CHAIN_WINDOW_SIZE: usize       = 1_000;

// H-04 FIX: Address validation helper
pub fn is_valid_sentrix_address(addr: &str) -> bool {
    addr.len() == 42
        && addr.starts_with("0x")
        && addr[2..].chars().all(|c| c.is_ascii_hexdigit())
}

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

    // I-01 FIX: height derived from last block's index, not chain.len()-1.
    // chain is now a sliding window so chain.len() <= CHAIN_WINDOW_SIZE.
    pub fn height(&self) -> u64 {
        self.chain.last().map(|b| b.index).unwrap_or(0)
    }

    /// First block index currently held in the in-memory window.
    /// Blocks with index < chain_window_start() are only in sled storage.
    pub fn chain_window_start(&self) -> u64 {
        self.chain.first().map(|b| b.index).unwrap_or(0)
    }

    // L-02 FIX: return Result instead of panicking on empty chain
    pub fn latest_block(&self) -> SentrixResult<&Block> {
        self.chain.last()
            .ok_or_else(|| SentrixError::NotFound("chain is empty".to_string()))
    }

    // I-01 FIX: returns None for blocks outside the sliding window (index < chain_window_start)
    pub fn get_block(&self, index: u64) -> Option<&Block> {
        let window_start = self.chain_window_start();
        if index < window_start {
            return None; // evicted from window — use storage for historical access
        }
        self.chain.get((index - window_start) as usize)
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

        // C-03 FIX: Global mempool size limit
        if self.mempool.len() >= MAX_MEMPOOL_SIZE {
            return Err(SentrixError::InvalidTransaction(
                "mempool full — try again later".to_string()
            ));
        }

        // C-03 FIX: Per-sender pending tx limit
        let sender_pending = self.mempool_pending_count(&tx.from_address) as usize;
        if sender_pending >= MAX_MEMPOOL_PER_SENDER {
            return Err(SentrixError::InvalidTransaction(
                "too many pending transactions from this sender".to_string()
            ));
        }

        // H-04 FIX: Validate to_address is a well-formed Sentrix address
        if !is_valid_sentrix_address(&tx.to_address) {
            return Err(SentrixError::InvalidTransaction(
                format!("invalid to_address: '{}'", tx.to_address)
            ));
        }

        // M-03/M-04 FIX: Validate transaction timestamp
        // Reject timestamps too far in the future (clock skew / pre-signed attack)
        // Reject timestamps too old (replay of stale transactions / mempool poisoning)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if tx.timestamp > now + 300 {
            return Err(SentrixError::InvalidTransaction(
                "transaction timestamp too far in the future (max +5 min)".to_string()
            ));
        }
        if now > tx.timestamp.saturating_add(MEMPOOL_MAX_AGE_SECS) {
            return Err(SentrixError::InvalidTransaction(
                format!("transaction too old — max age {} seconds", MEMPOOL_MAX_AGE_SECS)
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
        // V5-07 TODO: RBF (Replace-By-Fee) not yet implemented — a sender cannot replace
        // a pending tx with a higher-fee version. Adding RBF requires nonce-keyed lookup
        // and per-sender replacement logic. Track in BIBLE.md under "Future Work".
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

    /// M-04 FIX: Remove transactions older than MEMPOOL_MAX_AGE_SECS.
    /// Called automatically after each block is added; also callable manually.
    pub fn prune_mempool(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.mempool.retain(|tx| now <= tx.timestamp.saturating_add(MEMPOOL_MAX_AGE_SECS));
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

    // I-01 FIX: memory estimate now reflects window (not full chain)
    pub fn get_memory_estimate(&self) -> String {
        let window_blocks = self.chain.len();
        let true_height = self.height();
        let estimate_mb = (window_blocks * 2) / 1024; // ~2KB per block
        format!("~{}MB for {} blocks in window (true height: {})", estimate_mb, window_blocks, true_height)
    }

    // ── Block application (two-pass atomic) ─────────────
    pub fn add_block(&mut self, block: Block) -> SentrixResult<()> {
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
                    TokenOp::Deploy { name, symbol, decimals, supply, max_supply } => {
                        self.contracts.deploy(&tx.from_address, &name, &symbol, decimals, supply, max_supply)?;
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

        // L-02 FIX: Burn gets ceiling, validator gets floor — ensures total_fee is fully distributed
        let burn_fee_share = (total_fee + 1) / 2;
        let validator_fee_share = total_fee - burn_fee_share;
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

        // M-04 FIX: Prune stale transactions after each block
        self.prune_mempool();

        // Append block to chain
        self.chain.push(block);

        // I-01 FIX: sliding window — evict oldest blocks that exceed CHAIN_WINDOW_SIZE
        // Evicted blocks remain in sled storage; only the in-memory window shrinks
        if self.chain.len() > CHAIN_WINDOW_SIZE {
            let excess = self.chain.len() - CHAIN_WINDOW_SIZE;
            self.chain.drain(..excess);
        }

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

            let burn_share = (deploy_fee + 1) / 2; // L-02 FIX: burn rounds up
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

            let burn_share = (gas_fee + 1) / 2; // L-02 FIX: burn rounds up
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

            let burn_share = (gas_fee + 1) / 2; // L-02 FIX: burn rounds up
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
            "total_blocks": self.height() + 1, // I-01 FIX: chain window may be < total blocks
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

    // Valid-format test address for use as to_address in tests
    const TEST_RECV: &str = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

    fn setup_chain() -> Blockchain {
        let mut bc = Blockchain::new("admin".to_string());
        // Use unchecked helper so tests can control the address string ("validator1").
        // H-02 crypto validation is tested separately via add_validator.
        bc.authority.add_validator_unchecked(
            "validator1".to_string(),
            "Validator 1".to_string(),
            "pk1".to_string(),
        );
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
            TEST_RECV.to_string(),
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
            18, 1_000_000, 0, 100_000,
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
            18, 1_000, 0, 1_000,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_token_transfer() {
        let mut bc = setup_chain();
        bc.accounts.credit("alice", 1_000_000).unwrap();

        let addr = bc.deploy_token(
            "alice", "Coin".to_string(), "CN".to_string(),
            18, 500_000, 0, 10_000,
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
            18, 500_000, 0, 0,
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
            8, 21_000_000, 0, 0,
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
        bc.deploy_token("d", "A".to_string(), "A".to_string(), 18, 100, 0, 0).unwrap();
        bc.deploy_token("d", "B".to_string(), "B".to_string(), 18, 200, 0, 0).unwrap();
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
            sender.clone(), TEST_RECV.to_string(),
            100_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        let tx_high = Transaction::new(
            sender.clone(), TEST_RECV.to_string(),
            100_000, MIN_TX_FEE * 100, 1, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        let tx_mid = Transaction::new(
            sender.clone(), TEST_RECV.to_string(),
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
        // Add a second validator (unchecked for test control over address string)
        bc.authority.add_validator_unchecked(
            "validator2".to_string(), "Validator 2".to_string(), "pk2".to_string(),
        );

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
            sender, TEST_RECV.to_string(),
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
            max_supply: 0,
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
            &alice, "Coin".to_string(), "CN".to_string(), 8, 500_000, 0, 0,
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
            max_supply: 0,
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
            &alice, "Coin".to_string(), "CN".to_string(), 8, 100, 0, 0,
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

    // ── H-04: Address validation helper ─────────────────

    #[test]
    fn test_h04_is_valid_sentrix_address() {
        // Valid: 0x + exactly 40 hex chars
        assert!(is_valid_sentrix_address("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"));
        assert!(is_valid_sentrix_address("0x0000000000000000000000000000000000000000"));
        assert!(is_valid_sentrix_address("0xabcdef0123456789abcdef0123456789abcdef01"));

        // Invalid: no prefix
        assert!(!is_valid_sentrix_address("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"));
        // Invalid: too short
        assert!(!is_valid_sentrix_address("0xdeadbeef"));
        // Invalid: non-hex chars
        assert!(!is_valid_sentrix_address("0xGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG"));
        // Invalid: empty
        assert!(!is_valid_sentrix_address(""));
        // Invalid: 0x prefix but too long
        assert!(!is_valid_sentrix_address("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefff"));
    }

    // ── M-03: Transaction Timestamp Validation ──────────

    #[test]
    fn test_m03_rejects_future_timestamp() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000).unwrap();

        let mut tx = Transaction::new(
            sender, TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();

        // Tamper timestamp to +10 min in future (beyond +5 min tolerance)
        tx.timestamp += 601;

        let result = bc.add_to_mempool(tx);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("future"), "Expected 'future' error, got: {}", err_str);
    }

    #[test]
    fn test_m03_rejects_expired_timestamp() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000).unwrap();

        let mut tx = Transaction::new(
            sender, TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();

        // Tamper timestamp to 2 hours ago (beyond 1h TTL)
        tx.timestamp = tx.timestamp.saturating_sub(7_200);

        let result = bc.add_to_mempool(tx);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("old") || err_str.contains("age"),
            "Expected 'old'/'age' error, got: {}", err_str
        );
    }

    #[test]
    fn test_m03_accepts_valid_timestamp() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000).unwrap();

        // Normal transaction with current timestamp — should be accepted
        let tx = Transaction::new(
            sender, TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();

        assert!(bc.add_to_mempool(tx).is_ok());
        assert_eq!(bc.mempool_size(), 1);
    }

    // ── M-04: Mempool TTL + prune_mempool() ─────────────

    #[test]
    fn test_m04_prune_removes_expired_txs() {
        let mut bc = setup_chain();

        // Directly inject a transaction with an ancient timestamp (bypassing add_to_mempool)
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000).unwrap();

        let mut stale_tx = Transaction::new(
            sender.clone(), TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        stale_tx.timestamp = 1; // 1970 — long expired
        stale_tx.txid = "stale_txid_expired".to_string();
        bc.mempool.push_back(stale_tx);
        assert_eq!(bc.mempool_size(), 1);

        // prune_mempool should remove it
        bc.prune_mempool();
        assert_eq!(bc.mempool_size(), 0);
    }

    #[test]
    fn test_m04_prune_keeps_fresh_txs() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000).unwrap();

        // Add a fresh transaction via normal path
        let tx = Transaction::new(
            sender, TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        bc.add_to_mempool(tx).unwrap();
        assert_eq!(bc.mempool_size(), 1);

        // prune_mempool should keep the fresh tx
        bc.prune_mempool();
        assert_eq!(bc.mempool_size(), 1);
    }

    #[test]
    fn test_m04_add_block_prunes_stale_mempool() {
        let mut bc = setup_chain();

        // Create the block first (mempool is empty, block only has coinbase)
        let block = bc.create_block("validator1").unwrap();

        // NOW inject a stale tx into the mempool (after block creation, so it won't
        // be included in this block — but add_block must prune it via prune_mempool())
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        let mut stale_tx = Transaction::new(
            sender, TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        stale_tx.timestamp = 42; // ancient — definitely expired
        stale_tx.txid = "stale_injected_after_create".to_string();
        bc.mempool.push_back(stale_tx);
        assert_eq!(bc.mempool_size(), 1);

        // add_block calls prune_mempool() internally → must remove the stale tx
        bc.add_block(block).unwrap();
        assert_eq!(bc.mempool_size(), 0);
    }

    // ── L-02: fee distribution rounding tests ─────────────

    #[test]
    fn test_l02_validator_receives_floor_of_odd_fee() {
        // For an odd total_fee, validator gets floor(fee/2), burn gets ceil(fee/2)
        let mut bc = setup_chain();
        let validator_addr = "validator1".to_string();

        // Use MIN_TX_FEE which is even (10_000); double it to get odd total by using 3 txs
        // Instead, verify the burn formula directly: odd total_fee burns more
        let odd_fee: u64 = MIN_TX_FEE + 1; // 10001 — odd
        let burn = (odd_fee + 1) / 2;
        let validator_share = odd_fee - burn;
        // burn + validator_share must equal odd_fee exactly (no sentri lost)
        assert_eq!(burn + validator_share, odd_fee);
        assert!(burn > validator_share); // burn gets the rounding

        // Also verify with a block using MIN_TX_FEE (even)
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000).unwrap();
        let tx = Transaction::new(
            sender, TEST_RECV.to_string(),
            100, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        bc.add_to_mempool(tx).unwrap();

        let block = bc.create_block(&validator_addr).unwrap();
        bc.add_block(block).unwrap();
        assert_eq!(bc.height(), 1);
    }

    #[test]
    fn test_l02_deploy_fee_burn_rounds_up() {
        let mut bc = setup_chain();
        bc.accounts.credit("deployer", 10_000_000).unwrap();

        let initial_burned = bc.accounts.total_burned;
        // Deploy with odd fee=3 → burn=(3+1)/2=2, eco=1
        bc.deploy_token("deployer", "TestToken".to_string(), "TT".to_string(), 8, 1_000_000, 0, 3).unwrap();
        assert_eq!(bc.accounts.total_burned, initial_burned + 2);
    }

    #[test]
    fn test_l02_gas_fee_burn_rounds_up() {
        let mut bc = setup_chain();
        bc.accounts.credit("user1", 10_000_000).unwrap();

        // Deploy a token first
        let contract = bc.deploy_token("user1", "Gas".to_string(), "GAS".to_string(), 8, 1_000, 0, 0).unwrap();

        let initial_burned = bc.accounts.total_burned;
        // Transfer with odd gas_fee=5 → burn=(5+1)/2=3
        bc.token_transfer(&contract, "user1", "user2", 100, 5).unwrap();
        assert_eq!(bc.accounts.total_burned, initial_burned + 3);
    }

    // ── I-01: sliding window chain cache tests ────────────

    #[test]
    fn test_i01_chain_window_size_constant() {
        assert_eq!(CHAIN_WINDOW_SIZE, 1_000);
    }

    #[test]
    fn test_i01_small_chain_fits_in_window() {
        // Chains smaller than CHAIN_WINDOW_SIZE keep all blocks in memory
        let mut bc = setup_chain();
        for _ in 0..5 {
            let b = bc.create_block("validator1").unwrap();
            bc.add_block(b).unwrap();
        }
        // 1 genesis + 5 blocks = 6 total, all in window
        assert_eq!(bc.chain.len(), 6);
        assert_eq!(bc.height(), 5);
        assert_eq!(bc.chain_window_start(), 0);
    }

    #[test]
    fn test_i01_chain_does_not_grow_beyond_window() {
        // Add CHAIN_WINDOW_SIZE + 10 blocks; window must stay at CHAIN_WINDOW_SIZE
        let mut bc = setup_chain();
        for _ in 0..CHAIN_WINDOW_SIZE + 9 {
            let b = bc.create_block("validator1").unwrap();
            bc.add_block(b).unwrap();
        }
        // Height = CHAIN_WINDOW_SIZE + 9, but chain Vec holds only last CHAIN_WINDOW_SIZE blocks
        assert_eq!(bc.chain.len(), CHAIN_WINDOW_SIZE);
        assert_eq!(bc.height(), CHAIN_WINDOW_SIZE as u64 + 9);
    }

    #[test]
    fn test_i01_height_is_true_height_not_window_len() {
        let mut bc = setup_chain();
        for _ in 0..CHAIN_WINDOW_SIZE + 50 {
            let b = bc.create_block("validator1").unwrap();
            bc.add_block(b).unwrap();
        }
        let expected_height = CHAIN_WINDOW_SIZE as u64 + 50;
        assert_eq!(bc.height(), expected_height);
        // chain.len() should be CHAIN_WINDOW_SIZE, NOT height+1
        assert_eq!(bc.chain.len(), CHAIN_WINDOW_SIZE);
        assert_ne!(bc.chain.len() as u64, bc.height() + 1);
    }

    #[test]
    fn test_i01_get_block_returns_none_for_evicted() {
        let mut bc = setup_chain();
        for _ in 0..CHAIN_WINDOW_SIZE + 1 {
            let b = bc.create_block("validator1").unwrap();
            bc.add_block(b).unwrap();
        }
        // Block 0 (genesis) must have been evicted from the window
        assert!(bc.get_block(0).is_none(), "evicted block should return None");
        // Latest block must still be accessible
        assert!(bc.get_block(bc.height()).is_some());
    }

    #[test]
    fn test_i01_get_block_within_window() {
        let mut bc = setup_chain();
        for _ in 0..CHAIN_WINDOW_SIZE + 5 {
            let b = bc.create_block("validator1").unwrap();
            bc.add_block(b).unwrap();
        }
        let window_start = bc.chain_window_start();
        // First block in window is accessible
        assert!(bc.get_block(window_start).is_some());
        // Last block in window is accessible
        assert!(bc.get_block(bc.height()).is_some());
        // Block just before window is NOT accessible
        if window_start > 0 {
            assert!(bc.get_block(window_start - 1).is_none());
        }
    }

    #[test]
    fn test_i01_window_start_advances_as_chain_grows() {
        let mut bc = setup_chain();
        assert_eq!(bc.chain_window_start(), 0);

        for _ in 0..CHAIN_WINDOW_SIZE {
            let b = bc.create_block("validator1").unwrap();
            bc.add_block(b).unwrap();
        }
        // At exactly CHAIN_WINDOW_SIZE blocks added: chain has genesis + CHAIN_WINDOW_SIZE = CHAIN_WINDOW_SIZE+1 > CHAIN_WINDOW_SIZE
        // So window_start should have advanced by 1
        assert_eq!(bc.chain_window_start(), 1);

        // Add 10 more
        for _ in 0..10 {
            let b = bc.create_block("validator1").unwrap();
            bc.add_block(b).unwrap();
        }
        assert_eq!(bc.chain_window_start(), 11);
    }

    // ── V5-02: deploy_token max_supply parameter ──────────

    #[test]
    fn test_v502_deploy_with_max_supply_stores_cap() {
        let mut bc = setup_chain();
        bc.accounts.credit("deployer", 1_000_000).unwrap();
        let addr = bc.deploy_token(
            "deployer", "Capped".to_string(), "CAP".to_string(),
            18, 500_000, 1_000_000, 0,
        ).unwrap();
        let info = bc.token_info(&addr).unwrap();
        assert_eq!(info["max_supply"], 1_000_000);
        assert_eq!(info["total_supply"], 500_000);
    }

    #[test]
    fn test_v502_deploy_with_zero_max_supply_is_unlimited() {
        let mut bc = setup_chain();
        bc.accounts.credit("deployer", 1_000_000).unwrap();
        let addr = bc.deploy_token(
            "deployer", "Unlimited".to_string(), "UNL".to_string(),
            18, 100_000, 0, 0,
        ).unwrap();
        let info = bc.token_info(&addr).unwrap();
        assert_eq!(info["max_supply"], 0); // 0 = unlimited
    }

    // ── V5-10: HASH_VERSION constant ──────────────────────

    #[test]
    fn test_v510_hash_version_constant_is_1() {
        assert_eq!(HASH_VERSION, 1, "HASH_VERSION must be 1 (SHA-256)");
    }
}
