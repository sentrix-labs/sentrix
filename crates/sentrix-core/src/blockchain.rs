// blockchain.rs - Sentrix — Blockchain struct, constants, genesis, core state methods

use crate::authority::AuthorityManager;
use crate::vm::ContractRegistry;
use sentrix_primitives::account::AccountDB;
use sentrix_primitives::block::Block;
use sentrix_primitives::merkle::merkle_root;
use sentrix_primitives::transaction::Transaction;
use sentrix_storage::MdbxStorage;
use sentrix_trie::tree::SentrixTrie;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;

// ── Tokenomics constants ─────────────────────────────────
// MAX_SUPPLY, MAX_SUPPLY_V2, BLOCK_REWARD, HALVING_INTERVAL,
// HALVING_INTERVAL_V2, and the `max_supply_srx()` display helper
// live in `crate::tokenomics` now — re-export so existing import
// paths (`crate::blockchain::MAX_SUPPLY` etc.) still resolve.
pub use crate::tokenomics::{
    BLOCK_REWARD, HALVING_INTERVAL, HALVING_INTERVAL_V2, MAX_SUPPLY, MAX_SUPPLY_V2, max_supply_srx,
};

// Chain parameter consts + chain-id accessor live in `crate::chain_params`
// now — re-export so existing `crate::blockchain::{BLOCK_TIME_SECS,
// MAX_TX_PER_BLOCK, CHAIN_ID, HASH_VERSION, CHAIN_WINDOW_SIZE, get_chain_id}`
// import paths still resolve unchanged.
pub use crate::chain_params::{
    BLOCK_TIME_SECS, CHAIN_ID, CHAIN_WINDOW_SIZE, HASH_VERSION, MAX_TX_PER_BLOCK, get_chain_id,
};

// Fork-height accessors live in `crate::fork_heights` now — re-export so
// every existing caller path (`crate::blockchain::get_*_height(...)`) keeps
// resolving bit-identically.
pub use crate::fork_heights::{
    get_add_self_stake_height, get_bft_gate_relax_height, get_evm_fork_height,
    get_evm_gas_fix_height, get_evm_value_transfer_height, get_extended_touch_list_height,
    get_jail_consensus_height, get_nft_tokenop_height, get_reward_v2_fork_height,
    get_strict_justification_height, get_tokenomics_v2_height, get_voyager_fork_height,
    warn_if_jail_consensus_armed,
};

// Mempool consts live in `crate::mempool` now (next to the only code
// that uses them) — re-export so `crate::blockchain::MAX_MEMPOOL_SIZE`
// etc. still resolve for any path that was relying on it.
pub use crate::mempool::{MAX_MEMPOOL_PER_SENDER, MAX_MEMPOOL_SIZE, MEMPOOL_MAX_AGE_SECS};

// Address validation + protocol-reserved address constants live in
// `crate::address` now — re-export so the existing import paths
// (`crate::blockchain::is_valid_sentrix_address` etc.) still resolve.
pub use crate::address::{
    ECOSYSTEM_FUND_ADDRESS, TOTAL_PREMINE, ZERO_ADDRESS, is_spendable_sentrix_address,
    is_valid_sentrix_address,
};

// ── Blockchain struct ────────────────────────────────────
// Chain field excluded from serde — blocks are saved individually in MDBX storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blockchain {
    // Critical chain-state fields use pub to enforce validated access:
    //   bc.chain.push(unvalidated_block) → use add_block() instead
    //   bc.total_minted += n             → only block execution should change this
    //   bc.mempool.push_back(invalid_tx) → use add_to_mempool() instead
    // authority / accounts / contracts stay pub — they have their own validation in their methods
    // and main.rs legitimately calls them for CLI operations.
    #[serde(skip, default)]
    pub chain: Vec<Block>,
    pub accounts: AccountDB, // pub: main.rs uses accounts.get_balance() for CLI display
    pub authority: AuthorityManager, // pub: main.rs uses authority.* for validator management
    pub contracts: ContractRegistry,
    pub mempool: VecDeque<Transaction>,
    /// Audit M6 (2026-05-06): O(1) duplicate-txid check sidecar for
    /// `add_to_mempool`. Pre-fix the dup-scan was `mempool.iter().any(...)`
    /// — at MAX_MEMPOOL_SIZE=10K + a 5K-tx burst the per-block cost is
    /// 25M string comparisons. The sidecar is fully derived from
    /// `mempool` (txid set), so anything that mutates `mempool`
    /// rebuilds it via `rebuild_mempool_sidecars` (the snapshot
    /// rollback path does, and so does each mempool `retain` call).
    /// `#[serde(skip)]` because it's a derived index — chain.db only
    /// needs to persist the authoritative `mempool` itself.
    #[serde(skip)]
    pub mempool_txids: std::collections::HashSet<String>,
    /// Audit M6 (sister index): O(1) per-sender pending count. Backs
    /// `mempool_pending_count`, called twice per `add_to_mempool` (once
    /// for the per-sender cap check, once to compute the next nonce);
    /// pre-fix each call was an O(n) iter+filter+count.
    #[serde(skip)]
    pub mempool_sender_count: std::collections::HashMap<String, u32>,
    pub total_minted: u64,
    pub chain_id: u64, // kept pub — read-only constant used by external clients
    /// Display name of this network (eg "Sentrix Chain", "Sentrix Testnet").
    /// Sourced from the loaded genesis `[chain].name` so testnet binaries
    /// don't lie about being mainnet on the `/` self-describe endpoint.
    /// Default exists only because pre-genesis ctors (tests) skip the
    /// genesis path; real boot always overwrites this in
    /// `new_with_genesis`.
    #[serde(default = "Blockchain::default_chain_name")]
    pub chain_name: String,
    /// Binary Sparse Merkle Tree for account state.
    /// None until init_trie() is called; not persisted in MDBX state blob.
    #[serde(skip)]
    pub state_trie: Option<SentrixTrie>,

    /// MDBX storage handle for txid_index lookups and on-demand block loading.
    /// Populated by `init_storage_handle()` at startup. Allows O(1) tx lookups
    /// for blocks that have been evicted from the in-memory chain window.
    /// Cheap clone — `Arc<MdbxStorage>`.
    #[serde(skip)]
    pub mdbx_storage: Option<Arc<MdbxStorage>>,

    // ── Voyager DPoS state (Phase 2a) ────────────────────
    /// Staking registry for DPoS validator management
    #[serde(default)]
    pub stake_registry: sentrix_staking::staking::StakeRegistry,
    /// Epoch manager for validator set rotation
    #[serde(default = "sentrix_staking::epoch::EpochManager::new")]
    pub epoch_manager: sentrix_staking::epoch::EpochManager,
    /// Slashing engine for liveness + double-sign tracking
    #[serde(default)]
    pub slashing: sentrix_staking::slashing::SlashingEngine,

    /// Origin of the block currently being admitted. Set by the
    /// `add_block*` family before calling `apply_block_pass2` and
    /// cleared after. Peer blocks trigger strict state_root checks;
    /// self-produced blocks are allowed to stamp state_root in
    /// Pass 2. Backlog #1e. Not persisted.
    #[serde(skip, default = "default_block_source")]
    pub(crate) source_for_current_add: crate::block_executor::BlockSource,

    /// Rolling tracker for state_root divergences from peers.
    ///
    /// Added 2026-04-23 after the second mainnet fork where Core node was
    /// silently rejecting peer blocks for 4+ hours (4000+ state_root
    /// mismatches per hour) without any operator alert. The existing
    /// per-event ERROR log was lost in log noise. This tracker emits
    /// a rate-limited LOUD alarm when the rejection rate crosses a
    /// threshold, pointing operators at the rsync-from-peer recovery.
    /// Not persisted (rebuilds from scratch on every boot, which is
    /// the correct behavior — a validator that was diverging 6h ago
    /// but is clean now shouldn't keep alarming).
    #[serde(skip, default)]
    pub(crate) divergence_tracker: DivergenceTracker,

    /// Persistent one-shot guard for `activate_voyager`. Set to `true`
    /// inside `activate_voyager` after the migration commits successfully;
    /// any subsequent call to `activate_voyager` (e.g. on validator
    /// restart, when the local `voyager_activated` boolean in the
    /// validator loop has reset) is a no-op. Without this guard the
    /// loop re-registers the same 4 mainnet validators on every boot
    /// post-fork, which double-runs `update_active_set` /
    /// `epoch_manager.initialize` deterministically (so consensus stays
    /// safe today) but trips noisy "validator already registered" warns
    /// and is fragile against any future non-deterministic mutation in
    /// that path. Phase 1 hard-gate per
    /// `internal design doc`.
    #[serde(default)]
    pub voyager_activated: bool,

    /// Persistent one-shot guard for `activate_evm`. Same rationale as
    /// `voyager_activated`: prevents redundant `migrate_to_evm` runs at
    /// every restart post-fork.
    #[serde(default)]
    pub evm_activated: bool,

    /// Optional event emitter for WebSocket / SSE subscribers. Set at
    /// startup by `bin/sentrix/main.rs` after the RPC layer constructs
    /// its `EventBus`. Default `None` means no event emission (tests,
    /// CLI tools that don't expose RPC). Block production must NEVER
    /// depend on subscriber liveness — `emit_new_head` is non-blocking
    /// and infallible by trait contract. See `sentrix-primitives::events`.
    #[serde(skip, default)]
    pub event_emitter: Option<sentrix_primitives::SharedEmitter>,
}

