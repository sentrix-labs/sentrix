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
/// block-commit on disk-cache or in memory.
///
/// The `trie_root` field was added 2026-04-24 after the post-PR #184
/// audit found the original "self-heal" claim was wrong: the trie is
/// NOT rebuilt from `accounts` on each `update_trie_for_block` call —
/// insert/delete walk against the current in-memory root, so a partial
/// insert/delete left behind by a failed Pass 2 would silently combine
/// with the next block's updates and diverge from the restored
/// `accounts` state. Capturing the pre-mutation root and restoring it
/// on failure closes that gap. Nodes persisted by the failed block
/// remain in MDBX as unreachable orphans until the next
/// `prune(keep_versions)` GC pass.
pub(crate) struct BlockchainSnapshot {
    accounts: AccountDB,
    contracts: ContractRegistry,
    authority: AuthorityManager,
    mempool: VecDeque<Transaction>,
    total_minted: u64,
    chain_len: usize,
    /// Pre-Pass-2 trie root, captured only if a trie is initialised.
    /// Restored via `SentrixTrie::set_root` on Pass 2 failure so the
    /// next block's `update_trie_for_block` walks the correct state.
    trie_root: Option<[u8; 32]>,
}

/// Frontier Phase F-2 shadow observer. Calls into the F-1 scaffold's
/// `build_batches` and logs the resulting batch shape for the given
/// block. Read-only — does NOT mutate any state.
///
/// Gated by `SENTRIX_FRONTIER_F2_SHADOW=1` env var (handled at the
/// call site in `apply_block_pass2`). Default OFF — shadow mode is
/// opt-in observation only, useful for validating that the parallel
/// scheduler's output makes sense on real chain traffic before
/// committing to F-3 (real parallel apply).
///
/// The function intentionally short-circuits on empty blocks (only the
/// coinbase tx) to keep log volume sane on quiet chains.
fn shadow_observe_parallel_batching(block: &Block) {
    // Skip coinbase-only blocks — no useful batching signal from a
    // single-tx block.
    if block.tx_count() <= 1 {
        return;
    }

    // Encode each non-coinbase tx as a byte slice for build_batches.
    // The F-1 stub treats each tx as opaque bytes — it doesn't decode
    // sender/receiver, so we don't need the full tx structure. Real
    // F-3 implementation will need the actual sender/receiver/data.
    let tx_bytes: Vec<Vec<u8>> = block
        .transactions
        .iter()
        .skip(1) // skip coinbase
        .map(|tx| tx.txid.as_bytes().to_vec())
        .collect();

    let batches = crate::parallel::scheduler::build_batches(&tx_bytes, &block.validator);
    let batch_count = batches.len();
    let parallel_tx_count: usize = batches.iter().map(|b| b.tx_indices.len()).sum();

    tracing::info!(
        target: "frontier::f2_shadow",
        block_height = block.index,
        validator = %&block.validator[..12.min(block.validator.len())],
        tx_count = block.tx_count(),
        batch_count = batch_count,
        parallel_tx_count = parallel_tx_count,
        "F-2 shadow: build_batches output for block"
    );
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
            let local_evidence = self.slashing.compute_jail_evidence(&active_set);
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
        //
        // Offline replay bypass (SENTRIX_REPLAY_BYPASS_AUTHZ=1): skip the
        // round-robin slot check so genesis-to-tip replay can apply blocks
        // without reconstructing the full historical authority state. Used
        // by the rca_env_repro::replay_and_compare diagnostic harness.
        // Production validators MUST NOT set this (would let any address
        // produce blocks at any height — only safe when chain.db is offline
        // and we're rederiving state from authoritative block history).
        let bypass_authz = std::env::var("SENTRIX_REPLAY_BYPASS_AUTHZ").is_ok();
        // Same 2026-04-26 fix as the read-only `validate_block` path —
        // use voyager_mode_for() runtime-aware check.
        if !bypass_authz
            && !self.voyager_mode_for(expected_index)
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
                    op if op.is_nft_family() => {
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
        // updated without the tx that triggered it, etc.).
        //
        // As of 2026-04-24 the trie's in-memory root is ALSO snapshotted
        // (see `BlockchainSnapshot::trie_root`). An earlier comment here
        // claimed the trie "self-heals" because it's rebuilt from
        // `accounts` on each `update_trie_for_block` call — that claim
        // was wrong post-PR #184 (trie insert/delete walks the current
        // root; it is NOT recomputed from scratch). Without the
        // root snapshot + restore, a Pass 2 failure partway through
        // `update_trie_for_block` would leave the trie's in-memory
        // `root` pointing at a half-updated state while `accounts`
        // was reverted — silent divergence on the next block.
        let snap = BlockchainSnapshot {
            accounts: self.accounts.clone(),
            contracts: self.contracts.clone(),
            authority: self.authority.clone(),
            mempool: self.mempool.clone(),
            total_minted: self.total_minted,
            chain_len: self.chain.len(),
            trie_root: self.state_trie.as_ref().map(|t| t.root_hash()),
        };

        match self.apply_block_pass2(block) {
            Ok(()) => {
                // #252 / #244-revert: the earlier BACKLOG #16 "durable"
                // fix called `persist_block_durable` here under the
                // blockchain write-lock — three MDBX puts + one fsync
                // on every single commit. On a 4-validator Voyager
                // testnet that pushed BFT rounds past the 12s
                // precommit timeout under sustained load, causing the
                // prevote→nil-precommit flip livelock tracked in #252.
                //
                // The gap-formation risk it was guarding against
                // (BACKLOG #16, PR #226 sweep found 7,352 missing
                // `block:N` keys) is already covered without blocking
                // the hot path:
                //   - #243 turned the silent peer-block save_block
                //     failure into `error!` + Prometheus counter +
                //     alert rule, so gaps get caught at the moment
                //     of formation.
                //   - #225 taught `GetBlocks` to serve evicted blocks
                //     from MDBX, so gaps that do form can be healed
                //     via p2p sync instead of requiring an operator
                //     rsync.
                // Durability + observability + recovery without the
                // hot-lock fsync cost.
                //
                // `persist_block_durable` remains on `Blockchain` as
                // an opt-in tool — operator CLI ops, recovery
                // scripts, and explicit admin flows can still call
                // it when they genuinely need an immediate fsync.
                // The validator loop does not.
                Ok(())
            }
            Err(e) => {
                self.accounts = snap.accounts;
                self.contracts = snap.contracts;
                self.authority = snap.authority;
                self.mempool = snap.mempool;
                self.total_minted = snap.total_minted;
                self.chain.truncate(snap.chain_len);
                // Rewind trie to pre-Pass-2 root if one was captured.
                // Orphan nodes from the failed block's partial inserts
                // remain in MDBX but are unreachable from any committed
                // root; next `prune(keep_versions)` GCs them.
                if let (Some(trie), Some(root)) = (self.state_trie.as_mut(), snap.trie_root) {
                    trie.set_root(root);
                }
                Err(e)
            }
        }
    }

    /// C-03 Pass 2: applies all block mutations. Must only be called
    /// from `add_block` after Pass 1 has validated the block and the
    /// caller has taken a `BlockchainSnapshot` for rollback.
    fn apply_block_pass2(&mut self, block: Block) -> SentrixResult<()> {
        // Frontier Phase F-2 (shadow-mode wiring): when
        // SENTRIX_FRONTIER_F2_SHADOW=1, run the parallel-batching
        // scheduler over the block's transactions and log the result.
        // The scheduler does NOT mutate state — sequential apply below
        // still drives the actual block execution. This shadow path lets
        // operators observe the batching output on real chain traffic
        // without committing to parallel execution. When the
        // batches-vs-sequential equivalence has been validated for long
        // enough, F-3 (real parallel apply) replaces this stub with a
        // production code path.
        //
        // Default OFF: env var unset → zero-cost (the env-var read is
        // gated by a `var_os` check that doesn't allocate when missing).
        if std::env::var_os("SENTRIX_FRONTIER_F2_SHADOW")
            .is_some_and(|v| v == "1")
        {
            shadow_observe_parallel_batching(&block);
        }

        // Coinbase was validated in Pass 1; re-extract for mutation.
        let (coinbase_amount, coinbase_validator) = {
            let coinbase = block
                .coinbase()
                .ok_or_else(|| SentrixError::InvalidBlock("missing coinbase".to_string()))?;
            (coinbase.amount, block.validator.clone())
        };

        // Apply coinbase reward.
        //
        // V4 Step 3 / reward-v2 hard-fork: at/after VOYAGER_REWARD_V2_HEIGHT,
        // mint goes to PROTOCOL_TREASURY instead of directly to the proposer.
        // distribute_reward then updates in-registry accumulators
        // (pending_rewards + delegator_rewards) which are claims against
        // the treasury; ClaimRewards dispatch below transfers treasury →
        // claimer's balance on claim.
        //
        // Pre-fork behaviour is preserved exactly: proposer's balance gets
        // the full block_reward at commit time, same as v2.1.x today.
        //
        // Accumulator reset at fork activation: on the FIRST post-fork
        // block, zero out every pre-existing pending_rewards +
        // delegator_rewards entry. Pre-fork accumulator values represented
        // rewards that were ALREADY credited via coinbase → proposer
        // balance, so they are not real claims against the new treasury.
        // Reset keeps the supply invariant
        //   `accounts[TREASURY] == sum(pending_rewards) + sum(delegator_rewards)`
        // load-bearing from block 0 of the post-fork era onward.
        if Self::is_reward_v2_height(block.index)
            && !Self::is_reward_v2_height(block.index.saturating_sub(1))
        {
            self.reset_reward_accumulators_for_fork_activation();
            tracing::info!(
                "V4 reward-v2 fork activated at height {} — pre-fork pending_rewards + delegator_rewards cleared (supply invariant reset)",
                block.index
            );
        }

        let coinbase_recipient = if Self::is_reward_v2_height(block.index) {
            sentrix_primitives::transaction::PROTOCOL_TREASURY
        } else {
            coinbase_validator.as_str()
        };
        if std::env::var("SENTRIX_TRIE_TRACE").is_ok() {
            let pre = self.accounts.get_balance(coinbase_recipient);
            eprintln!(
                "[apply-trace] block {} coinbase: recipient={} amount={} pre_balance={}",
                block.index, coinbase_recipient, coinbase_amount, pre
            );
        }
        self.accounts.credit(coinbase_recipient, coinbase_amount)?;
        // saturating_add hardens against overflow on inflated-reward testnets
        // and future tunables. Production reward * MAX_HEIGHT is ~210M SRX
        // (= 21e15 sentri = ~0.11% of u64::MAX) so overflow is unreachable
        // at mainnet parameters, but the saturating form costs nothing and
        // matches the rest of this module (see line 780 reward summation).
        // If saturation ever fires, the next supply check will reject the
        // block via the MAX_SUPPLY invariant guard rather than silently wrap.
        self.total_minted = self.total_minted.saturating_add(coinbase_amount);
        if std::env::var("SENTRIX_TRIE_TRACE").is_ok() {
            let post = self.accounts.get_balance(coinbase_recipient);
            eprintln!(
                "[apply-trace] block {} post-coinbase balance={}",
                block.index, post
            );
        }

        // Apply all transactions
        let mut total_fee: u64 = 0;
        for tx in block.transactions.iter().skip(1) {
            // Phase D: system-emitted txs (JailEvidenceBundle from
            // PROTOCOL_TREASURY) skip account transfer + nonce increment.
            // They carry amount=0, fee=0 and a zero-balance "self-transfer"
            // would still bump PROTOCOL_TREASURY's nonce, polluting state.
            // Dispatch (staking_op match below) is the only state mutation.
            if !tx.is_system_tx() {
                if tx.is_evm_tx() {
                    // EVM tx: revm owns nonce + value + recipient credit
                    // when `execute_evm_tx_in_block` runs below. Native
                    // pass must NOT bump nonce or transfer value — doing
                    // so caused `NonceTooLow { tx, state }` in revm
                    // because state.nonce was already bumped by the time
                    // revm read it. See
                    // `audits/evm-create-nonce-bug-2026-04-27.md`.
                    // Only the fee is collected here (split 50/50
                    // burn/validator like every other tx).
                    self.accounts.charge_fee_only(&tx.from_address, tx.fee)?;
                } else {
                    self.accounts
                        .transfer(&tx.from_address, &tx.to_address, tx.amount, tx.fee)?;
                }
                // P1: checked_add — 5000 tx × max fee is far below u64::MAX
                // in practice, but the guard is cheap and prevents a silent
                // wrap if MAX_TX_PER_BLOCK or MIN_TX_FEE are ever tuned
                // upward past the implicit ceiling.
                total_fee = total_fee
                    .checked_add(tx.fee)
                    .ok_or_else(|| SentrixError::Internal("block total_fee overflow".to_string()))?;
            }

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
                    op if op.is_nft_family() => {
                        // Pass-2 apply path: NFT TokenOp dispatch is
                        // gated by NFT_TOKENOP_HEIGHT fork. Pre-fork:
                        // reject (Pass-1 already rejected; this is
                        // belt-and-suspenders). Storage layer handlers
                        // land in the follow-up PR.
                        if !Self::is_nft_tokenop_height(block.index) {
                            return Err(SentrixError::InvalidTransaction(
                                "NFT TokenOp dispatch is gated by \
                                 NFT_TOKENOP_HEIGHT fork (currently disabled)"
                                    .into(),
                            ));
                        }
                        // Post-fork dispatch land in follow-up PR; for
                        // now reject explicitly so accidentally enabling
                        // the fork doesn't silently apply non-existent
                        // handlers.
                        return Err(SentrixError::InvalidTransaction(
                            "NFT TokenOp dispatch handlers not yet wired \
                             (Phase B follow-up PR)"
                                .into(),
                        ));
                    }
                    _ => unreachable!("TokenOp variant handled above"),
                }
            }

            // V4 / staking-via-tx dispatch. Gated on
            // `is_reward_v2_height(block.index)` — pre-fork chains ignore
            // the op entirely (same as today's pre-V4 behaviour where
            // StakingOp has no runtime effect).
            //
            // Convention: staking txs MUST set `to_address = PROTOCOL_TREASURY`.
            // The outer `accounts.transfer` at the top of this loop
            // routes `tx.amount` into treasury as the escrow move for
            // Delegate / RegisterValidator. Other variants (Undelegate,
            // Redelegate, Unjail, ClaimRewards, SubmitEvidence) expect
            // `tx.amount = 0` — only the fee is consumed. We enforce
            // the `to_address == TREASURY` invariant inside dispatch
            // below; wrong address → Err → Pass 2 rollback.
            if Self::is_reward_v2_height(block.index)
                && let Some(staking_op) = sentrix_primitives::transaction::StakingOp::decode(&tx.data)
            {
                use sentrix_primitives::transaction::{PROTOCOL_TREASURY, StakingOp};
                if tx.to_address != PROTOCOL_TREASURY {
                    return Err(SentrixError::InvalidTransaction(format!(
                        "staking op tx must target PROTOCOL_TREASURY; got to_address={}",
                        tx.to_address
                    )));
                }
                match staking_op {
                    StakingOp::ClaimRewards => {
                        // Drain claimer's accumulator (delegator + validator).
                        let claimer = tx.from_address.clone();
                        let delegator_amount = self.stake_registry.take_delegator_rewards(&claimer);
                        let validator_amount = self
                            .stake_registry
                            .validators
                            .get_mut(&claimer)
                            .map(|v| std::mem::take(&mut v.pending_rewards))
                            .unwrap_or(0);
                        let total_claim = delegator_amount.saturating_add(validator_amount);
                        if total_claim > 0 {
                            self.accounts.transfer(
                                PROTOCOL_TREASURY,
                                &claimer,
                                total_claim,
                                0,
                            )?;
                        }
                    }
                    StakingOp::RegisterValidator {
                        self_stake,
                        commission_rate,
                        public_key,
                    } => {
                        // Outer transfer moved `tx.amount` sender → treasury.
                        // Enforce that amount exactly covers the declared self_stake.
                        if tx.amount != self_stake {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "RegisterValidator: tx.amount ({}) must equal self_stake ({})",
                                tx.amount, self_stake
                            )));
                        }
                        self.stake_registry.register_validator(
                            &tx.from_address,
                            self_stake,
                            commission_rate,
                            block.index,
                        )?;
                        // Mirror into authority so round-robin picks this
                        // validator for block production once activated.
                        self.authority.add_validator_unchecked(
                            tx.from_address.clone(),
                            format!("Community:{}", &tx.from_address[..10]),
                            public_key,
                        );
                    }
                    StakingOp::Delegate { validator, amount } => {
                        if tx.amount != amount {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "Delegate: tx.amount ({}) must equal delegation amount ({})",
                                tx.amount, amount
                            )));
                        }
                        self.stake_registry.delegate(
                            &tx.from_address,
                            &validator,
                            amount,
                            block.index,
                        )?;
                    }
                    StakingOp::Undelegate { validator, amount } => {
                        // No escrow movement on request — money stays in
                        // treasury until the unbonding queue matures at an
                        // epoch boundary. `tx.amount` must be 0.
                        if tx.amount != 0 {
                            return Err(SentrixError::InvalidTransaction(
                                "Undelegate: tx.amount must be 0 (amount is in data field)".into(),
                            ));
                        }
                        self.stake_registry.undelegate(
                            &tx.from_address,
                            &validator,
                            amount,
                            block.index,
                        )?;
                    }
                    StakingOp::Redelegate {
                        from_validator,
                        to_validator,
                        amount,
                    } => {
                        if tx.amount != 0 {
                            return Err(SentrixError::InvalidTransaction(
                                "Redelegate: tx.amount must be 0".into(),
                            ));
                        }
                        self.stake_registry.redelegate(
                            &tx.from_address,
                            &from_validator,
                            &to_validator,
                            amount,
                            block.index,
                        )?;
                    }
                    StakingOp::Unjail => {
                        if tx.amount != 0 {
                            return Err(SentrixError::InvalidTransaction(
                                "Unjail: tx.amount must be 0".into(),
                            ));
                        }
                        self.stake_registry
                            .unjail(&tx.from_address, block.index)?;
                    }
                    StakingOp::SubmitEvidence {
                        height,
                        block_hash_a,
                        block_hash_b,
                        signature_a,
                        signature_b,
                    } => {
                        if tx.amount != 0 {
                            return Err(SentrixError::InvalidTransaction(
                                "SubmitEvidence: tx.amount must be 0".into(),
                            ));
                        }
                        // Evidence targets the validator accused of
                        // double-signing. Slashing engine verifies the
                        // evidence + applies slash + tombstone if valid.
                        let evidence = sentrix_staking::slashing::DoubleSignEvidence {
                            validator: tx.from_address.clone(),
                            height,
                            block_hash_a,
                            block_hash_b,
                            signature_a,
                            signature_b,
                        };
                        let _ = self
                            .slashing
                            .process_double_sign(&mut self.stake_registry, &evidence);
                        // Bounty to submitter deferred — current design
                        // has no reporter field in SubmitEvidence (the
                        // submitter IS the offender in this naive shape).
                        // Follow-up: separate submitter + offender fields.
                    }
                    StakingOp::JailEvidenceBundle {
                        epoch: claimed_epoch,
                        epoch_start_block: _,
                        epoch_end_block: _,
                        evidence: claimed_evidence,
                    } => {
                        // Phase C: consensus-applied jail dispatch.
                        //
                        // Pre-fork (JAIL_CONSENSUS_HEIGHT=u64::MAX, default):
                        //   reject this op type as invalid (wire format stable
                        //   per Phase A but dispatch only valid post-fork).
                        // Post-fork: verify evidence + apply jail.
                        if !Self::is_jail_consensus_height(self.height()) {
                            return Err(SentrixError::InvalidTransaction(
                                "JailEvidenceBundle dispatch is gated by \
                                 JAIL_CONSENSUS_HEIGHT fork (currently disabled)"
                                    .into(),
                            ));
                        }

                        // Verify the cited epoch matches current epoch boundary.
                        // Boundary block's epoch should be `(height + 1) / EPOCH_LENGTH - 1`
                        // when the boundary is the LAST block of the epoch.
                        let current_epoch =
                            sentrix_staking::epoch::EpochManager::epoch_for_height(
                                self.height(),
                            );
                        if claimed_epoch != current_epoch {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "JailEvidenceBundle epoch {} != current epoch {}",
                                claimed_epoch, current_epoch
                            )));
                        }

                        // Verification: recompute evidence locally + compare.
                        // Determinism: each validator's LivenessTracker should
                        // produce the same evidence list (post asymmetric-record
                        // fix in PR #356 + #362). If a validator's local view
                        // differs, it'll reject this block — that's the safety
                        // mechanism (block can't finalize unless 2/3+ of
                        // stake-weighted validators agree on evidence).
                        let active_set = self.stake_registry.active_set.clone();
                        let local_evidence =
                            self.slashing.compute_jail_evidence(&active_set);

                        if local_evidence != *claimed_evidence {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "JailEvidenceBundle verification failed: \
                                 local recompute differs from claim \
                                 (local={} claimed={})",
                                local_evidence.len(),
                                claimed_evidence.len()
                            )));
                        }

                        // Verified — apply jail to each cited validator.
                        // jail() updates stake_registry (consensus state mutation).
                        let current_height = self.height();
                        for ev in claimed_evidence {
                            if let Err(e) = self.stake_registry.jail(
                                &ev.validator,
                                sentrix_staking::slashing::DOWNTIME_JAIL_BLOCKS,
                                current_height,
                            ) {
                                tracing::warn!(
                                    "JailEvidenceBundle apply: jail({}) failed: {}",
                                    ev.validator,
                                    e
                                );
                                // Don't fail the whole block — individual jail
                                // can fail (e.g., already-jailed). Log and continue.
                                continue;
                            }
                            // Reset liveness tracker for this validator (matches
                            // legacy check_liveness behavior).
                            self.slashing.liveness.reset(&ev.validator);
                        }
                    }
                    StakingOp::AddSelfStake { amount } => {
                        // Fork-gated: pre-`ADD_SELF_STAKE_HEIGHT` reject.
                        // Wire format is stable from the activation PR;
                        // gate keeps dispatch dormant until operator
                        // rollout (halt-all + simultaneous-start with
                        // env var set on every validator).
                        if !Self::is_add_self_stake_height(block.index) {
                            return Err(SentrixError::InvalidTransaction(
                                "AddSelfStake dispatch is gated by \
                                 ADD_SELF_STAKE_HEIGHT fork (currently \
                                 disabled)"
                                    .into(),
                            ));
                        }
                        // Authorization: only the validator itself may
                        // add to its own self_stake. tx.from_address is
                        // the validator's wallet; the fn must be called
                        // with the same address as the registry key.
                        // Outer accounts.transfer in apply-Pass-2 has
                        // already moved tx.amount from from_address →
                        // PROTOCOL_TREASURY at this point; dispatch only
                        // updates the registry. tx.amount must equal
                        // data.amount (escrow / dispatch agreement).
                        if tx.amount != amount {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "AddSelfStake: tx.amount ({}) must equal \
                                 stake amount ({})",
                                tx.amount, amount
                            )));
                        }
                        self.stake_registry
                            .add_self_stake(&tx.from_address, amount)?;
                        // Refresh active set so a previously-slashed
                        // validator that crosses MIN_SELF_STAKE re-enters
                        // proposer rotation immediately rather than
                        // waiting for the next epoch tick.
                        self.stake_registry.update_active_set();
                    }
                }
            }

            // Execute EVM transaction if present (data field starts with "EVM:")
            // tx_index skips coinbase at slot 0 — first real tx is index 1.
            // 2026-04-26: use voyager_mode_for() runtime-aware check (see #324).
            if tx.is_evm_tx() && self.voyager_mode_for(self.height()) {
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
            // TABLE_BLOOM is a query-side optimization (feeds
            // `eth_getLogs` fast-path); a put failure is non-consensus
            // (block still commits, logs still stored in TABLE_LOGS,
            // queries just fall back to linear scan). Log at WARN so
            // an MDBX write pathology shows up in journalctl instead of
            // sitting silent under a `let _ =`.
            if let Err(e) = storage.put(
                sentrix_storage::tables::TABLE_BLOOM,
                &block.index.to_be_bytes(),
                &bloom,
            ) {
                tracing::warn!(
                    "TABLE_BLOOM put failed for block {}: {} — eth_getLogs \
                     will fall back to linear scan for this block",
                    block.index,
                    e
                );
            }
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

        // Notify WebSocket / SSE subscribers — non-blocking, infallible
        // by trait contract. See sentrix-primitives::events.
        // The chain.last() is guaranteed Some here since we just pushed.
        if let Some(emitter) = &self.event_emitter
            && let Some(latest) = self.chain.last()
        {
            // EVM-compat: eth_subscribe(newHeads)
            emitter.emit_new_head(latest);
            // Sentrix-native: sentrix_subscribe(finalized)
            // BFT supplies the justification — count signers if present.
            let signers = latest
                .justification
                .as_ref()
                .map(|j| j.precommits.len())
                .unwrap_or(0);
            emitter.emit_finalized(latest.index, &latest.hash, signers);
        }

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
                            let block_index = last.index;

                            // Phase 1 mainnet activation legacy-compat (#268 RCA 2026-04-25):
                            // mainnet's pre-cutoff chain.db carries historical state_root
                            // artifacts from past repair operations (BACKLOG #16 7K-block gap
                            // patches, 2026-04-21 mainnet 3-way fork recovery, etc.) that
                            // v2.1.16+ binaries correctly cannot reproduce. To unblock
                            // mainnet upgrade without rebuilding chain.db, allow per-validator
                            // opt-in tolerance for the legacy region via env var.
                            //
                            // SENTRIX_LEGACY_VALIDATION_HEIGHT: blocks with index strictly
                            // less than this height are tolerated on mismatch (warn-only,
                            // received_root retained as-is so block hash chain stays
                            // intact). Blocks at or above the cutoff get strict rejection
                            // as today. Default unset = strict everywhere (current behaviour).
                            //
                            // See internal design doc
                            let legacy_cutoff = std::env::var("SENTRIX_LEGACY_VALIDATION_HEIGHT")
                                .ok()
                                .and_then(|s| s.parse::<u64>().ok())
                                .unwrap_or(0);

                            if legacy_cutoff > 0 && block_index < legacy_cutoff {
                                tracing::warn!(
                                    "LEGACY #1e tolerated at block {} (cutoff={}): received {} \
                                     vs computed {}. Pre-cutoff blocks carry historical \
                                     state_root artifacts; chain history preserved as-is.",
                                    block_index,
                                    legacy_cutoff,
                                    hex::encode(received_root),
                                    hex::encode(computed_root),
                                );
                                // Track in divergence tracker so legacy-region rate is visible
                                // in metrics, but don't fire the LOUD alarm since these are
                                // expected historical mismatches not active divergence.
                                self.divergence_tracker.record_rejection(block_index);
                                // Retain stamped (received) state_root so block hash chain
                                // stays intact. Caller's expectation of `block.state_root`
                                // continuity is preserved.
                                last.state_root = Some(received_root);
                                self.maybe_prune_trie();
                                return Ok(());
                            }

                            tracing::error!(
                                "CRITICAL #1e: state_root mismatch at block {} — received {} \
                                 vs computed {}. Local trie and peer's trie disagree on the \
                                 post-block state. Rejecting.",
                                block_index,
                                hex::encode(received_root),
                                hex::encode(computed_root),
                            );
                            // 2026-04-23 divergence rate-alarm: per-event ERROR
                            // line above is truthful but gets lost in log noise
                            // during a real divergence (~1/s). Record the
                            // rejection in the rolling tracker, which emits a
                            // LOUD rate-limited alarm pointing at the rsync
                            // recovery playbook when the rate crosses threshold.
                            // See `DivergenceTracker` in blockchain.rs for the
                            // full rationale.
                            self.divergence_tracker.record_rejection(block_index);
                            return Err(SentrixError::ChainValidationFailed(format!(
                                "state_root mismatch at block {}: received {}, computed {}",
                                block_index,
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

        // Reclaim historical trie storage on a periodic schedule. The trie's
        // insert/delete paths intentionally do NOT clean up replaced nodes
        // inline (that was unsound — see the 2026-04-20 missing-node
        // incident). prune() is the only sound GC, so it runs here.
        self.maybe_prune_trie();

        Ok(())
    }

    /// V4 reward-v2 fork activation reset. Zeros every pre-existing
    /// `pending_rewards` + the full `delegator_rewards` map. Pre-fork
    /// accumulator values represented rewards that were ALREADY credited
    /// via coinbase → proposer balance, so they are NOT real claims
    /// against the new `PROTOCOL_TREASURY`. Reset keeps the supply
    /// invariant
    ///   `accounts[TREASURY] == sum(pending_rewards) + sum(delegator_rewards)`
    /// load-bearing from block 0 of the post-fork era onward.
    ///
    /// Called exactly once by `apply_block_pass2` on the single
    /// transition block, gated by
    /// `is_reward_v2_height(block.index) && !is_reward_v2_height(block.index - 1)`.
    fn reset_reward_accumulators_for_fork_activation(&mut self) {
        for v in self.stake_registry.validators.values_mut() {
            v.pending_rewards = 0;
        }
        self.stake_registry.delegator_rewards.clear();
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
        use alloy_primitives::{B256, U256};
        use revm::context::TxEnv;
        use revm::primitives::TxKind;
        use revm::state::Bytecode;
        use sentrix_evm::database::{SentrixEvmDb, parse_sentrix_address};
        use sentrix_evm::executor::execute_tx_with_state;
        use sentrix_evm::gas::INITIAL_BASE_FEE;
        use sentrix_evm::writeback::commit_state_to_account_db;

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

        // Build EVM db from CURRENT AccountDB so the EVM sees real balances,
        // nonces, and contract code pointers — not a fresh InMemoryDB as pre-fix.
        // from_account_db handles sentri → wei scaling for all accounts.
        let mut evm_db = SentrixEvmDb::from_account_db(&self.accounts);
        let sender_nonce = self.accounts.get_nonce(&tx.from_address);

        // For Call txs, pre-load the target contract's bytecode + existing
        // storage slots. Without this, revm sees an empty-code address and
        // executes nothing useful. CREATE txs don't need pre-load — code
        // comes from tx data and gets deployed by revm itself.
        if let TxKind::Call(target_addr) = tx_kind {
            let target_str = format!("0x{}", hex::encode(target_addr.as_slice()));
            if let Some(target_account) = self.accounts.accounts.get(&target_str)
                && target_account.code_hash != sentrix_primitives::EMPTY_CODE_HASH
            {
                let code_hash_hex = hex::encode(target_account.code_hash);
                if let Some(bytecode) = self.accounts.get_contract_code(&code_hash_hex) {
                    let code = Bytecode::new_raw(alloy_primitives::Bytes::from(bytecode.clone()));
                    evm_db.insert_code(B256::from(target_account.code_hash), code);
                }
                // Pre-load storage slots whose key starts with "{target_addr}:".
                // Sorted by slot_hex so the insertion order is deterministic
                // across validator processes (HashMap iteration otherwise is
                // non-deterministic; same pattern as init_trie backfill).
                let prefix = format!("{}:", target_str);
                let mut slots: Vec<(String, Vec<u8>)> = self
                    .accounts
                    .contract_storage
                    .iter()
                    .filter(|(k, _)| k.starts_with(&prefix))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                slots.sort_by(|a, b| a.0.cmp(&b.0));
                for (key, value) in slots {
                    if let Some(slot_hex) = key.strip_prefix(&prefix)
                        && let Ok(slot) = U256::from_str_radix(slot_hex, 16)
                    {
                        // Left-pad short values with leading zeros to 32 bytes
                        // (big-endian high bits on the left). Note: normally
                        // values ARE already 32 bytes because we always store
                        // them that way in writeback, but defensive for any
                        // legacy entries or state-imports with short blobs.
                        let mut val_bytes = [0u8; 32];
                        let n = value.len().min(32);
                        val_bytes[32 - n..].copy_from_slice(&value[..n]);
                        let val = U256::from_be_slice(&val_bytes);
                        evm_db.insert_storage(target_addr, slot, val);
                    }
                }
            }
        }

        let evm_tx = TxEnv::builder()
            .caller(from_addr)
            .kind(tx_kind)
            .data(alloy_primitives::Bytes::from(calldata))
            .gas_limit(gas_limit)
            .gas_price(INITIAL_BASE_FEE as u128)
            // EVM CREATE/CALL nonce: revm checks `tx.nonce == state.nonce`
            // and bumps state.nonce internally. Native pass no longer
            // pre-bumps nonce for EVM txs (charge_fee_only above), so
            // `sender_nonce` here equals what the EVM tx claimed in its
            // RLP. Pre-fix this used `sender_nonce.saturating_sub(1)` to
            // compensate for native double-bump; that was a band-aid that
            // broke for fresh-wallet CREATE (nonce 0 saturating to 0,
            // state already at 1 → NonceTooLow). See
            // `audits/evm-create-nonce-bug-2026-04-27.md`.
            .nonce(sender_nonce)
            .chain_id(Some(tx.chain_id))
            .build()
            .unwrap_or_default();

        match execute_tx_with_state(evm_db, evm_tx, INITIAL_BASE_FEE, self.chain_id) {
            Ok((receipt, state)) => {
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

                        // Notify WebSocket subscribers — eth_subscribe(logs).
                        // Convert StoredLog to the trait-friendly LogData
                        // shape (sentrix-primitives doesn't depend on
                        // sentrix-evm so the trait can't take StoredLog
                        // directly). Non-blocking, infallible.
                        if let Some(emitter) = &self.event_emitter {
                            let log_data = sentrix_primitives::events::LogData {
                                block_height,
                                block_hash: block_hash_hex.to_string(),
                                tx_hash: tx.txid.clone(),
                                tx_index,
                                log_index: log_idx as u32,
                                address: stored.address,
                                topics: stored.topics.clone(),
                                data: stored.data.clone(),
                            };
                            emitter.emit_log(&log_data);
                        }
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

                // ── EVM state writeback (closes Bug #1) ─────────────
                // On success, commit every touched account's balance,
                // nonce, storage slots, and (for CREATE) bytecode back
                // to AccountDB so the next tx / next block sees them.
                // Skip on revert — EVM spec requires state changes to
                // be discarded on revert; the gas fee was already
                // debited by the native Pass-1 .transfer() call above,
                // so there's nothing else to do.
                //
                // The EIP-170 guard above may flip the tx to failed
                // even for a successful revm receipt — in that case
                // we skip the writeback too. Check mark AFTER the
                // EIP-170 block ran so "was marked failed" is
                // authoritative.
                if receipt.success && !self.accounts.is_evm_tx_failed(&tx.txid) {
                    commit_state_to_account_db(&state, &mut self.accounts)?;
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

    /// V4 Step 3 regression: default reward-v2 fork height must be
    /// u64::MAX so a node without `VOYAGER_REWARD_V2_HEIGHT` env var
    /// never activates the treasury-escrow path. This pins the
    /// mainnet-safe default and prevents a silent consensus drift if
    /// someone inadvertently flips the default.
    #[test]
    fn test_v4_reward_v2_fork_height_default_disabled() {
        // Phase D tests now also touch VOYAGER_REWARD_V2_HEIGHT, so we need
        // crate-wide serialization to avoid races on the global env table.
        let _guard = crate::test_util::env_test_lock();
        unsafe {
            std::env::remove_var("VOYAGER_REWARD_V2_HEIGHT");
        }
        assert_eq!(
            crate::blockchain::get_reward_v2_fork_height(),
            u64::MAX,
            "default must keep mainnet on pre-V4 behaviour until operator opts in"
        );
        assert!(
            !Blockchain::is_reward_v2_height(0),
            "height 0 must be pre-fork with default env"
        );
        assert!(
            !Blockchain::is_reward_v2_height(1_000_000_000),
            "even huge heights must be pre-fork with default env"
        );
    }

    /// V4 Step 3 regression: the fork-activation reset must zero EVERY
    /// validator's `pending_rewards` and clear the full
    /// `delegator_rewards` map. Pre-fork values represented rewards
    /// already credited via coinbase → proposer; carrying them forward
    /// past the fork would double-mint when a `ClaimRewards` tx drains
    /// treasury for stale pre-fork claims.
    ///
    /// Scope: unit-tests the helper in isolation. The gate predicate
    /// `is_reward_v2_height(h) && !is_reward_v2_height(h-1)` fires at
    /// exactly one block per fork boundary; verifying the gate under
    /// real block production belongs to the clean-testnet bake (see
    /// CHANGELOG v2.1.19).
    #[test]
    fn test_v4_accumulator_reset_zeros_pre_fork_state() {
        use sentrix_staking::staking::ValidatorStake;

        let mut bc = setup();

        bc.stake_registry.validators.insert(
            "val_a".to_string(),
            ValidatorStake {
                address: "val_a".to_string(),
                self_stake: 15_000,
                total_delegated: 0,
                commission_rate: 1000,
                max_commission_rate: 2000,
                is_jailed: false,
                jail_until: 0,
                is_tombstoned: false,
                blocks_signed: 0,
                blocks_missed: 0,
                pending_rewards: 12_345,
                registration_height: 0,
                last_commission_change_height: 0,
            },
        );
        bc.stake_registry.validators.insert(
            "val_b".to_string(),
            ValidatorStake {
                address: "val_b".to_string(),
                self_stake: 15_000,
                total_delegated: 0,
                commission_rate: 1000,
                max_commission_rate: 2000,
                is_jailed: false,
                jail_until: 0,
                is_tombstoned: false,
                blocks_signed: 0,
                blocks_missed: 0,
                pending_rewards: 999,
                registration_height: 0,
                last_commission_change_height: 0,
            },
        );
        bc.stake_registry
            .delegator_rewards
            .insert("del_x".to_string(), 500);
        bc.stake_registry
            .delegator_rewards
            .insert("del_y".to_string(), 250);

        bc.reset_reward_accumulators_for_fork_activation();

        assert_eq!(
            bc.stake_registry.validators["val_a"].pending_rewards, 0,
            "val_a pending_rewards must be zeroed"
        );
        assert_eq!(
            bc.stake_registry.validators["val_b"].pending_rewards, 0,
            "val_b pending_rewards must be zeroed"
        );
        assert!(
            bc.stake_registry.delegator_rewards.is_empty(),
            "delegator_rewards must be fully cleared"
        );
        assert_eq!(
            bc.stake_registry.validators.len(),
            2,
            "validators themselves must NOT be removed — only their reward accumulators zeroed"
        );
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

    /// Phase D Step 5-lite: end-to-end exercise of the consensus-jail flow
    /// in single-validator mode. Drives:
    ///   1. proposer-side helper (Step 1+2): build_jail_evidence_system_tx
    ///   2. block_producer wire-up (Step 3): tx[1] = system tx
    ///   3. Pass-1 skip (Step 4a): system tx bypasses nonce/balance checks
    ///   4. Pass-1 Q4 required-presence: ours has system tx, passes
    ///   5. Pass-2 skip (Step 4b): no transfer for system tx
    ///   6. Phase C dispatch: recompute-and-compare matches (single validator,
    ///      same LivenessTracker), jail applied to stake_registry
    ///
    /// Asserts: post-add_block the cited validator is jailed in stake_registry.
    #[test]
    fn test_phase_d_e2e_emit_validate_apply_jail() {
        let _guard = crate::test_util::env_test_lock();
        // Both forks active (consensus-jail dispatch needs reward_v2 active
        // since dispatch lives inside `if is_reward_v2_height(...)`)
        unsafe {
            std::env::set_var("VOYAGER_REWARD_V2_HEIGHT", "0");
            std::env::set_var("JAIL_CONSENSUS_HEIGHT", "0");
        }

        let mut bc = setup();
        bc.voyager_activated = true; // bypass Pioneer auth in validate_block

        // Inject a downer in active_set + populate liveness window with
        // all-missed records so is_downtime triggers.
        let downer = "0xfeedfacefeedfacefeedfacefeedfacefeedface".to_string();
        bc.stake_registry.active_set = vec![downer.clone()];
        bc.stake_registry
            .register_validator(&downer, sentrix_staking::staking::MIN_SELF_STAKE, 1000, 0)
            .expect("register downer");
        let window = sentrix_staking::slashing::LIVENESS_WINDOW;
        for h in 0..window {
            bc.slashing.liveness.record(&downer, h, false);
        }

        // Pad chain to (boundary - 1) so next produced block lands on boundary.
        let target_height = sentrix_staking::epoch::EPOCH_LENGTH - 2;
        let prev_hash = bc.latest_block().unwrap().hash.clone();
        let pad = sentrix_primitives::block::Block::new(
            target_height,
            prev_hash,
            vec![Transaction::new_coinbase(
                "v1".into(),
                0,
                target_height,
                1_700_000_000,
            )],
            "v1".into(),
        );
        bc.chain.push(pad);

        // Pre-condition: downer not jailed
        let pre_jailed = bc
            .stake_registry
            .get_validator(&downer)
            .map(|v| v.is_jailed)
            .unwrap_or(false);
        assert!(!pre_jailed, "downer must not be jailed pre-emission");

        // Drive proposer → emits block with system tx at [1]
        let block = bc.create_block_voyager("v1").expect("create_block_voyager");
        assert_eq!(block.transactions.len(), 2);
        assert!(block.transactions[1].is_system_tx());

        // add_block runs full Pass-1 + Pass-2 + dispatch + state mutation
        bc.add_block(block).expect("add_block must accept Phase D system tx");

        // Post-condition: downer jailed
        let post_jailed = bc
            .stake_registry
            .get_validator(&downer)
            .map(|v| v.is_jailed)
            .unwrap_or(false);
        assert!(
            post_jailed,
            "downer must be jailed after consensus-jail dispatch applied"
        );

        unsafe {
            std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
            std::env::remove_var("VOYAGER_REWARD_V2_HEIGHT");
        }
    }

    /// Phase D Q4 required-presence: post-fork at boundary with downtime
    /// evidence locally, a block missing the JailEvidenceBundle is rejected.
    #[test]
    fn test_phase_d_q4_required_presence_rejects_missing_bundle() {
        let _guard = crate::test_util::env_test_lock();
        unsafe {
            std::env::set_var("VOYAGER_REWARD_V2_HEIGHT", "0");
            std::env::set_var("JAIL_CONSENSUS_HEIGHT", "0");
        }

        let mut bc = setup();
        bc.voyager_activated = true;

        // Inject downer + downtime
        let downer = "0xfeedfacefeedfacefeedfacefeedfacefeedface".to_string();
        bc.stake_registry.active_set = vec![downer.clone()];
        bc.stake_registry
            .register_validator(&downer, sentrix_staking::staking::MIN_SELF_STAKE, 1000, 0)
            .unwrap();
        let window = sentrix_staking::slashing::LIVENESS_WINDOW;
        for h in 0..window {
            bc.slashing.liveness.record(&downer, h, false);
        }

        // Pad to boundary - 1
        let target_height = sentrix_staking::epoch::EPOCH_LENGTH - 2;
        let prev_hash = bc.latest_block().unwrap().hash.clone();
        let pad = sentrix_primitives::block::Block::new(
            target_height,
            prev_hash,
            vec![Transaction::new_coinbase(
                "v1".into(),
                0,
                target_height,
                1_700_000_000,
            )],
            "v1".into(),
        );
        bc.chain.push(pad);

        // Hand-craft a boundary block WITHOUT a system tx (simulates faulty
        // proposer that omits the required JailEvidenceBundle).
        let boundary = sentrix_staking::epoch::EPOCH_LENGTH - 1;
        let reward = bc.get_block_reward();
        let coinbase = Transaction::new_coinbase("v1".into(), reward, boundary, 1_700_000_001);
        let bad_block = sentrix_primitives::block::Block::new(
            boundary,
            bc.latest_block().unwrap().hash.clone(),
            vec![coinbase],
            "v1".into(),
        );

        let err = bc
            .validate_block(&bad_block)
            .expect_err("missing JailEvidenceBundle at boundary post-fork must reject");
        assert!(
            format!("{err:?}").contains("missing required JailEvidenceBundle"),
            "expected required-presence rejection; got: {err:?}"
        );

        unsafe {
            std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
            std::env::remove_var("VOYAGER_REWARD_V2_HEIGHT");
        }
    }
}
