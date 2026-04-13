// block_executor.rs - Sentrix — Block validation and commit (two-pass)

use std::collections::{HashMap, HashSet};
use crate::core::blockchain::{Blockchain, CHAIN_WINDOW_SIZE};
use crate::core::block::Block;
use crate::core::transaction::TokenOp;
use crate::types::error::{SentrixError, SentrixResult};

impl Blockchain {
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
        let mut working_balances: HashMap<String, u64> = HashMap::new();
        let mut working_nonces: HashMap<String, u64> = HashMap::new();

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
        let burn_fee_share = total_fee.div_ceil(2);
        let validator_fee_share = total_fee - burn_fee_share;
        if validator_fee_share > 0 {
            self.accounts.credit(&block.validator, validator_fee_share)?;
        }

        // Record validator stats
        self.authority.record_block_produced(&block.validator, block.timestamp);

        // Remove mined transactions from mempool
        let mined_txids: HashSet<String> = block.transactions
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
}