// DivergenceTracker lives in `crate::divergence` now — a `use` brings it
// back into scope so the `Blockchain` field and constructor reference
// remain unchanged.
use crate::divergence::DivergenceTracker;

fn default_block_source() -> crate::block_executor::BlockSource {
    crate::block_executor::BlockSource::SelfProduced
}

impl Blockchain {
    /// Default for the `chain_name` serde-skip field — only matters for
    /// pre-genesis state-blob deserialisations that predate the field.
    /// Real boot always overwrites via `new_with_genesis`.
    fn default_chain_name() -> String {
        "Sentrix Chain".to_string()
    }

    /// Construct a blockchain initialised from the embedded canonical mainnet
    /// genesis. Thin wrapper over [`Blockchain::new_with_genesis`].
    ///
    /// The embedded genesis parses and validates at compile time (enforced
    /// by `test_mainnet_embedded_parses_and_validates`). A parse failure
    /// here means the binary is fundamentally broken, in the same class as
    /// a corrupt `include_str!` target; we fail loud rather than silently.
    pub fn new(admin_address: String) -> Self {
        #[allow(clippy::expect_used)]
        let genesis =
            crate::Genesis::mainnet().expect("embedded mainnet genesis must parse and validate");
        Self::new_with_genesis(admin_address, &genesis)
    }

    /// Construct a blockchain from an arbitrary [`Genesis`] config. Used by
    /// the `sentrix start --genesis <path>` flag to boot non-mainnet chains
    /// (testnets, devnets) from TOML without rebuilding the binary.
    pub fn new_with_genesis(admin_address: String, genesis: &crate::Genesis) -> Self {
        let mut bc = Self {
            chain: Vec::new(),
            accounts: AccountDB::new(),
            authority: AuthorityManager::new(admin_address),
            contracts: ContractRegistry::new(),
            mempool: VecDeque::new(),
            mempool_txids: std::collections::HashSet::new(),
            mempool_sender_count: std::collections::HashMap::new(),
            total_minted: 0,
            // Prefer the TOML's declared chain_id, but defer to the
            // SENTRIX_CHAIN_ID env var when set (matches previous semantics
            // so live operators can keep using env-based overrides).
            chain_id: std::env::var("SENTRIX_CHAIN_ID")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(genesis.chain.chain_id),
            chain_name: genesis.chain.name.clone(),
            state_trie: None,
            mdbx_storage: None,
            stake_registry: sentrix_staking::staking::StakeRegistry::new(),
            epoch_manager: sentrix_staking::epoch::EpochManager::new(),
            slashing: sentrix_staking::slashing::SlashingEngine::new(),
            source_for_current_add: crate::block_executor::BlockSource::SelfProduced,
            divergence_tracker: DivergenceTracker::default(),
            voyager_activated: false,
            evm_activated: false,
            event_emitter: None,
        };
        bc.initialize_genesis(genesis);
        bc
    }

    /// Wire a WebSocket / SSE event emitter into this blockchain. Called
    /// once at startup by `bin/sentrix/main.rs` after the RPC layer
    /// constructs its `EventBus`. After this returns, every successful
    /// `add_block` / `add_block_from_peer` call will fire `emit_new_head`
    /// against the supplied emitter. Pass `None` to detach (rare).
    pub fn set_event_emitter(&mut self, emitter: Option<sentrix_primitives::SharedEmitter>) {
        self.event_emitter = emitter;
    }

    /// Credit premine balances and seat block 0 on the chain. Staking
    /// registry is intentionally left empty — PoA Pioneer chains track
    /// validators via `AuthorityManager`; the `[[genesis.validators]]`
    /// section is informational until the Voyager DPoS fork activates.
    /// Keeping this path unchanged preserves the state-root identity with
    /// chains that were initialised by the pre-Genesis-TOML code path.
    fn initialize_genesis(&mut self, genesis: &crate::Genesis) {
        // Apply premine allocations in the order declared in the TOML.
        // HashMap iteration order inside AccountDB is not observable at
        // genesis (state_root starts being stamped at STATE_ROOT_FORK_HEIGHT
        // = 100_000), but we still iterate a Vec here for determinism and
        // to match the historical order from the hardcoded constants.
        //
        // credit() can only fail on u64 overflow; with ~63M SRX premine vs
        // u64::MAX (~184B SRX) the overflow path is unreachable in practice.
        // A failure here means the program is fundamentally broken and the
        // chain cannot start — abort cleanly rather than silently discard.
        for balance in &genesis.genesis.balances {
            if let Err(e) = self.accounts.credit(&balance.address, balance.amount) {
                tracing::error!(
                    "FATAL: genesis premine credit failed for {} ({}): {}",
                    balance.address,
                    balance.amount,
                    e
                );
                std::process::exit(1);
            }
        }
        self.total_minted = genesis.total_premine();

        // Genesis block is produced from the same Genesis config so the
        // block hash is fully derived from declared state.
        self.chain.push(genesis.build_block());
    }

    // Storage I/O methods (init_storage_handle, persist_block_durable,
    // record_tx_in_index, lookup_tx_in_storage, backfill_txid_index)
    // live in `crate::blockchain_storage_io` — split out so the MDBX
    // storage seam is in one focused file. Rust permits a single
    // `impl Blockchain { … }` to span multiple modules within the
    // same crate, so all the original `bc.method(…)` call sites keep
    // resolving unchanged.

    // ── Fork-height predicates ──────────────────────────────
    //
    // The substance + docs for each predicate live in
    // `crate::fork_heights`. These `impl Blockchain` methods stay as
    // thin delegators so all the external callers that already write
    // `Blockchain::is_X_height(h)` keep working — and so the instance
    // variants (`is_X_active(&self)`) have one obvious home.

    pub fn is_voyager_height(height: u64) -> bool {
        crate::fork_heights::is_voyager_height(height)
    }

    pub fn is_voyager_active(&self) -> bool {
        crate::fork_heights::is_voyager_height(self.height())
    }

    /// Voyager-mode check that respects BOTH the env-var fork height
    /// AND the runtime persisted `voyager_activated` flag. Consensus-
    /// safe — use this in `validate_block`. The OR semantics mean a
    /// chain that activated Voyager via the runtime path (with env var
    /// unset / wrong) continues to apply blocks correctly.
    pub fn voyager_mode_for(&self, height: u64) -> bool {
        self.voyager_activated || crate::fork_heights::is_voyager_height(height)
    }

    pub fn is_reward_v2_height(height: u64) -> bool {
        crate::fork_heights::is_reward_v2_height(height)
    }

    pub fn is_reward_v2_active(&self) -> bool {
        crate::fork_heights::is_reward_v2_height(self.height())
    }

    pub fn is_tokenomics_v2_height(height: u64) -> bool {
        crate::fork_heights::is_tokenomics_v2_height(height)
    }

    pub fn is_jail_consensus_height(height: u64) -> bool {
        crate::fork_heights::is_jail_consensus_height(height)
    }

    pub fn is_nft_tokenop_height(height: u64) -> bool {
        crate::fork_heights::is_nft_tokenop_height(height)
    }

    pub fn is_add_self_stake_height(height: u64) -> bool {
        crate::fork_heights::is_add_self_stake_height(height)
    }

    pub fn is_evm_value_transfer_height(height: u64) -> bool {
        crate::fork_heights::is_evm_value_transfer_height(height)
    }

    pub fn is_evm_gas_fix_height(height: u64) -> bool {
        crate::fork_heights::is_evm_gas_fix_height(height)
    }

    pub fn is_extended_touch_list_height(height: u64) -> bool {
        crate::fork_heights::is_extended_touch_list_height(height)
    }

    pub fn is_strict_justification_height(height: u64) -> bool {
        crate::fork_heights::is_strict_justification_height(height)
    }

    pub fn is_bft_gate_relax_height(height: u64) -> bool {
        crate::fork_heights::is_bft_gate_relax_height(height)
    }

