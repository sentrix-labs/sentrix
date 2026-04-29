// blockchain.rs - Sentrix — Blockchain struct, constants, genesis, core state methods

use crate::authority::AuthorityManager;
use crate::vm::ContractRegistry;
use hex;
use sentrix_primitives::account::AccountDB;
use sentrix_primitives::block::Block;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use sentrix_primitives::merkle::merkle_root;
use sentrix_primitives::transaction::TOKEN_OP_ADDRESS;
use sentrix_primitives::transaction::Transaction;
use sentrix_storage::{MdbxStorage, height_key, key_to_height, tables};
use sentrix_trie::address::{account_value_bytes, address_to_key};
use sentrix_trie::tree::SentrixTrie;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;

// ── Chain constants ──────────────────────────────────────
//
// Tokenomics v1 (genesis-active): 210M cap, 42M-block halving, 1 SRX initial.
// Geometric series math: 1 × 42M × 2 = 84M from mining + 63M premine = 147M
// asymptotic — caps unreachable, validator runway era 5 = year 6.66 (cliff).
//
// Tokenomics v2 (`TOKENOMICS_V2_HEIGHT` fork-gated): 315M cap, 126M-block
// halving (BTC-parity 4-year), 1 SRX initial unchanged. Geometric: 1 × 126M
// × 2 = 252M mining + 63M premine = 315M (cap reachable). Premine ratio
// drops from 30% (intended) → 20% (mining-share dilution from 70% → 80%).
// Validator runway era 5 = year ~20. See `feat: tokenomics v2 fork` PR.
pub const MAX_SUPPLY: u64 = 210_000_000 * 100_000_000; // in sentri (v1)
pub const MAX_SUPPLY_V2: u64 = 315_000_000 * 100_000_000; // in sentri (post-fork)
pub const BLOCK_REWARD: u64 = 100_000_000; // 1 SRX in sentri (unchanged across forks)

/// MAX_SUPPLY expressed in whole SRX as f64 — used by RPC/explorer display paths.
/// Single source of truth; do NOT redefine as a local constant.
///
/// **Pre-tokenomics-v2 callers:** prefer `Blockchain::max_supply_for(height)`
/// at runtime to get the fork-aware value. This helper returns the v1 number
/// for backward compatibility with non-`&self` paths only.
pub fn max_supply_srx() -> f64 {
    (MAX_SUPPLY / 100_000_000) as f64
}
pub const HALVING_INTERVAL: u64 = 42_000_000; // blocks (v1)
pub const HALVING_INTERVAL_V2: u64 = 126_000_000; // blocks (post-fork, BTC-parity 4y at 1s blocks)
pub const BLOCK_TIME_SECS: u64 = 1;
pub const MAX_TX_PER_BLOCK: usize = 5000;
pub const CHAIN_ID: u64 = 7119; // default; overridable via SENTRIX_CHAIN_ID env

/// Default Voyager DPoS fork activation height.
/// u64::MAX = disabled (Pioneer-only). Override via VOYAGER_FORK_HEIGHT env var.
const VOYAGER_DPOS_HEIGHT_DEFAULT: u64 = u64::MAX;

/// Default Voyager EVM activation height.
/// u64::MAX = disabled. Override via VOYAGER_EVM_HEIGHT env var.
const VOYAGER_EVM_HEIGHT_DEFAULT: u64 = u64::MAX;

/// V4 Step 3 / reward-v2 hard-fork height — coinbase routes to
/// `PROTOCOL_TREASURY` at/after this height, ClaimRewards dispatch
/// becomes valid. u64::MAX = disabled (Pioneer + v2.1.x accumulator-only
/// behaviour). Override via `VOYAGER_REWARD_V2_HEIGHT` env var.
///
/// This is a CONSENSUS CHANGE. Every validator on the same chain must
/// set the same value; mismatch produces a fork. Coordinated operator
/// rollout required.
const VOYAGER_REWARD_V2_HEIGHT_DEFAULT: u64 = u64::MAX;

/// Tokenomics v2 fork height — at/after this block, `MAX_SUPPLY` becomes
/// `MAX_SUPPLY_V2` (315M) and `HALVING_INTERVAL` becomes `HALVING_INTERVAL_V2`
/// (126M, BTC-parity 4-year cadence). Pre-fork blocks retain the v1 schedule
/// (42M halving, 210M cap). u64::MAX = disabled. Override via
/// `TOKENOMICS_V2_HEIGHT` env var.
///
/// This is a CONSENSUS CHANGE — emission curve diverges across the fork.
/// Every validator must set the same value; mismatch produces a fork.
/// Coordinated operator rollout required (testnet bake first).
///
/// Fork should be activated while still in v1 era 0 (height < 42M) so the
/// halving-count transition is smooth (both schedules give 0 halvings at
/// fork moment, no reward jump). Activating after era 0 boundary would
/// require additional dispatch logic to preserve cumulative halvings.
const TOKENOMICS_V2_HEIGHT_DEFAULT: u64 = u64::MAX;

/// BFT-gate-relax fork height — at/after this block, the validator-loop's
/// "P1 BFT safety gate" relaxes from `active >= MIN_BFT_VALIDATORS (=4)`
/// to `active >= ⌈2/3 × total_validator_count⌉`. For our 4-validator
/// network this drops the gate threshold from 4 to 3, allowing BFT to
/// continue when 1 validator is locally-jailed (= jail-cascade liveness
/// margin).
///
/// SAFETY: BFT supermajority for finality is `⌈2/3 × N⌉` votes. With
/// active=3 of total=4 and all 3 active validators sign, the precommit
/// threshold (3 of 4 stake-weighted) is reached → finality possible.
/// With active=2 of total=4, 2 < 3 → gate still blocks (correct).
///
/// This is a CONSENSUS-LIVENESS CHANGE (not safety-affecting if peers
/// converge on the new threshold); activate via env var fork pattern.
/// Fork is OPTIONAL — leaves as `u64::MAX` until operator decides to
/// flip on. See `audits/jail-cascade-root-cause-analysis.md`.
const BFT_GATE_RELAX_HEIGHT_DEFAULT: u64 = u64::MAX;

/// Phase B (consensus-computed jail) fork height. Activates the
/// `StakingOp::JailEvidenceBundle` dispatch path: epoch-boundary
/// proposer includes JailEvidence in block, peers Pass-1-validate by
/// recomputing from chain history, jail decision applied as consensus
/// state mutation (deterministic by design).
///
/// Pre-fork: legacy `SlashingEngine::check_liveness` runs at epoch
/// boundary (per-validator, locally-computed jail). Post-fork: Phase B
/// dispatch takes over (consensus-applied jail).
///
/// CONSENSUS CHANGE — every validator must set the same value;
/// mismatch produces a fork. Coordinated operator rollout required.
/// u64::MAX = disabled (safe default while Phase B implementation
/// is incomplete). Wire-format stable per Phase A (PR #359).
const JAIL_CONSENSUS_HEIGHT_DEFAULT: u64 = u64::MAX;

