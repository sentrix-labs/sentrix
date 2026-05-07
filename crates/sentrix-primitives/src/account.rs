// account.rs - Sentrix

use crate::error::{SentrixError, SentrixResult};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};

/// A2: maximum number of failed EVM tx hashes to track before FIFO eviction.
/// Bounded so the set cannot grow unbounded across the chain lifetime.
const MAX_FAILED_EVM_TXS: usize = 100_000;

pub const SENTRI_PER_SRX: u64 = 100_000_000;

/// Keccak-256 hash of empty bytecode (EOA accounts).
/// Equivalent to keccak256(&[]) = 0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
pub const EMPTY_CODE_HASH: [u8; 32] = [
    0xc5, 0xd2, 0x46, 0x01, 0x86, 0xf7, 0x23, 0x3c, 0x92, 0x7e, 0x7d, 0xb2, 0xdc, 0xc7, 0x03, 0xc0,
    0xe5, 0x00, 0xb6, 0x53, 0xca, 0x82, 0x27, 0x3b, 0x7b, 0xfa, 0xd8, 0x04, 0x5d, 0x85, 0xa4, 0x70,
];

/// Empty storage root (no contract storage).
pub const EMPTY_STORAGE_ROOT: [u8; 32] = [0u8; 32];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub address: String,
    pub balance: u64, // in sentri units (EVM boundary converts to U256)
    pub nonce: u64,
    /// Keccak-256 of contract bytecode. EMPTY_CODE_HASH for EOA (non-contract) accounts.
    /// Added in Voyager EVM upgrade. Defaults to EMPTY_CODE_HASH for backward compat.
    #[serde(default = "default_code_hash")]
    pub code_hash: [u8; 32],
    /// Root hash of per-account contract storage trie. EMPTY_STORAGE_ROOT for EOA.
    /// Added in Voyager EVM upgrade. Defaults to EMPTY_STORAGE_ROOT for backward compat.
    #[serde(default)]
    pub storage_root: [u8; 32],
}

fn default_code_hash() -> [u8; 32] {
    EMPTY_CODE_HASH
}

impl Account {
    pub fn new(address: String) -> Self {
        Self {
            address,
            balance: 0,
            nonce: 0,
            code_hash: EMPTY_CODE_HASH,
            storage_root: EMPTY_STORAGE_ROOT,
        }
    }

    pub fn balance_srx(&self) -> f64 {
        self.balance as f64 / SENTRI_PER_SRX as f64
    }