    /// BFT-gate-relax: minimum active validator count for BFT participation.
    /// Pre-fork: returns `MIN_BFT_VALIDATORS` (= 4 absolute, current behavior).
    /// Post-fork: returns `⌈2/3 × total_validator_count⌉` (supermajority for
    /// finality). For N=4: 3 (= 1-jail tolerance). For N=7: 5. For N=10: 7.
    ///
    /// `total_validator_count` = total registered validators (active + jailed).
    /// Returns USIZE for direct comparison with `active_count() (-> usize)`.
    ///
    /// NOTE: The network-design floor (`MIN_BFT_VALIDATORS = 4` total
    /// registered validators) is enforced separately at Voyager activation
    /// time, NOT in this per-block gate. Once Voyager is active, total ≥ 4
    /// is invariant, so the post-fork return is always ≥ ⌈8/3⌉ = 3.
    /// Clamping post-fork return to MIN_BFT_VALIDATORS=4 would defeat the
    /// purpose of the relaxation (4-validator network would still gate at 4).
    pub fn min_active_for_bft(height: u64, total_validator_count: usize) -> usize {
        if !Self::is_bft_gate_relax_height(height) {
            // Pre-fork: legacy gate. active < MIN_BFT_VALIDATORS = stall.
            return sentrix_staking::staking::MIN_BFT_VALIDATORS;
        }
        // Post-fork: ⌈2/3 × N⌉ supermajority. For N=4 → 3 (= 1-jail tolerance).
        // Integer math: ⌈2N/3⌉ = (2N + 2) / 3 (exact for N ≥ 1).
        total_validator_count.saturating_mul(2).saturating_add(2) / 3
    }

    /// Phase D: build a JailEvidenceBundle system transaction for the given
    /// boundary height, if one should be emitted. Returns:
    /// - `None` if pre-fork (JAIL_CONSENSUS_HEIGHT not reached)
    /// - `None` if `next_height` is not an epoch boundary
    /// - `None` if local LivenessTracker shows no validators meeting the
    ///   downtime threshold (Q3-A: skip emission for empty bundles)
    /// - `Some(tx)` otherwise: a fully-formed system tx with PROTOCOL_TREASURY
    ///   sender, empty signature, JSON-encoded `StakingOp::JailEvidenceBundle`
    ///
    /// The proposer's block_producer calls this at build_block time. Peers
    /// recompute via `compute_jail_evidence` in dispatch (see block_executor)
    /// and reject the block if the evidence diverges.
    ///
    /// `next_height` is the height the proposer is about to produce (NOT the
    /// current chain head). The boundary check uses `next_height`.
    /// `block_timestamp` is the timestamp the proposer chose for the block.
    pub fn build_jail_evidence_system_tx(
        &self,
        next_height: u64,
        block_timestamp: u64,
    ) -> Option<Transaction> {
        // Gate 1: post-fork only
        if !Self::is_jail_consensus_height(next_height) {
            return None;
        }
        // Gate 2: epoch boundary only
        if !sentrix_staking::epoch::EpochManager::is_epoch_boundary(next_height) {
            return None;
        }
        // Gate 3: must have evidence (Q3-A: skip emission for empty bundles)
        // 2026-04-29: pass next_height (the boundary block we're building for),
        // not self.height(). At call time `self.height()` is one less than
        // the block being constructed, so the deterministic is_downtime_at
        // window check needs the about-to-be-applied height.
        let active_set = self.stake_registry.active_set.clone();
        let evidence = self
            .slashing
            .compute_jail_evidence(&active_set, next_height);
        if evidence.is_empty() {
            return None;
        }

        // Compute epoch metadata for the bundle
        let epoch = sentrix_staking::epoch::EpochManager::epoch_for_height(next_height);
        let epoch_length = sentrix_staking::epoch::EPOCH_LENGTH;
        let epoch_start_block = epoch.saturating_mul(epoch_length);
        let epoch_end_block = next_height; // boundary block IS the end

        let op = sentrix_primitives::transaction::StakingOp::JailEvidenceBundle {
            epoch,
            epoch_start_block,
            epoch_end_block,
            evidence,
            // V2 (2026-05-06): carry the active_set we used for evidence
            // computation so verifiers iterate the same list. Closes the
            // divergence class where each node's local active_set drifted
            // post-jail/unjail and broke JailEvidenceBundle equality even
            // when LivenessTracker contents agreed.
            active_set,
        };

        match Transaction::new_jail_evidence_bundle(op, next_height, block_timestamp) {
            Ok(tx) => Some(tx),
            Err(e) => {
                tracing::error!(
                    "build_jail_evidence_system_tx: failed to build tx at h={}: {}",
                    next_height,
                    e
                );
                None
            }
        }
    }

    // Tokenomics emission math — substance lives in `crate::tokenomics`.
    // These delegators keep `bc.max_supply_for(h)` / `bc.halving_interval_for(h)`
    // call sites working (sentrix-rpc). The previous `Blockchain::halvings_at`
    // wrapper was removed when `get_block_reward` moved to
    // `crate::blockchain_block_accessors`; tests in this file call
    // `crate::tokenomics::halvings_at` directly now.

    pub fn max_supply_for(&self, height: u64) -> u64 {
        crate::tokenomics::max_supply_for(height)
    }

    pub fn halving_interval_for(&self, height: u64) -> u64 {
        crate::tokenomics::halving_interval_for(height)
    }

    /// Is the given height at or after the EVM fork?
    pub fn is_evm_height(height: u64) -> bool {
        let fork = get_evm_fork_height();
        fork != u64::MAX && height >= fork
    }

    /// Is the current chain past the EVM fork?
    pub fn is_evm_active(&self) -> bool {
        Self::is_evm_height(self.height())
    }

