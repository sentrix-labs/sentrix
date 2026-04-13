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
                    TokenOp::Deploy { name, symbol, .. } => {
                        // V6-C-01: validate name/symbol lengths now so Pass 2 won't fail mid-commit
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
                    }
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
                        // V6-C-01 FIX: use tx.txid as deterministic seed — same txid on every node
                        self.contracts.deploy(&tx.from_address, &name, &symbol, decimals, supply, max_supply, &tx.txid)?;
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

// ── Tests ─────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use secp256k1::{Secp256k1, SecretKey, PublicKey};
    use secp256k1::rand::rngs::OsRng;
    use crate::core::transaction::{Transaction, MIN_TX_FEE, TOKEN_OP_ADDRESS, TokenOp};
    use crate::core::blockchain::{Blockchain, CHAIN_ID};

    fn make_keypair() -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        secp.generate_keypair(&mut OsRng)
    }

    fn derive_addr(pk: &PublicKey) -> String {
        crate::wallet::wallet::Wallet::derive_address(pk)
    }

    fn setup() -> Blockchain {
        let mut bc = Blockchain::new("admin".to_string());
        bc.authority.add_validator_unchecked("v1".to_string(), "V1".to_string(), "pk1".to_string());
        bc
    }

    const RECV: &str = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

    // Pass 1 rejection must not mutate state
    #[test]
    fn test_add_block_invalid_validator_leaves_state_clean() {
        let mut bc = setup();
        let height_before = bc.height();
        let balance_before = bc.accounts.get_balance("v1");

        // Create block for v1 then try to submit it as a different (unauthorized) validator
        let mut block = bc.create_block("v1").unwrap();
        block.validator = "not_authorized".to_string();

        let result = bc.add_block(block);
        assert!(result.is_err());
        // State must not change
        assert_eq!(bc.height(), height_before);
        assert_eq!(bc.accounts.get_balance("v1"), balance_before);
    }

    // V6-C-01 test: contract address is deterministic — same seed produces same address
    #[test]
    fn test_contract_address_deterministic() {
        let mut bc1 = setup();
        let mut bc2 = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);

        let fund = 10_000_000_000u64;
        bc1.accounts.credit(&sender, fund).unwrap();
        bc2.accounts.credit(&sender, fund).unwrap();

        let token_op = TokenOp::Deploy {
            name: "TestToken".to_string(), symbol: "TTK".to_string(),
            decimals: 8, supply: 1_000_000, max_supply: 0,
        };
        let data = token_op.encode().unwrap();
        let tx = Transaction::new(
            sender.clone(), TOKEN_OP_ADDRESS.to_string(),
            0, MIN_TX_FEE, 0, data, CHAIN_ID, &sk, &pk,
        ).unwrap();

        // Add the SAME tx to both chains and produce blocks
        bc1.add_to_mempool(tx.clone()).unwrap();
        bc2.add_to_mempool(tx.clone()).unwrap();

        let block1 = bc1.create_block("v1").unwrap();
        let block2 = bc2.create_block("v1").unwrap();

        // Apply to both chains
        bc1.add_block(block1).unwrap();
        bc2.add_block(block2).unwrap();

        // Contract registry should have identical addresses
        let tokens1 = bc1.list_tokens();
        let tokens2 = bc2.list_tokens();
        assert_eq!(tokens1.len(), tokens2.len(), "both chains should have same number of tokens");
        assert_eq!(
            tokens1[0]["contract_address"],
            tokens2[0]["contract_address"],
            "V6-C-01: contract address must be deterministic across nodes"
        );
    }

    // Block with timestamp before previous block is rejected
    #[test]
    fn test_block_with_old_timestamp_rejected() {
        let mut bc = setup();
        let mut block = bc.create_block("v1").unwrap();
        // Set timestamp to before genesis (timestamp=0)
        block.timestamp = 0;
        let result = bc.add_block(block);
        assert!(result.is_err());
    }
}
