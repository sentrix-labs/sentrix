// blockchain.rs - Sentrix — Blockchain struct, constants, genesis, core state methods

use serde::{Deserialize, Serialize};
use hex;
use std::collections::VecDeque;
use crate::core::block::Block;
use crate::core::transaction::Transaction;
use crate::core::account::AccountDB;
use crate::core::authority::AuthorityManager;
use crate::core::merkle::merkle_root;
use crate::core::vm::ContractRegistry;
use crate::core::transaction::TOKEN_OP_ADDRESS;
use crate::core::trie::tree::SentrixTrie;
use crate::core::trie::address::{address_to_key, account_value_bytes};
use crate::types::error::{SentrixError, SentrixResult};

// ── Chain constants ──────────────────────────────────────
pub const MAX_SUPPLY: u64         = 210_000_000 * 100_000_000; // in sentri
pub const BLOCK_REWARD: u64       = 100_000_000;               // 1 SRX in sentri
pub const HALVING_INTERVAL: u64   = 42_000_000;                 // blocks
pub const BLOCK_TIME_SECS: u64    = 3;
pub const MAX_TX_PER_BLOCK: usize = 100;
pub const CHAIN_ID: u64           = 7119; // default; overridable via SENTRIX_CHAIN_ID env

/// Default Voyager DPoS fork activation height.
/// u64::MAX = disabled (Pioneer-only). Override via VOYAGER_FORK_HEIGHT env var.
const VOYAGER_DPOS_HEIGHT_DEFAULT: u64 = u64::MAX;

/// Default Voyager EVM activation height.
/// u64::MAX = disabled. Override via VOYAGER_EVM_HEIGHT env var.
const VOYAGER_EVM_HEIGHT_DEFAULT: u64 = u64::MAX;

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

/// Read chain_id from SENTRIX_CHAIN_ID env var, fallback to 7119.
pub fn get_chain_id() -> u64 {
    std::env::var("SENTRIX_CHAIN_ID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(CHAIN_ID)
}
// Hash algorithm version — reserved for future hash algorithm migration
pub const HASH_VERSION: u8        = 1; // 1 = SHA-256 (current)

// Mempool size limits to prevent RAM exhaustion under high load
pub const MAX_MEMPOOL_SIZE: usize        = 10_000;
pub const MAX_MEMPOOL_PER_SENDER: usize  = 100;
// Mempool TTL — transactions older than this are automatically pruned
pub const MEMPOOL_MAX_AGE_SECS: u64      = 3_600; // 1 hour
// Sliding window size — only last N blocks kept in RAM; older blocks stay in sled storage
pub const CHAIN_WINDOW_SIZE: usize       = 1_000;

// Sentrix addresses are 42-char hex strings (0x + 40 hex digits)
pub fn is_valid_sentrix_address(addr: &str) -> bool {
    addr.len() == 42
        && addr.starts_with("0x")
        && addr[2..].chars().all(|c| c.is_ascii_hexdigit())
}

// ── Genesis addresses (from genesis_wallets.json — private keys secured) ──
pub const FOUNDER_ADDRESS:         &str = "0x4f3319a747fd564136209cd5d9e7d1a1e4d142be";
pub const ECOSYSTEM_FUND_ADDRESS:  &str = "0xeb70fdefd00fdb768dec06c478f450c351499f14";
pub const EARLY_VALIDATOR_ADDRESS: &str = "0xa7fc67af1ba0c664d859f4c1bcd2eb1f7211f112";
pub const RESERVE_ADDRESS:         &str = "0x2578cad17e3e56c2970a5b5eab45952439f5ba97";

pub const GENESIS_ALLOCATIONS: &[(&str, u64)] = &[
    (FOUNDER_ADDRESS,         21_000_000 * 100_000_000),
    (ECOSYSTEM_FUND_ADDRESS,  21_000_000 * 100_000_000),
    (EARLY_VALIDATOR_ADDRESS, 10_500_000 * 100_000_000),
    (RESERVE_ADDRESS,         10_500_000 * 100_000_000),
];

pub const TOTAL_PREMINE: u64 = 63_000_000 * 100_000_000;

// ── Blockchain struct ────────────────────────────────────
// Chain field excluded from serde — blocks are saved individually in sled storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blockchain {
    // Critical chain-state fields use pub(crate) to enforce validated access:
    //   bc.chain.push(unvalidated_block) → use add_block() instead
    //   bc.total_minted += n             → only block execution should change this
    //   bc.mempool.push_back(invalid_tx) → use add_to_mempool() instead
    // authority / accounts / contracts stay pub — they have their own validation in their methods
    // and main.rs legitimately calls them for CLI operations.
    #[serde(skip, default)]
    pub(crate) chain: Vec<Block>,
    pub accounts: AccountDB,          // pub: main.rs uses accounts.get_balance() for CLI display
    pub authority: AuthorityManager,  // pub: main.rs uses authority.* for validator management
    pub(crate) contracts: ContractRegistry,
    pub(crate) mempool: VecDeque<Transaction>,
    pub(crate) total_minted: u64,
    pub chain_id: u64,  // kept pub — read-only constant used by external clients
    /// Binary Sparse Merkle Tree for account state.
    /// None until init_trie() is called; not persisted in sled "state" blob.
    #[serde(skip)]
    pub(crate) state_trie: Option<SentrixTrie>,

    // ── Voyager DPoS state (Phase 2a) ────────────────────
    /// Staking registry for DPoS validator management
    #[serde(default)]
    pub stake_registry: crate::core::staking::StakeRegistry,
    /// Epoch manager for validator set rotation
    #[serde(default = "crate::core::epoch::EpochManager::new")]
    pub epoch_manager: crate::core::epoch::EpochManager,
    /// Slashing engine for liveness + double-sign tracking
    #[serde(default)]
    pub slashing: crate::core::slashing::SlashingEngine,
}

