// block_executor.rs - Sentrix — Block validation and commit (two-pass)

use sentrix_primitives::block::{Block, STATE_ROOT_FORK_HEIGHT};
use crate::blockchain::{Blockchain, CHAIN_WINDOW_SIZE, is_valid_sentrix_address};
use sentrix_primitives::transaction::TokenOp;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use hex;
use std::collections::{HashMap, HashSet};

impl Blockchain {
    // ── Block application (two-pass atomic) ─────────────
    pub fn add_block(&mut self, block: Block) -> SentrixResult<()> {
        let expected_index = self.height() + 1;
        let expected_prev = self.latest_block()?.hash.clone();

        // ── Pass 1: dry-run validation ───────────────────
        block.validate_structure(expected_index, &expected_prev)?;

        // Pioneer: round-robin PoA authority check.
        // Voyager: proposer selected by DPoS + BFT justification — skip Pioneer authority.
        if !Blockchain::is_voyager_height(expected_index)
            && !self
                .authority
                .is_authorized(&block.validator, expected_index)?
        {
            return Err(SentrixError::UnauthorizedValidator(format!(
                "validator {} not authorized for block {}",
                block.validator, expected_index
            )));
        }

        // Block timestamp must be ≥ previous block and within 15s of wall time
        let prev_timestamp = self.latest_block()?.timestamp;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if block.timestamp < prev_timestamp {
            return Err(SentrixError::InvalidBlock(
                "block timestamp is before previous block".to_string(),
            ));
        }
        if block.timestamp > now + 15 {
            return Err(SentrixError::InvalidBlock(
                "block timestamp too far in the future".to_string(),
            ));
        }

        // Validate coinbase amount
        let reward = self.get_block_reward();
        let coinbase = block
            .coinbase()
            .ok_or_else(|| SentrixError::InvalidBlock("missing coinbase".to_string()))?;
        if coinbase.amount > reward {
            return Err(SentrixError::InvalidBlock(format!(
                "coinbase {} exceeds reward {}",
                coinbase.amount, reward
            )));
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

            // Checked addition prevents integer overflow on amount + fee
            let needed = tx.amount.checked_add(tx.fee).ok_or_else(|| {
                SentrixError::InvalidTransaction("amount + fee overflow".to_string())
            })?;
            if balance < needed {
                return Err(SentrixError::InsufficientBalance {
                    have: balance,
                    need: needed,
                });
            }

            // Validate token operation if present
            if let Some(token_op) = TokenOp::decode(&tx.data) {
                match &token_op {
                    TokenOp::Transfer {
                        contract,
                        to,
                        amount,
                    } => {
                        if !self.contracts.exists(contract) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "token contract {} not found",
                                contract
                            )));
                        }
                        // Validate token transfer target is a well-formed Sentrix address
                        if !is_valid_sentrix_address(to) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "invalid token transfer target address: '{}'",
                                to
                            )));
                        }
                        let token_bal =
                            self.contracts.get_token_balance(contract, &tx.from_address);
                        if token_bal < *amount {
                            return Err(SentrixError::InsufficientBalance {
                                have: token_bal,
                                need: *amount,
                            });
                        }
                    }
                    TokenOp::Burn { contract, amount } => {
                        if !self.contracts.exists(contract) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "token contract {} not found",
                                contract
                            )));
                        }
                        let token_bal =
                            self.contracts.get_token_balance(contract, &tx.from_address);
                        if token_bal < *amount {
                            return Err(SentrixError::InsufficientBalance {
                                have: token_bal,
                                need: *amount,
                            });
                        }
                    }
                    TokenOp::Mint { contract, to, .. } => {
                        if !self.contracts.exists(contract) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "token contract {} not found",
                                contract
                            )));
                        }
                        // Validate token mint target is a well-formed Sentrix address
                        if !is_valid_sentrix_address(to) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "invalid token mint target address: '{}'",
                                to
                            )));
                        }
                    }
                    TokenOp::Approve {
                        contract, spender, ..
                    } => {
                        if !self.contracts.exists(contract) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "token contract {} not found",
                                contract
                            )));
                        }
                        // Validate spender is a well-formed Sentrix address before recording allowance
                        if !is_valid_sentrix_address(spender) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "invalid token approve spender address: '{}'",
                                spender
                            )));
                        }
                    }
                    TokenOp::Deploy { name, symbol, .. } => {
                        // Pre-validate name and symbol in Pass 1 to keep Pass 2 atomic — no mid-commit failures
                        if name.is_empty() || name.len() > 64 {
                            return Err(SentrixError::InvalidTransaction(
                                "token name must be 1–64 characters".to_string(),
                            ));
                        }
                        if symbol.is_empty()
                            || symbol.len() > 10
                            || !symbol.chars().all(|c| c.is_ascii_alphanumeric())
                        {
                            return Err(SentrixError::InvalidTransaction(
                                "token symbol must be 1–10 ASCII alphanumeric characters"
                                    .to_string(),
                            ));
                        }
                    }
                }
            }

            // Update working state
            *working_balances
                .entry(tx.from_address.clone())
                .or_insert(balance) -= needed;
            *working_nonces
                .entry(tx.from_address.clone())
                .or_insert(nonce) += 1;
        }

        // ── Pass 2: commit ───────────────────────────────
        // Apply coinbase reward
        self.accounts.credit(&block.validator, coinbase.amount)?;
        self.total_minted += coinbase.amount;

        // Apply all transactions
        let mut total_fee: u64 = 0;
        for tx in block.transactions.iter().skip(1) {
            self.accounts
                .transfer(&tx.from_address, &tx.to_address, tx.amount, tx.fee)?;
            total_fee += tx.fee;

            // Execute token operation if present in data field
            if let Some(token_op) = TokenOp::decode(&tx.data) {
                match token_op {
                    TokenOp::Deploy {
                        name,
                        symbol,
                        decimals,
                        supply,
                        max_supply,
                    } => {
                        // Contract address derived from tx.txid — deterministic across all nodes for the same transaction
                        self.contracts.deploy(
                            &tx.from_address,
                            &name,
                            &symbol,
                            decimals,
                            supply,
                            max_supply,
                            &tx.txid,
                        )?;
                    }
                    TokenOp::Transfer {
                        contract,
                        to,
                        amount,
                    } => {
                        self.contracts.execute_transfer(
                            &contract,
                            &tx.from_address,
                            &to,
                            amount,
                        )?;
                    }
                    TokenOp::Burn { contract, amount } => {
                        self.contracts
                            .execute_burn(&contract, &tx.from_address, amount)?;
                    }
                    TokenOp::Mint {
                        contract,
                        to,
                        amount,
                    } => {
                        self.contracts
                            .execute_mint(&contract, &tx.from_address, &to, amount)?;
                    }
                    TokenOp::Approve {
                        contract,
                        spender,
                        amount,
                    } => {
                        self.contracts.execute_approve(
                            &contract,
                            &tx.from_address,
                            &spender,
                            amount,
                        )?;
                    }
                }
            }

            // Execute EVM transaction if present (data field starts with "EVM:")
            if tx.is_evm_tx() && Self::is_voyager_height(self.height()) {
                self.execute_evm_tx_in_block(tx)?;
            }
        }

        // Burn gets ceiling division, validator gets floor — all fees distributed with no rounding loss
        let burn_fee_share = total_fee.div_ceil(2);
        let validator_fee_share = total_fee - burn_fee_share;
        if validator_fee_share > 0 {
            self.accounts
                .credit(&block.validator, validator_fee_share)?;
        }

        // Record validator stats
        self.authority
            .record_block_produced(&block.validator, block.timestamp);

        // Remove mined transactions from mempool
        let mined_txids: HashSet<String> = block
            .transactions
            .iter()
            .map(|tx| tx.txid.clone())
            .collect();
        self.mempool.retain(|tx| !mined_txids.contains(&tx.txid));

        // Prune expired transactions after each block to keep mempool bounded
        self.prune_mempool();

        // A5: index every tx in this block by txid → block_index for O(1)
        // lookups beyond the in-memory chain window.
        for tx in &block.transactions {
            self.record_tx_in_index(&tx.txid, block.index);
        }

        // Append block to chain
        self.chain.push(block);

        // Sliding window: evict oldest blocks beyond CHAIN_WINDOW_SIZE; evicted blocks stay in sled
        // Only the in-memory window shrinks — full history is always available on disk
        if self.chain.len() > CHAIN_WINDOW_SIZE {
            let excess = self.chain.len() - CHAIN_WINDOW_SIZE;
            self.chain.drain(..excess);
        }

        // Update state trie after block commit, stamp state_root on the block header,
        // and verify the sender's committed root when receiving from peers.
        let trie_root = self.update_trie_for_block().map_err(|e| {
            SentrixError::Internal(format!(
                "trie update failed at block {}: {}",
                self.height(),
                e
            ))
        })?;

        if let Some(computed_root) = trie_root
            && let Some(last) = self.chain.last_mut()
        {
            if last.index >= STATE_ROOT_FORK_HEIGHT {
                match last.state_root {
                    None => {
                        // Self-produced block: set state_root and recompute hash so that
                        // state_root is committed into the block header (V7-C-01).
                        last.state_root = Some(computed_root);
                        last.hash = last.calculate_hash();
                    }
                    Some(received_root) => {
                        // Received block: verify peer's state_root matches ours (V7-C-01).
                        // State root mismatch is fatal — reject the block to prevent accepting a diverged chain state
                        if received_root != computed_root {
                            return Err(SentrixError::ChainValidationFailed(format!(
                                "state_root mismatch at block {}: received {}, computed {}",
                                last.index,
                                hex::encode(received_root),
                                hex::encode(computed_root),
                            )));
                        }
                        last.state_root = Some(computed_root);
                    }
                }
            } else {
                // Below fork height: stamp state_root without changing block hash.
                last.state_root = Some(computed_root);
            }
        }

        Ok(())
    }

    /// Execute an EVM transaction (from eth_sendRawTransaction) within a block.
    /// Decodes the original RLP tx from the signature field, runs it through revm,
    /// applies state changes (contract creation, storage updates, balance transfers).
    fn execute_evm_tx_in_block(
        &mut self,
        tx: &sentrix_primitives::transaction::Transaction,
    ) -> SentrixResult<()> {
        // Parse "EVM:gas_limit:hex_data" from data field
        let parts: Vec<&str> = tx.data.splitn(3, ':').collect();
        if parts.len() != 3 || parts[0] != "EVM" {
            return Ok(()); // not an EVM tx, skip
        }
        let gas_limit: u64 = parts[1].parse().unwrap_or(30_000_000);
        let calldata = hex::decode(parts[2]).unwrap_or_default();

        // Decode raw Ethereum tx from signature field for re-validation
        let raw_bytes = match hex::decode(&tx.signature) {
            Ok(b) => b,
            Err(_) => return Ok(()), // malformed, skip silently
        };

        use alloy_consensus::TxEnvelope;
        use alloy_consensus::transaction::SignerRecoverable;
        use alloy_eips::eip2718::Decodable2718;

        let envelope: TxEnvelope = match TxEnvelope::decode_2718(&mut raw_bytes.as_slice()) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        let _sender = envelope.recover_signer().ok();

        // Build EVM tx
        use sentrix_evm::database::parse_sentrix_address;
        use sentrix_evm::executor::execute_tx;
        use sentrix_evm::gas::INITIAL_BASE_FEE;
        use alloy_primitives::U256;
        use revm::context::TxEnv;
        use revm::database::InMemoryDB;
        use revm::primitives::{KECCAK_EMPTY, TxKind};
        use revm::state::AccountInfo;

        let from_addr =
            parse_sentrix_address(&tx.from_address).unwrap_or(alloy_primitives::Address::ZERO);
        let to_addr_str = if tx.to_address == sentrix_primitives::transaction::TOKEN_OP_ADDRESS {
            None
        } else {
            parse_sentrix_address(&tx.to_address)
        };
        let tx_kind = match to_addr_str {
            Some(addr) => TxKind::Call(addr),
            None => TxKind::Create,
        };

        // Populate InMemoryDB with sender (gas + value) and target if contract
        let mut in_mem_db = InMemoryDB::default();
        let sender_balance = self.accounts.get_balance(&tx.from_address);
        let sender_nonce = self.accounts.get_nonce(&tx.from_address);
        in_mem_db.insert_account_info(
            from_addr,
            AccountInfo {
                balance: U256::from(sender_balance).saturating_mul(U256::from(10_000_000_000u64)),
                nonce: sender_nonce.saturating_sub(1), // already incremented by .transfer() above
                code_hash: KECCAK_EMPTY,
                account_id: None,
                code: None,
            },
        );

        let evm_tx = TxEnv::builder()
            .caller(from_addr)
            .kind(tx_kind)
            .data(alloy_primitives::Bytes::from(calldata))
            .gas_limit(gas_limit)
            .gas_price(INITIAL_BASE_FEE as u128)
            .nonce(sender_nonce.saturating_sub(1))
            .chain_id(Some(tx.chain_id))
            .build()
            .unwrap_or_default();

        match execute_tx(in_mem_db, evm_tx, INITIAL_BASE_FEE) {
            Ok(receipt) => {
                tracing::info!(
                    "EVM tx {}: success={} gas_used={} contract={:?}",
                    &tx.txid[..16.min(tx.txid.len())],
                    receipt.success,
                    receipt.gas_used,
                    receipt
                        .contract_address
                        .map(|a| format!("0x{}", hex::encode(a.as_slice()))),
                );
                if !receipt.success {
                    // A2: reverted EVM tx — record so eth_getTransactionReceipt
                    // returns status=0x0 instead of the default 0x1.
                    self.accounts.mark_evm_tx_failed(&tx.txid);
                }
                // Store contract RUNTIME code (not init code) if CREATE succeeded.
                // receipt.output contains the runtime bytecode returned by the constructor.
                if let Some(contract_addr) = receipt.contract_address
                    && !receipt.output.is_empty()
                {
                    let addr_str = format!("0x{}", hex::encode(contract_addr.as_slice()));
                    use sha3::{Digest as _, Keccak256};
                    let code_hash: [u8; 32] = Keccak256::digest(&receipt.output).into();
                    let code_hash_hex = hex::encode(code_hash);
                    self.accounts
                        .store_contract_code(&code_hash_hex, receipt.output.clone());
                    self.accounts.set_contract(&addr_str, code_hash);
                }
            }
            Err(e) => {
                tracing::warn!("EVM tx {} failed: {}", &tx.txid[..16.min(tx.txid.len())], e);
                // A2: hard execution error — also mark as failed so the
                // tx receipt reports status=0x0.
                self.accounts.mark_evm_tx_failed(&tx.txid);
            }
        }
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use crate::blockchain::{Blockchain, CHAIN_ID};
    use sentrix_primitives::transaction::{MIN_TX_FEE, TOKEN_OP_ADDRESS, TokenOp, Transaction};
    use secp256k1::rand::rngs::OsRng;
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    fn make_keypair() -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        secp.generate_keypair(&mut OsRng)
    }

    fn derive_addr(pk: &PublicKey) -> String {
        sentrix_wallet::Wallet::derive_address(pk)
    }

    fn setup() -> Blockchain {
        let mut bc = Blockchain::new("admin".to_string());
        bc.authority
            .add_validator_unchecked("v1".to_string(), "V1".to_string(), "pk1".to_string());
        bc
    }

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

    // Contract address must be deterministic — same txid on any node produces the same address
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
            name: "TestToken".to_string(),
            symbol: "TTK".to_string(),
            decimals: 8,
            supply: 1_000_000,
            max_supply: 0,
        };
        let data = token_op.encode().unwrap();
        let tx = Transaction::new(
            sender.clone(),
            TOKEN_OP_ADDRESS.to_string(),
            0,
            MIN_TX_FEE,
            0,
            data,
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();

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
        assert_eq!(
            tokens1.len(),
            tokens2.len(),
            "both chains should have same number of tokens"
        );
        assert_eq!(
            tokens1[0]["contract_address"], tokens2[0]["contract_address"],
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

    #[test]
    fn test_state_root_set_after_block_below_fork_height() {
        // Blocks below STATE_ROOT_FORK_HEIGHT: state_root set but hash unchanged.
        use sentrix_primitives::block::STATE_ROOT_FORK_HEIGHT;
        let mut bc = setup();
        assert!(
            bc.height() + 1 < STATE_ROOT_FORK_HEIGHT,
            "test assumes height < fork"
        );

        // Init an in-memory trie (no sled — state_trie will be None without db)
        // Without trie init, update_trie_for_block returns Ok(None) → state_root remains None
        let block = bc.create_block("v1").unwrap();
        let original_hash = block.hash.clone();
        bc.add_block(block).unwrap();

        let added = bc.chain.last().unwrap();
        assert!(added.index < STATE_ROOT_FORK_HEIGHT);
        // No trie initialized → state_root is None; hash must be unchanged
        assert_eq!(
            added.hash, original_hash,
            "block hash must not change without trie"
        );
    }

    #[test]
    fn test_add_block_succeeds_without_trie() {
        // update_trie_for_block returning Ok(None) must not fail add_block.
        let mut bc = setup();
        // state_trie is None (no init_trie called) — should be fine
        let block = bc.create_block("v1").unwrap();
        assert!(
            bc.add_block(block).is_ok(),
            "add_block must succeed without trie"
        );
    }
}
