// Pure-read block validation. The block-apply path (`add_block_impl`)
// re-runs all of these checks under the write lock as the single source
// of truth, but exposing the same checks under a read-lock-only entry
// point lets RPC readers (and the BFT engine) reject obviously-broken
// blocks without contending for the chain write lock. Backlog #P1.

use crate::blockchain::{Blockchain, is_spendable_sentrix_address, is_valid_sentrix_address};
use sentrix_primitives::block::Block;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use sentrix_primitives::transaction::TokenOp;
use std::collections::{HashMap, HashSet};

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

        // 2026-04-26 fix: use the runtime-aware voyager_mode_for() check
        // (ORs env-var fork-height with chain.db `voyager_activated` flag)
        // instead of the static is_voyager_height(). Prevents the 2026-04-26
        // mainnet stall where env var defaulted to u64::MAX, the static
        // check returned false, and Pioneer auth rejected legit skip-round
        // Voyager blocks. See `incidents/2026-04-26-voyager-fork-height-env-bug.md`.
        if !self.voyager_mode_for(expected_index)
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

        // Phase D Q4 (required-presence): post-fork at epoch boundaries, if
        // local LivenessTracker shows downtime evidence, the block MUST
        // contain a JailEvidenceBundle system tx. Otherwise the proposer
        // omitted a required system op — reject the block.
        //
        // Symmetry note: if local_evidence is empty, dispatch (block_executor
        // apply path) already rejects blocks that include a JailEvidenceBundle
        // whose claimed evidence diverges from local recompute. So we only
        // need to enforce presence here, not absence.
        //
        // Pre-fork (default JAIL_CONSENSUS_HEIGHT=u64::MAX): this branch is
        // unreachable, so behavior is unchanged on default builds.
        if Self::is_jail_consensus_height(expected_index)
            && sentrix_staking::epoch::EpochManager::is_epoch_boundary(expected_index)
        {
            let active_set = self.stake_registry.active_set.clone();
            // 2026-04-29: pass current_height so the deterministic
            // is_downtime_at check has the window cutoff it needs.
            let local_evidence = self
                .slashing
                .compute_jail_evidence(&active_set, expected_index);
            if !local_evidence.is_empty()
                && !block.transactions.iter().any(|tx| tx.is_system_tx())
            {
                return Err(SentrixError::InvalidBlock(format!(
                    "boundary block {} missing required JailEvidenceBundle \
                     (local evidence: {} entries — proposer omitted required system op)",
                    expected_index,
                    local_evidence.len()
                )));
            }
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
            // Phase D: system-emitted txs (JailEvidenceBundle from PROTOCOL_TREASURY)
            // skip standard nonce/balance validation. Auth is consensus-driven:
            // verified at apply via recompute-and-compare in block_executor.
            if tx.is_system_tx() {
                continue;
            }

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
                    op if op.is_nft_family() => {
                        // SRC-721 + SRC-1155 dispatch is gated by
                        // NFT_TOKENOP_HEIGHT fork. Pre-fork: reject. Wire
                        // format stable from this PR; storage layer +
                        // dispatch land in the follow-up PR.
                        if !Self::is_nft_tokenop_height(self.height() + 1) {
                            return Err(SentrixError::InvalidTransaction(
                                "NFT TokenOp dispatch is gated by \
                                 NFT_TOKENOP_HEIGHT fork (currently disabled)"
                                    .into(),
                            ));
                        }
                    }
                    _ => unreachable!("TokenOp variant handled above"),
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
}
