// block_executor.rs - Sentrix — Block validation and commit (two-pass)

use crate::authority::AuthorityManager;
use crate::blockchain::{
    Blockchain, CHAIN_WINDOW_SIZE, is_spendable_sentrix_address, is_valid_sentrix_address,
};
use crate::vm::ContractRegistry;
use hex;
use sentrix_primitives::account::AccountDB;
use sentrix_primitives::block::{Block, STATE_ROOT_FORK_HEIGHT};
use sentrix_primitives::error::{SentrixError, SentrixResult};
use sentrix_primitives::transaction::{TokenOp, Transaction};
use std::collections::{HashMap, HashSet, VecDeque};

/// Origin of a block being admitted to the chain. Distinguishes
/// proposals this validator just produced locally (where `state_root`
/// is legitimately `None` until `update_trie_for_block` stamps it)
/// from blocks that arrived over the wire (where a `None` state_root
/// past `STATE_ROOT_FORK_HEIGHT` means the sender's trie is broken
/// and accepting would fork the chain). Backlog #1e.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockSource {
    /// Produced by this validator (block_producer::build_block).
    /// state_root starts None and is stamped in Pass 2.
    SelfProduced,
    /// Received from a peer via P2P sync or BFT finalize.
    /// state_root must already be Some past STATE_ROOT_FORK_HEIGHT.
    Peer,
}

/// C-03: snapshot of the mutable Blockchain state taken immediately
/// before Pass 2 of `add_block`. If any step in Pass 2 returns `Err`,
/// the snapshot is restored so the chain never observes a partial
/// block-commit on disk-cache or in memory. The snapshot is scoped to
/// the fields Pass 2 actually mutates; `state_trie` self-heals on the
/// next successful `update_trie_for_block` because the trie is rebuilt
/// from `accounts` (which is included here) on each subsequent call.
pub(crate) struct BlockchainSnapshot {
    accounts: AccountDB,
    contracts: ContractRegistry,
    authority: AuthorityManager,
    mempool: VecDeque<Transaction>,
    total_minted: u64,
    chain_len: usize,
}