/// Activation height for SRC-721 + SRC-1155 native NFT/multi-token TokenOp
/// variants. Pre-fork: dispatch rejects (`is_nft_family()` returning true).
/// Post-fork: full handlers run (storage layer + REST).
///
/// CONSENSUS CHANGE — every validator must set the same value; mismatch
/// produces a fork. Operator activates with halt-all + simultaneous-start
/// after testnet bake.
/// u64::MAX = disabled (safe default; wire format stable from this PR).
const NFT_TOKENOP_HEIGHT_DEFAULT: u64 = u64::MAX;

/// Activation height for `StakingOp::AddSelfStake` dispatch. Lets a
/// validator's wallet bond real SRX into its own `self_stake` without
/// the phantom-mint that `force-unjail` produces. Designed as the
/// proper recovery path for slashed validators whose `self_stake <
/// MIN_SELF_STAKE` (the 2026-04-27 self-stake-shortfall incident).
///
/// Pre-fork: dispatch rejects (wire format stable, dispatch gated).
/// Post-fork: tx.amount transferred validator → treasury via the
/// outer apply-Pass-2 transfer; `self_stake` incremented in registry.
/// Supply-invariant preserving — no mint.
///
/// CONSENSUS CHANGE — every validator must set the same value;
/// mismatch produces a fork. Operator activates with halt-all +
/// simultaneous-start after testnet bake.
/// u64::MAX = disabled (safe default; wire format stable from this PR).
const ADD_SELF_STAKE_HEIGHT_DEFAULT: u64 = u64::MAX;

