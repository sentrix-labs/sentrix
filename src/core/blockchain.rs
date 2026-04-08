// blockchain.rs - Sentrix Chain

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use crate::core::block::Block;
use crate::core::transaction::Transaction;
use crate::core::account::AccountDB;
use crate::core::authority::AuthorityManager;
use crate::core::merkle::merkle_root;
use crate::types::error::{SentrixError, SentrixResult};

// ── Chain constants ──────────────────────────────────────
pub const MAX_SUPPLY: u64         = 210_000_000 * 100_000_000; // in sentri
pub const BLOCK_REWARD: u64       = 1 * 100_000_000;           // 1 SRX in sentri
pub const HALVING_INTERVAL: u64   = 42_000_000;                 // blocks
pub const BLOCK_TIME_SECS: u64    = 3;
pub const MAX_TX_PER_BLOCK: usize = 100;
pub const CHAIN_ID: u64           = 7119;

// ── Genesis addresses (placeholder — replace before mainnet) ──
pub const FOUNDER_ADDRESS:         &str = "0x89639929a133562d871dd47304ad3ff597908b79";
pub const ECOSYSTEM_FUND_ADDRESS:  &str = "0x840a2e3ef9433811fc345bcd90e3586a7fc56287";
pub const EARLY_VALIDATOR_ADDRESS: &str = "0xc556cb23a35ecdb6c22e94ec57789bdbf19da05e";
pub const RESERVE_ADDRESS:         &str = "0x5d1206cef9461da934eecc4f04743a46a62d3d40";

pub const GENESIS_ALLOCATIONS: &[(&str, u64)] = &[
    (FOUNDER_ADDRESS,         21_000_000 * 100_000_000),
    (ECOSYSTEM_FUND_ADDRESS,  21_000_000 * 100_000_000),
    (EARLY_VALIDATOR_ADDRESS, 10_500_000 * 100_000_000),
    (RESERVE_ADDRESS,         10_500_000 * 100_000_000),
];

pub const TOTAL_PREMINE: u64 = 63_000_000 * 100_000_000;

// ── Blockchain struct ────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blockchain {
    pub chain: Vec<Block>,
    pub accounts: AccountDB,
    pub authority: AuthorityManager,
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
            self.accounts.credit(address, *amount);
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

    pub fn latest_block(&self) -> &Block {
        self.chain.last().unwrap()
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
        tx.validate(expected_nonce)?;

        // Check balance including pending mempool spends
        let pending_spend = self.mempool_pending_spend(&tx.from_address);
        let available = self.accounts.get_balance(&tx.from_address)
            .saturating_sub(pending_spend);
        let needed = tx.amount + tx.fee;

        if available < needed {
            return Err(SentrixError::InsufficientBalance {
                have: available,
                need: needed,
            });
        }

        self.mempool.push_back(tx);
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
            .map(|tx| tx.amount + tx.fee)
            .sum()
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
            self.latest_block().hash.clone(),
            transactions,
            validator_address.to_string(),
        );

        Ok(block)
    }

    // ── Block application (two-pass atomic) ─────────────
    pub fn add_block(&mut self, block: Block) -> SentrixResult<()> {
        let expected_index = self.height() + 1;
        let expected_prev = self.latest_block().hash.clone();

        // ── Pass 1: dry-run validation ───────────────────
        block.validate_structure(expected_index, &expected_prev)?;

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
            tx.validate(nonce)?;

            let needed = tx.amount + tx.fee;
            if balance < needed {
                return Err(SentrixError::InsufficientBalance {
                    have: balance,
                    need: needed,
                });
            }

            // Update working state
            *working_balances.entry(tx.from_address.clone()).or_insert(balance) -= needed;
            *working_nonces.entry(tx.from_address.clone()).or_insert(nonce) += 1;
        }

        // ── Pass 2: commit ───────────────────────────────
        // Apply coinbase reward
        self.accounts.credit(&block.validator, coinbase.amount);
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
        }

        // Validator gets 50% of fees (other 50% already burned in transfer)
        let validator_fee_share = total_fee / 2;
        if validator_fee_share > 0 {
            self.accounts.credit(&block.validator, validator_fee_share);
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

    // ── Stats ────────────────────────────────────────────
    pub fn chain_stats(&self) -> serde_json::Value {
        serde_json::json!({
            "height": self.height(),
            "total_blocks": self.chain.len(),
            "total_minted_srx": self.total_minted as f64 / 100_000_000.0,
            "max_supply_srx": MAX_SUPPLY as f64 / 100_000_000.0,
            "mempool_size": self.mempool.len(),
            "active_validators": self.authority.active_count(),
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

        // Fund an account
        bc.accounts.credit("sender", 10_000_000);

        let tx = Transaction::new(
            "sender".to_string(),
            "receiver".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
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
}