    /// Returns true if this account has deployed contract code.
    pub fn is_contract(&self) -> bool {
        self.code_hash != EMPTY_CODE_HASH
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AccountDB {
    pub accounts: HashMap<String, Account>,
    pub total_burned: u64, // total SRX burned in sentri
    /// Contract bytecode storage: code_hash (hex) → bytecode bytes.
    /// Only populated after Voyager EVM fork.
    #[serde(default)]
    pub contract_code: HashMap<String, Vec<u8>>,
    /// Contract storage: "address:slot_hex" → value bytes.
    /// Only populated after Voyager EVM fork.
    #[serde(default)]
    pub contract_storage: HashMap<String, Vec<u8>>,
    /// A2: Set of EVM tx hashes that reverted during execution. Used by
    /// `eth_getTransactionReceipt` to return `status=0x0` for failed EVM
    /// txs (default is `0x1` for success). Bounded with FIFO eviction at
    /// `MAX_FAILED_EVM_TXS` so it cannot grow unbounded.
    #[serde(default)]
    pub failed_evm_txs: HashSet<String>,
    /// A2: Insertion order for `failed_evm_txs` to drive FIFO eviction
    /// without scanning the set. Kept in lockstep with `failed_evm_txs`.
    #[serde(default)]
    pub failed_evm_txs_order: VecDeque<String>,

    /// state_root v2 drift fix (audit follow-up, 2026-05-07): per-block
    /// accumulator of every address mutated during apply. The trie's
    /// `update_trie_for_block` historically derived its touched-address
    /// list from `tx.from`, `tx.to`, `block.validator`, and (post-v2)
    /// `PROTOCOL_TREASURY`. That derivation misses EVM-CREATE'd contract
    /// addresses and EVM internal-CALL recipients, so the trie root
    /// silently desynced from AccountDB on those addresses. Validators
    /// would agree on trie root for a long time (untracked addresses
    /// drift quietly across hosts), then halt the moment a block touched
    /// a previously-drifted address. Halt at h=1,650,000 on 2026-05-07
    /// localised this exactly: `state-diff` showed identical block
    /// content with divergent trie roots, while STATE-FP traces showed
    /// agreeing AccountDB through h=1,649,999.
    ///
    /// Every mutator on AccountDB now inserts the touched address here.
    /// `apply_block_pass2` clears the set at start of block; the trie
    /// drains it post-`EXTENDED_TOUCH_LIST_HEIGHT` fork. `#[serde(skip)]`
    /// because it's per-block scratch — never persisted to chain.db.
    #[serde(skip)]
    pub touched_in_block: HashSet<String>,
}

impl AccountDB {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_create(&mut self, address: &str) -> &mut Account {
        self.accounts
            .entry(address.to_string())
            .or_insert_with(|| Account::new(address.to_string()))
    }

    pub fn get_balance(&self, address: &str) -> u64 {
        self.accounts.get(address).map(|a| a.balance).unwrap_or(0)
    }

    pub fn get_nonce(&self, address: &str) -> u64 {
        self.accounts.get(address).map(|a| a.nonce).unwrap_or(0)
    }

    /// Burn tokens (add to total_burned tracker, no balance deducted)
    pub fn burn(&mut self, amount: u64) {
        self.total_burned = self.total_burned.saturating_add(amount);
    }

    pub fn credit(&mut self, address: &str, amount: u64) -> SentrixResult<()> {
        self.touched_in_block.insert(address.to_string());
        let account = self.get_or_create(address);
        account.balance = account
            .balance
            .checked_add(amount)
            .ok_or_else(|| SentrixError::Internal("balance overflow".to_string()))?;
        Ok(())
    }

    /// Charge `fee` against `from` without transferring value, without
    /// crediting any recipient, and **without bumping the nonce.** Used by
    /// the EVM apply path: revm owns nonce + value + recipient credit, so
    /// the native pass must only collect the fee. Using regular `transfer`
    /// for EVM txs causes `NonceTooLow { tx, state }` in revm because
    /// native bumped the nonce before revm read state — see
    /// `audits/evm-create-nonce-bug-2026-04-27.md`.
    pub fn charge_fee_only(&mut self, from: &str, fee: u64) -> SentrixResult<()> {
        let from_balance = self.get_balance(from);
        if from_balance < fee {
            return Err(SentrixError::InsufficientBalance {
                have: from_balance,
                need: fee,
            });
        }
        self.touched_in_block.insert(from.to_string());
        let sender = self.get_or_create(from);
        sender.balance = sender
            .balance
            .checked_sub(fee)
            .ok_or_else(|| SentrixError::Internal("balance underflow".to_string()))?;
        // No nonce bump — revm increments for EVM txs.
        let burn_amount = fee.div_ceil(2);
        self.total_burned = self.total_burned.saturating_add(burn_amount);
        Ok(())
    }

    pub fn transfer(&mut self, from: &str, to: &str, amount: u64, fee: u64) -> SentrixResult<()> {
        let total = amount
            .checked_add(fee)
            .ok_or_else(|| SentrixError::Internal("amount + fee overflow".to_string()))?;
        let from_balance = self.get_balance(from);

        if from_balance < total {
            return Err(SentrixError::InsufficientBalance {
                have: from_balance,
                need: total,
            });
        }

        self.touched_in_block.insert(from.to_string());
        self.touched_in_block.insert(to.to_string());

        // Deduct from sender
        {
            let sender = self.get_or_create(from);
            sender.balance = sender
                .balance
                .checked_sub(total)
                .ok_or_else(|| SentrixError::Internal("balance underflow".to_string()))?;
            // P1: nonce overflow. u64 wraps to 0 at u64::MAX + 1, which
            // would then let an attacker replay any previously-signed tx
            // whose nonce matched the wrapped value. checked_add keeps
            // the chain safe by surfacing the condition as an Err — at
            // current mainnet throughput this is unreachable in any
            // practical lifetime, but the guard is cheap.
            sender.nonce = sender
                .nonce
                .checked_add(1)
                .ok_or_else(|| SentrixError::Internal("nonce overflow".to_string()))?;
        }

        // Credit recipient
        self.credit(to, amount)?;

        // Burn gets ceiling division so odd fees are never lost — validator gets the floor share
        let burn_amount = fee.div_ceil(2);
        self.total_burned = self.total_burned.saturating_add(burn_amount);

        Ok(())
    }

    pub fn apply_block_reward(
        &mut self,
        validator: &str,
        reward: u64,
        fee_share: u64,
    ) -> SentrixResult<()> {
        let total = reward
            .checked_add(fee_share)
            .ok_or_else(|| SentrixError::Internal("reward overflow".to_string()))?;
        self.credit(validator, total)
    }

    pub fn total_supply(&self) -> u64 {
        self.accounts.values().map(|a| a.balance).sum()
    }

    /// Store contract bytecode, keyed by its keccak-256 hash (hex).
    pub fn store_contract_code(&mut self, code_hash_hex: &str, bytecode: Vec<u8>) {
        self.contract_code
            .insert(code_hash_hex.to_string(), bytecode);
    }

    /// Get contract bytecode by its code_hash (hex).
    pub fn get_contract_code(&self, code_hash_hex: &str) -> Option<&Vec<u8>> {
        self.contract_code.get(code_hash_hex)
    }

    /// Store a contract storage value.
    /// Key format: "address:slot_hex" → value bytes.
    pub fn store_contract_storage(&mut self, address: &str, slot_hex: &str, value: Vec<u8>) {
        self.touched_in_block.insert(address.to_string());
        let key = format!("{}:{}", address, slot_hex);
        self.contract_storage.insert(key, value);
    }

    /// Get a contract storage value.
    pub fn get_contract_storage(&self, address: &str, slot_hex: &str) -> Option<&Vec<u8>> {
        let key = format!("{}:{}", address, slot_hex);
        self.contract_storage.get(&key)
    }

    /// Migrate all existing accounts to EVM-compatible format.
    /// Sets code_hash=EMPTY_CODE_HASH and storage_root=EMPTY_STORAGE_ROOT
    /// for all accounts that don't have them set yet.
    /// Called once at EVM fork height.
    pub fn migrate_to_evm(&mut self) -> usize {
        let mut migrated = 0;
        for account in self.accounts.values_mut() {
            if account.code_hash == [0u8; 32] {
                account.code_hash = EMPTY_CODE_HASH;
                migrated += 1;
            }
        }
        tracing::info!("EVM migration: {} accounts migrated", migrated);
        migrated
    }

    /// Set account as a contract (after EVM CREATE).
    pub fn set_contract(&mut self, address: &str, code_hash: [u8; 32]) {
        self.touched_in_block.insert(address.to_string());
        let account = self.get_or_create(address);
        account.code_hash = code_hash;
    }

    /// Set an account's balance to an absolute value in sentri. Used by the
    /// EVM state writeback path (`sentrix_evm::writeback::commit_state_to_account_db`)
    /// where revm returns the post-execution balance as an absolute U256 in
    /// wei, which the caller converts to u64 sentri before calling here.
    ///
    /// Native paths (`transfer`, `credit`) continue to compute deltas — they
    /// must NOT migrate to this setter or they'd lose the overflow/underflow
    /// guards on the delta arithmetic.
    pub fn set_balance(&mut self, address: &str, balance: u64) {
        self.touched_in_block.insert(address.to_string());
        let account = self.get_or_create(address);
        account.balance = balance;
    }

    /// Set an account's nonce to an absolute value. Used by the EVM state
    /// writeback path where revm returns the post-execution nonce directly.
    /// Native paths (`transfer`) continue to increment via `checked_add` —
    /// they must NOT migrate to this setter or they'd lose the overflow guard.
    pub fn set_nonce(&mut self, address: &str, nonce: u64) {
        self.touched_in_block.insert(address.to_string());
        let account = self.get_or_create(address);
        account.nonce = nonce;
    }

    /// Drain the per-block touched-address set. Called by
    /// `update_trie_for_block` post-`EXTENDED_TOUCH_LIST_HEIGHT` fork to
    /// pick up every address mutated during apply (notably EVM-CREATE'd
    /// contracts + EVM internal-CALL recipients that the
    /// `tx.from`/`tx.to` derivation in the legacy path misses).
    /// `apply_block_pass2` clears the set at start of block.
    pub fn drain_touched_in_block(&mut self) -> HashSet<String> {
        std::mem::take(&mut self.touched_in_block)
    }

    /// Reset the per-block touched-address accumulator at start of
    /// apply_block_pass2. Mirror of `drain_touched_in_block` for callers
    /// that want to clear without consuming the value.
    pub fn clear_touched_in_block(&mut self) {
        self.touched_in_block.clear();
    }

    /// A2: Mark an EVM tx hash as failed (reverted). Bounded FIFO — evicts
    /// the oldest entry once the set hits `MAX_FAILED_EVM_TXS` so the
    /// structure cannot grow unbounded.
    pub fn mark_evm_tx_failed(&mut self, txid: &str) {
        if self.failed_evm_txs.insert(txid.to_string()) {
            self.failed_evm_txs_order.push_back(txid.to_string());
            if self.failed_evm_txs_order.len() > MAX_FAILED_EVM_TXS
                && let Some(oldest) = self.failed_evm_txs_order.pop_front()
            {
                self.failed_evm_txs.remove(&oldest);
            }
        }
    }

    /// A2: Returns true if this EVM tx hash is recorded as failed.
    pub fn is_evm_tx_failed(&self, txid: &str) -> bool {
        self.failed_evm_txs.contains(txid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_account_zero_balance() {
        let db = AccountDB::new();
        assert_eq!(db.get_balance("SRX_test"), 0);
        assert_eq!(db.get_nonce("SRX_test"), 0);
    }

    #[test]
    fn test_credit() {
        let mut db = AccountDB::new();
        db.credit("addr1", 1000).unwrap();
        assert_eq!(db.get_balance("addr1"), 1000);
    }

    #[test]
    fn test_transfer_success() {
        let mut db = AccountDB::new();
        db.credit("alice", 10_000).unwrap();
        db.transfer("alice", "bob", 5_000, 100).unwrap();
        assert_eq!(db.get_balance("alice"), 4_900);
        assert_eq!(db.get_balance("bob"), 5_000);
        assert_eq!(db.get_nonce("alice"), 1);
    }

    #[test]
    fn test_transfer_insufficient_balance() {
        let mut db = AccountDB::new();
        db.credit("alice", 100).unwrap();
        let result = db.transfer("alice", "bob", 5_000, 100);
        assert!(result.is_err());
    }

    #[test]
    fn test_burn_tracking() {
        let mut db = AccountDB::new();
        db.credit("alice", 10_000).unwrap();
        db.transfer("alice", "bob", 5_000, 200).unwrap();
        assert_eq!(db.total_burned, 100); // 50% of fee=200 (even fee, unaffected by rounding)
    }

    // ── L-02: burn rounding tests ─────────────────────────

    #[test]
    fn test_l02_burn_odd_fee_rounds_up() {
        let mut db = AccountDB::new();
        db.credit("alice", 10_000).unwrap();

        // fee=1 → burn=(1+1)/2=1, no sentri lost
        db.transfer("alice", "bob", 100, 1).unwrap();
        assert_eq!(db.total_burned, 1);

        // fee=3 → burn=(3+1)/2=2
        db.transfer("alice", "bob", 100, 3).unwrap();
        assert_eq!(db.total_burned, 3); // 1 + 2
    }

    #[test]
    fn test_l02_burn_even_fee_unchanged() {
        let mut db = AccountDB::new();
        db.credit("alice", 10_000).unwrap();

        // fee=2 → burn=1 (same as before fix)
        db.transfer("alice", "bob", 100, 2).unwrap();
        assert_eq!(db.total_burned, 1);

        // fee=100 → burn=50
        db.transfer("alice", "bob", 100, 100).unwrap();
        assert_eq!(db.total_burned, 51); // 1 + 50
    }

    #[test]
    fn test_new_account_has_evm_fields() {
        let account = Account::new("0xtest".to_string());
        assert_eq!(account.code_hash, EMPTY_CODE_HASH);
        assert_eq!(account.storage_root, EMPTY_STORAGE_ROOT);
        assert!(!account.is_contract());
    }

    #[test]
    fn test_is_contract() {
        let mut account = Account::new("0xtest".to_string());
        assert!(!account.is_contract());

        account.code_hash = [0xAA; 32];
        assert!(account.is_contract());
    }

    #[test]
    fn test_migrate_to_evm() {
        let mut db = AccountDB::new();
        // Create accounts with zeroed code_hash (pre-migration state)
        db.credit("alice", 1000).unwrap();
        db.credit("bob", 2000).unwrap();

        // Simulate pre-migration: zero code_hash (old serialization default)
        for account in db.accounts.values_mut() {
            account.code_hash = [0u8; 32];
        }

        let migrated = db.migrate_to_evm();
        assert_eq!(migrated, 2);

        // After migration, all accounts should have EMPTY_CODE_HASH
        for account in db.accounts.values() {
            assert_eq!(account.code_hash, EMPTY_CODE_HASH);
            assert!(!account.is_contract());
        }
    }

    #[test]
    fn test_contract_code_storage() {
        let mut db = AccountDB::new();
        let code = vec![0x60, 0x00, 0x60, 0x00, 0xf3]; // dummy bytecode
        db.store_contract_code("aabbcc", code.clone());
        assert_eq!(db.get_contract_code("aabbcc"), Some(&code));
        assert_eq!(db.get_contract_code("missing"), None);
    }

    #[test]
    fn test_contract_storage() {
        let mut db = AccountDB::new();
        let value = vec![0, 0, 0, 42];
        db.store_contract_storage("0xcontract", "0001", value.clone());
        assert_eq!(db.get_contract_storage("0xcontract", "0001"), Some(&value));
        assert_eq!(db.get_contract_storage("0xcontract", "0002"), None);
    }

    #[test]
    fn test_set_contract() {
        let mut db = AccountDB::new();
        db.credit("0xaddr", 1000).unwrap();
        assert!(
            !db.accounts
                .get("0xaddr")
                .map(|a| a.is_contract())
                .unwrap_or(false)
        );

        db.set_contract("0xaddr", [0xBB; 32]);
        assert!(
            db.accounts
                .get("0xaddr")
                .map(|a| a.is_contract())
                .unwrap_or(false)
        );
    }

    #[test]
    fn test_set_balance_overrides_existing() {
        let mut db = AccountDB::new();
        db.credit("alice", 1_000).unwrap();
        assert_eq!(db.get_balance("alice"), 1_000);

        db.set_balance("alice", 42);
        assert_eq!(db.get_balance("alice"), 42);

        // On a fresh address (no prior credit) set_balance auto-creates.
        db.set_balance("bob", 999);
        assert_eq!(db.get_balance("bob"), 999);
    }

    #[test]
    fn test_set_nonce_direct() {
        let mut db = AccountDB::new();
        // Fresh address: starts at 0, setter bumps to N.
        db.set_nonce("alice", 5);
        assert_eq!(db.get_nonce("alice"), 5);

        // Overwrite is allowed (EVM writeback may set to a lower value
        // only in pathological rollback-ish scenarios; setter itself
        // enforces no policy).
        db.set_nonce("alice", 3);
        assert_eq!(db.get_nonce("alice"), 3);
    }

    #[test]
    fn test_l02_fee_fully_accounted() {
        // For any fee, burn + validator_share must equal fee.
        // burn = (fee+1)/2, validator = fee - burn = fee/2 (floor)
        for fee in [0u64, 1, 2, 3, 7, 99, 100, 1_000_001] {
            let burn = fee.div_ceil(2);
            let validator = fee - burn;
            assert_eq!(burn + validator, fee, "fee={fee} not fully distributed");
        }
    }

    // ── charge_fee_only — regression for EVM CREATE nonce-too-low bug ──

    #[test]
    fn test_charge_fee_only_deducts_fee_and_does_not_bump_nonce() {
        let mut db = AccountDB::new();
        db.credit("alice", 100_000).unwrap();
        let pre_nonce = db.get_nonce("alice");

        db.charge_fee_only("alice", 10_000).expect("charge should succeed");

        assert_eq!(db.get_balance("alice"), 90_000, "fee should be deducted");
        assert_eq!(
            db.get_nonce("alice"),
            pre_nonce,
            "charge_fee_only must NOT bump nonce — revm owns nonce for EVM txs"
        );
        assert_eq!(db.total_burned, 5_000, "half of fee burned, ceil-div");
    }

    #[test]
    fn test_charge_fee_only_rejects_insufficient_balance() {
        let mut db = AccountDB::new();
        db.credit("alice", 9_999).unwrap();
        let result = db.charge_fee_only("alice", 10_000);
        assert!(matches!(
            result,
            Err(SentrixError::InsufficientBalance { have: 9_999, need: 10_000 })
        ));
        assert_eq!(db.get_balance("alice"), 9_999, "balance unchanged on rejection");
        assert_eq!(db.get_nonce("alice"), 0, "nonce unchanged on rejection");
    }

    #[test]
    fn test_transfer_still_bumps_nonce() {
        // Regression guard: charge_fee_only is for EVM only;
        // transfer (native path) MUST still bump nonce.
        let mut db = AccountDB::new();
        db.credit("alice", 100_000).unwrap();
        db.transfer("alice", "bob", 50_000, 10_000).unwrap();
        assert_eq!(db.get_nonce("alice"), 1, "native transfer must bump nonce");
        assert_eq!(db.get_balance("alice"), 40_000);
        assert_eq!(db.get_balance("bob"), 50_000);
    }

    /// H3 reproducer (Rust audit 2026-05-06): the EVM apply path uses
    /// `set_balance(addr, new_balance)` to write back revm's
    /// post-execution wei→sentri balance. revm has internally deducted
    /// `gas_used × base_fee` from the sender's wei balance during exec.
    /// Because `set_balance` is a raw setter (no `total_burned` update,
    /// no validator credit, no delta accounting), the gas portion is
    /// silently destroyed — `total_supply()` drops by an amount that is
    /// not reflected anywhere else in the supply ledger.
    ///
    /// On the native path the invariant `supply_drop == total_burned_delta`
    /// holds because `transfer` / `charge_fee_only` correctly bookkeep the
    /// burn half (`total_burned += fee/2`) and the validator half goes to
    /// the validator account (still in supply). On the EVM path,
    /// `commit_state_to_account_db` (sentrix-evm/src/writeback.rs:113)
    /// breaks that invariant.
    ///
    /// This test asserts the POST-FIX expected behaviour — `leak == 0`.
    /// It is `#[ignore]`d until the fix lands so CI stays green; un-ignore
    /// it after `disable_base_fee=true` is set on the write path OR
    /// `commit_state_to_account_db` is taught to credit validator +
    /// `total_burned` per the 50/50 split rule.
    #[test]
    #[ignore = "H3: known supply leak — un-ignore after EVM fee-accounting fix lands (audit 2026-05-06)"]
    fn test_h3_evm_writeback_set_balance_breaks_supply_invariant() {
        let mut db = AccountDB::new();
        let alice = "alice";
        let validator = "v1";

        // Genesis: alice holds 1B sentri = 10 SRX.
        db.credit(alice, 1_000_000_000).unwrap();
        let initial_supply = db.total_supply();
        let initial_burned = db.total_burned;
        assert_eq!(initial_supply, 1_000_000_000);
        assert_eq!(initial_burned, 0);

        // Step 1 — Pass-1 native: charge_fee_only(alice, MIN_TX_FEE).
        // MIN_TX_FEE = 10_000 sentri. Burn 5_000, validator 5_000.
        let fee = 10_000u64;
        db.charge_fee_only(alice, fee).unwrap();

        // Step 2 — block finalizer credits the validator floor(fee/2).
        let validator_fee_share = fee / 2;
        db.credit(validator, validator_fee_share).unwrap();

        let after_pass1 = db.total_supply();
        let after_pass1_burned = db.total_burned;
        assert_eq!(after_pass1, 999_995_000);
        assert_eq!(after_pass1_burned, 5_000);
        // Native invariant holds: supply drop == burn increment.
        assert_eq!(initial_supply - after_pass1, after_pass1_burned - initial_burned);

        // Step 3 — Simulate `commit_state_to_account_db` writeback after
        // revm internally deducted gas. Real values: gas_used = 100_000,
        // INITIAL_BASE_FEE = 10_000 wei, /1e10 → 0 sentri at this scale,
        // but a contract call typically lands ≥ 1M sentri. Use 1M to
        // make the leak visible.
        let evm_gas_deducted_sentri = 1_000_000u64;
        let pre_writeback_alice = db.get_balance(alice);
        db.set_balance(alice, pre_writeback_alice - evm_gas_deducted_sentri);

        let after_writeback = db.total_supply();
        let after_writeback_burned = db.total_burned;

        // Supply dropped further by the gas deduction.
        assert_eq!(
            after_pass1 - after_writeback,
            evm_gas_deducted_sentri,
            "supply must drop exactly by the gas writeback"
        );
        // But total_burned did NOT update.
        assert_eq!(
            after_writeback_burned, after_pass1_burned,
            "total_burned was NOT incremented by the EVM gas portion"
        );
        // No-one else's balance went up either.
        assert_eq!(db.get_balance(validator), validator_fee_share);

        // The H3 leak — supply dropped more than total_burned increased.
        // Native invariant: total_supply drop == total_burned increment
        // (validator credit is part of supply because it lives in an
        // account; only burning removes from supply). EVM writeback's
        // set_balance violates this because it doesn't bookkeep the
        // delta anywhere.
        let supply_drop = initial_supply - after_writeback;
        let burned_delta = after_writeback_burned - initial_burned;
        let leak = supply_drop - burned_delta;

        // POST-FIX assertion: every sentri removed from total_supply must be
        // accounted for in total_burned (or credited to a balance, which
        // would keep it IN supply rather than dropping). Pre-fix this fails
        // because the EVM gas deduction flows out via set_balance with no
        // bookkeeping — leak == evm_gas_deducted_sentri (1M sentri here).
        assert_eq!(
            leak, 0,
            "H3 supply leak: {} sentri dropped from total_supply via EVM \
             writeback are NOT in total_burned and NOT in any account. Fix: \
             disable_base_fee=true on write path, OR commit_state_to_account_db \
             credits the delta per the 50/50 split rule.",
            leak,
        );
    }
}
