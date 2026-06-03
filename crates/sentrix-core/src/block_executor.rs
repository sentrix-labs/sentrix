// block_executor.rs - Sentrix — Block validation and commit (two-pass)

// Per-tx EVM execution and the read-only validate path live in their
// own files so this one can stay focused on the block-level apply
// hot path.
mod evm;
mod validate;

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
    /// Native NFT registry snapshot — restored alongside `contracts` on a
    /// Pass-2 failure so a half-applied NFT op never survives a rolled-back
    /// block (same atomicity contract as SRC-20 contract state).
    nft_registry: sentrix_nft::NftRegistry,
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

        // 2026-04-30: peer-broadcast finalization justification check.
        // Pins the receiver-side gap from the 2026-04-28 validator
        // block-773012 divergence runbook: every post-Voyager peer
        // block ships the proposer's
        // BlockJustification with the precommits that finalized it.
        // Trusting that blob unconditionally lets a peer with stake-
        // registry drift (or a Byzantine peer) ship a block whose
        // precommits do NOT reach 2/3+ on our view of the active set —
        // we'd then silently apply a fork. Verify the precommit stake
        // weights sum to our local supermajority threshold before
        // accepting. Self-produced blocks skip the check because their
        // justification was just built locally from votes we collected
        // ourselves; bypass-authz replay also skips so genesis-to-tip
        // replay can apply historical blocks even after the active
        // set has drifted past their original finalization view.
        if !bypass_authz
            && self.source_for_current_add == BlockSource::Peer
            && self.voyager_mode_for(expected_index)
            && let Some(j) = block.justification.as_ref()
        {
            let total_active_stake: u64 = self
                .stake_registry
                .active_set
                .iter()
                .filter_map(|a| self.stake_registry.get_validator(a))
                .map(|v| v.total_stake())
                .sum();

            // Strict justification verification (audit halt #9 fix,
            // 2026-05-07). Pre-fork the gate verified ONLY the
            // arithmetic of stake-weight (peer-supplied number summed
            // against receiver's threshold). Signatures themselves
            // were never recovered — a peer with drifted active-set
            // view (post-halt-recovery, simul-start race) could ship
            // a block whose precommits were signed by the wrong
            // keys or weighted from a different registry snapshot;
            // receiver would accept silently → fork. Halt #9 was
            // exactly this. Post-fork: recover every precommit's
            // signer via `Precommit::signing_payload_for_height` +
            // `recover_signer`, match against claimed validator,
            // sum verified-stake using the RECEIVER's own registry,
            // reject if threshold not met.
            if Self::is_strict_justification_height(expected_index) {
                use sentrix_bft::messages::{Precommit, recover_signer};
                // 2026-05-31 add: block ↔ justification hash consistency.
                // A block with `block.hash=X, justification.block_hash=Y`
                // and precommits validly signed for Y passes signature
                // recovery + threshold checks because every check below
                // uses `j.block_hash` as the signing payload. But the
                // STORED block has hash X, so the chain accumulates
                // internally-inconsistent blocks. Discovered live on
                // 2026-05-31 testnet h=5817132: val4's self-produced
                // block had hash=H2 (post-state_root recompute) while
                // its embedded justification still referenced H1
                // (pre-recompute hash collected by engine). Strictly
                // gate so legacy blocks pre-fork stay accepted, and
                // the consensus stack must produce hash-consistent
                // blocks going forward. The producer-side architectural
                // fix (block.hash committed BEFORE state_root stamp,
                // OR speculative apply at propose time) ships separately;
                // this check stops the bad block from being accepted
                // by receivers in the meantime.
                if j.block_hash != block.hash {
                    return Err(SentrixError::InvalidBlock(format!(
                        "block {} hash mismatch: block.hash={} != \
                         justification.block_hash={} — peer block has \
                         inconsistent block/justification refs, refusing \
                         to apply",
                        expected_index, block.hash, j.block_hash,
                    )));
                }
                let mut verified_stake: u64 = 0;
                let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
                for p in &j.precommits {
                    // Reject duplicate validators in the justification —
                    // pre-fix the dedup was implicit via the stake-weight
                    // sum but now we tally per validator.
                    if !seen.insert(p.validator.as_str()) {
                        return Err(SentrixError::InvalidBlock(format!(
                            "block {} justification has duplicate precommit from {}",
                            expected_index, p.validator,
                        )));
                    }
                    // Validator must be in our active set.
                    let val = match self.stake_registry.get_validator(&p.validator) {
                        Some(v) => v,
                        None => {
                            return Err(SentrixError::InvalidBlock(format!(
                                "block {} justification cites validator {} not in our active \
                                 set — peer-finalised view diverges, refusing to apply",
                                expected_index, p.validator,
                            )));
                        }
                    };
                    // Recover signer + match against claimed validator.
                    let payload = Precommit::signing_payload_for_height(
                        j.height,
                        j.round,
                        &Some(j.block_hash.clone()),
                        self.chain_id,
                    );
                    let recovered = match recover_signer(&payload, &p.signature) {
                        Ok(addr) => addr,
                        Err(_) => {
                            return Err(SentrixError::InvalidBlock(format!(
                                "block {} precommit from {} has invalid signature",
                                expected_index, p.validator,
                            )));
                        }
                    };
                    if recovered.to_lowercase() != p.validator.to_lowercase() {
                        return Err(SentrixError::InvalidBlock(format!(
                            "block {} precommit from {} signed by different key (recovered={})",
                            expected_index, p.validator, recovered,
                        )));
                    }
                    verified_stake = verified_stake.saturating_add(val.total_stake());
                }
                if total_active_stake == 0 {
                    return Err(SentrixError::InvalidBlock(format!(
                        "block {} arrived during cold-start (total_active_stake=0); strict-\
                         justification fork active so we refuse to bypass — bypass_authz \
                         replay should be used to catch up",
                        expected_index,
                    )));
                }
                let threshold = sentrix_primitives::supermajority_threshold(total_active_stake);
                if verified_stake < threshold {
                    return Err(SentrixError::InvalidBlock(format!(
                        "block {} verified-stake {} < threshold {} (total_active_stake={}, \
                         signers={}) — peer-finalised view diverges from ours, refusing \
                         to apply",
                        expected_index,
                        verified_stake,
                        threshold,
                        total_active_stake,
                        j.signer_count(),
                    )));
                }
            } else if total_active_stake > 0 && !j.has_supermajority(total_active_stake) {
                // Pre-fork legacy gate (stake-weight arithmetic only).
                // total_active_stake==0 cold-start bypass preserved for
                // bit-identical chain history.
                return Err(SentrixError::InvalidBlock(format!(
                    "block {} justification stake {} is below the local supermajority \
                     threshold {} (total_active_stake={}, signers={}) — peer-finalised \
                     view diverges from ours, refusing to apply",
                    expected_index,
                    j.total_stake(),
                    sentrix_primitives::supermajority_threshold(total_active_stake),
                    total_active_stake,
                    j.signer_count(),
                )));
            }
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
        // Pass-1 NFT dry-run state. Lazily cloned from `self.nft_registry`
        // on the first NFT op so a deploy-then-mint within the same block
        // validates against the in-block working registry, exactly mirroring
        // what Pass-2 will mutate — without touching real state. Mirrors the
        // working_balances / working_nonces clone pattern used for SRX.
        let mut working_nft: Option<sentrix_nft::NftRegistry> = None;

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
                        // Dry-run the op against a working clone so any
                        // domain error (bad auth, unknown collection,
                        // soulbound transfer, supply cap, id reuse) fails
                        // here without mutating real state. Pass-2 re-applies
                        // identically as the source of truth.
                        let working = working_nft.get_or_insert_with(|| self.nft_registry.clone());
                        crate::nft::apply_nft_token_op(working, op, &tx.from_address, &tx.txid)?;
                    }
                    // Audit L3 (2026-05-06): pre-fix this was
                    // `unreachable!("TokenOp variant handled above")`,
                    // which would `panic!` + `std::process::abort()` if a
                    // future TokenOp variant landed without being added to
                    // an arm above. Reject as InvalidTransaction so the
                    // failing tx surfaces a normal block-apply error
                    // instead of taking the validator down.
                    _ => {
                        return Err(SentrixError::InvalidTransaction(
                            "unhandled TokenOp variant — add a Pass-1 \
                             validation arm above before shipping"
                                .into(),
                        ));
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
            nft_registry: self.nft_registry.clone(),
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
                self.nft_registry = snap.nft_registry;
                self.authority = snap.authority;
                self.mempool = snap.mempool;
                // Audit M6: snapshot-restored `mempool` is the
                // authoritative state; rebuild sidecars before any
                // subsequent admission reads them.
                self.rebuild_mempool_sidecars();
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
        // Per-block apply profile. Env-gated zero-cost when off
        // (var_os check + Instant::now allocations skipped). When
        // SENTRIX_APPLY_PROFILE=1, emits one tracing::info line per
        // block at function exit with phase breakdowns:
        //   apply-profile h=<height> txs=<n> tx_apply=<ms> trie=<ms> post=<ms> total=<ms>
        // tx_apply = coinbase + tx loop + state mutations (everything
        // before update_trie_for_block). trie = update_trie_for_block
        // duration. post = state_root stamp + state_root check + prune
        // dispatch. The prune itself runs on a background thread (v2.2.4)
        // so its walk time is NOT included here.
        let apply_profile = std::env::var_os("SENTRIX_APPLY_PROFILE").is_some_and(|v| v == "1");
        let profile_t0 = if apply_profile {
            Some(std::time::Instant::now())
        } else {
            None
        };
        // Capture before `block` gets moved into self.chain mid-fn.
        let profile_height = block.index;
        let profile_txs = block.transactions.len();

        // EXTENDED_TOUCH_LIST fork (2026-05-07): clear the per-block
        // accumulator at start so each block sees a fresh set. Mutators
        // populate `accounts.touched_in_block` during apply;
        // `update_trie_for_block` post-fork drains it into the trie
        // touch list. Pre-fork the field is populated but ignored — the
        // clear keeps memory bounded regardless.
        self.accounts.clear_touched_in_block();

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
        if std::env::var_os("SENTRIX_FRONTIER_F2_SHADOW").is_some_and(|v| v == "1") {
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
                total_fee = total_fee.checked_add(tx.fee).ok_or_else(|| {
                    SentrixError::Internal("block total_fee overflow".to_string())
                })?;
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
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_token_op(&sentrix_primitives::events::TokenOpEvent {
                                op: "deploy".to_string(),
                                contract: tx.txid.clone(),
                                from: tx.from_address.clone(),
                                to: String::new(),
                                amount: supply,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                        }
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
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_token_op(&sentrix_primitives::events::TokenOpEvent {
                                op: "transfer".to_string(),
                                contract: contract.clone(),
                                from: tx.from_address.clone(),
                                to: to.clone(),
                                amount,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                        }
                    }
                    TokenOp::Burn { contract, amount } => {
                        self.contracts
                            .execute_burn(&contract, &tx.from_address, amount)?;
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_token_op(&sentrix_primitives::events::TokenOpEvent {
                                op: "burn".to_string(),
                                contract: contract.clone(),
                                from: tx.from_address.clone(),
                                to: String::new(),
                                amount,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                        }
                    }
                    TokenOp::Mint {
                        contract,
                        to,
                        amount,
                    } => {
                        self.contracts
                            .execute_mint(&contract, &tx.from_address, &to, amount)?;
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_token_op(&sentrix_primitives::events::TokenOpEvent {
                                op: "mint".to_string(),
                                contract: contract.clone(),
                                from: tx.from_address.clone(),
                                to: to.clone(),
                                amount,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                        }
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
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_token_op(&sentrix_primitives::events::TokenOpEvent {
                                op: "approve".to_string(),
                                contract: contract.clone(),
                                from: tx.from_address.clone(),
                                to: spender.clone(),
                                amount,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                        }
                    }
                    op if op.is_nft_family() => {
                        // Pass-2 apply path: NFT TokenOp dispatch is gated by
                        // NFT_TOKENOP_HEIGHT fork. Pre-fork: reject (Pass-1
                        // already rejected; belt-and-suspenders).
                        if !Self::is_nft_tokenop_height(block.index) {
                            return Err(SentrixError::InvalidTransaction(
                                "NFT TokenOp dispatch is gated by \
                                 NFT_TOKENOP_HEIGHT fork (currently disabled)"
                                    .into(),
                            ));
                        }
                        // Authority is the authenticated sender
                        // (`tx.from_address`); the deterministic collection
                        // seed is `tx.txid` (SRC-20 precedent). On Err the
                        // Pass-2 snapshot restores `nft_registry`, so a
                        // failed NFT op leaves no partial state.
                        let events = crate::nft::apply_nft_token_op(
                            &mut self.nft_registry,
                            &op,
                            &tx.from_address,
                            &tx.txid,
                        )?;
                        if let Some(emitter) = &self.event_emitter {
                            for ev in &events {
                                emitter.emit_token_op(&crate::nft::nft_event_to_token_op_event(
                                    ev,
                                    &tx.txid,
                                    block.index,
                                ));
                            }
                        }
                    }
                    // Audit L3 sister site (Pass-2 dispatch). Same
                    // rationale as the Pass-1 fallthrough above — fail
                    // the tx, don't abort the validator.
                    _ => {
                        return Err(SentrixError::InvalidTransaction(
                            "unhandled TokenOp variant in dispatch — add \
                             a handler arm above before shipping"
                                .into(),
                        ));
                    }
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
                && let Some(staking_op) =
                    sentrix_primitives::transaction::StakingOp::decode(&tx.data)
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
                        // Audit H5 (2026-05-06): peek-then-transfer-then-drain.
                        // Pre-fix this drained the accumulators FIRST
                        // (`take_delegator_rewards` + `std::mem::take` on
                        // pending_rewards), then attempted the
                        // PROTOCOL_TREASURY → claimer transfer. If the
                        // transfer failed (treasury transient
                        // insufficiency, error in transfer arithmetic),
                        // Pass-2 rollback restores `accounts` from
                        // BlockchainSnapshot but the snapshot doesn't
                        // capture stake_registry — accumulators stay
                        // zero, claimer's rewards permanently lost with
                        // no error surfaced to the wallet.
                        //
                        // Now: peek both accumulators without mutation,
                        // run the transfer, drain on success only. If
                        // the transfer Errs, accumulators stay intact
                        // and the claimer can re-submit later.
                        let claimer = tx.from_address.clone();
                        let delegator_amount = self.stake_registry.peek_delegator_rewards(&claimer);
                        let validator_amount = self
                            .stake_registry
                            .validators
                            .get(&claimer)
                            .map(|v| v.pending_rewards)
                            .unwrap_or(0);
                        let total_claim = delegator_amount.saturating_add(validator_amount);
                        if total_claim > 0 {
                            // Transfer first; only drain on success.
                            self.accounts
                                .transfer(PROTOCOL_TREASURY, &claimer, total_claim, 0)?;
                            // Transfer succeeded — drain accumulators.
                            // `take_delegator_rewards` removes the entry
                            // entirely (matches semantics — claimer
                            // collected everything available at peek).
                            // For the validator side we explicitly zero
                            // `pending_rewards` rather than std::mem::take
                            // because we already consumed the value at
                            // peek and the assignment is clearer about
                            // the post-condition.
                            let _ = self.stake_registry.take_delegator_rewards(&claimer);
                            if let Some(v) = self.stake_registry.validators.get_mut(&claimer) {
                                v.pending_rewards = 0;
                            }
                        }
                        // Phase 3 WS: notify sentrix_subscribe(stakingOps).
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_staking_op(&sentrix_primitives::events::StakingOpEvent {
                                op: "claim_rewards".to_string(),
                                validator: claimer.clone(),
                                delegator: claimer.clone(),
                                amount: total_claim,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
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
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_staking_op(&sentrix_primitives::events::StakingOpEvent {
                                op: "register_validator".to_string(),
                                validator: tx.from_address.clone(),
                                delegator: tx.from_address.clone(),
                                amount: self_stake,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                        }
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
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_staking_op(&sentrix_primitives::events::StakingOpEvent {
                                op: "delegate".to_string(),
                                validator: validator.clone(),
                                delegator: tx.from_address.clone(),
                                amount,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                        }
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
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_staking_op(&sentrix_primitives::events::StakingOpEvent {
                                op: "undelegate".to_string(),
                                validator: validator.clone(),
                                delegator: tx.from_address.clone(),
                                amount,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                        }
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
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_staking_op(&sentrix_primitives::events::StakingOpEvent {
                                op: format!("redelegate:{}->{}", from_validator, to_validator),
                                validator: to_validator.clone(),
                                delegator: tx.from_address.clone(),
                                amount,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                        }
                    }
                    StakingOp::Unjail => {
                        if tx.amount != 0 {
                            return Err(SentrixError::InvalidTransaction(
                                "Unjail: tx.amount must be 0".into(),
                            ));
                        }
                        self.stake_registry.unjail(&tx.from_address, block.index)?;
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_staking_op(&sentrix_primitives::events::StakingOpEvent {
                                op: "unjail".to_string(),
                                validator: tx.from_address.clone(),
                                delegator: tx.from_address.clone(),
                                amount: 0,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                        }
                    }
                    StakingOp::SubmitEvidence {
                        height,
                        block_hash_a,
                        block_hash_b,
                        signature_a,
                        signature_b,
                        offender,
                    } => {
                        if tx.amount != 0 {
                            return Err(SentrixError::InvalidTransaction(
                                "SubmitEvidence: tx.amount must be 0".into(),
                            ));
                        }
                        // Audit H4 (2026-05-06): submitter / offender split.
                        // Pre-fix `evidence.validator` was forced to
                        // `tx.from_address`, meaning the submitter
                        // accused themselves. Now `offender` is a
                        // dedicated field on the variant; reject the
                        // tx if it's empty so the wire-format change
                        // is mandatory going forward (back-compat
                        // payloads with empty offender can't slash
                        // anyone, which is the desired behaviour
                        // anyway — JAIL_CONSENSUS_HEIGHT=u64::MAX
                        // already rejects auto-jail dispatch).
                        if offender.is_empty() {
                            return Err(SentrixError::InvalidTransaction(
                                "SubmitEvidence: offender field must be populated".into(),
                            ));
                        }
                        // Evidence targets the validator named in the
                        // `offender` field, NOT the submitter.
                        let evidence = sentrix_staking::slashing::DoubleSignEvidence {
                            validator: offender.clone(),
                            height,
                            block_hash_a,
                            block_hash_b,
                            signature_a,
                            signature_b,
                        };
                        // Audit H4: surface process_double_sign Err
                        // and reject the tx instead of silently
                        // swallowing. Pre-fix `let _ =` discarded the
                        // Result, so a malformed or already-processed
                        // evidence claim would silently succeed at the
                        // outer apply-Pass-2 level.
                        if let Err(e) = self
                            .slashing
                            .process_double_sign(&mut self.stake_registry, &evidence)
                        {
                            tracing::warn!(
                                "SubmitEvidence (offender={}): process_double_sign failed: {}",
                                offender,
                                e,
                            );
                            return Err(e);
                        }
                        // Note: full signature verification of
                        // signature_a / signature_b against
                        // `Precommit::signing_payload(height, round,
                        // block_hash, chain_id)` is deferred — the
                        // wire format doesn't carry `round_a` /
                        // `round_b` yet, and adding them is a
                        // fork-gated wire-format extension separate
                        // from this submitter/offender split.
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_staking_op(&sentrix_primitives::events::StakingOpEvent {
                                op: "submit_evidence".to_string(),
                                validator: offender.clone(),
                                delegator: tx.from_address.clone(),
                                amount: 0,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                        }
                    }
                    StakingOp::JailEvidenceBundle {
                        epoch: claimed_epoch,
                        epoch_start_block: _,
                        epoch_end_block: _,
                        evidence: claimed_evidence,
                        active_set: claimed_active_set,
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
                            sentrix_staking::epoch::EpochManager::epoch_for_height(self.height());
                        if claimed_epoch != current_epoch {
                            return Err(SentrixError::InvalidTransaction(format!(
                                "JailEvidenceBundle epoch {} != current epoch {}",
                                claimed_epoch, current_epoch
                            )));
                        }

                        // Verification: recompute evidence using the proposer's
                        // claimed active_set, then compare against the claimed
                        // evidence. Determinism property: identical
                        // LivenessTracker contents (canonical via
                        // record_signed) + identical iteration set → identical
                        // evidence list. Closes the 2026-04-29 divergence
                        // class where verifiers iterated their LOCAL
                        // stake_registry.active_set, which can drift across
                        // the fleet post-jail/unjail or mid-catchup.
                        //
                        // Sanity gate: every validator in the claimed set must
                        // already be registered. Stops a malicious proposer
                        // from inventing addresses to forge evidence; honest
                        // validators are expected to vote-no on a block whose
                        // claimed set diverges materially from their local
                        // view (BFT majority is the trust anchor for the set
                        // itself, not equality with each verifier's view).
                        if claimed_active_set.is_empty() {
                            return Err(SentrixError::InvalidTransaction(
                                "JailEvidenceBundle V2: claimed active_set must \
                                 be non-empty"
                                    .into(),
                            ));
                        }
                        for v in claimed_active_set.iter() {
                            if !self.stake_registry.validators.contains_key(v) {
                                return Err(SentrixError::InvalidTransaction(format!(
                                    "JailEvidenceBundle V2: claimed active_set \
                                     contains unregistered validator {v}"
                                )));
                            }
                        }
                        let local_evidence = self
                            .slashing
                            .compute_jail_evidence(&claimed_active_set, self.height());

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
                        let evidence_count = claimed_evidence.len() as u64;
                        for ev in &claimed_evidence {
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

                            // Phase 3 WS: notify sentrix_subscribe(jail) per
                            // jailed validator. Fires only when JAIL_CONSENSUS
                            // dispatch actually applies a jail (post-fork only;
                            // the gate at line 1192 ensures pre-fork rejects
                            // before reaching here).
                            if let Some(emitter) = &self.event_emitter {
                                emitter.emit_jail(&sentrix_primitives::events::JailEvent {
                                    validator: ev.validator.clone(),
                                    epoch: claimed_epoch,
                                    missed_blocks: ev.missed_count,
                                    block_height: block.index,
                                });
                            }
                        }
                        // One staking_op event for the bundle as a whole, so
                        // a dApp watching sentrix_stakingOps sees the
                        // JailEvidenceBundle dispatch even if it doesn't
                        // separately subscribe to sentrix_jail.
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_staking_op(&sentrix_primitives::events::StakingOpEvent {
                                op: "jail_evidence_bundle".to_string(),
                                validator: tx.from_address.clone(),
                                delegator: tx.from_address.clone(),
                                amount: evidence_count,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
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
                        if let Some(emitter) = &self.event_emitter {
                            emitter.emit_staking_op(&sentrix_primitives::events::StakingOpEvent {
                                op: "add_self_stake".to_string(),
                                validator: tx.from_address.clone(),
                                delegator: tx.from_address.clone(),
                                amount,
                                txid: tx.txid.clone(),
                                block_height: block.index,
                            });
                            // The active-set refresh effectively a validator
                            // set rotation event when a previously-jailed
                            // validator re-enters; emit so dApps tracking
                            // active set get a notif.
                            let active: Vec<String> = self.stake_registry.active_set.to_vec();
                            let epoch =
                                sentrix_staking::epoch::EpochManager::epoch_for_height(block.index);
                            emitter.emit_validator_set(epoch, &active);
                        }
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
        // Sprint 2: compute + persist per-block logs bloom. Walks the
        // 8-byte height prefix in TABLE_LOGS via a cursor (single
        // sequential read, no Vec materialisation of the entire log
        // table — see 2026-05-11 audit finding D-G3 for the prior
        // O(total_logs) per-block scan that this replaces).
        if let Some(storage) = self.mdbx_storage.as_ref() {
            use sentrix_evm::{StoredLog, add_log_to_bloom, empty_bloom};
            let mut bloom = empty_bloom();
            let prefix = block.index.to_be_bytes();
            let _ = storage.iter_range(sentrix_storage::tables::TABLE_LOGS, &prefix, |_k, v| {
                if let Ok(log) = bincode::deserialize::<StoredLog>(v) {
                    add_log_to_bloom(&mut bloom, &log.address, &log.topics);
                }
                true
            });
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
        // Audit M6 (2026-05-06): retain mutated `mempool`; the txid +
        // sender-count sidecars must rebuild before the next
        // `add_to_mempool` reads them.
        self.rebuild_mempool_sidecars();

        // Evict stale-nonce poison pills from senders that just had a
        // nonce-bumping tx finalize in this block. v2.1.58 did this
        // via a full-mempool scan inside prune_mempool(); under load
        // that 10K-entry retain() could push add_block past the BFT
        // round window. Now bounded to block.transactions.len() ×
        // mempool.len() via a senders-snapshot pass.
        let bumped_senders: Vec<&str> = block
            .transactions
            .iter()
            .map(|tx| tx.from_address.as_str())
            .collect();
        self.evict_stale_nonce_for_senders(bumped_senders);

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

        // Reward-apply-path fork: run ALL per-block bookkeeping here, exactly
        // once per block, instead of in the 5 network/finalize receive paths
        // (which applied it a per-node-variable number of times and drifted
        // consensus state). Two pieces, both deterministic off the committed
        // block + justification, run BEFORE update_trie_for_block so their
        // mutations land in THIS block's state_root:
        //
        //   1. reward / liveness / epoch-record bundle (every block) — drifted
        //      PROTOCOL_TREASURY = sum(pending_rewards + delegator_rewards).
        //   2. run_epoch_bookkeeping (epoch boundaries only) — active-set
        //      rotation, unbonding release, liveness slashing. NOT idempotent
        //      (advances epoch_number, pushes history, slashes), so a per-node-
        //      variable application count would corrupt epoch_state (trie-
        //      committed) + double-slash. Runs AFTER the bundle so it sees the
        //      bundle's fresh liveness counts + pre-rotation active_set, matching
        //      the external ordering.
        //
        // Pre-fork: skipped here; the external receive-path call sites still do
        // it (bit-identical to today). Inside Pass-2 → covered by the snapshot/
        // rollback if a later step fails.
        if Self::is_reward_apply_path_height(self.height()) {
            self.apply_reward_bookkeeping_for_latest_block();
            self.run_epoch_bookkeeping(self.height());
        }

        // Update state trie after block commit, stamp state_root on the block header,
        // and verify the sender's committed root when receiving from peers.
        let profile_t1 = profile_t0.map(|_| std::time::Instant::now());
        let trie_root = self.update_trie_for_block().map_err(|e| {
            SentrixError::Internal(format!(
                "trie update failed at block {}: {}",
                self.height(),
                e
            ))
        })?;
        let profile_t2 = profile_t0.map(|_| std::time::Instant::now());

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
                                // Mirror the end-of-function profile emit so
                                // SENTRIX_APPLY_PROFILE=1 still records timing
                                // on the legacy-tolerated path. Without this,
                                // pre-cutoff replay leaves a gap in the
                                // apply-profile log.
                                emit_apply_profile(
                                    profile_t0,
                                    profile_t1,
                                    profile_t2,
                                    profile_height,
                                    profile_txs,
                                );
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

        // State-drift instrumentation (gated by SENTRIX_STATE_FINGERPRINT=1).
        // Emits a deterministic hash of AccountDB + total_minted at the end
        // of every block-apply so post-mortem `grep STATE-FP` across the 4
        // mainnet hosts can pinpoint the exact block where validators
        // diverged. We've been chasing state_root v2 drift for a week
        // without a localised cause — this gives us the first per-block
        // fingerprint to compare across the fleet at next halt.
        emit_state_fingerprint(self, self.height());

        // Per-block apply-phase profile (see top of fn for rationale).
        emit_apply_profile(
            profile_t0,
            profile_t1,
            profile_t2,
            profile_height,
            profile_txs,
        );

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
    /// Reward / liveness / epoch bookkeeping for the just-committed latest
    /// block, run once inside `apply_block_pass2` post `REWARD_APPLY_PATH_HEIGHT`.
    ///
    /// This is the deterministic home for the bundle that pre-fork lived in 5
    /// separate network/finalize receive paths (gossip-apply, peer-apply,
    /// catch-up-sync, validator-finalize ×2). Because those paths covered
    /// blocks unevenly, a block's reward got applied a per-node-variable number
    /// of times → `pending_rewards` / `delegator_rewards` (and thus
    /// PROTOCOL_TREASURY = their sum) drifted → state_root divergence. Running
    /// it here, keyed off the committed block's justification, guarantees
    /// exactly-once application on every node regardless of how the block
    /// arrived.
    ///
    /// Mirrors the external call sites exactly: proposer = block.validator,
    /// signer stakes = justification precommit `stake_weight`, reward =
    /// `get_block_reward()`, fee_share = 0. No-op for Pioneer blocks (no
    /// justification).
    fn apply_reward_bookkeeping_for_latest_block(&mut self) {
        // Pull everything we need out of the committed block first so the
        // immutable borrow of `self.chain` ends before the mutable borrows.
        let (block_index, proposer, signers, reward_signers) = {
            let Some(latest) = self.chain.last() else {
                return;
            };
            let Some(j) = latest.justification.as_ref() else {
                return; // Pioneer / no-justification block — nothing to do.
            };
            let signers: Vec<String> = j.precommits.iter().map(|p| p.validator.clone()).collect();
            let reward_signers: Vec<(String, u64)> = j
                .precommits
                .iter()
                .map(|p| (p.validator.clone(), p.stake_weight))
                .collect();
            (
                latest.index,
                latest.validator.clone(),
                signers,
                reward_signers,
            )
        };

        let active = self.stake_registry.active_set.clone();
        let reward = self.get_block_reward();

        // 1. Liveness (signed/missed per validator).
        self.slashing
            .record_block_signatures(&active, &signers, block_index);
        // 2. Reward accumulators (validator pending_rewards + delegator_rewards).
        let _ = self
            .stake_registry
            .distribute_reward(&proposer, &reward_signers, reward, 0);
        // 3. Epoch accounting.
        self.epoch_manager.record_block(reward);
    }

    /// Called exactly once by `apply_block_pass2` on the single
    /// transition block, gated by
    /// `is_reward_v2_height(block.index) && !is_reward_v2_height(block.index - 1)`.
    fn reset_reward_accumulators_for_fork_activation(&mut self) {
        for v in self.stake_registry.validators.values_mut() {
            v.pending_rewards = 0;
        }
        self.stake_registry.delegator_rewards.clear();
    }
}

/// State-drift fingerprint emitter (debug-only). When
/// `SENTRIX_STATE_FINGERPRINT=1` the apply path calls this at the end
/// of every block; cross-host log diff at next halt pinpoints the
/// first diverging block.
///
/// What we hash:
///   1. AccountDB.accounts — sorted by address, each (balance, nonce,
///      code_hash, storage_root)
///   2. AccountDB.total_burned
///   3. AccountDB.contract_code — sorted by code_hash, each
///      sha256(bytecode)
///   4. AccountDB.contract_storage — sorted by composite key, raw
///      value bytes
///   5. Blockchain.total_minted
///
/// We deliberately skip stake_registry — bincode HashMap serialisation
/// is non-deterministic, and the drift we're chasing manifests in
/// AccountDB (via trie roots that read from AccountDB).
///
/// Output line shape:
///   [STATE-FP] h=<height> acc=<8-byte-hex> fp=<8-byte-hex>
///
/// Cost when enabled: ~O(N) sha256 over AccountDB content per block,
/// Per-block apply-phase profile emitter. Single exit point for the
/// `apply-profile h=... txs=... tx_apply=...ms trie=...ms post=...ms total=...ms`
/// log line, so every return path through `apply_block_pass2` records
/// timing when SENTRIX_APPLY_PROFILE=1. Pre-helper, the legacy-tolerated
/// branch returned without emitting — replay of pre-cutoff blocks left a
/// gap in the profile log.
///
/// No-op when any of the three timestamps is None (profiling disabled).
fn emit_apply_profile(
    t0: Option<std::time::Instant>,
    t1: Option<std::time::Instant>,
    t2: Option<std::time::Instant>,
    height: u64,
    txs: usize,
) {
    if let (Some(t0), Some(t1), Some(t2)) = (t0, t1, t2) {
        let t3 = std::time::Instant::now();
        tracing::info!(
            target: "apply_profile",
            "apply-profile h={} txs={} tx_apply={}ms trie={}ms post={}ms total={}ms",
            height,
            txs,
            t1.duration_since(t0).as_millis(),
            t2.duration_since(t1).as_millis(),
            t3.duration_since(t2).as_millis(),
            t3.duration_since(t0).as_millis(),
        );
    }
}

/// where N = total accounts + contract storage entries. At Sentrix
/// mainnet scale (~tens of thousands of accounts, sparse contract
/// storage) this adds ~1-2 ms per block. Acceptable for debug runs.
fn emit_state_fingerprint(bc: &Blockchain, height: u64) {
    if std::env::var_os("SENTRIX_STATE_FINGERPRINT").is_none() {
        return;
    }
    let (acc_fp, fp) = compute_state_fingerprint(bc);
    // eprintln! matches the existing `[V2-DBG]` trace pattern in
    // `update_trie_for_block` — guaranteed journalctl visibility
    // regardless of RUST_LOG / tracing subscriber filter config.
    eprintln!(
        "[STATE-FP] h={} acc={} fp={}",
        height,
        hex::encode(&acc_fp[..8]),
        hex::encode(&fp[..8]),
    );
}

/// Compute `(account_fingerprint, combined_fingerprint)` for the current
/// state. Split out of `emit_state_fingerprint` so it's testable without the
/// `SENTRIX_STATE_FINGERPRINT` env gate or stderr capture.
///
/// The combined fingerprint folds, in fixed order:
///   account state · total_minted · SRC-20 ContractRegistry · NFT NftRegistry
///
/// Native-module state commitment (2026-06-03): before this, the fingerprint
/// hashed only accounts + total_minted + EVM code/storage, so two validators
/// could diverge purely in SRC-20 or NFT state and still print identical
/// `[STATE-FP]` lines — the incident tool was blind on those paths. Both
/// registries now contribute via their `canonical_hash()` (sorted, HashMap-
/// order-independent). Consensus-enforced trie/state_root commitment of the
/// same state is the fork-gated follow-up; this closes the debug-visibility
/// gap without a consensus change.
fn compute_state_fingerprint(bc: &Blockchain) -> ([u8; 32], [u8; 32]) {
    use sha2::{Digest, Sha256};

    let mut acc_hasher = Sha256::new();
    let mut accounts: Vec<(&String, &sentrix_primitives::account::Account)> =
        bc.accounts.accounts.iter().collect();
    accounts.sort_by(|a, b| a.0.cmp(b.0));
    for (addr, account) in accounts {
        acc_hasher.update(addr.as_bytes());
        acc_hasher.update(account.balance.to_be_bytes());
        acc_hasher.update(account.nonce.to_be_bytes());
        acc_hasher.update(account.code_hash);
        acc_hasher.update(account.storage_root);
    }
    acc_hasher.update(bc.accounts.total_burned.to_be_bytes());

    let mut codes: Vec<(&String, &Vec<u8>)> = bc.accounts.contract_code.iter().collect();
    codes.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in codes {
        acc_hasher.update(k.as_bytes());
        let h: [u8; 32] = Sha256::digest(v).into();
        acc_hasher.update(h);
    }

    let mut storage: Vec<(&String, &Vec<u8>)> = bc.accounts.contract_storage.iter().collect();
    storage.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in storage {
        acc_hasher.update(k.as_bytes());
        acc_hasher.update(v);
    }

    let acc_fp: [u8; 32] = acc_hasher.finalize().into();
    let mut combined = Sha256::new();
    combined.update(acc_fp);
    combined.update(bc.total_minted.to_be_bytes());
    // Native-module state: SRC-20 then NFT, each canonical (sorted) so
    // HashMap iteration order can't move the fingerprint.
    combined.update(bc.contracts.canonical_hash());
    combined.update(bc.nft_registry.canonical_hash());
    let fp: [u8; 32] = combined.finalize().into();
    (acc_fp, fp)
}

// ── Tests ─────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use crate::block_executor::BlockSource;
    use crate::blockchain::{Blockchain, CHAIN_ID};
    use secp256k1::{PublicKey, Secp256k1, SecretKey};
    use sentrix_primitives::error::{SentrixError, SentrixResult};
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

    // ── Native module state commitment (fingerprint) ─────────
    //
    // compute_state_fingerprint folds account state + total_minted + the
    // SRC-20 ContractRegistry + the NFT NftRegistry (the last two via their
    // canonical_hash). These tests prove the fingerprint moves on native-state
    // changes, is replay-deterministic, and is HashMap-order-independent.

    const NFT_ADDR_A: &str = "0x1111111111111111111111111111111111111111";
    const NFT_ADDR_B: &str = "0x2222222222222222222222222222222222222222";

    /// 1. SRC-20 state changes move the fingerprint.
    #[test]
    fn fingerprint_tracks_src20_state() {
        let mut bc = setup();
        let (_, fp_empty) = super::compute_state_fingerprint(&bc);
        bc.contracts
            .deploy("0xowner", "Tok", "TOK", 8, 1000, 0, "seed1")
            .unwrap();
        let (_, fp_deploy) = super::compute_state_fingerprint(&bc);
        assert_ne!(fp_empty, fp_deploy, "SRC-20 deploy must move fingerprint");
    }

    /// 2. NFT state changes move the fingerprint.
    #[test]
    fn fingerprint_tracks_nft_state() {
        let mut bc = setup();
        let (_, fp_empty) = super::compute_state_fingerprint(&bc);
        bc.nft_registry
            .deploy_collection(NFT_ADDR_A, "C", "C", "u", None, true, true, "seed")
            .unwrap();
        let (_, fp_deploy) = super::compute_state_fingerprint(&bc);
        assert_ne!(fp_empty, fp_deploy, "NFT deploy must move fingerprint");
    }

    /// 3. Same native-op sequence on fresh state ⇒ identical fingerprint.
    #[test]
    fn fingerprint_replay_deterministic() {
        let build = || {
            let mut bc = setup();
            bc.contracts
                .deploy("0xowner", "Tok", "TOK", 8, 1000, 0, "seed1")
                .unwrap();
            let (cid, _) = bc
                .nft_registry
                .deploy_collection(NFT_ADDR_A, "C", "C", "u", None, true, true, "ns")
                .unwrap();
            bc.nft_registry
                .get_collection_mut(&cid)
                .unwrap()
                .mint(NFT_ADDR_A, NFT_ADDR_B, 7, "", None)
                .unwrap();
            super::compute_state_fingerprint(&bc).1
        };
        assert_eq!(build(), build(), "replayed native state must match");
    }

    /// 4. Different SRC-20 supply ⇒ different fingerprint.
    #[test]
    fn fingerprint_distinguishes_src20_supply() {
        let make = |supply: u64| {
            let mut bc = setup();
            bc.contracts
                .deploy("0xowner", "Tok", "TOK", 8, supply, 0, "seed1")
                .unwrap();
            super::compute_state_fingerprint(&bc).1
        };
        assert_ne!(make(1000), make(2000));
    }

    /// 5. Different NFT owner ⇒ different fingerprint.
    #[test]
    fn fingerprint_distinguishes_nft_owner() {
        let make = |owner: &str| {
            let mut bc = setup();
            let (cid, _) = bc
                .nft_registry
                .deploy_collection(NFT_ADDR_A, "C", "C", "u", None, true, true, "ns")
                .unwrap();
            bc.nft_registry
                .get_collection_mut(&cid)
                .unwrap()
                .mint(NFT_ADDR_A, owner, 1, "", None)
                .unwrap();
            super::compute_state_fingerprint(&bc).1
        };
        assert_ne!(make(NFT_ADDR_A), make(NFT_ADDR_B));
    }

    /// 6. SRC-20 contract insertion order does not change the fingerprint.
    #[test]
    fn fingerprint_src20_order_independent() {
        let mut a = setup();
        a.contracts
            .deploy("0xowner", "A", "AAA", 8, 1, 0, "s1")
            .unwrap();
        a.contracts
            .deploy("0xowner", "B", "BBB", 8, 1, 0, "s2")
            .unwrap();
        let mut b = setup();
        b.contracts
            .deploy("0xowner", "B", "BBB", 8, 1, 0, "s2")
            .unwrap();
        b.contracts
            .deploy("0xowner", "A", "AAA", 8, 1, 0, "s1")
            .unwrap();
        assert_eq!(
            super::compute_state_fingerprint(&a).1,
            super::compute_state_fingerprint(&b).1,
            "deploy order must not affect fingerprint"
        );
    }

    /// 7. Invalid addresses are rejected at the NFT apply boundary.
    #[test]
    fn apply_rejects_invalid_nft_addresses() {
        let mut reg = sentrix_nft::NftRegistry::new();
        let (cid, _) = reg
            .deploy_collection(NFT_ADDR_A, "C", "C", "u", None, true, true, "ns")
            .unwrap();
        // mint to a malformed address
        let bad_mint = TokenOp::MintNft {
            contract: cid.clone(),
            to: "0xnothex".into(),
            token_id: 1,
            metadata_uri: String::new(),
        };
        let err =
            crate::nft::apply_nft_token_op(&mut reg, &bad_mint, NFT_ADDR_A, "tx").unwrap_err();
        assert!(
            matches!(err, SentrixError::InvalidTransaction(ref m) if m.contains("invalid NFT"))
        );
        // mint to the zero address (valid format, not spendable)
        let zero_mint = TokenOp::MintNft {
            contract: cid,
            to: "0x0000000000000000000000000000000000000000".into(),
            token_id: 1,
            metadata_uri: String::new(),
        };
        let err =
            crate::nft::apply_nft_token_op(&mut reg, &zero_mint, NFT_ADDR_A, "tx").unwrap_err();
        assert!(matches!(err, SentrixError::InvalidTransaction(_)));
    }

    // ── Native NFT apply-path E2E (through add_block) ─────────
    //
    // These drive the full block path (mempool → create_block → add_block →
    // Pass-1 dry-run → Pass-2 apply) to prove the wiring: the NFT_TOKENOP_HEIGHT
    // fork gate, the `nft_registry` state field, and cross-node determinism.
    // Behavioural depth (auth, soulbound, no-reuse, supply) is covered by the
    // dispatch tests in `nft.rs`. NFT forks default to u64::MAX, so these set
    // the height env var under the shared `env_test_lock` to avoid racing other
    // env-touching tests.

    fn nft_tx(op: &TokenOp, sk: &SecretKey, pk: &PublicKey, from: &str, nonce: u64) -> Transaction {
        Transaction::new(
            from.to_string(),
            TOKEN_OP_ADDRESS.to_string(),
            0,
            MIN_TX_FEE,
            nonce,
            op.encode().unwrap(),
            CHAIN_ID,
            sk,
            pk,
        )
        .unwrap()
    }

    /// Mine a single-tx block carrying `tx` onto `bc`.
    fn mine(bc: &mut Blockchain, tx: Transaction) -> SentrixResult<()> {
        bc.add_to_mempool(tx)?;
        let block = bc.create_block("v1")?;
        bc.add_block(block)
    }

    #[test]
    fn nft_deploy_mint_transfer_through_add_block() {
        let _guard = crate::test_util::env_test_lock();
        unsafe {
            std::env::set_var("NFT_TOKENOP_HEIGHT", "1");
        }
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let admin = derive_addr(&pk);
        let (sk2, pk2) = make_keypair();
        let bob = derive_addr(&pk2);
        bc.accounts.credit(&admin, 10_000_000_000).unwrap();
        bc.accounts.credit(&bob, 10_000_000_000).unwrap();

        // Block 1: deploy.
        let deploy = TokenOp::DeployNft {
            name: "Validator Proof".into(),
            symbol: "VPRF".into(),
            base_uri: "ipfs://Q/".into(),
            max_supply: 0,
        };
        let dtx = nft_tx(&deploy, &sk, &pk, &admin, 0);
        let cid = crate::nft::compute_collection_id(&admin, &dtx.txid);
        mine(&mut bc, dtx).expect("deploy block");
        assert!(
            bc.nft_registry.collection_exists(&cid),
            "collection must exist after add_block"
        );

        // Block 2: mint token 1 to bob.
        let mint = TokenOp::MintNft {
            contract: cid.clone(),
            to: bob.clone(),
            token_id: 1,
            metadata_uri: String::new(),
        };
        mine(&mut bc, nft_tx(&mint, &sk, &pk, &admin, 1)).expect("mint block");
        assert_eq!(
            bc.nft_registry.get_collection(&cid).unwrap().owner_of(1),
            Some(bob.as_str())
        );

        // Block 3: bob transfers token 1 back to admin.
        let xfer = TokenOp::TransferNft {
            contract: cid.clone(),
            from: bob.clone(),
            to: admin.clone(),
            token_id: 1,
        };
        mine(&mut bc, nft_tx(&xfer, &sk2, &pk2, &bob, 0)).expect("transfer block");
        assert_eq!(
            bc.nft_registry.get_collection(&cid).unwrap().owner_of(1),
            Some(admin.as_str())
        );

        unsafe {
            std::env::remove_var("NFT_TOKENOP_HEIGHT");
        }
    }

    #[test]
    fn nft_rejected_pre_fork_through_add_block() {
        let _guard = crate::test_util::env_test_lock();
        unsafe {
            std::env::remove_var("NFT_TOKENOP_HEIGHT"); // default u64::MAX = disabled
        }
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let admin = derive_addr(&pk);
        bc.accounts.credit(&admin, 10_000_000_000).unwrap();

        let deploy = TokenOp::DeployNft {
            name: "Too Early".into(),
            symbol: "EARLY".into(),
            base_uri: "u".into(),
            max_supply: 0,
        };
        let err = mine(&mut bc, nft_tx(&deploy, &sk, &pk, &admin, 0)).unwrap_err();
        assert!(
            format!("{err:?}").contains("NFT_TOKENOP_HEIGHT"),
            "pre-fork NFT op must be gated, got {err:?}"
        );
        assert_eq!(bc.nft_registry.collection_count(), 0);
    }

    #[test]
    fn nft_collection_id_deterministic_across_nodes() {
        let _guard = crate::test_util::env_test_lock();
        unsafe {
            std::env::set_var("NFT_TOKENOP_HEIGHT", "1");
        }
        let mut bc1 = setup();
        let mut bc2 = setup();
        let (sk, pk) = make_keypair();
        let admin = derive_addr(&pk);
        bc1.accounts.credit(&admin, 10_000_000_000).unwrap();
        bc2.accounts.credit(&admin, 10_000_000_000).unwrap();

        let deploy = TokenOp::DeployNft {
            name: "Genesis Proof".into(),
            symbol: "GEN".into(),
            base_uri: "u".into(),
            max_supply: 0,
        };
        // Same signed tx applied to both nodes → identical txid → identical id.
        let tx = nft_tx(&deploy, &sk, &pk, &admin, 0);
        let cid = crate::nft::compute_collection_id(&admin, &tx.txid);
        mine(&mut bc1, tx.clone()).unwrap();
        mine(&mut bc2, tx).unwrap();

        assert_eq!(bc1.nft_registry.collection_count(), 1);
        assert_eq!(bc2.nft_registry.collection_count(), 1);
        // Compare by value (order-independent PartialEq), not serialized
        // bytes — HashMap iteration order is per-process.
        assert_eq!(
            bc1.nft_registry.get_collection(&cid),
            bc2.nft_registry.get_collection(&cid),
            "both nodes must derive identical NFT state"
        );

        unsafe {
            std::env::remove_var("NFT_TOKENOP_HEIGHT");
        }
    }

    #[test]
    fn nft_failed_block_leaves_registry_unchanged() {
        let _guard = crate::test_util::env_test_lock();
        unsafe {
            std::env::set_var("NFT_TOKENOP_HEIGHT", "1");
        }
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let admin = derive_addr(&pk);
        bc.accounts.credit(&admin, 10_000_000_000).unwrap();

        // Deploy ok.
        let deploy = TokenOp::DeployNft {
            name: "Proof".into(),
            symbol: "PRF".into(),
            base_uri: "u".into(),
            max_supply: 0,
        };
        let dtx = nft_tx(&deploy, &sk, &pk, &admin, 0);
        let cid = crate::nft::compute_collection_id(&admin, &dtx.txid);
        mine(&mut bc, dtx).unwrap();
        let before = bc.nft_registry.get_collection(&cid).cloned();

        // A block whose NFT op fails (mint by non-admin) must not mutate state.
        let (sk2, pk2) = make_keypair();
        let mallory = derive_addr(&pk2);
        bc.accounts.credit(&mallory, 10_000_000_000).unwrap();
        let mint = TokenOp::MintNft {
            contract: cid.clone(),
            to: mallory.clone(),
            token_id: 1,
            metadata_uri: String::new(),
        };
        let err = mine(&mut bc, nft_tx(&mint, &sk2, &pk2, &mallory, 0)).unwrap_err();
        assert!(matches!(err, SentrixError::UnauthorizedValidator(_)));
        // Registry unchanged (no token minted), compared by value.
        assert_eq!(
            bc.nft_registry.get_collection(&cid).cloned(),
            before,
            "failed block must leave nft_registry unchanged"
        );
        assert_eq!(
            bc.nft_registry.get_collection(&cid).unwrap().owner_of(1),
            None
        );

        unsafe {
            std::env::remove_var("NFT_TOKENOP_HEIGHT");
        }
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
        let _window = sentrix_staking::slashing::LIVENESS_WINDOW;
        // 2026-04-29 fix: under the new canonical-only LivenessTracker
        // recording, "downtime" is the absence of recent signed entries,
        // not a wall of explicit signed=false. Anchor the downer with
        // ONE signed entry at h=0 (proves "we've been watching them"),
        // then leave them silent. By the time we reach the epoch boundary
        // their window is empty → is_downtime_at fires.
        bc.slashing.liveness.record_signed(&downer, 0);

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
        bc.add_block(block)
            .expect("add_block must accept Phase D system tx");

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

    /// 2026-04-30 regression for the receiver-side eager-write
    /// artifact pinned in the 2026-04-28 validator block-773012
    /// divergence runbook. A peer broadcasts a block whose
    /// `BlockJustification` claims
    /// finalization but the precommit stake doesn't actually reach
    /// our local supermajority threshold. Pre-fix: silently applied
    /// → divergent chain.db, livelock at the next height. Post-fix:
    /// rejected with InvalidBlock so the chain stays canonical.
    #[test]
    fn test_peer_block_with_weak_justification_rejected() {
        use sentrix_primitives::justification::BlockJustification;
        use sentrix_staking::staking::ValidatorStake;

        let mut bc = setup();
        bc.voyager_activated = true;

        // 4 validators each at stake 1000 → total 4000, supermajority
        // threshold = 4000 * 2/3 + 1 = 2667. A justification with
        // precommit stake of 1000 (one signer) is well under that.
        for addr in ["v1", "v2", "v3", "v4"] {
            bc.stake_registry.validators.insert(
                addr.to_string(),
                ValidatorStake {
                    address: addr.to_string(),
                    self_stake: 1000,
                    total_delegated: 0,
                    commission_rate: 1000,
                    max_commission_rate: 2000,
                    is_jailed: false,
                    jail_until: 0,
                    is_tombstoned: false,
                    blocks_signed: 0,
                    blocks_missed: 0,
                    pending_rewards: 0,
                    registration_height: 0,
                    last_commission_change_height: 0,
                },
            );
        }
        bc.stake_registry.active_set = vec!["v1".into(), "v2".into(), "v3".into(), "v4".into()];

        let prev_hash = bc.latest_block().unwrap().hash.clone();
        let height = bc.height() + 1;
        let reward = bc.get_block_reward();
        let coinbase = Transaction::new_coinbase("v1".into(), reward, height, 1_777_000_000);
        let mut block =
            sentrix_primitives::block::Block::new(height, prev_hash, vec![coinbase], "v1".into());
        block.timestamp = 1_777_000_000;
        block.hash = block.calculate_hash();
        let mut just = BlockJustification::new(height, 0, block.hash.clone());
        // Single weak precommit — well under 2/3+1 of 4000.
        just.add_precommit("v1".into(), vec![], 1000);
        block.justification = Some(just);

        let err = bc
            .add_block_with_source(block, BlockSource::Peer)
            .expect_err("peer block with weak justification must be rejected");
        assert!(
            format!("{err:?}").contains("below the local supermajority threshold"),
            "expected supermajority-threshold rejection; got: {err:?}"
        );
    }

    /// Inverse of the above: a peer block whose justification meets
    /// supermajority on our local active set must be accepted (modulo
    /// the rest of Pass 1 / Pass 2 validation). Pins that the new
    /// guard does not regress legitimate finalised broadcasts.
    #[test]
    fn test_peer_block_with_strong_justification_passes_check() {
        use sentrix_primitives::justification::BlockJustification;
        use sentrix_staking::staking::ValidatorStake;

        let mut bc = setup();
        bc.voyager_activated = true;
        for addr in ["v1", "v2", "v3", "v4"] {
            bc.stake_registry.validators.insert(
                addr.to_string(),
                ValidatorStake {
                    address: addr.to_string(),
                    self_stake: 1000,
                    total_delegated: 0,
                    commission_rate: 1000,
                    max_commission_rate: 2000,
                    is_jailed: false,
                    jail_until: 0,
                    is_tombstoned: false,
                    blocks_signed: 0,
                    blocks_missed: 0,
                    pending_rewards: 0,
                    registration_height: 0,
                    last_commission_change_height: 0,
                },
            );
        }
        bc.stake_registry.active_set = vec!["v1".into(), "v2".into(), "v3".into(), "v4".into()];

        let prev_hash = bc.latest_block().unwrap().hash.clone();
        let height = bc.height() + 1;
        let reward = bc.get_block_reward();
        let coinbase = Transaction::new_coinbase("v1".into(), reward, height, 1_777_000_000);
        let mut block =
            sentrix_primitives::block::Block::new(height, prev_hash, vec![coinbase], "v1".into());
        block.timestamp = 1_777_000_000;
        block.hash = block.calculate_hash();
        let mut just = BlockJustification::new(height, 0, block.hash.clone());
        // Three precommits at stake 1000 each = 3000 ≥ supermajority
        // threshold (2667) on a 4000-total active set.
        just.add_precommit("v1".into(), vec![], 1000);
        just.add_precommit("v2".into(), vec![], 1000);
        just.add_precommit("v3".into(), vec![], 1000);
        block.justification = Some(just);

        // Result may still error on later Pass-1 / Pass-2 checks (we
        // didn't bother to forge a valid state_root etc.), but the
        // error MUST NOT be the new supermajority-threshold one.
        let result = bc.add_block_with_source(block, BlockSource::Peer);
        if let Err(ref err) = result {
            assert!(
                !format!("{err:?}").contains("below the local supermajority threshold"),
                "supermajority guard incorrectly tripped on a strong justification: {err:?}"
            );
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
        let _window = sentrix_staking::slashing::LIVENESS_WINDOW;
        // 2026-04-29 fix: under the new canonical-only LivenessTracker
        // recording, "downtime" is the absence of recent signed entries,
        // not a wall of explicit signed=false. Anchor the downer with
        // ONE signed entry at h=0 (proves "we've been watching them"),
        // then leave them silent. By the time we reach the epoch boundary
        // their window is empty → is_downtime_at fires.
        bc.slashing.liveness.record_signed(&downer, 0);

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

    // ── Reward-apply-path determinism (centralized bookkeeping) ──────────
    //
    // The bundle (liveness + reward + epoch-record) was moved out of the 5
    // network/finalize receive paths into apply_block_pass2, gated by
    // REWARD_APPLY_PATH_HEIGHT, to run exactly once per block. These exercise
    // the relocated helper directly (the gate itself is tested in fork_heights).

    #[test]
    fn reward_apply_path_credits_pending_rewards_once() {
        use sentrix_primitives::justification::BlockJustification;
        let mut bc = setup();
        let val = format!("0x{}", "77".repeat(20));
        // Register a single staker so it receives the whole signer share.
        bc.stake_registry
            .register_validator(&val, 15_000 * 100_000_000, 1000, bc.height())
            .unwrap();
        bc.stake_registry.active_set = vec![val.clone()];
        let before = bc
            .stake_registry
            .validators
            .get(&val)
            .unwrap()
            .pending_rewards;

        // A block proposed by `val` with `val` as the sole precommit signer.
        let mut block = bc.create_block("v1").unwrap();
        let mut just = BlockJustification::new(block.index, 0, block.hash.clone());
        just.add_precommit(val.clone(), vec![], 15_000 * 100_000_000);
        block.validator = val.clone();
        block.justification = Some(just);
        bc.chain.push(block);

        bc.apply_reward_bookkeeping_for_latest_block();
        let after = bc
            .stake_registry
            .validators
            .get(&val)
            .unwrap()
            .pending_rewards;
        assert!(
            after > before,
            "apply-path bookkeeping must credit pending_rewards (before={before} after={after})"
        );

        // Idempotency-of-inputs check: it is NOT internally idempotent (each
        // call distributes another block's worth) — so the gate/caller must
        // ensure exactly-once. A second call credits again, proving each
        // invocation = one distribution (caller runs it once per block).
        bc.apply_reward_bookkeeping_for_latest_block();
        let twice = bc
            .stake_registry
            .validators
            .get(&val)
            .unwrap()
            .pending_rewards;
        assert!(
            twice > after,
            "second call distributes a second time as expected"
        );
    }

    #[test]
    fn reward_apply_path_noop_without_justification() {
        let mut bc = setup();
        let block = bc.create_block("v1").unwrap(); // Pioneer-style: no justification
        bc.chain.push(block);
        // Must be a clean no-op (no panic, no state change) when justification absent.
        bc.apply_reward_bookkeeping_for_latest_block();
    }
}