impl Blockchain {
    /// P1 (write-lock scope split): pure read-only validation of a block
    /// against the current chain state. Safe to call under a shared
    /// `RwLock::read()` guard — performs no mutations. Mirrors Pass 1 of
    /// `add_block` so that callers can reject obviously-invalid blocks
    /// (wrong height, bad signature, insufficient balance, duplicate
    /// nonce, etc.) WITHOUT needing the write lock that would starve
    /// RPC readers for the duration of the check. On success the
    /// caller takes the write lock and calls `add_block` for the
    /// commit; `add_block` re-runs Pass 1 internally as the single
    /// source of truth for safety, so this pre-flight is an
    /// optimisation only — never a correctness substitute.
    #[allow(clippy::cognitive_complexity)]
    pub fn validate_block(&self, block: &Block) -> SentrixResult<()> {
        let expected_index = self.height() + 1;
        let expected_prev = self.latest_block()?.hash.clone();

        block.validate_structure(expected_index, &expected_prev)?;

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

        let reward = self.get_block_reward();
        let coinbase = block
            .coinbase()
            .ok_or_else(|| SentrixError::InvalidBlock("missing coinbase".to_string()))?;
        if coinbase.amount != reward {
            return Err(SentrixError::InvalidBlock(format!(
                "coinbase amount {} must equal block reward {}",
                coinbase.amount, reward
            )));
        }
        if coinbase.to_address != block.validator {
            return Err(SentrixError::InvalidBlock(format!(
                "coinbase recipient {} must equal block validator {}",
                coinbase.to_address, block.validator
            )));
        }

        let mut working_balances: HashMap<String, u64> = HashMap::new();
        let mut working_nonces: HashMap<String, u64> = HashMap::new();
        let mut seen_sender_nonce: HashSet<(String, u64)> = HashSet::new();

        for tx in block.transactions.iter().skip(1) {
            if !seen_sender_nonce.insert((tx.from_address.clone(), tx.nonce)) {
                return Err(SentrixError::InvalidBlock(format!(
                    "duplicate (sender, nonce) pair for {} nonce {} in block",
                    tx.from_address, tx.nonce
                )));
            }

            let balance = working_balances
                .get(&tx.from_address)
                .copied()
                .unwrap_or_else(|| self.accounts.get_balance(&tx.from_address));

            let nonce = working_nonces
                .get(&tx.from_address)
                .copied()
                .unwrap_or_else(|| self.accounts.get_nonce(&tx.from_address));

            tx.validate(nonce, self.chain_id)?;

            let needed = tx.amount.checked_add(tx.fee).ok_or_else(|| {
                SentrixError::InvalidTransaction("amount + fee overflow".to_string())
            })?;
            if balance < needed {
                return Err(SentrixError::InsufficientBalance {
                    have: balance,
                    need: needed,
                });
            }

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
                        if !is_spendable_sentrix_address(to) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "invalid token transfer target address: '{}' \
                                 (zero address rejected — use Burn op to destroy tokens)",
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
                        if !is_spendable_sentrix_address(to) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "invalid token mint target address: '{}' (zero \
                                 address rejected)",
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
                        if !is_valid_sentrix_address(spender) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "invalid token approve spender address: '{}'",
                                spender
                            )));
                        }
                    }
                    TokenOp::Deploy { name, symbol, .. } => {
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

            *working_balances
                .entry(tx.from_address.clone())
                .or_insert(balance) -= needed;
            *working_nonces
                .entry(tx.from_address.clone())
                .or_insert(nonce) += 1;
        }

        Ok(())
    }

    // ── Block application (two-pass atomic) ─────────────
    /// Admit a block produced locally. Preserves existing call sites that
    /// don't care about origin (tests, legacy integrations). For blocks
    /// arriving from peers past `STATE_ROOT_FORK_HEIGHT`, use
    /// [`add_block_from_peer`](Self::add_block_from_peer) instead — it
    /// rejects state_root=None rather than stamping it locally (which
    /// would silently fork the chain when the peer's trie is broken,
    /// backlog #1e / 2026-04-20 mainnet incident).
    pub fn add_block(&mut self, block: Block) -> SentrixResult<()> {
        self.add_block_with_source(block, BlockSource::SelfProduced)
    }

    /// Admit a block received from a peer. Past fork height, the block
    /// must carry `state_root = Some(root)` — a `None` from a peer
    /// indicates the peer's trie failed to commit (backlog #1e) and
    /// accepting it would cause us to stamp our own root and recompute
    /// the block hash, diverging from the peer's persisted hash →
    /// "invalid previous hash" fork on the next block.
    pub fn add_block_from_peer(&mut self, block: Block) -> SentrixResult<()> {
        self.add_block_with_source(block, BlockSource::Peer)
    }

    /// Core admit path. `source` is consulted only in the state_root
    /// stamping branch — everything else is identical for self-produced
    /// and peer-received blocks.
    pub fn add_block_with_source(
        &mut self,
        block: Block,
        source: BlockSource,
    ) -> SentrixResult<()> {
        self.source_for_current_add = source;
        let result = self.add_block_impl(block);
        // Clear the source marker so stale state can't leak into a later
        // unrelated call (e.g. if apply_block_pass2 were ever called
        // directly from tests).
        self.source_for_current_add = BlockSource::SelfProduced;
        result
    }

    fn add_block_impl(&mut self, block: Block) -> SentrixResult<()> {
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

        // C-04: validate coinbase amount AND recipient. Amount must equal the
        // current era's block reward exactly (no silent underpay, no inflation).
        // Recipient must equal block.validator so that if credit() is ever
        // refactored to use coinbase.to_address instead of block.validator,
        // the two cannot diverge and redirect the subsidy to an attacker.
        let reward = self.get_block_reward();
        let coinbase = block
            .coinbase()
            .ok_or_else(|| SentrixError::InvalidBlock("missing coinbase".to_string()))?;
        if coinbase.amount != reward {
            return Err(SentrixError::InvalidBlock(format!(
                "coinbase amount {} must equal block reward {}",
                coinbase.amount, reward
            )));
        }
        if coinbase.to_address != block.validator {
            return Err(SentrixError::InvalidBlock(format!(
                "coinbase recipient {} must equal block validator {}",
                coinbase.to_address, block.validator
            )));
        }

        // Validate all non-coinbase transactions on working state copy.
        //
        // H-06: reject blocks containing duplicate (from_address, nonce)
        // pairs. The working_nonces update at loop end already rejects a
        // second tx with the stale nonce via tx.validate(), but explicit
        // dedup makes the intent unambiguous and survives future refactors
        // of the Pass 1 loop. Duplicate txids are rejected earlier by
        // Block::validate_structure (C-05).
        let mut working_balances: HashMap<String, u64> = HashMap::new();
        let mut working_nonces: HashMap<String, u64> = HashMap::new();
        let mut seen_sender_nonce: HashSet<(String, u64)> = HashSet::new();

        for tx in block.transactions.iter().skip(1) {
            if !seen_sender_nonce.insert((tx.from_address.clone(), tx.nonce)) {
                return Err(SentrixError::InvalidBlock(format!(
                    "duplicate (sender, nonce) pair for {} nonce {} in block",
                    tx.from_address, tx.nonce
                )));
            }

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
                        // M-02: token transfer target must be valid AND not
                        // the zero address. Zero-address targets would
                        // otherwise silently increase the zero account's
                        // token balance, acting as an unaccounted burn that
                        // doesn't update `total_burned`. Use the dedicated
                        // burn op if the intent was to destroy tokens.
                        if !is_spendable_sentrix_address(to) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "invalid token transfer target address: '{}' \
                                 (zero address rejected — use Burn op to destroy tokens)",
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
                        // M-02: mint target must not be zero address.
                        if !is_spendable_sentrix_address(to) {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "invalid token mint target address: '{}' (zero \
                                 address rejected)",
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

        // ── Pass 2: commit (atomic via snapshot rollback on Err) ────
        // C-03: snapshot pre-Pass-2 state. If any mutation below returns
        // `Err`, the snapshot is restored before propagating the error,
        // so the chain never observes a partial commit (half-applied
        // transactions, fee credit without fee debit, contract state
        // updated without the tx that triggered it, etc.). The trie is
        // not snapshotted: it is rebuilt from `accounts` on the next
        // successful `update_trie_for_block`, so a failed trie commit
        // self-heals when the same or a later block succeeds.
        let snap = BlockchainSnapshot {
            accounts: self.accounts.clone(),
            contracts: self.contracts.clone(),
            authority: self.authority.clone(),
            mempool: self.mempool.clone(),
            total_minted: self.total_minted,
            chain_len: self.chain.len(),
        };

        match self.apply_block_pass2(block) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.accounts = snap.accounts;
                self.contracts = snap.contracts;
                self.authority = snap.authority;
                self.mempool = snap.mempool;
                self.total_minted = snap.total_minted;
                self.chain.truncate(snap.chain_len);
                Err(e)
            }
        }
    }

    /// C-03 Pass 2: applies all block mutations. Must only be called
    /// from `add_block` after Pass 1 has validated the block and the
    /// caller has taken a `BlockchainSnapshot` for rollback.
    fn apply_block_pass2(&mut self, block: Block) -> SentrixResult<()> {
        // Coinbase was validated in Pass 1; re-extract for mutation.
        let (coinbase_amount, coinbase_validator) = {
            let coinbase = block
                .coinbase()
                .ok_or_else(|| SentrixError::InvalidBlock("missing coinbase".to_string()))?;
            (coinbase.amount, block.validator.clone())
        };

        // Apply coinbase reward
        self.accounts.credit(&coinbase_validator, coinbase_amount)?;
        self.total_minted += coinbase_amount;

        // Apply all transactions
        let mut total_fee: u64 = 0;
        for tx in block.transactions.iter().skip(1) {
            self.accounts
                .transfer(&tx.from_address, &tx.to_address, tx.amount, tx.fee)?;
            // P1: checked_add — 5000 tx × max fee is far below u64::MAX
            // in practice, but the guard is cheap and prevents a silent
            // wrap if MAX_TX_PER_BLOCK or MIN_TX_FEE are ever tuned
            // upward past the implicit ceiling.
            total_fee = total_fee
                .checked_add(tx.fee)
                .ok_or_else(|| SentrixError::Internal("block total_fee overflow".to_string()))?;

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
            // tx_index skips coinbase at slot 0 — first real tx is index 1.
            if tx.is_evm_tx() && Self::is_voyager_height(self.height()) {
                let tx_index = (block
                    .transactions
                    .iter()
                    .position(|t| t.txid == tx.txid)
                    .unwrap_or(0)) as u32;
                self.execute_evm_tx_in_block(tx, block.index, &block.hash, tx_index)?;
            }
        }
        // Sprint 2: compute + persist per-block logs bloom. Cheap enough to
        // re-scan the height-prefix range because EVM txs per block are
        // bounded; keeps the bloom exactly aligned with TABLE_LOGS without a
        // parallel in-memory accumulator.
        if let Some(storage) = self.mdbx_storage.as_ref() {
            use sentrix_evm::{StoredLog, add_log_to_bloom, empty_bloom};
            let mut bloom = empty_bloom();
            let prefix = block.index.to_be_bytes();
            if let Ok(entries) = storage.iter(sentrix_storage::tables::TABLE_LOGS) {
                for (k, v) in entries {
                    if k.len() >= 8
                        && k[..8] == prefix
                        && let Ok(log) = bincode::deserialize::<StoredLog>(&v)
                    {
                        add_log_to_bloom(&mut bloom, &log.address, &log.topics);
                    }
                }
            }
            let _ = storage.put(
                sentrix_storage::tables::TABLE_BLOOM,
                &block.index.to_be_bytes(),
                &bloom,
            );
        }

        // Burn gets ceiling division, validator gets floor — all fees distributed with no rounding loss
        let burn_fee_share = total_fee.div_ceil(2);
        let validator_fee_share = total_fee - burn_fee_share;
        if validator_fee_share > 0 {
            self.accounts
                .credit(&coinbase_validator, validator_fee_share)?;
        }

        // Record validator stats
        self.authority
            .record_block_produced(&coinbase_validator, block.timestamp);

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

        // Sliding window: evict oldest blocks beyond CHAIN_WINDOW_SIZE; evicted blocks stay in MDBX
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
                        // state_root=None past fork height is only legitimate
                        // when WE just produced this block — build_block creates
                        // fresh blocks with state_root=None and add_block is
                        // expected to stamp it here. A peer-sent block with
                        // state_root=None means the peer's trie is broken
                        // (backlog #1e / 2026-04-20 mainnet incident) — if we
                        // stamp it ourselves we silently recompute the block
                        // hash, diverging from what the peer persisted, and
                        // the next block's previous_hash check fails → fork.
                        //
                        // Peer blocks with None get rejected loud, not stamped.
                        if self.source_for_current_add == BlockSource::Peer {
                            tracing::error!(
                                "CRITICAL #1e: peer block {} arrived with state_root=None past \
                                 STATE_ROOT_FORK_HEIGHT — sender's trie is broken. Rejecting to \
                                 prevent silent fork. Expected local trie root: {}",
                                last.index,
                                hex::encode(computed_root)
                            );
                            return Err(SentrixError::ChainValidationFailed(format!(
                                "peer block {} has state_root=None past fork height (#1e)",
                                last.index
                            )));
                        }
                        // Self-produced: stamp and recompute hash (V7-C-01).
                        last.state_root = Some(computed_root);
                        last.hash = last.calculate_hash();
                    }
                    Some(received_root) => {
                        // Received block: verify peer's state_root matches ours (V7-C-01).
                        // State root mismatch is fatal — reject the block to prevent accepting a diverged chain state
                        if received_root != computed_root {
                            tracing::error!(
                                "CRITICAL #1e: state_root mismatch at block {} — received {} \
                                 vs computed {}. Local trie and peer's trie disagree on the \
                                 post-block state. Rejecting.",
                                last.index,
                                hex::encode(received_root),
                                hex::encode(computed_root),
                            );
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
        block_height: u64,
        block_hash_hex: &str,
        tx_index: u32,
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
        use alloy_primitives::U256;
        use revm::context::TxEnv;
        use revm::database::InMemoryDB;
        use revm::primitives::{KECCAK_EMPTY, TxKind};
        use revm::state::AccountInfo;
        use sentrix_evm::database::parse_sentrix_address;
        use sentrix_evm::executor::execute_tx;
        use sentrix_evm::gas::INITIAL_BASE_FEE;

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

        match execute_tx(in_mem_db, evm_tx, INITIAL_BASE_FEE, self.chain_id) {
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
                // Sprint 2: persist every log emitted by this tx. Key is
                // (height, tx_index, log_index) BE-packed so range scans
                // return logs in canonical Ethereum order.
                if let Some(storage) = self.mdbx_storage.as_ref() {
                    use sentrix_evm::{StoredLog, log_key};
                    let mut block_hash_bytes = [0u8; 32];
                    if let Ok(decoded) = hex::decode(block_hash_hex.trim_start_matches("0x")) {
                        let n = decoded.len().min(32);
                        block_hash_bytes[..n].copy_from_slice(&decoded[..n]);
                    }
                    let mut tx_hash_bytes = [0u8; 32];
                    if let Ok(decoded) = hex::decode(tx.txid.trim_start_matches("0x")) {
                        let n = decoded.len().min(32);
                        tx_hash_bytes[..n].copy_from_slice(&decoded[..n]);
                    }
                    for (log_idx, log) in receipt.logs.iter().enumerate() {
                        let stored = StoredLog::from_revm(
                            log,
                            block_height,
                            block_hash_bytes,
                            tx_hash_bytes,
                            tx_index,
                            log_idx as u32,
                        );
                        let key = log_key(block_height, tx_index, log_idx as u32);
                        let _ =
                            storage.put_bincode(sentrix_storage::tables::TABLE_LOGS, &key, &stored);
                    }
                }
                // Store contract RUNTIME code (not init code) if CREATE succeeded.
                // receipt.output contains the runtime bytecode returned by the constructor.
                //
                // P1 (EIP-170): cap deployed runtime bytecode at 24_576
                // bytes. Without this an attacker could ship a contract
                // whose runtime code expands state and trie pages
                // disproportionately per gas unit, amplifying the cost
                // of every subsequent block that touches the account.
                // We reject the CREATE by dropping the stored code and
                // marking the tx failed; the sender still paid gas but
                // no contract is registered.
                const EIP170_MAX_CODE_SIZE: usize = 24_576;
                if let Some(contract_addr) = receipt.contract_address
                    && !receipt.output.is_empty()
                {
                    if receipt.output.len() > EIP170_MAX_CODE_SIZE {
                        tracing::warn!(
                            "P1/EIP-170: contract {} runtime bytecode {} B > {} B limit; \
                             rejecting deploy (tx {})",
                            format!("0x{}", hex::encode(contract_addr.as_slice())),
                            receipt.output.len(),
                            EIP170_MAX_CODE_SIZE,
                            &tx.txid[..16.min(tx.txid.len())]
                        );
                        self.accounts.mark_evm_tx_failed(&tx.txid);
                    } else {
                        let addr_str = format!("0x{}", hex::encode(contract_addr.as_slice()));
                        use sha3::{Digest as _, Keccak256};
                        let code_hash: [u8; 32] = Keccak256::digest(&receipt.output).into();
                        let code_hash_hex = hex::encode(code_hash);
                        self.accounts
                            .store_contract_code(&code_hash_hex, receipt.output.clone());
                        self.accounts.set_contract(&addr_str, code_hash);
                    }
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
    use secp256k1::{PublicKey, Secp256k1, SecretKey};
    use sentrix_primitives::transaction::{MIN_TX_FEE, TOKEN_OP_ADDRESS, TokenOp, Transaction};

    fn make_keypair() -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        let mut rng = secp256k1::rand::rng();
        secp.generate_keypair(&mut rng)
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

    // C-04: coinbase amount must equal the exact block reward (no silent
    // underpay, no inflation). Previously `coinbase.amount > reward` only
    // guarded against inflation; a block with 0 amount was accepted, wasting
    // the subsidy.
    #[test]
    fn test_c04_coinbase_amount_too_high_rejected() {
        use sentrix_primitives::block::Block;

        let mut bc = setup();
        let reward = bc.get_block_reward();
        let prev = bc.latest_block().unwrap().hash.clone();
        let ts = bc.latest_block().unwrap().timestamp + 1;

        // Inflated coinbase: amount > reward
        let bad = Transaction::new_coinbase("v1".to_string(), reward + 1, 1, ts);
        let block = Block::new(1, prev, vec![bad], "v1".to_string());

        let err = bc.add_block(block).unwrap_err();
        assert!(
            format!("{err:?}").contains("coinbase amount"),
            "expected amount-mismatch rejection, got: {err:?}"
        );
    }

    #[test]
    fn test_c04_coinbase_amount_too_low_rejected() {
        use sentrix_primitives::block::Block;

        let mut bc = setup();
        let prev = bc.latest_block().unwrap().hash.clone();
        let ts = bc.latest_block().unwrap().timestamp + 1;

        // Underpaid coinbase: amount < reward
        let bad = Transaction::new_coinbase("v1".to_string(), 0, 1, ts);
        let block = Block::new(1, prev, vec![bad], "v1".to_string());

        let err = bc.add_block(block).unwrap_err();
        assert!(
            format!("{err:?}").contains("coinbase amount"),
            "expected amount-mismatch rejection, got: {err:?}"
        );
    }

    // C-04: coinbase.to_address must match block.validator. Enforced so that
    // a future refactor of credit() to use coinbase.to_address instead of
    // block.validator cannot redirect the subsidy to an attacker-chosen address.
    #[test]
    fn test_c04_coinbase_recipient_must_equal_validator() {
        use sentrix_primitives::block::Block;

        let mut bc = setup();
        let reward = bc.get_block_reward();
        let prev = bc.latest_block().unwrap().hash.clone();
        let ts = bc.latest_block().unwrap().timestamp + 1;

        // Coinbase paid to attacker while block is signed by authorized v1
        let bad = Transaction::new_coinbase("attacker".to_string(), reward, 1, ts);
        let block = Block::new(1, prev, vec![bad], "v1".to_string());

        let err = bc.add_block(block).unwrap_err();
        assert!(
            format!("{err:?}").contains("coinbase recipient"),
            "expected recipient-mismatch rejection, got: {err:?}"
        );
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

        // Init an in-memory trie (no MDBX — state_trie will be None without storage)
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

    // H-06: block with two txs sharing the same (sender, nonce) must be
    // rejected in Pass 1 before any state mutation.
    #[test]
    fn test_h06_duplicate_nonce_in_block_rejected() {
        use sentrix_primitives::block::Block;

        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000_000).unwrap();

        let reward = bc.get_block_reward();
        let prev = bc.latest_block().unwrap().hash.clone();
        let ts = bc.latest_block().unwrap().timestamp + 1;
        let coinbase = Transaction::new_coinbase("v1".to_string(), reward, 1, ts);

        // Two distinct txs (different recipients → different txids) sharing
        // the same nonce. Sender nonce starts at 0.
        let tx1 = Transaction::new(
            sender.clone(),
            "0x0000000000000000000000000000000000000001".to_string(),
            1,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        let tx2 = Transaction::new(
            sender.clone(),
            "0x0000000000000000000000000000000000000002".to_string(),
            1,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();

        assert_ne!(tx1.txid, tx2.txid, "precondition: txids must differ");
        assert_eq!(tx1.nonce, tx2.nonce, "precondition: nonces must match");

        let block = Block::new(1, prev, vec![coinbase, tx1, tx2], "v1".to_string());
        let err = bc.add_block(block).unwrap_err();
        assert!(
            format!("{err:?}").contains("duplicate (sender, nonce)"),
            "expected duplicate-nonce rejection, got: {err:?}"
        );
    }

    // H-06: block containing the exact same transaction twice (same txid)
    // must be rejected before any state mutation.
    #[test]
    fn test_h06_duplicate_txid_in_block_rejected() {
        use sentrix_primitives::block::Block;

        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000_000).unwrap();

        let reward = bc.get_block_reward();
        let prev = bc.latest_block().unwrap().hash.clone();
        let ts = bc.latest_block().unwrap().timestamp + 1;
        let coinbase = Transaction::new_coinbase("v1".to_string(), reward, 1, ts);

        let tx = Transaction::new(
            sender.clone(),
            "0x0000000000000000000000000000000000000001".to_string(),
            1,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();

        // Clone the same tx twice into a block.
        let block = Block::new(1, prev, vec![coinbase, tx.clone(), tx], "v1".to_string());
        let err = bc.add_block(block).unwrap_err();
        assert!(
            format!("{err:?}").contains("duplicate txid"),
            "expected duplicate-txid rejection, got: {err:?}"
        );
    }

    // C-03: if Pass 2 fails mid-commit, all state mutations must roll
    // back so the chain never observes a partial block-commit. Triggered
    // here by pre-funding the validator to the point where crediting
    // one block reward overflows u64; Pass 1 does not check the
    // validator's SRX balance against the coinbase reward, so the
    // failure surfaces inside Pass 2 at the very first mutation.
    #[test]
    fn test_c03_pass2_failure_rolls_back_state() {
        use sentrix_primitives::block::Block;

        let mut bc = setup();
        let reward = bc.get_block_reward();
        // Credit the validator to the ceiling so the next reward credit
        // (checked_add inside AccountDB::credit) will overflow.
        bc.accounts
            .credit("v1", u64::MAX - reward.saturating_sub(1))
            .unwrap();

        // Snapshot expected-invariant values from the pre-call state.
        let height_before = bc.height();
        let v1_balance_before = bc.accounts.get_balance("v1");
        let total_minted_before = bc.total_minted;
        let chain_len_before = bc.chain.len();

        let prev = bc.latest_block().unwrap().hash.clone();
        let ts = bc.latest_block().unwrap().timestamp + 1;
        let cb = Transaction::new_coinbase("v1".to_string(), reward, 1, ts);
        let block = Block::new(1, prev, vec![cb], "v1".to_string());

        let err = bc.add_block(block).unwrap_err();
        assert!(
            format!("{err:?}").to_lowercase().contains("overflow"),
            "expected overflow Err from Pass 2 coinbase credit, got: {err:?}"
        );

        // Rollback: every mutable field Pass 2 touches must be restored.
        assert_eq!(bc.height(), height_before, "chain len must be unchanged");
        assert_eq!(bc.chain.len(), chain_len_before);
        assert_eq!(
            bc.accounts.get_balance("v1"),
            v1_balance_before,
            "validator balance must not retain the partial credit"
        );
        assert_eq!(
            bc.total_minted, total_minted_before,
            "total_minted must not advance on failed Pass 2"
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