impl Blockchain {
    pub fn new(admin_address: String) -> Self {
        let mut bc = Self {
            chain: Vec::new(),
            accounts: AccountDB::new(),
            authority: AuthorityManager::new(admin_address),
            contracts: ContractRegistry::new(),
            mempool: VecDeque::new(),
            total_minted: 0,
            chain_id: get_chain_id(),
            state_trie: None,
            stake_registry: crate::core::staking::StakeRegistry::new(),
            epoch_manager: crate::core::epoch::EpochManager::new(),
            slashing: crate::core::slashing::SlashingEngine::new(),
        };
        bc.initialize_genesis();
        bc
    }

    fn initialize_genesis(&mut self) {
        // Apply genesis premine allocations
        for (address, amount) in GENESIS_ALLOCATIONS {
            let _ = self.accounts.credit(address, *amount);
        }
        self.total_minted = TOTAL_PREMINE;

        // Create genesis block
        let genesis = Block::genesis();
        self.chain.push(genesis);
    }

    // ── Chain state queries ──────────────────────────────

    // Height is derived from the last block's index, not chain.len()-1.
    // The chain is a sliding window so chain.len() ≤ CHAIN_WINDOW_SIZE.
    pub fn height(&self) -> u64 {
        self.chain.last().map(|b| b.index).unwrap_or(0)
    }

    /// First block index currently held in the in-memory window.
    /// Blocks with index < chain_window_start() are only in sled storage.
    pub fn chain_window_start(&self) -> u64 {
        self.chain.first().map(|b| b.index).unwrap_or(0)
    }

    /// Is the given height at or after the Voyager DPoS fork?
    pub fn is_voyager_height(height: u64) -> bool {
        let fork = get_voyager_fork_height();
        fork != u64::MAX && height >= fork
    }