    // ── Chain validation ─────────────────────────────────
    // Validates the in-memory window only — not a full historical chain scan
    pub fn is_valid_chain_window(&self) -> bool {
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
            let txids: Vec<String> = block
                .transactions
                .iter()
                .map(|tx| tx.txid.clone())
                .collect();
            if merkle_root(&txids) != block.merkle_root {
                return false;
            }
        }
        true
    }

    /// Total SRX minted so far (in sentri, 1 SRX = 100_000_000 sentri).
    pub fn total_minted(&self) -> u64 {
        self.total_minted
    }

    // Memory estimate reflects the in-memory window, not the full historical chain
    pub fn get_memory_estimate(&self) -> String {
        let window_blocks = self.chain.len();
        let true_height = self.height();
        let estimate_mb = (window_blocks * 2) / 1024; // ~2KB per block
        format!(
            "~{}MB for {} blocks in window (true height: {})",
            estimate_mb, window_blocks, true_height
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::env_test_lock;
    use secp256k1::{PublicKey, Secp256k1, SecretKey};
    use sentrix_primitives::transaction::{MIN_TX_FEE, Transaction};

    fn make_keypair() -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        let mut rng = secp256k1::rand::rng();
        secp.generate_keypair(&mut rng)
    }

    fn derive_addr(pk: &PublicKey) -> String {
        sentrix_wallet::Wallet::derive_address(pk)
    }

    // DivergenceTracker tests live in `crate::divergence::tests` now.

    // Valid-format test address for use as to_address in tests
    const TEST_RECV: &str = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

    fn setup_chain() -> Blockchain {
        let mut bc = Blockchain::new("admin".to_string());
        // Use unchecked helper so tests can control the address string ("validator1").
        // Crypto validation is tested separately via add_validator; skip here for simpler test setup.
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
        assert!(bc.is_valid_chain_window());
    }

    // REGRESSION BARRIER — the live chain's genesis block was produced by
    // the pre-TOML hardcoded code path. Wiring Blockchain::new through
    // Genesis::mainnet() must yield bit-identical block 0; any drift here
    // forks the chain on next restart.
    #[test]
    fn test_blockchain_new_genesis_block_hash_stable() {
        let bc = Blockchain::new("admin".to_string());
        let block0 = bc
            .chain
            .first()
            .expect("genesis block must exist after Blockchain::new");
        let reference = sentrix_primitives::block::Block::genesis();
        assert_eq!(
            block0.hash, reference.hash,
            "genesis block hash drift detected — TOML wiring broke invariant"
        );
        assert_eq!(block0.timestamp, reference.timestamp);
        assert_eq!(block0.merkle_root, reference.merkle_root);
        assert_eq!(block0.previous_hash, reference.previous_hash);
        assert_eq!(block0.validator, reference.validator);
        // total_minted must equal TOTAL_PREMINE exactly (no drift from the
        // sum of genesis balances).
        assert_eq!(bc.total_minted, TOTAL_PREMINE);
    }

    // Every premine address from mainnet.toml must end up in AccountDB
    // with the exact declared balance. Guards against silent credit
    // failures or reordering that would skip entries.
    #[test]
    fn test_blockchain_new_premine_balances_match_toml() {
        let bc = Blockchain::new("admin".to_string());
        let genesis = crate::Genesis::mainnet().expect("mainnet.toml");
        for balance in &genesis.genesis.balances {
            assert_eq!(
                bc.accounts.get_balance(&balance.address),
                balance.amount,
                "balance for {} diverges from TOML",
                balance.address
            );
        }
    }

    #[test]
    fn test_block_reward_era0() {
        let bc = setup_chain();
        assert_eq!(bc.get_block_reward(), BLOCK_REWARD);
    }

    /// Tokenomics v2 fork: pre-fork era 0 uses 42M halving + 210M cap;
    /// post-fork era 0 uses 126M halving + 315M cap. At fork moment
    /// (and any height before either era boundary), reward stays at
    /// BLOCK_REWARD = 1 SRX in sentri — no jump.
    #[test]
    fn test_tokenomics_v2_fork_boundary_no_reward_jump() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("TOKENOMICS_V2_HEIGHT", "100");
        }

        // Pre-fork (h=99): v1 schedule. h/42M = 0 halvings → reward = 1 SRX.
        assert_eq!(crate::tokenomics::halvings_at(99), 0);

        // At fork boundary (h=100): v2 schedule activates. (h - fork) / 126M
        // = 0 / 126M = 0 halvings. Smooth transition: reward stays 1 SRX.
        assert_eq!(crate::tokenomics::halvings_at(100), 0);

        // Post-fork era 0: still 0 halvings until fork+126M.
        assert_eq!(crate::tokenomics::halvings_at(100 + 126_000_000 - 1), 0);

        // Post-fork era 1: at fork+126M, halvings = 1. Reward halves to 0.5.
        assert_eq!(crate::tokenomics::halvings_at(100 + 126_000_000), 1);
        assert_eq!(crate::tokenomics::halvings_at(100 + 2 * 126_000_000), 2);

        // Cap dispatch: pre-fork queries return 210M, post-fork return 315M.
        // Need a Blockchain instance for the helper (it's &self).
        let bc = setup_chain();
        assert_eq!(bc.max_supply_for(99), MAX_SUPPLY);
        assert_eq!(bc.max_supply_for(100), MAX_SUPPLY_V2);
        assert_eq!(bc.halving_interval_for(99), HALVING_INTERVAL);
        assert_eq!(bc.halving_interval_for(100), HALVING_INTERVAL_V2);

        unsafe {
            std::env::remove_var("TOKENOMICS_V2_HEIGHT");
        }
    }

    /// Tokenomics v2: confirm the geometric series math reaches 315M cap.
    /// Era 0 (1.0 SRX × 126M) = 126M minted. Era 1 (0.5 × 126M) = 63M.
    /// Era 2 (0.25 × 126M) = 31.5M. Cumulative through era N converges
    /// to 252M from mining + 63M premine = 315M cap (asymptote).
    #[test]
    fn test_tokenomics_v2_geometric_reaches_315m_cap() {
        // Sum of 1 SRX × 126M × (1 + 1/2 + 1/4 + ...) in sentri.
        // Discrete sum truncated at era where reward = 0 (h ≥ ~27 halvings).
        let initial: u64 = 100_000_000; // 1 SRX
        let interval: u64 = 126_000_000; // 126M blocks
        let mut total_mined: u64 = 0;
        for halvings in 0u32..27 {
            let reward = initial.checked_shr(halvings).unwrap_or(0);
            if reward == 0 {
                break;
            }
            total_mined = total_mined.saturating_add(reward.saturating_mul(interval));
        }
        // Geometric asymptote: 1 × 126M × 2 = 252M SRX = 252M × 100M sentri
        let expected_sentri: u64 = 252_000_000 * 100_000_000;
        // Discrete sum reaches expected within 1-sentri rounding (last
        // non-zero reward at era 26 contributes 1 sentri × 126M blocks).
        let diff = expected_sentri.abs_diff(total_mined);
        // Tail truncation: rewards below 1 sentri (after ~27 halvings) drop
        // to 0 in integer arithmetic, leaving a small undershoot vs the
        // real-valued geometric asymptote. Bound: 2 × initial × interval /
        // 2^27 ≈ 1.9B sentri. Use 5B as comfortable tolerance.
        assert!(
            diff <= 5_000_000_000,
            "geometric sum {} sentri diverges from expected {} sentri by {} (> 5B tolerance)",
            total_mined,
            expected_sentri,
            diff
        );
        // Cap math: 63M premine + 252M mining (asymptote) = 315M = MAX_SUPPLY_V2.
        let premine: u64 = 63_000_000 * 100_000_000;
        let total = premine + expected_sentri;
        assert_eq!(total, MAX_SUPPLY_V2);
        // Sanity: discrete actual is within ~5B sentri of the cap.
        let actual_total = premine + total_mined;
        assert!(actual_total <= MAX_SUPPLY_V2, "discrete sum exceeds cap");
        assert!(
            MAX_SUPPLY_V2 - actual_total <= 5_000_000_000,
            "discrete asymptote gap > 5B sentri (= 50 SRX)"
        );
    }

    #[test]
    fn test_create_and_add_block() {
        let mut bc = setup_chain();
        let block = bc.create_block("validator1").unwrap();
        assert_eq!(block.index, 1);
        bc.add_block(block).unwrap();
        assert_eq!(bc.height(), 1);
        assert!(bc.is_valid_chain_window());
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
        )
        .unwrap();

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
        bc.create_block("validator1")
            .map(|b| bc.add_block(b))
            .unwrap()
            .unwrap();

        // Tamper with txid — breaks merkle root integrity
        bc.chain[1].transactions[0].txid = "tampered".to_string();
        assert!(!bc.is_valid_chain_window());
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

    // ── SRC-20 Token Tests ──────────────────────────────

    #[test]
    fn test_deploy_token() {
        let mut bc = setup_chain();
        // Fund deployer
        bc.accounts.credit("deployer", 1_000_000).unwrap();

        let addr = bc
            .deploy_token(
                "deployer",
                "TestToken".to_string(),
                "TT".to_string(),
                18,
                1_000_000,
                0,
                100_000,
            )
            .unwrap();

        assert!(addr.starts_with("SRC20_"));
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
            "deployer",
            "Token".to_string(),
            "TK".to_string(),
            18,
            1_000,
            0,
            1_000,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_token_transfer() {
        let mut bc = setup_chain();
        bc.accounts.credit("alice", 1_000_000).unwrap();

        let addr = bc
            .deploy_token(
                "alice",
                "Coin".to_string(),
                "CN".to_string(),
                18,
                500_000,
                0,
                10_000,
            )
            .unwrap();

        bc.token_transfer(&addr, "alice", "bob", 100_000, 1_000)
            .unwrap();
        assert_eq!(bc.token_balance(&addr, "alice"), 400_000);
        assert_eq!(bc.token_balance(&addr, "bob"), 100_000);
    }

    #[test]
    fn test_token_transfer_gas_burned() {
        let mut bc = setup_chain();
        bc.accounts.credit("alice", 1_000_000).unwrap();

        let addr = bc
            .deploy_token(
                "alice",
                "Coin".to_string(),
                "CN".to_string(),
                18,
                500_000,
                0,
                0,
            )
            .unwrap();

        let burned_before = bc.accounts.total_burned;
        bc.token_transfer(&addr, "alice", "bob", 100, 10_000)
            .unwrap();
        // 50% of 10_000 gas = 5_000 burned
        assert_eq!(bc.accounts.total_burned - burned_before, 5_000);
    }

    #[test]
    fn test_token_info() {
        let mut bc = setup_chain();
        bc.accounts.credit("deployer", 1_000_000).unwrap();

        let addr = bc
            .deploy_token(
                "deployer",
                "MyToken".to_string(),
                "MT".to_string(),
                8,
                21_000_000,
                0,
                0,
            )
            .unwrap();

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
        bc.deploy_token("d", "A".to_string(), "A".to_string(), 18, 100, 0, 0)
            .unwrap();
        bc.deploy_token("d", "B".to_string(), "B".to_string(), 18, 200, 0, 0)
            .unwrap();
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
            sender.clone(),
            TEST_RECV.to_string(),
            100_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        let tx_high = Transaction::new(
            sender.clone(),
            TEST_RECV.to_string(),
            100_000,
            MIN_TX_FEE * 100,
            1,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        let tx_mid = Transaction::new(
            sender.clone(),
            TEST_RECV.to_string(),
            100_000,
            MIN_TX_FEE * 10,
            2,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();

        bc.add_to_mempool(tx_low).unwrap();
        bc.add_to_mempool(tx_high).unwrap();
        bc.add_to_mempool(tx_mid).unwrap();

        // Backlog #10 fix: within the same sender, nonce order trumps
        // fee — otherwise block production would pick nonce=1 first and
        // trip the "expected nonce 0" rejection. Fee priority only
        // applies across *different* senders.
        let fees: Vec<u64> = bc.mempool.iter().map(|tx| tx.fee).collect();
        let nonces: Vec<u64> = bc.mempool.iter().map(|tx| tx.nonce).collect();
        assert_eq!(
            nonces,
            vec![0, 1, 2],
            "same-sender txs must stay in nonce order regardless of fee"
        );
        assert_eq!(fees, vec![MIN_TX_FEE, MIN_TX_FEE * 100, MIN_TX_FEE * 10]);
    }

    #[test]
    fn test_c02_add_block_rejects_unauthorized_validator() {
        let mut bc = setup_chain();
        // Add a second validator (unchecked for test control over address string)
        bc.authority.add_validator_unchecked(
            "validator2".to_string(),
            "Validator 2".to_string(),
            "pk2".to_string(),
        );

        // Determine who is authorized for block 1
        let expected = bc.authority.expected_validator(1).unwrap().address.clone();
        let unauthorized = if expected == "validator1" {
            "validator2"
        } else {
            "validator1"
        };

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
        assert!(
            err_str.contains("not authorized"),
            "Expected 'not authorized' error, got: {}",
            err_str
        );
    }

    #[test]
    fn test_h02_mempool_rejects_overflow_amount_fee() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);

        bc.accounts.credit(&sender, 100_000_000).unwrap();

        // Create tx with amount = u64::MAX and fee = 1 — would overflow
        let tx = Transaction::new(
            sender,
            TEST_RECV.to_string(),
            u64::MAX,
            1,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();

        let result = bc.add_to_mempool(tx);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("overflow") || err_str.contains("fee"),
            "Expected overflow error, got: {}",
            err_str
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
        assert!(
            err_str.contains("timestamp"),
            "Expected timestamp error, got: {}",
            err_str
        );
    }

    #[test]
    fn test_h06_add_block_rejects_future_timestamp() {
        let mut bc = setup_chain();

        let mut block = bc.create_block("validator1").unwrap();
        // Set timestamp far in the future (1 hour from now)
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + 3600;
        block.timestamp = future;
        block.hash = block.calculate_hash();

        let result = bc.add_block(block);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("future"),
            "Expected future timestamp error, got: {}",
            err_str
        );
    }

    #[test]
    fn test_l02_latest_block_on_empty_chain_returns_err() {
        let mut bc = Blockchain::new("admin".to_string());
        bc.chain.clear();
        let result = bc.latest_block();
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("empty"),
            "Expected 'empty' error, got: {}",
            err_str
        );
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
        use sentrix_primitives::transaction::{TOKEN_OP_ADDRESS, TokenOp};

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
            deployer.clone(),
            TOKEN_OP_ADDRESS.to_string(),
            0,
            MIN_TX_FEE,
            0,
            token_op.encode().unwrap(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
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
        use sentrix_primitives::transaction::{TOKEN_OP_ADDRESS, TokenOp};

        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let alice = derive_addr(&pk);
        bc.accounts.credit(&alice, 10_000_000).unwrap();

        // Deploy token first (old method for setup)
        let contract = bc
            .deploy_token(
                &alice,
                "Coin".to_string(),
                "CN".to_string(),
                8,
                500_000,
                0,
                0,
            )
            .unwrap();

        // Create transfer transaction
        let bob = TEST_RECV; // V8-H-02: use valid-format address
        let token_op = TokenOp::Transfer {
            contract: contract.clone(),
            to: bob.to_string(),
            amount: 100_000,
        };
        let tx = Transaction::new(
            alice.clone(),
            TOKEN_OP_ADDRESS.to_string(),
            0,
            MIN_TX_FEE,
            0,
            token_op.encode().unwrap(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        bc.add_to_mempool(tx).unwrap();

        // Mine block
        let block = bc.create_block("validator1").unwrap();
        bc.add_block(block).unwrap();

        // Verify token balances
        assert_eq!(bc.token_balance(&contract, &alice), 400_000);
        assert_eq!(bc.token_balance(&contract, bob), 100_000);
    }

    #[test]
    fn test_onchain_token_op_recorded_in_block() {
        use sentrix_primitives::transaction::{TOKEN_OP_ADDRESS, TokenOp};

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
            deployer.clone(),
            TOKEN_OP_ADDRESS.to_string(),
            0,
            MIN_TX_FEE,
            0,
            token_op.encode().unwrap(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
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
        use sentrix_primitives::transaction::{TOKEN_OP_ADDRESS, TokenOp};

        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let alice = derive_addr(&pk);
        bc.accounts.credit(&alice, 10_000_000).unwrap();

        let contract = bc
            .deploy_token(&alice, "Coin".to_string(), "CN".to_string(), 8, 100, 0, 0)
            .unwrap();

        // Try to transfer more than token balance
        let token_op = TokenOp::Transfer {
            contract: contract.clone(),
            to: "bob".to_string(),
            amount: 999, // alice only has 100
        };
        let tx = Transaction::new(
            alice.clone(),
            TOKEN_OP_ADDRESS.to_string(),
            0,
            MIN_TX_FEE,
            0,
            token_op.encode().unwrap(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();

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
        assert!(is_valid_sentrix_address(
            "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
        ));
        assert!(is_valid_sentrix_address(
            "0x0000000000000000000000000000000000000000"
        ));
        assert!(is_valid_sentrix_address(
            "0xabcdef0123456789abcdef0123456789abcdef01"
        ));

        // Invalid: no prefix
        assert!(!is_valid_sentrix_address(
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
        ));
        // Invalid: too short
        assert!(!is_valid_sentrix_address("0xdeadbeef"));
        // Invalid: non-hex chars
        assert!(!is_valid_sentrix_address(
            "0xGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG"
        ));
        // Invalid: empty
        assert!(!is_valid_sentrix_address(""));
        // Invalid: 0x prefix but too long
        assert!(!is_valid_sentrix_address(
            "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefff"
        ));
    }

    // ── M-03: Transaction Timestamp Validation ──────────

    #[test]
    fn test_m03_rejects_future_timestamp() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000).unwrap();

        let mut tx = Transaction::new(
            sender,
            TEST_RECV.to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();

        // Tamper timestamp to +10 min in future (beyond +5 min tolerance)
        tx.timestamp += 601;

        let result = bc.add_to_mempool(tx);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("future"),
            "Expected 'future' error, got: {}",
            err_str
        );
    }

    #[test]
    fn test_m03_rejects_expired_timestamp() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000).unwrap();

        let mut tx = Transaction::new(
            sender,
            TEST_RECV.to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();

        // Tamper timestamp to 2 hours ago (beyond 1h TTL)
        tx.timestamp = tx.timestamp.saturating_sub(7_200);

        let result = bc.add_to_mempool(tx);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("old") || err_str.contains("age"),
            "Expected 'old'/'age' error, got: {}",
            err_str
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
            sender,
            TEST_RECV.to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();

        assert!(bc.add_to_mempool(tx).is_ok());
        assert_eq!(bc.mempool_size(), 1);
    }

    // ── Archive-mode opt-in: SENTRIX_DISABLE_TRIE_PRUNE ─────────

    /// `crate::blockchain_trie_ops::trie_prune_disabled()` reflects exactly the env var state.
    /// Default off; set "1" to enable; other values (empty, "true",
    /// "yes", "0") all map to disabled-flag-off-prune-still-runs.
    #[test]
    fn test_trie_prune_disabled_env_var() {
        let _guard = crate::test_util::env_test_lock();
        unsafe {
            // Baseline: unset → prune runs (predicate false).
            std::env::remove_var("SENTRIX_DISABLE_TRIE_PRUNE");
            assert!(!crate::blockchain_trie_ops::trie_prune_disabled());

            // Strict "1" → archive mode on.
            std::env::set_var("SENTRIX_DISABLE_TRIE_PRUNE", "1");
            assert!(crate::blockchain_trie_ops::trie_prune_disabled());

            // Anything else → treated as off (no silent activation).
            std::env::set_var("SENTRIX_DISABLE_TRIE_PRUNE", "");
            assert!(!crate::blockchain_trie_ops::trie_prune_disabled());
            std::env::set_var("SENTRIX_DISABLE_TRIE_PRUNE", "true");
            assert!(!crate::blockchain_trie_ops::trie_prune_disabled());
            std::env::set_var("SENTRIX_DISABLE_TRIE_PRUNE", "yes");
            assert!(!crate::blockchain_trie_ops::trie_prune_disabled());
            std::env::set_var("SENTRIX_DISABLE_TRIE_PRUNE", "0");
            assert!(!crate::blockchain_trie_ops::trie_prune_disabled());

            // Cleanup so other tests see a clean env.
            std::env::remove_var("SENTRIX_DISABLE_TRIE_PRUNE");
            assert!(!crate::blockchain_trie_ops::trie_prune_disabled());
        }
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
            sender.clone(),
            TEST_RECV.to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
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
            sender,
            TEST_RECV.to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
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
            sender,
            TEST_RECV.to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
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
        let burn = odd_fee.div_ceil(2);
        let validator_share = odd_fee - burn;
        // burn + validator_share must equal odd_fee exactly (no sentri lost)
        assert_eq!(burn + validator_share, odd_fee);
        assert!(burn > validator_share); // burn gets the rounding

        // Also verify with a block using MIN_TX_FEE (even)
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000).unwrap();
        let tx = Transaction::new(
            sender,
            TEST_RECV.to_string(),
            100,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
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
        bc.deploy_token(
            "deployer",
            "TestToken".to_string(),
            "TT".to_string(),
            8,
            1_000_000,
            0,
            3,
        )
        .unwrap();
        assert_eq!(bc.accounts.total_burned, initial_burned + 2);
    }

    #[test]
    fn test_l02_gas_fee_burn_rounds_up() {
        let mut bc = setup_chain();
        bc.accounts.credit("user1", 10_000_000).unwrap();

        // Deploy a token first
        let contract = bc
            .deploy_token(
                "user1",
                "Gas".to_string(),
                "GAS".to_string(),
                8,
                1_000,
                0,
                0,
            )
            .unwrap();

        let initial_burned = bc.accounts.total_burned;
        // Transfer with odd gas_fee=5 → burn=(5+1)/2=3
        bc.token_transfer(&contract, "user1", "user2", 100, 5)
            .unwrap();
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
        assert!(
            bc.get_block(0).is_none(),
            "evicted block should return None"
        );
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
        let addr = bc
            .deploy_token(
                "deployer",
                "Capped".to_string(),
                "CAP".to_string(),
                18,
                500_000,
                1_000_000,
                0,
            )
            .unwrap();
        let info = bc.token_info(&addr).unwrap();
        assert_eq!(info["max_supply"], 1_000_000);
        assert_eq!(info["total_supply"], 500_000);
    }

    #[test]
    fn test_v502_deploy_with_zero_max_supply_is_unlimited() {
        let mut bc = setup_chain();
        bc.accounts.credit("deployer", 1_000_000).unwrap();
        let addr = bc
            .deploy_token(
                "deployer",
                "Unlimited".to_string(),
                "UNL".to_string(),
                18,
                100_000,
                0,
                0,
            )
            .unwrap();
        let info = bc.token_info(&addr).unwrap();
        assert_eq!(info["max_supply"], 0); // 0 = unlimited
    }

    // ── V5-10: HASH_VERSION constant ──────────────────────

    #[test]
    fn test_v510_hash_version_constant_is_1() {
        assert_eq!(HASH_VERSION, 1, "HASH_VERSION must be 1 (SHA-256)");
    }

    // ── SentrixTrie unit tests ────────────────────────────

    fn temp_mdbx() -> (tempfile::TempDir, Arc<MdbxStorage>) {
        let dir = tempfile::TempDir::new().unwrap();
        let mdbx = Arc::new(MdbxStorage::open(dir.path()).unwrap());
        (dir, mdbx)
    }

    /// A freshly constructed Blockchain must have state_trie = None.
    #[test]
    fn test_state_trie_none_by_default() {
        let bc = setup_chain();
        assert!(
            bc.state_trie.is_none(),
            "state_trie must be None before init_trie()"
        );
    }

    /// trie_root_at() must return None when the trie has not been initialized.
    #[test]
    fn test_trie_root_at_without_init_returns_none() {
        let bc = setup_chain();
        assert_eq!(bc.trie_root_at(0), None);
        assert_eq!(bc.trie_root_at(1), None);
    }

    /// After init_trie() + add_block(), trie_root_at(1) must return Some(root).
    #[test]
    fn test_trie_initialized_commits_root_per_block() {
        let (_dir, mdbx) = temp_mdbx();
        let mut bc = setup_chain();
        bc.init_trie(Arc::clone(&mdbx)).unwrap();
        assert!(bc.state_trie.is_some());

        let block = bc.create_block("validator1").unwrap();
        bc.add_block(block).unwrap();

        let root = bc.trie_root_at(1);
        assert!(
            root.is_some(),
            "trie_root_at(1) must be Some after adding block 1"
        );
    }

    /// trie_root_at() must return None for a version that has not been committed yet.
    #[test]
    fn test_trie_root_at_uncommitted_version_returns_none() {
        let (_dir, mdbx) = temp_mdbx();
        let mut bc = setup_chain();
        bc.init_trie(Arc::clone(&mdbx)).unwrap();
        // No blocks added — version 1 has not been committed
        assert_eq!(
            bc.trie_root_at(1),
            None,
            "uncommitted version must return None"
        );
    }

    /// Multiple blocks must each have a distinct committed root persisted in the trie.
    #[test]
    fn test_trie_multiple_blocks_all_roots_persisted() {
        let (_dir, mdbx) = temp_mdbx();
        let mut bc = setup_chain();
        bc.init_trie(Arc::clone(&mdbx)).unwrap();

        for i in 1u64..=3 {
            let block = bc.create_block("validator1").unwrap();
            bc.add_block(block).unwrap();
            assert!(
                bc.trie_root_at(i).is_some(),
                "root at height {} must be committed",
                i
            );
        }
    }

    /// Block.state_root must be Some when the trie is active, None otherwise.
    #[test]
    fn test_state_root_stamped_on_block_iff_trie_active() {
        // Without trie: state_root should be None
        let mut bc_no_trie = setup_chain();
        let b1 = bc_no_trie.create_block("validator1").unwrap();
        bc_no_trie.add_block(b1).unwrap();
        assert!(
            bc_no_trie.latest_block().unwrap().state_root.is_none(),
            "state_root must be None when trie is not initialized"
        );

        // With trie: state_root should be Some
        let (_dir, mdbx) = temp_mdbx();
        let mut bc_trie = setup_chain();
        bc_trie.init_trie(Arc::clone(&mdbx)).unwrap();
        let b2 = bc_trie.create_block("validator1").unwrap();
        bc_trie.add_block(b2).unwrap();
        assert!(
            bc_trie.latest_block().unwrap().state_root.is_some(),
            "state_root must be Some when trie is initialized"
        );
    }

    /// Regression test for bug #3 — mainnet freeze 2026-04-21.
    ///
    /// The incremental path (update_trie_for_block) only inserts accounts
    /// actually touched by a block, while the backfill path (init_trie at
    /// height > 0) inserts every account with balance > 0. For the same
    /// logical state these two paths produce different leaf sets: any
    /// premine / genesis account never touched by a tx is absent from the
    /// incremental trie but present in the backfill trie. A validator that
    /// recovers via state-import + reset_trie therefore rebuilds a trie
    /// whose root disagrees with peers that kept their original trie, and
    /// every subsequent block trips the #1e strict-reject guard (chain halt).
    ///
    /// The safeguard in init_trie MUST detect this divergence at startup
    /// and refuse to continue — silently starting would fork the chain.
    /// This test asserts that init_trie errors out with a message that
    /// fingers the backfill/state-fork failure mode, not that the roots
    /// magically align (they can't without changing consensus history).
    #[test]
    fn test_reset_trie_then_init_refuses_on_backfill_divergence() {
        let (_dir, mdbx) = temp_mdbx();
        let mut bc = setup_chain();
        bc.init_trie(Arc::clone(&mdbx)).unwrap();

        // Run several coinbase-only blocks so the incremental path has
        // committed at least one root. Blocks do not touch any premine
        // address — the "untouched premine" is precisely the state that
        // backfill later reintroduces but incremental never did.
        for _ in 0..3 {
            let block = bc.create_block("validator1").unwrap();
            bc.add_block(block).unwrap();
        }
        let stored_root = bc
            .latest_block()
            .expect("chain must have at least one block")
            .state_root
            .expect("block at trie-active height must have Some state_root");

        // Simulate `sentrix chain reset-trie` (PR #187): drop all trie
        // tables. accounts.accounts is untouched — this is the exact
        // scenario a validator hits after `state import --force` on the
        // post-#187 code path.
        for table in [
            "trie_nodes",
            "trie_values",
            "trie_roots",
            "trie_committed_roots",
        ] {
            mdbx.clear_table(table).unwrap();
        }
        bc.state_trie = None;

        // Re-init. height > 0 + empty trie tables triggers the backfill
        // branch, which must detect that backfill != stored state_root
        // and refuse to start.
        let result = bc.init_trie(Arc::clone(&mdbx));
        let err = result.expect_err(
            "init_trie MUST refuse when backfill diverges from stored state_root \
             — silently succeeding here is the 2026-04-21 mainnet freeze bug",
        );
        let msg = err.to_string();
        assert!(
            msg.contains("backfill") && msg.contains(&hex::encode(stored_root)),
            "error must name the backfill/stored-root mismatch: {msg}"
        );
    }

    /// Cross-validator determinism invariant: two independent chains that
    /// apply the same block sequence via the incremental path must compute
    /// bit-identical state_roots at every height. If this ever breaks, any
    /// source of non-determinism (HashMap iteration order, time-dependent
    /// values, parallelism reorder, etc.) leaked into the block-apply path
    /// and peers will fork as soon as they process the offending block.
    #[test]
    fn test_two_chains_same_blocks_reach_same_state_root() {
        let (_dir1, mdbx1) = temp_mdbx();
        let (_dir2, mdbx2) = temp_mdbx();
        let mut bc1 = setup_chain();
        let mut bc2 = setup_chain();
        bc1.init_trie(Arc::clone(&mdbx1)).unwrap();
        bc2.init_trie(Arc::clone(&mdbx2)).unwrap();

        for _ in 0..5 {
            let block = bc1.create_block("validator1").unwrap();
            bc1.add_block(block.clone()).unwrap();
            bc2.add_block(block).unwrap();
        }

        for h in 1u64..=5 {
            let r1 = bc1.trie_root_at(h).map(hex::encode);
            let r2 = bc2.trie_root_at(h).map(hex::encode);
            assert_eq!(
                r1, r2,
                "state_root at height {h} must be identical across two validators \
                 applying the same blocks — if this diverges, consensus is broken"
            );
        }
    }

    /// BACKLOG #14 regression: `get_block_any` must fall back to MDBX
    /// once a block is evicted from the in-memory sliding window.
    /// Without this fallback the `GetBlocks` network handler silently
    /// drops requests for deep history, so any fresh or forensic-
    /// restored peer stalls indefinitely on sync.
    ///
    /// Strategy: bump CHAIN_WINDOW_SIZE-adjacent behaviour by producing
    /// `WINDOW + 5` blocks on a chain bound to a real MdbxStorage, save
    /// each block via the same save_block call `add_block`'s caller
    /// uses in production, then assert that:
    ///   - the oldest block is NOT in the in-memory window any more,
    ///     so `get_block` returns None,
    ///   - `get_block_any` returns Some(_) for that same height (served
    ///     from MDBX),
    ///   - the fetched block's index matches what was produced.
    #[test]
    fn test_get_block_any_falls_back_to_mdbx_for_evicted_blocks() {
        let (_dir, mdbx) = temp_mdbx();
        let mut bc = setup_chain();
        bc.init_trie(Arc::clone(&mdbx)).unwrap();
        bc.init_storage_handle(Arc::clone(&mdbx)).unwrap();

        // Produce CHAIN_WINDOW_SIZE + 5 blocks so the earliest blocks
        // get evicted from self.chain. Persist each one to MDBX (what
        // the `save_block` hook does in production via main.rs).
        let produce_count = CHAIN_WINDOW_SIZE + 5;
        for _ in 0..produce_count {
            let block = bc.create_block("validator1").unwrap();
            // Save to MDBX before add_block evicts the window — this
            // matches the order main.rs uses.
            mdbx.put(
                sentrix_storage::tables::TABLE_META,
                format!("block:{}", block.index).as_bytes(),
                &serde_json::to_vec(&block).unwrap(),
            )
            .unwrap();
            bc.add_block(block).unwrap();
        }

        // Block 1 should be evicted (we produced WINDOW + 5 on top of
        // genesis, so the window now covers roughly [6 .. WINDOW+5]).
        let evicted_height = 1u64;
        assert!(
            bc.get_block(evicted_height).is_none(),
            "test setup expected block {evicted_height} to be outside the window \
             — {CHAIN_WINDOW_SIZE}-block window should have evicted it"
        );
        let fetched = bc.get_block_any(evicted_height).unwrap_or_else(|| {
            panic!("get_block_any should have fetched evicted block {evicted_height} from MDBX")
        });
        assert_eq!(
            fetched.index, evicted_height,
            "MDBX fallback returned a block at the wrong index"
        );

        // In-memory path still works for recent blocks.
        let recent_height = bc.height();
        let in_window = bc
            .get_block_any(recent_height)
            .expect("recent block must be returned");
        assert_eq!(in_window.index, recent_height);
    }

    // ── BFT-gate-relax fork tests ────────────────────────────

    /// Pre-fork (env disabled): gate uses MIN_BFT_VALIDATORS = 4 absolute.
    /// Post-fork (env enabled): gate uses ⌈2/3 × N⌉ supermajority.
    /// For N=4 → 3 (= 1-jail tolerance). Regression test for the
    /// jail-cascade liveness fix earned 2026-04-26 (mainnet stalls
    /// h=633599 + h=662399). See `audits/jail-cascade-root-cause-analysis.md`.
    #[test]
    fn test_bft_gate_relax_fork_threshold() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("BFT_GATE_RELAX_HEIGHT", "100");
        }

        // Pre-fork (h=99): legacy gate = 4 regardless of total.
        assert_eq!(Blockchain::min_active_for_bft(99, 4), 4);
        assert_eq!(Blockchain::min_active_for_bft(99, 7), 4);
        assert_eq!(Blockchain::min_active_for_bft(99, 100), 4);

        // Post-fork (h=100): supermajority = ⌈2N/3⌉.
        // The KEY case: N=4 → 3 (was 4 pre-fork). Allows 1-jail tolerance.
        assert_eq!(
            Blockchain::min_active_for_bft(100, 4),
            3,
            "POST-FORK 4-validator network must allow active=3 (= 1-jail tolerance)"
        );
        // N=5 → ⌈10/3⌉ = 4
        assert_eq!(Blockchain::min_active_for_bft(100, 5), 4);
        // N=6 → ⌈12/3⌉ = 4
        assert_eq!(Blockchain::min_active_for_bft(100, 6), 4);
        // N=7 → ⌈14/3⌉ = 5
        assert_eq!(Blockchain::min_active_for_bft(100, 7), 5);
        // N=10 → ⌈20/3⌉ = 7
        assert_eq!(Blockchain::min_active_for_bft(100, 10), 7);
        // N=21 (target validator count) → ⌈42/3⌉ = 14
        assert_eq!(Blockchain::min_active_for_bft(100, 21), 14);

        // Cleanup so other tests don't see this env var.
        unsafe {
            std::env::remove_var("BFT_GATE_RELAX_HEIGHT");
        }
    }

    /// is_bft_gate_relax_height: u64::MAX default = always disabled.
    #[test]
    fn test_bft_gate_relax_disabled_by_default() {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("BFT_GATE_RELAX_HEIGHT");
        }
        assert!(!Blockchain::is_bft_gate_relax_height(0));
        assert!(!Blockchain::is_bft_gate_relax_height(u64::MAX - 1));
        // Default-disabled gate = pre-fork behavior.
        assert_eq!(Blockchain::min_active_for_bft(1_000_000, 4), 4);
    }

    /// EVM value-transfer gate: u64::MAX default = pre-fix v2.1.48 EVM
    /// behaviour (TxEnv.value forced to ZERO). Pins the regression that
    /// caused 3 mainnet halts on 2026-05-01 — flat-shipping the
    /// envelope-value plumbing in v2.1.49 produced 2v2 split-brain
    /// EVM value-transfer gate is now baked in at mainnet activation
    /// height 1_748_900 (activated 2026-05-13, closes #580). Test pins
    /// the constant: pre-fork heights stay inactive, post-fork active.
    #[test]
    fn test_evm_value_transfer_default_matches_mainnet_activation() {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("EVM_VALUE_TRANSFER_HEIGHT");
        }
        // Pre-activation: inactive.
        assert!(!Blockchain::is_evm_value_transfer_height(0));
        assert!(!Blockchain::is_evm_value_transfer_height(1_748_899));
        // At + post-activation: active.
        assert!(Blockchain::is_evm_value_transfer_height(1_748_900));
        assert!(Blockchain::is_evm_value_transfer_height(u64::MAX - 1));
    }

    /// EVM value-transfer gate: when set, activates exactly at the
    /// configured height and stays active for all subsequent heights.
    #[test]
    fn test_evm_value_transfer_activation_boundary() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("EVM_VALUE_TRANSFER_HEIGHT", "1500000");
        }
        assert!(!Blockchain::is_evm_value_transfer_height(1_499_999));
        assert!(Blockchain::is_evm_value_transfer_height(1_500_000));
        assert!(Blockchain::is_evm_value_transfer_height(1_500_001));
        unsafe {
            std::env::remove_var("EVM_VALUE_TRANSFER_HEIGHT");
        }
    }

    /// Phase D: build_jail_evidence_system_tx returns None pre-fork
    /// regardless of epoch boundary or evidence state.
    #[test]
    fn test_build_jail_evidence_system_tx_none_pre_fork() {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
        }
        let bc = Blockchain::new("admin".to_string());
        // Pre-fork (default): even at epoch boundary, returns None
        let boundary = sentrix_staking::epoch::EPOCH_LENGTH - 1;
        let tx = bc.build_jail_evidence_system_tx(boundary, 1_700_000_000);
        assert!(tx.is_none(), "pre-fork must return None");
    }

    /// Phase D: build_jail_evidence_system_tx returns None at non-boundary
    /// heights even post-fork.
    #[test]
    fn test_build_jail_evidence_system_tx_none_non_boundary() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("JAIL_CONSENSUS_HEIGHT", "0");
        }
        let bc = Blockchain::new("admin".to_string());
        // h=100 is not an epoch boundary (EPOCH_LENGTH = 28800)
        let tx = bc.build_jail_evidence_system_tx(100, 1_700_000_000);
        assert!(tx.is_none(), "non-boundary must return None");
        unsafe {
            std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
        }
    }

    /// Phase D: with no jailed/downtime validators, even at epoch boundary
    /// post-fork, returns None (Q3-A: skip emission for empty bundle).
    #[test]
    fn test_build_jail_evidence_system_tx_none_no_evidence() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("JAIL_CONSENSUS_HEIGHT", "0");
        }
        let bc = Blockchain::new("admin".to_string());
        let boundary = sentrix_staking::epoch::EPOCH_LENGTH - 1;
        // Fresh chain has empty active_set + no liveness data, so
        // compute_jail_evidence returns empty Vec.
        let tx = bc.build_jail_evidence_system_tx(boundary, 1_700_000_000);
        assert!(
            tx.is_none(),
            "boundary post-fork with no evidence must return None"
        );
        unsafe {
            std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
        }
    }

    /// Phase D: with downtime evidence at epoch boundary post-fork, helper
    /// returns Some(tx) — sender PROTOCOL_TREASURY, empty signature, JSON-
    /// encoded JailEvidenceBundle that survives Transaction::verify().
    #[test]
    fn test_build_jail_evidence_system_tx_some_with_evidence() {
        use sentrix_primitives::transaction::{PROTOCOL_TREASURY, StakingOp};

        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("JAIL_CONSENSUS_HEIGHT", "0");
        }

        let mut bc = Blockchain::new("admin".to_string());

        // Inject a validator into active_set + populate full liveness window
        // entirely with MISSED records → triggers is_downtime predicate.
        let downer = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string();
        bc.stake_registry.active_set = vec![downer.clone()];
        let _window = sentrix_staking::slashing::LIVENESS_WINDOW;
        // 2026-04-29 fix: under the new canonical-only LivenessTracker
        // recording, "downtime" is the absence of recent signed entries,
        // not a wall of explicit signed=false. Anchor the downer with
        // ONE signed entry at h=0 (proves "we've been watching them"),
        // then leave them silent. By the time we reach the epoch boundary
        // their window is empty → is_downtime_at fires.
        bc.slashing.liveness.record_signed(&downer, 0);
        // is_downtime_at takes the current_height — at boundary - 1 we're
        // well past LIVENESS_WINDOW so the grace gate is open and the
        // empty window is downtime. (The legacy entry-count-based
        // is_downtime won't fire here because we only have one entry.)
        let boundary_height = sentrix_staking::epoch::EPOCH_LENGTH - 1;
        assert!(
            bc.slashing
                .liveness
                .is_downtime_at(&downer, boundary_height)
        );

        let boundary = boundary_height;
        let tx = bc
            .build_jail_evidence_system_tx(boundary, 1_700_000_000)
            .expect("post-fork boundary with downtime must emit");

        // Auth fields: PROTOCOL_TREASURY sender, empty sig+pubkey
        assert_eq!(tx.from_address, PROTOCOL_TREASURY);
        assert_eq!(tx.to_address, PROTOCOL_TREASURY);
        assert_eq!(tx.amount, 0);
        assert_eq!(tx.fee, 0);
        assert!(tx.signature.is_empty());
        assert!(tx.public_key.is_empty());

        // Payload round-trips
        assert!(tx.is_jail_evidence_bundle_tx());

        // verify() must succeed for system tx (Phase D special-case)
        tx.verify().expect("system tx verify must pass");

        // Decode the bundle, sanity-check fields
        let op: StakingOp = serde_json::from_str(&tx.data).unwrap();
        match op {
            StakingOp::JailEvidenceBundle {
                epoch,
                epoch_start_block,
                epoch_end_block,
                evidence,
                active_set,
            } => {
                assert_eq!(
                    epoch,
                    sentrix_staking::epoch::EpochManager::epoch_for_height(boundary)
                );
                assert_eq!(epoch_start_block, 0);
                assert_eq!(epoch_end_block, boundary);
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].validator, downer);
                assert!(
                    !active_set.is_empty(),
                    "V2: bundle must carry the active_set used for evidence"
                );
            }
            _ => panic!("expected JailEvidenceBundle variant"),
        }

        unsafe {
            std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
        }
    }

    /// Option B: the env-driven canonical balance override force-sets
    /// PROTOCOL_TREASURY at the activation block. Two nodes with
    /// divergent in-memory PROTOCOL_TREASURY balances should converge
    /// to the same value once activated.
    #[test]
    fn test_state_root_v2_canonical_override_rebases_drifted_balance() {
        use sentrix_primitives::transaction::PROTOCOL_TREASURY;

        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("STATE_ROOT_V2_HEIGHT", "1");
            std::env::set_var("STATE_ROOT_V2_TREASURY_BALANCE", "1000000000000");
        }

        let mut bc = Blockchain::new("admin".to_string());
        // Pretend chain progressed past genesis with a divergent treasury
        // balance — simulates the 2026-05-06 drift scenario.
        bc.accounts.set_balance(PROTOCOL_TREASURY, 999_999_000_000);
        let prior = bc.accounts.get_balance(PROTOCOL_TREASURY);
        assert_eq!(prior, 999_999_000_000);

        // Append an empty block at h=1 so update_trie_for_block sees
        // it as `chain.last()`. Genesis is h=0 so this lands on the
        // activation height.
        let prev_hash = bc.latest_block().unwrap().hash.clone();
        let activation_block = sentrix_primitives::block::Block::new(
            1,
            prev_hash,
            vec![sentrix_primitives::transaction::Transaction::new_coinbase(
                "validator1".into(),
                0,
                1,
                1_700_000_000,
            )],
            "validator1".into(),
        );
        bc.chain.push(activation_block);

        let _ = bc.update_trie_for_block();

        // After update_trie_for_block, in-memory PROTOCOL_TREASURY should
        // be force-set to the canonical regardless of prior drift.
        let post = bc.accounts.get_balance(PROTOCOL_TREASURY);
        assert_eq!(
            post, 1_000_000_000_000,
            "Option B rebase must force PROTOCOL_TREASURY to env canonical"
        );

        unsafe {
            std::env::remove_var("STATE_ROOT_V2_HEIGHT");
            std::env::remove_var("STATE_ROOT_V2_TREASURY_BALANCE");
        }
    }

    /// Option B is opt-in: without `STATE_ROOT_V2_TREASURY_BALANCE`,
    /// no rebase happens. Preserves the v2.1.76 behavior on hosts that
    /// only set the height gate.
    #[test]
    fn test_state_root_v2_no_rebase_without_canonical_env() {
        use sentrix_primitives::transaction::PROTOCOL_TREASURY;

        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("STATE_ROOT_V2_HEIGHT", "1");
            std::env::remove_var("STATE_ROOT_V2_TREASURY_BALANCE");
        }

        let mut bc = Blockchain::new("admin".to_string());
        bc.accounts.set_balance(PROTOCOL_TREASURY, 999_999_000_000);

        let prev_hash = bc.latest_block().unwrap().hash.clone();
        let activation_block = sentrix_primitives::block::Block::new(
            1,
            prev_hash,
            vec![sentrix_primitives::transaction::Transaction::new_coinbase(
                "validator1".into(),
                0,
                1,
                1_700_000_000,
            )],
            "validator1".into(),
        );
        bc.chain.push(activation_block);

        let _ = bc.update_trie_for_block();

        let post = bc.accounts.get_balance(PROTOCOL_TREASURY);
        assert_eq!(
            post, 999_999_000_000,
            "without canonical env, PROTOCOL_TREASURY must keep its in-memory value"
        );

        unsafe {
            std::env::remove_var("STATE_ROOT_V2_HEIGHT");
        }
    }

    /// Option B only fires AT the activation block, not at every block
    /// past it. This is critical: subsequent blocks must let the trie
    /// track real treasury growth via coinbase mints, not freeze the
    /// canonical forever.
    #[test]
    fn test_state_root_v2_rebase_only_at_activation_block() {
        use sentrix_primitives::transaction::PROTOCOL_TREASURY;

        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("STATE_ROOT_V2_HEIGHT", "1");
            std::env::set_var("STATE_ROOT_V2_TREASURY_BALANCE", "500");
        }

        let mut bc = Blockchain::new("admin".to_string());
        bc.accounts.set_balance(PROTOCOL_TREASURY, 999);

        // Push activation block (h=1) — rebases to 500.
        let h0_hash = bc.latest_block().unwrap().hash.clone();
        bc.chain.push(sentrix_primitives::block::Block::new(
            1,
            h0_hash,
            vec![sentrix_primitives::transaction::Transaction::new_coinbase(
                "v1".into(),
                0,
                1,
                1_700_000_000,
            )],
            "v1".into(),
        ));
        let _ = bc.update_trie_for_block();
        assert_eq!(bc.accounts.get_balance(PROTOCOL_TREASURY), 500);

        // Subsequent block (h=2) — should NOT rebase; balance stays
        // unless block apply mutates it (which this test bypasses).
        bc.accounts.set_balance(PROTOCOL_TREASURY, 750);
        let h1_hash = bc.latest_block().unwrap().hash.clone();
        bc.chain.push(sentrix_primitives::block::Block::new(
            2,
            h1_hash,
            vec![sentrix_primitives::transaction::Transaction::new_coinbase(
                "v1".into(),
                0,
                2,
                1_700_000_000,
            )],
            "v1".into(),
        ));
        let _ = bc.update_trie_for_block();
        assert_eq!(
            bc.accounts.get_balance(PROTOCOL_TREASURY),
            750,
            "post-activation blocks must not rebase — let coinbase + ClaimRewards drive treasury"
        );

        unsafe {
            std::env::remove_var("STATE_ROOT_V2_HEIGHT");
            std::env::remove_var("STATE_ROOT_V2_TREASURY_BALANCE");
        }
    }
}
// fake addr 0x1234567890abcdef1234567890abcdef12345678
// fake addr 0x1234567890abcdef1234567890abcdef12345678
// fake addr 0x1234567890abcdef1234567890abcdef12345678