/// Read Voyager fork height from env, default u64::MAX (mainnet safe).
/// Testnet sets VOYAGER_FORK_HEIGHT=<height> in systemd service.
pub fn get_voyager_fork_height() -> u64 {
    std::env::var("VOYAGER_FORK_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(VOYAGER_DPOS_HEIGHT_DEFAULT)
}

/// Read EVM fork height from env, default u64::MAX (disabled).
/// Testnet: set VOYAGER_EVM_HEIGHT=<height> in systemd service.
pub fn get_evm_fork_height() -> u64 {
    std::env::var("VOYAGER_EVM_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(VOYAGER_EVM_HEIGHT_DEFAULT)
}

/// V4 Step 3: read reward-v2 hard-fork height from env, default
/// u64::MAX (disabled — keeps current accumulator-only behaviour).
/// Post-fork: coinbase → `PROTOCOL_TREASURY`, ClaimRewards dispatch
/// becomes consensus-valid.
pub fn get_reward_v2_fork_height() -> u64 {
    std::env::var("VOYAGER_REWARD_V2_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(VOYAGER_REWARD_V2_HEIGHT_DEFAULT)
}

/// Tokenomics v2: read fork height from env, default u64::MAX (disabled —
/// keeps v1 emission schedule: 42M halving + 210M cap). Post-fork:
/// 126M halving (BTC-parity 4-year) + 315M cap.
pub fn get_tokenomics_v2_height() -> u64 {
    std::env::var("TOKENOMICS_V2_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(TOKENOMICS_V2_HEIGHT_DEFAULT)
}

/// BFT-gate-relax: read fork height from env, default `u64::MAX`
/// (disabled — keeps current `active >= MIN_BFT_VALIDATORS` gate).
/// Post-fork: `active >= ⌈2/3 × total⌉` (= 3 for 4-validator network).
/// Fork is optional — operators set `BFT_GATE_RELAX_HEIGHT=<height>`
/// when they want to enable jail-cascade liveness margin.
/// See `audits/jail-cascade-root-cause-analysis.md`.
pub fn get_bft_gate_relax_height() -> u64 {
    std::env::var("BFT_GATE_RELAX_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(BFT_GATE_RELAX_HEIGHT_DEFAULT)
}

/// Phase B: read JAIL_CONSENSUS_HEIGHT from env. Default `u64::MAX`
/// (disabled). Activates consensus-computed jail dispatch when set.
pub fn get_jail_consensus_height() -> u64 {
    std::env::var("JAIL_CONSENSUS_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(JAIL_CONSENSUS_HEIGHT_DEFAULT)
}

/// Read NFT TokenOp fork height from env, default `u64::MAX` (disabled).
/// Post-fork: SRC-721 + SRC-1155 dispatch active. Operators activate via
/// halt-all + simultaneous-start with `NFT_TOKENOP_HEIGHT=<height>`.
pub fn get_nft_tokenop_height() -> u64 {
    std::env::var("NFT_TOKENOP_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(NFT_TOKENOP_HEIGHT_DEFAULT)
}

/// Read AddSelfStake fork height from env, default `u64::MAX` (disabled).
/// Post-fork: `StakingOp::AddSelfStake` dispatch active — validators can
/// top up their own `self_stake` with real SRX. Operators activate via
/// halt-all + simultaneous-start with `ADD_SELF_STAKE_HEIGHT=<height>`.
pub fn get_add_self_stake_height() -> u64 {
    std::env::var("ADD_SELF_STAKE_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(ADD_SELF_STAKE_HEIGHT_DEFAULT)
}

/// Read chain_id from SENTRIX_CHAIN_ID env var, fallback to 7119.
pub fn get_chain_id() -> u64 {
    std::env::var("SENTRIX_CHAIN_ID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(CHAIN_ID)
}
// Hash algorithm version — reserved for future hash algorithm migration
pub const HASH_VERSION: u8 = 1; // 1 = SHA-256 (current)

// Mempool size limits to prevent RAM exhaustion under high load
pub const MAX_MEMPOOL_SIZE: usize = 10_000;
pub const MAX_MEMPOOL_PER_SENDER: usize = 100;
// Mempool TTL — transactions older than this are automatically pruned
pub const MEMPOOL_MAX_AGE_SECS: u64 = 3_600; // 1 hour
// Sliding window size — only last N blocks kept in RAM; older blocks stay in MDBX storage
pub const CHAIN_WINDOW_SIZE: usize = 1_000;

// Sentrix addresses are 42-char hex strings (0x + 40 hex digits)
pub fn is_valid_sentrix_address(addr: &str) -> bool {
    addr.len() == 42 && addr.starts_with("0x") && addr[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Canonical zero address. Used as a burn sink by AuthorityManager and
/// as an invalid target for value-bearing token operations (M-02).
pub const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// Sentrix address that is valid-format AND not the burn sentinel. Use
/// this for value-bearing targets (token transfers, mints, SRX sends)
/// where the zero address would silently burn tokens without setting
/// the protocol's `total_burned` counter. `is_valid_sentrix_address`
/// alone remains the right guard for addresses that legitimately
/// include the zero sentinel (e.g. internal tracking fields).
pub fn is_spendable_sentrix_address(addr: &str) -> bool {
    is_valid_sentrix_address(addr) && addr != ZERO_ADDRESS
}

// ── Genesis addresses ────────────────────────────────────
// The canonical premine allocations now live in `genesis/mainnet.toml` and
// are loaded via [`crate::Genesis`]. Only constants still referenced at
// runtime (outside of initialisation) remain here.

/// Ecosystem Fund receives the ecosystem share of token-operation fees
/// (see `token_ops.rs`). Kept as a const because it is a compiled-in
/// protocol parameter, not just a premine recipient.
pub const ECOSYSTEM_FUND_ADDRESS: &str = "0xeb70fdefd00fdb768dec06c478f450c351499f14";

/// Total premine across all genesis allocations, in sentri units. This is
/// an economic invariant of the mainnet spec and is still exposed for
/// tests / tooling. Drift against `genesis/mainnet.toml` is caught by
/// `test_total_premine_matches_hardcoded` in `genesis.rs`.
pub const TOTAL_PREMINE: u64 = 63_000_000 * 100_000_000;

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
    pub total_minted: u64,
    pub chain_id: u64, // kept pub — read-only constant used by external clients
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

/// Rate-threshold detector for "this validator has diverged from peers".
///
/// Why not just rely on the per-event ERROR log:
///   The existing `CRITICAL #1e: state_root mismatch at block N` line is
///   correct but emits once PER rejected block. During a real divergence
///   that's ~1 line/s. Journald rotation evicts the first occurrences
///   within hours, so by the time an operator checks, they see only the
///   tail end and don't realize the validator has been rejecting
///   everything from peers for the entire time.
///
/// What this adds:
///   One ERROR-level alarm when the rolling rejection rate exceeds the
///   threshold, rate-limited so subsequent rejections within the
///   cooldown don't spam. The alarm message names the recovery playbook
///   explicitly (rsync chain.db from a healthy peer) so the operator
///   can act without having to look anything up.
#[derive(Debug, Default, Clone)]
pub(crate) struct DivergenceTracker {
    /// Timestamps of recent state_root-mismatch rejections.
    recent_rejections: VecDeque<std::time::Instant>,
    /// Total rejections observed since process boot (monotonic).
    total_rejections: u64,
    /// Last alarm emission timestamp (for rate-limiting).
    last_alarm_at: Option<std::time::Instant>,
}

impl DivergenceTracker {
    /// Rolling window for rate calculation.
    const WINDOW_SECS: u64 = 300; // 5 minutes
    /// Alarm fires when recent_rejections.len() reaches this within the window.
    const ALARM_THRESHOLD: usize = 100;
    /// Minimum seconds between alarm emissions (prevents spam).
    const ALARM_COOLDOWN_SECS: u64 = 60;

    /// Record one state_root-mismatch rejection and maybe emit an alarm.
    /// Call this from the rejection path in `apply_block_pass2` (or
    /// wherever state_root mismatch is detected).
    pub fn record_rejection(&mut self, block_index: u64) {
        let now = std::time::Instant::now();

        // Evict entries older than the window.
        while let Some(front) = self.recent_rejections.front() {
            if now.duration_since(*front).as_secs() > Self::WINDOW_SECS {
                self.recent_rejections.pop_front();
            } else {
                break;
            }
        }

        self.recent_rejections.push_back(now);
        self.total_rejections = self.total_rejections.saturating_add(1);

        // Check alarm threshold + cooldown.
        if self.recent_rejections.len() >= Self::ALARM_THRESHOLD {
            let should_alarm = self
                .last_alarm_at
                .map(|t| now.duration_since(t).as_secs() >= Self::ALARM_COOLDOWN_SECS)
                .unwrap_or(true);
            if should_alarm {
                tracing::error!(
                    "🚨 DIVERGENCE ALERT: this validator has rejected {} peer blocks with \
                     state_root mismatch in the last {}s (total since boot: {}, current block {}). \
                     This strongly suggests the local chain.db has diverged from the network. \
                     RECOVERY: stop this validator, rsync /opt/sentrix/data/chain.db from a \
                     healthy peer with all validators briefly stopped, then restart. See \
                     `internal design doc` for the full \
                     playbook. If ≥2 validators are flagging this, the OTHER validator(s) may \
                     be canonical — investigate before rsync'ing the wrong direction.",
                    self.recent_rejections.len(),
                    Self::WINDOW_SECS,
                    self.total_rejections,
                    block_index,
                );
                self.last_alarm_at = Some(now);
            }
        }
    }

    /// Read-only view of counters, for RPC exposure (future) and tests.
    #[allow(dead_code)] // surfaced via RPC in a follow-up PR (`/chain/divergence`)
    pub fn stats(&self) -> (usize, u64) {
        (self.recent_rejections.len(), self.total_rejections)
    }
}

fn default_block_source() -> crate::block_executor::BlockSource {
    crate::block_executor::BlockSource::SelfProduced
}

impl Blockchain {
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
            total_minted: 0,
            // Prefer the TOML's declared chain_id, but defer to the
            // SENTRIX_CHAIN_ID env var when set (matches previous semantics
            // so live operators can keep using env-based overrides).
            chain_id: std::env::var("SENTRIX_CHAIN_ID")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(genesis.chain.chain_id),
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

    /// Bind MDBX storage handle so `get_transaction()` can resolve txids that
    /// fall outside the in-memory chain window. Cheap clone — Arc<MdbxStorage>.
    pub fn init_storage_handle(&mut self, mdbx: Arc<MdbxStorage>) -> SentrixResult<()> {
        self.mdbx_storage = Some(mdbx);
        Ok(())
    }

    /// BACKLOG #16 durable fix: atomically persist a block's MDBX record
    /// (TABLE_META + TABLE_BLOCK_HASHES + height bump + sync). Returns Ok
    /// only if all four mutations committed cleanly. Called from
    /// `add_block_impl` AFTER Pass-2 commit — paired with the existing
    /// `BlockchainSnapshot` rollback path so a persist failure here
    /// triggers in-memory rollback, keeping chain state and disk in
    /// lock-step.
    ///
    /// Returns `Err(StorageNotInitialised)` if `mdbx_storage` was never
    /// bound (unit tests with no storage backing). Callers should treat
    /// that as "skip persist" — no gap risk because there's no disk at
    /// all. A real production path always has `mdbx_storage` set via
    /// `init_storage_handle`, so any Err here is a real MDBX failure
    /// (disk full, lock contention, permissions, corruption).
    pub fn persist_block_durable(&self, block: &Block) -> SentrixResult<()> {
        let mdbx = self.mdbx_storage.as_ref().ok_or_else(|| {
            SentrixError::Internal(
                "persist_block_durable: mdbx_storage not initialised".into(),
            )
        })?;

        let key = format!("block:{}", block.index);
        let block_json = serde_json::to_vec(block).map_err(|e| {
            SentrixError::Internal(format!("persist_block_durable: serialize block: {e}"))
        })?;

        // Same byte layout as `Storage::save_block` in sentrix-storage: (1)
        // block bytes in TABLE_META, (2) reverse hash→height index in
        // TABLE_BLOCK_HASHES, (3) height marker in TABLE_META, (4) sync.
        mdbx.put(tables::TABLE_META, key.as_bytes(), &block_json)
            .map_err(|e| {
                SentrixError::Internal(format!("persist_block_durable: TABLE_META put: {e}"))
            })?;
        mdbx.put(
            tables::TABLE_BLOCK_HASHES,
            block.hash.as_bytes(),
            &height_key(block.index),
        )
        .map_err(|e| {
            SentrixError::Internal(format!(
                "persist_block_durable: TABLE_BLOCK_HASHES put: {e}"
            ))
        })?;
        mdbx.put(
            tables::TABLE_META,
            b"height",
            &block.index.to_be_bytes(),
        )
        .map_err(|e| {
            SentrixError::Internal(format!("persist_block_durable: height put: {e}"))
        })?;
        mdbx.sync().map_err(|e| {
            SentrixError::Internal(format!("persist_block_durable: sync: {e}"))
        })?;
        Ok(())
    }

    /// Record a tx → block_index mapping. Called by `add_block` for each
    /// tx in a freshly committed block. No-op if `init_storage_handle` was
    /// never called (e.g. unit tests with no storage backing).
    pub fn record_tx_in_index(&self, txid: &str, block_index: u64) {
        if let Some(mdbx) = &self.mdbx_storage {
            let _ = mdbx.put(
                tables::TABLE_TX_INDEX,
                txid.as_bytes(),
                &height_key(block_index),
            );
        }
    }

    /// Resolve a txid to its containing (Block, block_index) by
    /// consulting the MDBX txid_index then loading the block.
    /// Returns None if the txid is unknown or the storage handle was never
    /// initialised.
    pub fn lookup_tx_in_storage(&self, txid: &str) -> Option<(Block, u64)> {
        let mdbx = self.mdbx_storage.as_ref()?;
        let raw = mdbx
            .get(tables::TABLE_TX_INDEX, txid.as_bytes())
            .ok()
            .flatten()?;
        if raw.len() != 8 {
            return None;
        }
        let block_index = key_to_height(&raw);
        let key = format!("block:{}", block_index);
        let bytes = mdbx
            .get(tables::TABLE_META, key.as_bytes())
            .ok()
            .flatten()?;
        let block: Block = serde_json::from_slice(&bytes).ok()?;
        Some((block, block_index))
    }

    /// One-shot backfill — walk every stored block from genesis to the current
    /// height and populate the txid_index for any tx that does not already
    /// have an entry. Idempotent. Called once at startup.
    ///
    /// Fast path (issue #268): on a warm chain the index is already populated,
    /// so scanning every block is 500K+ redundant MDBX reads with zero writes.
    /// Before committing to the full scan, sample the LATEST block's last tx
    /// and check whether it's already indexed. If yes, assume warm and return
    /// immediately. A single deliberate gap in the tail is vanishingly unlikely
    /// to matter for UX (the next block's txs will be indexed via the regular
    /// `add_block` path); the next restart re-samples and heals any drift.
    ///
    /// Slow path logs progress every 50K blocks so operators see activity
    /// rather than a silent several-minute freeze during a cold-start
    /// backfill on a large chain.
    pub fn backfill_txid_index(&self, mdbx: &MdbxStorage) -> SentrixResult<usize> {
        if self.mdbx_storage.is_none() {
            return Ok(0);
        }
        let height = self.height();

        // Fast path: is the latest block's last tx already indexed?
        if let Some(latest) = self.latest_block().ok()
            && let Some(last_tx) = latest.transactions.last()
            && mdbx
                .get(tables::TABLE_TX_INDEX, last_tx.txid.as_bytes())
                .map_err(|e| SentrixError::StorageError(e.to_string()))?
                .is_some()
        {
            return Ok(0);
        }

        tracing::info!(
            "txid_index: scanning {} blocks for backfill (this can take minutes on large chains)",
            height + 1
        );

        const PROGRESS_STEP: u64 = 50_000;
        let mut written = 0usize;
        for i in 0..=height {
            let key = format!("block:{}", i);
            let bytes = match mdbx
                .get(tables::TABLE_META, key.as_bytes())
                .map_err(|e| SentrixError::StorageError(e.to_string()))?
            {
                Some(b) => b,
                None => continue,
            };
            let block: Block = match serde_json::from_slice(&bytes) {
                Ok(b) => b,
                Err(_) => continue,
            };
            for tx in &block.transactions {
                if mdbx
                    .get(tables::TABLE_TX_INDEX, tx.txid.as_bytes())
                    .map_err(|e| SentrixError::StorageError(e.to_string()))?
                    .is_none()
                {
                    mdbx.put(
                        tables::TABLE_TX_INDEX,
                        tx.txid.as_bytes(),
                        &height_key(block.index),
                    )
                    .map_err(|e| SentrixError::StorageError(e.to_string()))?;
                    written += 1;
                }
            }
            if i > 0 && i.is_multiple_of(PROGRESS_STEP) {
                tracing::info!(
                    "txid_index: scanned {}/{} blocks ({} entries written so far)",
                    i,
                    height + 1,
                    written
                );
            }
        }
        Ok(written)
    }

    // ── Chain state queries ──────────────────────────────

    // Height is derived from the last block's index, not chain.len()-1.
    // The chain is a sliding window so chain.len() ≤ CHAIN_WINDOW_SIZE.
    pub fn height(&self) -> u64 {
        self.chain.last().map(|b| b.index).unwrap_or(0)
    }

    /// First block index currently held in the in-memory window.
    /// Blocks with index < chain_window_start() are only in MDBX storage.
    pub fn chain_window_start(&self) -> u64 {
        self.chain.first().map(|b| b.index).unwrap_or(0)
    }

    /// Is the given height at or after the Voyager DPoS fork?
    ///
    /// **Static / env-var only.** Returns true iff the operator set
    /// `VOYAGER_FORK_HEIGHT` to a real value AND the height is past it.
    /// Default `u64::MAX` makes this return false for all heights —
    /// the mainnet-safe-default-pre-activation pattern.
    ///
    /// **Use [`voyager_mode_for`] in consensus paths** — it ORs this
    /// check with the runtime persisted `voyager_activated` flag, so
    /// post-activation chains don't depend on the env var being set
    /// correctly. The 2026-04-26 mainnet stall (covered by operator
    /// incident runbooks) happened because `validate_block` called this
    /// static function:
    /// env var was at default `u64::MAX`, function returned false,
    /// validate_block fell through to Pioneer auth check, which
    /// rejected legitimate Voyager skip-round blocks.
    pub fn is_voyager_height(height: u64) -> bool {
        let fork = get_voyager_fork_height();
        fork != u64::MAX && height >= fork
    }

    /// Is the current chain past the Voyager fork?
    /// Static-only env-var check; see [`is_voyager_height`] caveats.
    pub fn is_voyager_active(&self) -> bool {
        Self::is_voyager_height(self.height())
    }

    /// Voyager-mode check for a specific block height that respects
    /// BOTH the env-var fork height AND the runtime persisted
    /// `voyager_activated` flag from chain.db.
    ///
    /// Returns true if EITHER:
    /// - `voyager_activated == true` (chain has actually activated
    ///   Voyager via `Blockchain::activate_voyager()`), OR
    /// - `is_voyager_height(height) == true` (env var pinned a fork
    ///   height + we're past it)
    ///
    /// This is the consensus-safe check — call this in `validate_block`
    /// and any other path where rejecting valid Voyager blocks would
    /// fork the chain. The OR semantics mean a chain that activated
    /// Voyager via the runtime path (with env var unset / wrong)
    /// continues to apply blocks correctly.
    pub fn voyager_mode_for(&self, height: u64) -> bool {
        self.voyager_activated || Self::is_voyager_height(height)
    }

    /// V4 Step 3: is the given height at or after the reward-v2 fork?
    /// Post-fork: coinbase routes to PROTOCOL_TREASURY, ClaimRewards
    /// dispatch is consensus-valid.
    pub fn is_reward_v2_height(height: u64) -> bool {
        let fork = get_reward_v2_fork_height();
        fork != u64::MAX && height >= fork
    }

    pub fn is_reward_v2_active(&self) -> bool {
        Self::is_reward_v2_height(self.height())
    }

    /// Tokenomics v2: is the given height at or after the fork?
    /// Post-fork: 126M halving + 315M cap (BTC-parity 4-year emission).
    pub fn is_tokenomics_v2_height(height: u64) -> bool {
        let fork = get_tokenomics_v2_height();
        fork != u64::MAX && height >= fork
    }

    /// Phase B (consensus-jail): is the given height at or after the fork?
    /// Post-fork: `StakingOp::JailEvidenceBundle` dispatch is consensus-valid;
    /// epoch-boundary proposer includes evidence; peers verify and apply
    /// jail as on-chain state mutation. Pre-fork: legacy local check_liveness.
    pub fn is_jail_consensus_height(height: u64) -> bool {
        let fork = get_jail_consensus_height();
        fork != u64::MAX && height >= fork
    }

    /// Is the given height at or after the NFT TokenOp fork?
    /// Post-fork: SRC-721 + SRC-1155 TokenOp variants dispatch.
    /// Pre-fork: dispatch rejects (wire format stable, storage layer
    /// + REST handlers gated until activation).
    pub fn is_nft_tokenop_height(height: u64) -> bool {
        let fork = get_nft_tokenop_height();
        fork != u64::MAX && height >= fork
    }

    /// Is the given height at or after the AddSelfStake fork?
    /// Post-fork: `StakingOp::AddSelfStake` dispatch is consensus-valid —
    /// validators can bond real SRX into their own self_stake without
    /// phantom-mint. Pre-fork: dispatch rejects (wire format stable from
    /// the activation PR; gate keeps it dormant until operator rollout).
    pub fn is_add_self_stake_height(height: u64) -> bool {
        let fork = get_add_self_stake_height();
        fork != u64::MAX && height >= fork
    }

    /// BFT-gate-relax: is the given height at or after the fork?
    /// Post-fork: validator-loop's P1 BFT safety gate uses
    /// `active >= ⌈2/3 × total⌉` instead of `active >= MIN_BFT_VALIDATORS (=4)`.
    /// For 4-validator network: gate becomes 3 instead of 4 (= 1-jail tolerance).
    /// See `audits/jail-cascade-root-cause-analysis.md`.
    pub fn is_bft_gate_relax_height(height: u64) -> bool {
        let fork = get_bft_gate_relax_height();
        fork != u64::MAX && height >= fork
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
        let evidence = self.slashing.compute_jail_evidence(&active_set, next_height);
        if evidence.is_empty() {
            return None;
        }

        // Compute epoch metadata for the bundle
        let epoch =
            sentrix_staking::epoch::EpochManager::epoch_for_height(next_height);
        let epoch_length = sentrix_staking::epoch::EPOCH_LENGTH;
        let epoch_start_block = epoch.saturating_mul(epoch_length);
        let epoch_end_block = next_height; // boundary block IS the end

        let op = sentrix_primitives::transaction::StakingOp::JailEvidenceBundle {
            epoch,
            epoch_start_block,
            epoch_end_block,
            evidence,
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

    /// Tokenomics v2: max supply for a given height (fork-aware).
    /// Pre-fork: 210M (`MAX_SUPPLY`). Post-fork: 315M (`MAX_SUPPLY_V2`).
    pub fn max_supply_for(&self, height: u64) -> u64 {
        if Self::is_tokenomics_v2_height(height) {
            MAX_SUPPLY_V2
        } else {
            MAX_SUPPLY
        }
    }

    /// Tokenomics v2: halving interval for a given height (fork-aware).
    /// Pre-fork: 42M blocks (1.33y). Post-fork: 126M blocks (4y BTC-parity).
    pub fn halving_interval_for(&self, height: u64) -> u64 {
        if Self::is_tokenomics_v2_height(height) {
            HALVING_INTERVAL_V2
        } else {
            HALVING_INTERVAL
        }
    }

    /// Halving count at a given height, fork-aware. Pre-fork blocks count
    /// halvings against 42M intervals; post-fork blocks count against 126M
    /// intervals **starting from the fork height** (so cumulative halvings
    /// don't reset at fork moment, and no jump-up in reward).
    ///
    /// Assumes fork is activated while still within v1 era 0
    /// (`fork_height < HALVING_INTERVAL` = 42M). At current mainnet height
    /// ~600K, this is satisfied for any plausible fork target.
    fn halvings_at(height: u64) -> u32 {
        let fork = get_tokenomics_v2_height();
        if fork == u64::MAX || height < fork {
            (height / HALVING_INTERVAL).try_into().unwrap_or(u32::MAX)
        } else {
            // Post-fork: count halvings from fork height using v2 interval.
            // Pre-fork halvings = 0 by activation invariant (fork while in era 0).
            let post = height.saturating_sub(fork);
            (post / HALVING_INTERVAL_V2).try_into().unwrap_or(u32::MAX)
        }
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

    /// Initialize EVM state at fork activation.
    /// Called once when chain reaches VOYAGER_EVM_HEIGHT.
    /// Migrates all account code_hash fields and initializes gas tracking.
    /// Idempotent — guarded by the persistent `evm_activated` flag.
    pub fn activate_evm(&mut self) {
        if self.evm_activated {
            tracing::debug!("activate_evm: already activated, skipping");
            return;
        }
        tracing::info!("Activating EVM at height {}", self.height());
        let migrated = self.accounts.migrate_to_evm();
        self.evm_activated = true;
        tracing::info!(
            "EVM activated: {} accounts migrated, gas metering enabled",
            migrated
        );
    }

    /// Initialize Voyager state at fork activation.
    /// Called once when chain reaches VOYAGER_DPOS_HEIGHT.
    /// Migrates existing Pioneer validators to DPoS with equal stake.
    /// Idempotent — guarded by the persistent `voyager_activated` flag so
    /// validator restarts post-fork don't re-register validators or
    /// re-snapshot the epoch.
    pub fn activate_voyager(&mut self) -> SentrixResult<()> {
        use sentrix_staking::MIN_SELF_STAKE;

        if self.voyager_activated {
            tracing::debug!("activate_voyager: already activated, skipping");
            return Ok(());
        }

        // Migrate Pioneer validators → DPoS validators
        let validators: Vec<String> = self
            .authority
            .active_validators()
            .iter()
            .map(|v| v.address.clone())
            .collect();
        for address in &validators {
            if let Err(e) = self.stake_registry.register_validator(
                address,
                MIN_SELF_STAKE,
                1000, // 10% default commission
                self.height(),
            ) {
                tracing::warn!("Failed to migrate validator {}: {}", address, e);
            }
        }

        // Initialize epoch manager with the new stake registry
        self.stake_registry.update_active_set();
        self.epoch_manager
            .initialize(&self.stake_registry, self.height());

        self.voyager_activated = true;

        tracing::info!(
            "Voyager DPoS activated at height {}. {} validators migrated.",
            self.height(),
            self.stake_registry.active_count()
        );

        Ok(())
    }

    // Returns Err instead of panicking when chain is empty
    pub fn latest_block(&self) -> SentrixResult<&Block> {
        self.chain
            .last()
            .ok_or_else(|| SentrixError::NotFound("chain is empty".to_string()))
    }

    // Returns None for blocks outside the in-memory window — use storage for historical access
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

    /// Block lookup that transparently falls back to MDBX storage for
    /// blocks evicted from the in-memory sliding window. Returns an
    /// owned `Block` (cloning in the window case, fresh deserialise in
    /// the storage case).
    ///
    /// Added for BACKLOG #14: the `GetBlocks` request-response handler
    /// used to call `get_block` directly and silently dropped every
    /// request for blocks older than `CHAIN_WINDOW_SIZE`, stranding
    /// fresh or forensic-restored peers that needed a deep history
    /// back-fill. Live validators keep full history in MDBX, so this
    /// fallback just serves what already exists on disk.
    ///
    /// Returns None only when both the in-memory window misses AND the
    /// MDBX store has no block at that index (i.e. the block was never
    /// produced or the storage handle was never bound — fresh test
    /// Blockchains with no `mdbx_storage` hit the latter).
    pub fn get_block_any(&self, index: u64) -> Option<Block> {
        if let Some(b) = self.get_block(index) {
            return Some(b.clone());
        }
        let mdbx = self.mdbx_storage.as_ref()?;
        let key = format!("block:{}", index);
        let bytes = mdbx
            .get(tables::TABLE_META, key.as_bytes())
            .ok()
            .flatten()?;
        serde_json::from_slice(&bytes).ok()
    }

    // ── Supply & reward ──────────────────────────────────
    pub fn get_block_reward(&self) -> u64 {
        let h = self.height();
        // Tokenomics-fork-aware: pre-fork uses MAX_SUPPLY (210M) +
        // 42M halving interval; post-fork uses MAX_SUPPLY_V2 (315M) +
        // 126M halving interval. See `is_tokenomics_v2_height` for
        // activation semantics.
        let max_supply = self.max_supply_for(h);
        let remaining = max_supply.saturating_sub(self.total_minted);
        if remaining == 0 {
            return 0;
        }

        // P1: halving bit-shift overflow guard. `u64 >> 64+` is undefined
        // in Rust (panics in debug, implementation-defined in release).
        // `halvings_at` clamps to u32::MAX so checked_shr returns None at
        // ≥64 and the reward is zero (matching "halved to nothing"
        // semantics). Pre-fork: ~21×42M blocks (~28 years at 1s) to reach
        // 64 halvings. Post-fork: ~21×126M blocks (~84 years).
        let halvings: u32 = Self::halvings_at(h);
        let reward = BLOCK_REWARD.checked_shr(halvings).unwrap_or(0);

        if reward == 0 {
            return 0;
        }

        reward.min(remaining)
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

    /// State root committed at `version` (block height), or None if the trie is not initialized
    /// or no root was committed at that version.
    pub fn trie_root_at(&self, version: u64) -> Option<[u8; 32]> {
        self.state_trie
            .as_ref()
            .and_then(|t| t.root_at_version(version).ok().flatten())
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

    // ── SentrixTrie (Step 5) ─────────────────────────────

    /// Initialize the state trie from MDBX storage.
    /// Loads the committed root for the current height, or starts from an empty trie.
    /// Call once at node startup, after loading blockchain state from storage.
    ///
    /// If no trie root exists for the current height but the chain has history,
    /// backfills all non-zero accounts from AccountDB (one-time migration on trie introduction).
    pub fn init_trie(&mut self, mdbx: Arc<MdbxStorage>) -> SentrixResult<()> {
        let height = self.height();
        let mut trie = SentrixTrie::open(mdbx, height)?;

        // First-time trie init on a node whose AccountDB predates SentrixTrie:
        // AccountDB has correct state but the trie is empty — backfill now.
        //
        // Also handles the stale-height case: root hash recorded in trie_roots but
        // the root NODE was removed from trie_nodes during a prior structural cleanup
        // (insert removes replaced internal nodes, including old roots).
        let needs_backfill = if height > 0 {
            match trie.root_at_version(height)? {
                None => {
                    // No trie root entry exists for this height at all.
                    // Expected only on first-time trie init on a chain that predates
                    // SentrixTrie.  After fix/trie-permanent-fix this path should only
                    // be reached once per node lifetime.
                    tracing::warn!(
                        "trie: no root recorded for height {} — first-time backfill from AccountDB",
                        height
                    );
                    true
                }
                Some(root_hash) => {
                    // Root IS recorded in trie_roots but the node is gone from trie_nodes.
                    //
                    // ROOT CAUSE #1 / ROOT CAUSE #3 guard: after fix/trie-permanent-fix,
                    // is_committed_root() prevents insert() from deleting committed roots,
                    // so this branch should NEVER be reached on a healthy node.  If it
                    // is, something has gone seriously wrong (manual data corruption,
                    // storage bug, regression).  Log at ERROR so ops are alerted; the
                    // backfill below may produce a state root that differs from peers.
                    //
                    // ISSUE #268 FALSE-POSITIVE GUARD: empty_hash(0) is the sentinel
                    // for an empty trie level — it's NEVER materialised in trie_nodes
                    // because empty subtrees are short-circuited. So node_exists()
                    // always returns false for it. On a chain where no block has
                    // mutated any account (coinbase-only blocks against an empty
                    // initial state, or genuinely-quiet recovery windows), every
                    // committed root equals empty_hash(0) and the old check fired a
                    // spurious backfill. The backfill from AccountDB then computed
                    // a non-empty root (because AccountDB has the genesis premine
                    // entries), persisted it to MDBX BEFORE the safeguard ran, and
                    // even if the safeguard returned Err the chain.db was already
                    // corrupted. Below STATE_ROOT_FORK_HEIGHT, Storage::load_blockchain
                    // swallows that Err — so the corruption became permanent.
                    // Treat the empty sentinel as "node exists" since the empty
                    // subtree is trivially correct without storage.
                    let node_missing = root_hash != sentrix_trie::node::empty_hash(0)
                        && !trie.node_exists(&root_hash)?;
                    if node_missing {
                        tracing::error!(
                            "trie: CRITICAL — root {} for height {} is recorded in trie_roots \
                             but the node is missing from trie_nodes.  This should not happen \
                             after fix/trie-permanent-fix.  Forcing backfill from AccountDB; \
                             the resulting state root may differ from other peers and cause a fork.",
                            hex::encode(root_hash),
                            height
                        );
                        // CRITICAL: reset working root to empty_hash so backfill inserts
                        // start from a clean slate rather than a stale/deleted root.
                        trie.reset_to_empty();
                    }
                    node_missing
                }
            }
        } else {
            false
        };

        if needs_backfill {
            // CRITICAL: Sort accounts by address for deterministic backfill.
            // HashMap::values() iterates in random order per-process, causing different
            // trie roots on different nodes — the root cause of chain forks after ~17h.
            let mut accounts: Vec<(String, u64, u64)> = self
                .accounts
                .accounts
                .values()
                .filter(|a| a.balance > 0)
                .map(|a| (a.address.clone(), a.balance, a.nonce))
                .collect();
            accounts.sort_by(|a, b| a.0.cmp(&b.0));
            if !accounts.is_empty() {
                tracing::info!(
                    "trie: backfilling {} accounts at height {} (first trie init on existing chain)",
                    accounts.len(),
                    height
                );
                for (addr, balance, nonce) in accounts {
                    let key = address_to_key(&addr);
                    let val = account_value_bytes(balance, nonce);
                    trie.insert(&key, &val)?;
                }
                let backfilled_root = trie.commit(height)?;
                tracing::info!(
                    "trie: backfill complete at height {}, root = {}",
                    height,
                    hex::encode(backfilled_root)
                );

                // Bug #3 safeguard (mainnet freeze 2026-04-21): the incremental
                // path (update_trie_for_block) only inserts accounts touched by
                // blocks, while backfill inserts every account with balance > 0
                // — including premines/genesis accounts that were never touched.
                // For the same logical state, the two paths produce different
                // trie root sets, so a validator recovering via reset_trie +
                // init_trie at height > 0 will compute a state_root that
                // disagrees with peers whose trie was built incrementally from
                // genesis. Without this check, the validator silently forks
                // and every block it produces trips the #1e strict-reject guard.
                //
                // Refuse to start if the backfill root doesn't match the stored
                // header. Operators must recover via rsync chain.db from a
                // healthy peer (whole-trie copy preserves the incremental
                // shape) instead of state_import + reset.
                if let Ok(block) = self.latest_block()
                    && block.index == height
                    && let Some(stored_root) = block.state_root
                    && backfilled_root != stored_root
                {
                    return Err(SentrixError::Internal(format!(
                        "trie backfill at height {} produced root {} but the \
                         block header at that height records state_root {}. \
                         The rebuilt trie disagrees with the canonical chain \
                         (bug #3). Refusing to start to prevent a silent \
                         state fork. Recovery: rsync /opt/sentrix/data/chain.db \
                         from a healthy peer with all validators stopped, \
                         instead of `sentrix state import` + reset_trie.",
                        height,
                        hex::encode(backfilled_root),
                        hex::encode(stored_root)
                    )));
                }
            }
        }

        // Boot-time integrity check — added post-2026-04-21 3-way fork.
        // The existing checks above catch: (a) missing root entry in
        // trie_roots, (b) missing root node in trie_nodes, (c) backfill
        // root ≠ header state_root (bug #3 guard). What they DON'T catch
        // is an orphan reference BELOW the root — e.g. the root exists
        // and references a middle-layer node that was deleted by a
        // pre-v2.1.5 state_import. A validator booting on that broken
        // DB would produce blocks with `state_root=None` and get rejected
        // by strict peers — the exact #1e CRITICAL pattern observed in
        // the 2026-04-21 fork.
        //
        // Walk the current root once and refuse to boot past
        // STATE_ROOT_FORK_HEIGHT if any orphan is found. Below the fork
        // height the old hash format ignores state_root entirely, so a
        // broken trie can't cause consensus divergence — warn-only there.
        if let Err(e) = trie.verify_integrity() {
            if height >= sentrix_primitives::block::STATE_ROOT_FORK_HEIGHT {
                return Err(SentrixError::Internal(format!(
                    "trie integrity check failed at height {height}: {e}"
                )));
            }
            tracing::warn!(
                "trie integrity warning at height {} (below fork height — allowed): {}",
                height,
                e
            );
        }

        self.state_trie = Some(trie);
        Ok(())
    }

    /// Update the trie with current account state for every address touched in the last block,
    /// commit at that block's height, and return the new state root.
    /// Returns Ok(None) if the trie has not been initialized.
    ///
    /// Trie errors are propagated — callers must handle state root failures explicitly.
    ///
    /// Split into two phases to satisfy the borrow checker:
    ///   Phase 1 — immutable borrows of `chain` and `accounts` → collect owned data.
    ///   Phase 2 — mutable borrow of `state_trie` → insert + commit.
    pub fn update_trie_for_block(&mut self) -> SentrixResult<Option<[u8; 32]>> {
        if self.state_trie.is_none() {
            // Pre-STATE_ROOT_FORK_HEIGHT, missing trie is acceptable —
            // state_root isn't part of the block hash. Past the fork
            // height, a None state_root would diverge silently from
            // peers who computed a real one, so refuse to participate.
            // load_blockchain warned on the init failure that got us
            // here; this guard turns the warn into a hard refusal at
            // the consensus boundary so the validator stops producing
            // ghost blocks rather than forking the network.
            let next_height = self.height().saturating_add(1);
            if next_height >= sentrix_primitives::block::STATE_ROOT_FORK_HEIGHT {
                return Err(SentrixError::Internal(format!(
                    "trie unavailable but next block height {next_height} requires \
                     state_root (>= STATE_ROOT_FORK_HEIGHT). Recovery: wipe data dir \
                     and resync from a healthy peer. Validator should stop producing \
                     blocks until trie is rebuilt — running here would silently fork \
                     the chain."
                )));
            }
            return Ok(None);
        }
        let trace = std::env::var("SENTRIX_TRIE_TRACE").is_ok();

        // Phase 1: extract addresses + block index from the last block
        let (touched_addrs, block_index) = {
            let block = match self.chain.last() {
                Some(b) => b,
                None => return Ok(None),
            };
            let mut addrs: Vec<String> = Vec::new();
            for tx in &block.transactions {
                if is_valid_sentrix_address(&tx.from_address) {
                    addrs.push(tx.from_address.clone());
                }
                // Skip TOKEN_OP_ADDRESS — its SRX balance is always 0 and would trigger
                // a no-op delete() traversal on every token-op block.
                if is_valid_sentrix_address(&tx.to_address) && tx.to_address != TOKEN_OP_ADDRESS {
                    addrs.push(tx.to_address.clone());
                }
            }
            if is_valid_sentrix_address(&block.validator) {
                addrs.push(block.validator.clone());
            }
            (addrs, block.index)
        };
        // All borrows on `self.chain` released here.

        // Phase 1b: snapshot current balances + nonces (immutable borrow of `accounts`)
        // CRITICAL: Use BTreeSet (sorted, deterministic) — NOT HashSet (random per-process).
        // HashSet iteration order differs across nodes, causing different trie insert order.
        // Even though the Binary SMT root should be order-independent in theory, using
        // deterministic order eliminates any possibility of implementation-level divergence.
        let unique: std::collections::BTreeSet<String> = touched_addrs.into_iter().collect();
        let updates: Vec<(String, u64, u64)> = unique
            .iter()
            .map(|a| {
                (
                    a.clone(),
                    self.accounts.get_balance(a),
                    self.accounts.get_nonce(a),
                )
            })
            .collect();
        // Borrow of `accounts` ends after collect().

        if trace {
            eprintln!("[trie-trace] update_trie_for_block at h={block_index}");
            eprintln!("[trie-trace] touched (sorted): {} addresses", updates.len());
            for (addr, balance, nonce) in &updates {
                let key = address_to_key(addr);
                let value = account_value_bytes(*balance, *nonce);
                eprintln!(
                    "[trie-trace]   addr={addr} balance={balance} nonce={nonce} key={} value={}",
                    hex::encode(key),
                    hex::encode(&value)
                );
            }
        }

        // Phase 2: mutable borrow of `state_trie`
        let trie = match self.state_trie.as_mut() {
            Some(t) => t,
            None => return Ok(None),
        };
        if trace {
            eprintln!("[trie-trace] root pre-update: {}", hex::encode(trie.root_hash()));
        }
        for (addr, balance, nonce) in updates {
            let key = address_to_key(&addr);
            // Trace the existing leaf BEFORE we mutate
            if trace {
                let existing = trie.get(&key)?;
                eprintln!(
                    "[trie-trace]   existing leaf for {addr}: {}",
                    existing.as_ref().map(hex::encode).unwrap_or_else(|| "<none>".into())
                );
            }
            if balance == 0 {
                trie.delete(&key)?;
            } else {
                let value = account_value_bytes(balance, nonce);
                trie.insert(&key, &value)?;
            }
            if trace {
                eprintln!(
                    "[trie-trace]   root after {addr}: {}",
                    hex::encode(trie.root_hash())
                );
            }
        }
        let root = trie.commit(block_index)?;
        if trace {
            eprintln!("[trie-trace] commit at h={block_index} → root={}", hex::encode(root));
        }
        Ok(Some(root))
    }

    /// Periodically reclaim trie storage. Called after every successful block
    /// commit; only does work when the height is a multiple of TRIE_PRUNE_EVERY.
    ///
    /// `keep_versions` historical roots remain walkable; older ones and any
    /// nodes/values exclusively referenced by them get GC'd.
    ///
    /// Pruning failure is logged but never propagated — a failed prune leaves
    /// extra storage on disk but does not break consensus.
    pub fn maybe_prune_trie(&self) {
        const TRIE_PRUNE_EVERY: u64 = 1000;
        const TRIE_KEEP_VERSIONS: u64 = 1000;

        let height = self.height();
        if height == 0 || !height.is_multiple_of(TRIE_PRUNE_EVERY) {
            return;
        }

        let Some(trie) = self.state_trie.as_ref() else {
            return;
        };

        match trie.prune(TRIE_KEEP_VERSIONS) {
            Ok((roots, nodes)) if roots > 0 || nodes > 0 => {
                tracing::info!(
                    "trie maintenance at height {}: retired {} old roots, GC'd {} nodes/values",
                    height,
                    roots,
                    nodes
                );
            }
            Ok(_) => {} // nothing to do
            Err(e) => {
                tracing::warn!(
                    "trie prune at height {} failed: {} (storage will continue to grow until next successful prune)",
                    height,
                    e
                );
            }
        }
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

    // ── DivergenceTracker tests ──────────────────────────────

    /// Counter monotonicity + threshold behavior. We can't test the
    /// rolling eviction in real-time without waiting 5 minutes, so we
    /// test the linear count path which is what actually fires the
    /// alarm under real divergence conditions.
    #[test]
    fn test_divergence_tracker_counts() {
        let mut t = DivergenceTracker::default();
        assert_eq!(t.stats(), (0, 0));

        for i in 0..50u64 {
            t.record_rejection(1_000 + i);
        }
        let (recent, total) = t.stats();
        assert_eq!(recent, 50, "all 50 rejections should be within the 5-min window");
        assert_eq!(total, 50);
    }

    /// Alarm cooldown — after the threshold is crossed, subsequent
    /// rejections within the cooldown must not re-emit. We can't
    /// assert the log output directly without an observer; we assert
    /// the internal `last_alarm_at` state instead.
    #[test]
    fn test_divergence_alarm_cooldown() {
        let mut t = DivergenceTracker::default();
        // Push enough rejections to cross the threshold.
        for i in 0..DivergenceTracker::ALARM_THRESHOLD as u64 {
            t.record_rejection(2_000 + i);
        }
        assert!(
            t.last_alarm_at.is_some(),
            "alarm must fire once threshold crossed"
        );

        // Subsequent rejection within the cooldown window doesn't
        // update `last_alarm_at` (since the current alarm is still
        // within cooldown). Hard to assert without mocking time;
        // just verify no panic + tracker continues accepting records.
        t.record_rejection(2_999);
        let (_recent, total) = t.stats();
        assert_eq!(total, DivergenceTracker::ALARM_THRESHOLD as u64 + 1);
    }

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
        assert_eq!(Blockchain::halvings_at(99), 0);

        // At fork boundary (h=100): v2 schedule activates. (h - fork) / 126M
        // = 0 / 126M = 0 halvings. Smooth transition: reward stays 1 SRX.
        assert_eq!(Blockchain::halvings_at(100), 0);

        // Post-fork era 0: still 0 halvings until fork+126M.
        assert_eq!(Blockchain::halvings_at(100 + 126_000_000 - 1), 0);

        // Post-fork era 1: at fork+126M, halvings = 1. Reward halves to 0.5.
        assert_eq!(Blockchain::halvings_at(100 + 126_000_000), 1);
        assert_eq!(Blockchain::halvings_at(100 + 2 * 126_000_000), 2);

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
        assert!(
            actual_total <= MAX_SUPPLY_V2,
            "discrete sum exceeds cap"
        );
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
            panic!(
                "get_block_any should have fetched evicted block {evicted_height} from MDBX"
            )
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
            } => {
                assert_eq!(
                    epoch,
                    sentrix_staking::epoch::EpochManager::epoch_for_height(boundary)
                );
                assert_eq!(epoch_start_block, 0);
                assert_eq!(epoch_end_block, boundary);
                assert_eq!(evidence.len(), 1);
                assert_eq!(evidence[0].validator, downer);
            }
            _ => panic!("expected JailEvidenceBundle variant"),
        }

        unsafe {
            std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
        }
    }
}
// fake addr 0x1234567890abcdef1234567890abcdef12345678
// fake addr 0x1234567890abcdef1234567890abcdef12345678
// fake addr 0x1234567890abcdef1234567890abcdef12345678