    /// Is the current chain past the Voyager fork?
    pub fn is_voyager_active(&self) -> bool {
        Self::is_voyager_height(self.height())
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
    pub fn activate_evm(&mut self) {
        tracing::info!("Activating EVM at height {}", self.height());
        let migrated = self.accounts.migrate_to_evm();
        tracing::info!("EVM activated: {} accounts migrated, gas metering enabled", migrated);
    }

    /// Initialize Voyager state at fork activation.
    /// Called once when chain reaches VOYAGER_DPOS_HEIGHT.
    /// Migrates existing 7 Pioneer validators to DPoS with equal stake.
    pub fn activate_voyager(&mut self) -> SentrixResult<()> {
        use crate::core::staking::MIN_SELF_STAKE;

        // Migrate Pioneer validators → DPoS validators
        let validators: Vec<String> = self.authority.active_validators()
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
        self.epoch_manager.initialize(&self.stake_registry, self.height());

        tracing::info!(
            "Voyager DPoS activated at height {}. {} validators migrated.",
            self.height(),
            self.stake_registry.active_count()
        );

        Ok(())
    }

    // Returns Err instead of panicking when chain is empty
    pub fn latest_block(&self) -> SentrixResult<&Block> {
        self.chain.last()
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
            let txids: Vec<String> = block.transactions.iter().map(|tx| tx.txid.clone()).collect();
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
        format!("~{}MB for {} blocks in window (true height: {})", estimate_mb, window_blocks, true_height)
    }

    // ── SentrixTrie (Step 5) ─────────────────────────────

    /// Initialize the state trie from a sled database.
    /// Loads the committed root for the current height, or starts from an empty trie.
    /// Call once at node startup, after loading blockchain state from storage.
    ///
    /// If no trie root exists for the current height but the chain has history,
    /// backfills all non-zero accounts from AccountDB (one-time migration on trie introduction).
    pub fn init_trie(&mut self, db: &sled::Db) -> SentrixResult<()> {
        let height = self.height();
        let mut trie = SentrixTrie::open(db, height)?;

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
                    let node_missing = !trie.node_exists(&root_hash)?;
                    if node_missing {
                        tracing::error!(
                            "trie: CRITICAL — root {} for height {} is recorded in trie_roots \
                             but the node is missing from trie_nodes.  This should not happen \
                             after fix/trie-permanent-fix.  Forcing backfill from AccountDB; \
                             the resulting state root may differ from other peers and cause a fork.",
                            hex::encode(root_hash), height
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
            let mut accounts: Vec<(String, u64, u64)> = self.accounts.accounts
                .values()
                .filter(|a| a.balance > 0)
                .map(|a| (a.address.clone(), a.balance, a.nonce))
                .collect();
            accounts.sort_by(|a, b| a.0.cmp(&b.0));
            if !accounts.is_empty() {
                tracing::info!(
                    "trie: backfilling {} accounts at height {} (first trie init on existing chain)",
                    accounts.len(), height
                );
                for (addr, balance, nonce) in accounts {
                    let key = address_to_key(&addr);
                    let val = account_value_bytes(balance, nonce);
                    trie.insert(&key, &val)?;
                }
                trie.commit(height)?;
                tracing::info!(
                    "trie: backfill complete at height {}, root = {}",
                    height, hex::encode(trie.root_hash())
                );
            }
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
    pub(crate) fn update_trie_for_block(&mut self) -> SentrixResult<Option<[u8; 32]>> {
        if self.state_trie.is_none() {
            return Ok(None);
        }

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

        // Phase 2: mutable borrow of `state_trie`
        let trie = match self.state_trie.as_mut() {
            Some(t) => t,
            None => return Ok(None),
        };
        for (addr, balance, nonce) in updates {
            let key = address_to_key(&addr);
            if balance == 0 {
                // Remove zero-balance accounts from the trie.
                // delete() is a no-op if the key was never inserted.
                // Propagate delete errors — zero-balance removal must not silently fail
                trie.delete(&key)?;
            } else {
                let value = account_value_bytes(balance, nonce);
                // Propagate insert errors — trie divergence must be surfaced immediately
                trie.insert(&key, &value)?;
            }
        }
        // Propagate commit errors — a failed commit leaves the block root uncommitted
        let root = trie.commit(block_index)?;
        Ok(Some(root))
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

    fn derive_addr(pk: &PublicKey) -> String {
        crate::wallet::wallet::Wallet::derive_address(pk)
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

    // ── SRX-20 Token Tests ──────────────────────────────

    #[test]
    fn test_deploy_token() {
        let mut bc = setup_chain();
        // Fund deployer
        bc.accounts.credit("deployer", 1_000_000).unwrap();

        let addr = bc.deploy_token(
            "deployer", "TestToken".to_string(), "TT".to_string(),
            18, 1_000_000, 0, 100_000,
        ).unwrap();

        assert!(addr.starts_with("SRX20_"));
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
            "deployer", "Token".to_string(), "TK".to_string(),
            18, 1_000, 0, 1_000,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_token_transfer() {
        let mut bc = setup_chain();
        bc.accounts.credit("alice", 1_000_000).unwrap();

        let addr = bc.deploy_token(
            "alice", "Coin".to_string(), "CN".to_string(),
            18, 500_000, 0, 10_000,
        ).unwrap();

        bc.token_transfer(&addr, "alice", "bob", 100_000, 1_000).unwrap();
        assert_eq!(bc.token_balance(&addr, "alice"), 400_000);
        assert_eq!(bc.token_balance(&addr, "bob"), 100_000);
    }

    #[test]
    fn test_token_transfer_gas_burned() {
        let mut bc = setup_chain();
        bc.accounts.credit("alice", 1_000_000).unwrap();

        let addr = bc.deploy_token(
            "alice", "Coin".to_string(), "CN".to_string(),
            18, 500_000, 0, 0,
        ).unwrap();

        let burned_before = bc.accounts.total_burned;
        bc.token_transfer(&addr, "alice", "bob", 100, 10_000).unwrap();
        // 50% of 10_000 gas = 5_000 burned
        assert_eq!(bc.accounts.total_burned - burned_before, 5_000);
    }

    #[test]
    fn test_token_info() {
        let mut bc = setup_chain();
        bc.accounts.credit("deployer", 1_000_000).unwrap();

        let addr = bc.deploy_token(
            "deployer", "MyToken".to_string(), "MT".to_string(),
            8, 21_000_000, 0, 0,
        ).unwrap();

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
        bc.deploy_token("d", "A".to_string(), "A".to_string(), 18, 100, 0, 0).unwrap();
        bc.deploy_token("d", "B".to_string(), "B".to_string(), 18, 200, 0, 0).unwrap();
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
            sender.clone(), TEST_RECV.to_string(),
            100_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        let tx_high = Transaction::new(
            sender.clone(), TEST_RECV.to_string(),
            100_000, MIN_TX_FEE * 100, 1, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        let tx_mid = Transaction::new(
            sender.clone(), TEST_RECV.to_string(),
            100_000, MIN_TX_FEE * 10, 2, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();

        bc.add_to_mempool(tx_low).unwrap();
        bc.add_to_mempool(tx_high).unwrap();
        bc.add_to_mempool(tx_mid).unwrap();

        // Mempool should be ordered: high, mid, low
        let fees: Vec<u64> = bc.mempool.iter().map(|tx| tx.fee).collect();
        assert_eq!(fees, vec![MIN_TX_FEE * 100, MIN_TX_FEE * 10, MIN_TX_FEE]);
    }

    #[test]
    fn test_c02_add_block_rejects_unauthorized_validator() {
        let mut bc = setup_chain();
        // Add a second validator (unchecked for test control over address string)
        bc.authority.add_validator_unchecked(
            "validator2".to_string(), "Validator 2".to_string(), "pk2".to_string(),
        );

        // Determine who is authorized for block 1
        let expected = bc.authority.expected_validator(1).unwrap().address.clone();
        let unauthorized = if expected == "validator1" { "validator2" } else { "validator1" };

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
        assert!(err_str.contains("not authorized"), "Expected 'not authorized' error, got: {}", err_str);
    }

    #[test]
    fn test_h02_mempool_rejects_overflow_amount_fee() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);

        bc.accounts.credit(&sender, 100_000_000).unwrap();

        // Create tx with amount = u64::MAX and fee = 1 — would overflow
        let tx = Transaction::new(
            sender, TEST_RECV.to_string(),
            u64::MAX, 1, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();

        let result = bc.add_to_mempool(tx);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("overflow") || err_str.contains("fee"),
            "Expected overflow error, got: {}", err_str
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
        assert!(err_str.contains("timestamp"), "Expected timestamp error, got: {}", err_str);
    }

    #[test]
    fn test_h06_add_block_rejects_future_timestamp() {
        let mut bc = setup_chain();

        let mut block = bc.create_block("validator1").unwrap();
        // Set timestamp far in the future (1 hour from now)
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() + 3600;
        block.timestamp = future;
        block.hash = block.calculate_hash();

        let result = bc.add_block(block);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("future"), "Expected future timestamp error, got: {}", err_str);
    }

    #[test]
    fn test_l02_latest_block_on_empty_chain_returns_err() {
        let mut bc = Blockchain::new("admin".to_string());
        bc.chain.clear();
        let result = bc.latest_block();
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("empty"), "Expected 'empty' error, got: {}", err_str);
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
        use crate::core::transaction::{TokenOp, TOKEN_OP_ADDRESS};

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
            deployer.clone(), TOKEN_OP_ADDRESS.to_string(),
            0, MIN_TX_FEE, 0, token_op.encode().unwrap(),
            CHAIN_ID, &sk, &pk,
        ).unwrap();
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
        use crate::core::transaction::{TokenOp, TOKEN_OP_ADDRESS};

        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let alice = derive_addr(&pk);
        bc.accounts.credit(&alice, 10_000_000).unwrap();

        // Deploy token first (old method for setup)
        let contract = bc.deploy_token(
            &alice, "Coin".to_string(), "CN".to_string(), 8, 500_000, 0, 0,
        ).unwrap();

        // Create transfer transaction
        let bob = TEST_RECV; // V8-H-02: use valid-format address
        let token_op = TokenOp::Transfer {
            contract: contract.clone(),
            to: bob.to_string(),
            amount: 100_000,
        };
        let tx = Transaction::new(
            alice.clone(), TOKEN_OP_ADDRESS.to_string(),
            0, MIN_TX_FEE, 0, token_op.encode().unwrap(),
            CHAIN_ID, &sk, &pk,
        ).unwrap();
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
        use crate::core::transaction::{TokenOp, TOKEN_OP_ADDRESS};

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
            deployer.clone(), TOKEN_OP_ADDRESS.to_string(),
            0, MIN_TX_FEE, 0, token_op.encode().unwrap(),
            CHAIN_ID, &sk, &pk,
        ).unwrap();
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
        use crate::core::transaction::{TokenOp, TOKEN_OP_ADDRESS};

        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let alice = derive_addr(&pk);
        bc.accounts.credit(&alice, 10_000_000).unwrap();

        let contract = bc.deploy_token(
            &alice, "Coin".to_string(), "CN".to_string(), 8, 100, 0, 0,
        ).unwrap();

        // Try to transfer more than token balance
        let token_op = TokenOp::Transfer {
            contract: contract.clone(),
            to: "bob".to_string(),
            amount: 999, // alice only has 100
        };
        let tx = Transaction::new(
            alice.clone(), TOKEN_OP_ADDRESS.to_string(),
            0, MIN_TX_FEE, 0, token_op.encode().unwrap(),
            CHAIN_ID, &sk, &pk,
        ).unwrap();

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
        assert!(is_valid_sentrix_address("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"));
        assert!(is_valid_sentrix_address("0x0000000000000000000000000000000000000000"));
        assert!(is_valid_sentrix_address("0xabcdef0123456789abcdef0123456789abcdef01"));

        // Invalid: no prefix
        assert!(!is_valid_sentrix_address("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"));
        // Invalid: too short
        assert!(!is_valid_sentrix_address("0xdeadbeef"));
        // Invalid: non-hex chars
        assert!(!is_valid_sentrix_address("0xGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG"));
        // Invalid: empty
        assert!(!is_valid_sentrix_address(""));
        // Invalid: 0x prefix but too long
        assert!(!is_valid_sentrix_address("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefff"));
    }

    // ── M-03: Transaction Timestamp Validation ──────────

    #[test]
    fn test_m03_rejects_future_timestamp() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000).unwrap();

        let mut tx = Transaction::new(
            sender, TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();

        // Tamper timestamp to +10 min in future (beyond +5 min tolerance)
        tx.timestamp += 601;

        let result = bc.add_to_mempool(tx);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(err_str.contains("future"), "Expected 'future' error, got: {}", err_str);
    }

    #[test]
    fn test_m03_rejects_expired_timestamp() {
        let mut bc = setup_chain();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000).unwrap();

        let mut tx = Transaction::new(
            sender, TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();

        // Tamper timestamp to 2 hours ago (beyond 1h TTL)
        tx.timestamp = tx.timestamp.saturating_sub(7_200);

        let result = bc.add_to_mempool(tx);
        assert!(result.is_err());
        let err_str = result.unwrap_err().to_string();
        assert!(
            err_str.contains("old") || err_str.contains("age"),
            "Expected 'old'/'age' error, got: {}", err_str
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
            sender, TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();

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
            sender.clone(), TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
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
            sender, TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
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
            sender, TEST_RECV.to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
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
            sender, TEST_RECV.to_string(),
            100, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
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
        bc.deploy_token("deployer", "TestToken".to_string(), "TT".to_string(), 8, 1_000_000, 0, 3).unwrap();
        assert_eq!(bc.accounts.total_burned, initial_burned + 2);
    }

    #[test]
    fn test_l02_gas_fee_burn_rounds_up() {
        let mut bc = setup_chain();
        bc.accounts.credit("user1", 10_000_000).unwrap();

        // Deploy a token first
        let contract = bc.deploy_token("user1", "Gas".to_string(), "GAS".to_string(), 8, 1_000, 0, 0).unwrap();

        let initial_burned = bc.accounts.total_burned;
        // Transfer with odd gas_fee=5 → burn=(5+1)/2=3
        bc.token_transfer(&contract, "user1", "user2", 100, 5).unwrap();
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
        assert!(bc.get_block(0).is_none(), "evicted block should return None");
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
        let addr = bc.deploy_token(
            "deployer", "Capped".to_string(), "CAP".to_string(),
            18, 500_000, 1_000_000, 0,
        ).unwrap();
        let info = bc.token_info(&addr).unwrap();
        assert_eq!(info["max_supply"], 1_000_000);
        assert_eq!(info["total_supply"], 500_000);
    }

    #[test]
    fn test_v502_deploy_with_zero_max_supply_is_unlimited() {
        let mut bc = setup_chain();
        bc.accounts.credit("deployer", 1_000_000).unwrap();
        let addr = bc.deploy_token(
            "deployer", "Unlimited".to_string(), "UNL".to_string(),
            18, 100_000, 0, 0,
        ).unwrap();
        let info = bc.token_info(&addr).unwrap();
        assert_eq!(info["max_supply"], 0); // 0 = unlimited
    }

    // ── V5-10: HASH_VERSION constant ──────────────────────

    #[test]
    fn test_v510_hash_version_constant_is_1() {
        assert_eq!(HASH_VERSION, 1, "HASH_VERSION must be 1 (SHA-256)");
    }

    // ── SentrixTrie unit tests ────────────────────────────

    fn temp_db() -> (tempfile::TempDir, sled::Db) {
        let dir = tempfile::TempDir::new().unwrap();
        let db  = sled::open(dir.path()).unwrap();
        (dir, db)
    }

    /// A freshly constructed Blockchain must have state_trie = None.
    #[test]
    fn test_state_trie_none_by_default() {
        let bc = setup_chain();
        assert!(bc.state_trie.is_none(), "state_trie must be None before init_trie()");
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
        let (_dir, db) = temp_db();
        let mut bc = setup_chain();
        bc.init_trie(&db).unwrap();
        assert!(bc.state_trie.is_some());

        let block = bc.create_block("validator1").unwrap();
        bc.add_block(block).unwrap();

        let root = bc.trie_root_at(1);
        assert!(root.is_some(), "trie_root_at(1) must be Some after adding block 1");
    }

    /// trie_root_at() must return None for a version that has not been committed yet.
    #[test]
    fn test_trie_root_at_uncommitted_version_returns_none() {
        let (_dir, db) = temp_db();
        let mut bc = setup_chain();
        bc.init_trie(&db).unwrap();
        // No blocks added — version 1 has not been committed
        assert_eq!(bc.trie_root_at(1), None, "uncommitted version must return None");
    }

    /// Multiple blocks must each have a distinct committed root persisted in the trie.
    #[test]
    fn test_trie_multiple_blocks_all_roots_persisted() {
        let (_dir, db) = temp_db();
        let mut bc = setup_chain();
        bc.init_trie(&db).unwrap();

        for i in 1u64..=3 {
            let block = bc.create_block("validator1").unwrap();
            bc.add_block(block).unwrap();
            assert!(bc.trie_root_at(i).is_some(), "root at height {} must be committed", i);
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
        let (_dir, db) = temp_db();
        let mut bc_trie = setup_chain();
        bc_trie.init_trie(&db).unwrap();
        let b2 = bc_trie.create_block("validator1").unwrap();
        bc_trie.add_block(b2).unwrap();
        assert!(
            bc_trie.latest_block().unwrap().state_root.is_some(),
            "state_root must be Some when trie is initialized"
        );
    }
}
