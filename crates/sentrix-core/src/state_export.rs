// state_export.rs — State snapshot export/import for backup, bootstrap, and migration.

use crate::blockchain::Blockchain;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use serde::{Deserialize, Serialize};

/// Complete snapshot of chain state at a specific height. Includes account
/// balances, contract code, contract storage, validator set, and staking
/// state. Designed for:
/// - Backup before risky upgrades
/// - Bootstrap a new node without full history sync
/// - Fork the chain for testing
/// - Archive historical state
/// - Migrate between versions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Snapshot format version (bump on breaking changes).
    pub version: u32,
    /// Metadata at the point of export.
    pub metadata: SnapshotMetadata,
    /// All accounts with non-zero balance or non-zero nonce.
    pub accounts: Vec<AccountEntry>,
    /// Deployed contract bytecode (code_hash → hex-encoded bytes).
    pub contract_code: Vec<ContractCodeEntry>,
    /// Per-contract storage slots.
    pub contract_storage: Vec<StorageEntry>,
    /// Active + inactive validators.
    pub validators: Vec<ValidatorEntry>,
    /// Admin address (authority manager).
    pub admin_address: String,
    /// Chain-wide counters.
    pub total_minted: u64,
    pub total_burned: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMetadata {
    pub chain_id: u64,
    pub height: u64,
    pub block_hash: String,
    pub timestamp: u64,
    pub exported_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountEntry {
    pub address: String,
    pub balance: u64,
    pub nonce: u64,
    pub code_hash: String,
    pub storage_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractCodeEntry {
    pub code_hash: String,
    pub bytecode_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageEntry {
    pub key: String,
    pub value_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorEntry {
    pub address: String,
    pub name: String,
    pub public_key: String,
    pub is_active: bool,
    pub blocks_produced: u64,
}

const SNAPSHOT_VERSION: u32 = 1;

impl Blockchain {
    /// Export the current chain state as a `StateSnapshot`.
    /// Must be called while the node is STOPPED (no concurrent writes to MDBX).
    pub fn export_state(&self) -> SentrixResult<StateSnapshot> {
        let height = self.height();
        let latest = self.latest_block()?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let metadata = SnapshotMetadata {
            chain_id: self.chain_id,
            height,
            block_hash: latest.hash.clone(),
            timestamp: latest.timestamp,
            exported_at: now,
        };

        // Accounts: export all with balance > 0 or nonce > 0 or is_contract.
        let mut accounts: Vec<AccountEntry> = self
            .accounts
            .accounts
            .values()
            .filter(|a| a.balance > 0 || a.nonce > 0 || a.is_contract())
            .map(|a| AccountEntry {
                address: a.address.clone(),
                balance: a.balance,
                nonce: a.nonce,
                code_hash: hex::encode(a.code_hash),
                storage_root: hex::encode(a.storage_root),
            })
            .collect();
        accounts.sort_by(|a, b| a.address.cmp(&b.address));

        // Contract code
        let mut contract_code: Vec<ContractCodeEntry> = self
            .accounts
            .contract_code
            .iter()
            .map(|(hash, bytes)| ContractCodeEntry {
                code_hash: hash.clone(),
                bytecode_hex: hex::encode(bytes),
            })
            .collect();
        contract_code.sort_by(|a, b| a.code_hash.cmp(&b.code_hash));

        // Contract storage
        let mut contract_storage: Vec<StorageEntry> = self
            .accounts
            .contract_storage
            .iter()
            .map(|(key, value)| StorageEntry {
                key: key.clone(),
                value_hex: hex::encode(value),
            })
            .collect();
        contract_storage.sort_by(|a, b| a.key.cmp(&b.key));

        // Validators
        let mut validators: Vec<ValidatorEntry> = self
            .authority
            .validators
            .values()
            .map(|v| ValidatorEntry {
                address: v.address.clone(),
                name: v.name.clone(),
                public_key: v.public_key.clone(),
                is_active: v.is_active,
                blocks_produced: v.blocks_produced,
            })
            .collect();
        validators.sort_by(|a, b| a.address.cmp(&b.address));

        Ok(StateSnapshot {
            version: SNAPSHOT_VERSION,
            metadata,
            accounts,
            contract_code,
            contract_storage,
            validators,
            admin_address: self.authority.admin_address.clone(),
            total_minted: self.total_minted,
            total_burned: self.accounts.total_burned,
        })
    }

    /// Import state from a snapshot, REPLACING the current accounts,
    /// contracts, storage, and validator set. Chain history (blocks) is
    /// NOT imported — only the latest state. After import the node should
    /// be restarted to rebuild its in-memory trie.
    ///
    /// Returns the number of accounts imported.
    pub fn import_state(&mut self, snapshot: &StateSnapshot) -> SentrixResult<usize> {
        if snapshot.version != SNAPSHOT_VERSION {
            return Err(SentrixError::Internal(format!(
                "unsupported snapshot version: {} (expected {})",
                snapshot.version, SNAPSHOT_VERSION
            )));
        }

        // Clear current state
        self.accounts.accounts.clear();
        self.accounts.contract_code.clear();
        self.accounts.contract_storage.clear();
        self.accounts.total_burned = snapshot.total_burned;
        self.total_minted = snapshot.total_minted;

        // Import accounts
        for entry in &snapshot.accounts {
            let account = self.accounts.get_or_create(&entry.address);
            account.balance = entry.balance;
            account.nonce = entry.nonce;
            if let Ok(bytes) = hex::decode(&entry.code_hash)
                && bytes.len() == 32
            {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                account.code_hash = arr;
            }
            if let Ok(bytes) = hex::decode(&entry.storage_root)
                && bytes.len() == 32
            {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                account.storage_root = arr;
            }
        }

        // Import contract code
        for entry in &snapshot.contract_code {
            if let Ok(bytes) = hex::decode(&entry.bytecode_hex) {
                self.accounts.store_contract_code(&entry.code_hash, bytes);
            }
        }

        // Import contract storage
        for entry in &snapshot.contract_storage {
            if let Ok(bytes) = hex::decode(&entry.value_hex) {
                // Key format: "address:slot_hex"
                let parts: Vec<&str> = entry.key.splitn(2, ':').collect();
                if parts.len() == 2 {
                    self.accounts
                        .store_contract_storage(parts[0], parts[1], bytes);
                }
            }
        }

        // Import validators
        self.authority.validators.clear();
        self.authority.admin_address = snapshot.admin_address.clone();
        for v in &snapshot.validators {
            use crate::authority::Validator;
            let mut val = Validator::new(v.address.clone(), v.name.clone(), v.public_key.clone());
            val.is_active = v.is_active;
            val.blocks_produced = v.blocks_produced;
            self.authority.validators.insert(v.address.clone(), val);
        }

        Ok(snapshot.accounts.len())
    }

    /// Verify a snapshot's internal consistency.
    /// Returns Ok(summary_string) or Err if corrupt.
    pub fn verify_snapshot(snapshot: &StateSnapshot) -> SentrixResult<String> {
        if snapshot.version != SNAPSHOT_VERSION {
            return Err(SentrixError::Internal(format!(
                "unsupported snapshot version: {}",
                snapshot.version
            )));
        }

        // Check account addresses are valid format
        for a in &snapshot.accounts {
            if !crate::blockchain::is_valid_sentrix_address(&a.address) {
                return Err(SentrixError::Internal(format!(
                    "invalid account address: {}",
                    a.address
                )));
            }
        }

        // Check validators
        for v in &snapshot.validators {
            if !crate::blockchain::is_valid_sentrix_address(&v.address) {
                return Err(SentrixError::Internal(format!(
                    "invalid validator address: {}",
                    v.address
                )));
            }
        }

        // Check no duplicate addresses
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for a in &snapshot.accounts {
            if !seen.insert(&a.address) {
                return Err(SentrixError::Internal(format!(
                    "duplicate account address: {}",
                    a.address
                )));
            }
        }

        let total_balance: u64 = snapshot.accounts.iter().map(|a| a.balance).sum();

        Ok(format!(
            "Snapshot v{} OK: chain_id={} height={} accounts={} validators={} total_balance={:.4} SRX contracts={} storage_entries={}",
            snapshot.version,
            snapshot.metadata.chain_id,
            snapshot.metadata.height,
            snapshot.accounts.len(),
            snapshot.validators.len(),
            total_balance as f64 / 100_000_000.0,
            snapshot.contract_code.len(),
            snapshot.contract_storage.len(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockchain::Blockchain;

    fn setup() -> Blockchain {
        // Use valid Sentrix-format addresses so verify_snapshot passes.
        let admin = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let val = "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mut bc = Blockchain::new(admin.to_string());
        bc.authority.add_validator_unchecked(
            val.to_string(),
            "Validator 1".to_string(),
            "pk1".to_string(),
        );
        bc
    }

    #[test]
    fn test_export_import_roundtrip() {
        let mut bc = setup();
        bc.accounts
            .credit("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef", 1_000_000)
            .unwrap();

        let snapshot = bc.export_state().unwrap();
        assert_eq!(snapshot.version, SNAPSHOT_VERSION);
        assert_eq!(snapshot.metadata.chain_id, bc.chain_id);
        assert!(snapshot.accounts.len() >= 5); // 4 genesis + 1 test

        // Import into a fresh blockchain
        let mut bc2 = Blockchain::new("admin2".to_string());
        let count = bc2.import_state(&snapshot).unwrap();
        assert_eq!(count, snapshot.accounts.len());

        // Verify same balances
        assert_eq!(
            bc2.accounts
                .get_balance("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
            1_000_000
        );
        assert_eq!(bc2.total_minted, bc.total_minted);
        assert_eq!(bc2.accounts.total_burned, bc.accounts.total_burned);
    }

    #[test]
    fn test_verify_valid_snapshot() {
        let bc = setup();
        let snapshot = bc.export_state().unwrap();
        let result = Blockchain::verify_snapshot(&snapshot);
        assert!(result.is_ok());
        let summary = result.unwrap();
        assert!(summary.contains("OK"));
    }

    #[test]
    fn test_verify_bad_version() {
        let bc = setup();
        let mut snapshot = bc.export_state().unwrap();
        snapshot.version = 999;
        let result = Blockchain::verify_snapshot(&snapshot);
        assert!(result.is_err());
    }

    #[test]
    fn test_verify_duplicate_address() {
        let bc = setup();
        let mut snapshot = bc.export_state().unwrap();
        if let Some(first) = snapshot.accounts.first().cloned() {
            snapshot.accounts.push(first);
        }
        let result = Blockchain::verify_snapshot(&snapshot);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("duplicate"));
    }

    // ── Determinism regression tests (2026-04-21 post-mortem from deploy
    //    rollback: state import on one validator diverged state_root from
    //    peers because the export filter dropped zero-balance+zero-nonce
    //    accounts that the trie still cared about in a subtle way). Pin
    //    the invariants so we never regress: export→import→export must be
    //    byte-identical, and a rebuilt chain from snapshot must produce
    //    the same trie state_root as the original chain. ─────────────

    /// Normalize snapshot for equality comparison by zeroing out the
    /// wall-clock `exported_at` field — that's the only field that
    /// legitimately changes between two exports of the same state.
    fn normalize_for_compare(snap: &mut StateSnapshot) {
        snap.metadata.exported_at = 0;
    }

    /// Core determinism test: export → import → export must produce the
    /// same snapshot JSON both times. Any non-determinism in the export
    /// filter or iteration order surfaces here.
    #[test]
    fn test_export_import_export_roundtrip_is_byte_identical() {
        // Populate with a mix of states that exercise the export filter:
        // - non-zero balance, zero nonce
        // - non-zero balance, non-zero nonce
        // - zero balance, non-zero nonce (drained sender)
        // - zero balance, zero nonce (pristine / never touched — should
        //   appear if ever credited-then-drained cleanly)
        let mut bc1 = setup();

        let a1 = "0x1111111111111111111111111111111111111111";
        let a2 = "0x2222222222222222222222222222222222222222";
        let a3 = "0x3333333333333333333333333333333333333333";
        let a4 = "0x4444444444444444444444444444444444444444";

        bc1.accounts.credit(a1, 500_000).unwrap();

        bc1.accounts.credit(a2, 1_000_000).unwrap();
        bc1.accounts.get_or_create(a2).nonce = 5;

        bc1.accounts.credit(a3, 100).unwrap();
        bc1.accounts.transfer(a3, a1, 100, 0).unwrap(); // drain a3 to 0 balance, nonce advances
        assert_eq!(bc1.accounts.get_balance(a3), 0);
        assert!(bc1.accounts.get_nonce(a3) > 0);

        // a4: credit then fully drain back to untouched-looking state (both
        // balance and nonce zero on the receiving side — the corner case
        // the pre-fix filter dropped entirely).
        bc1.accounts.credit(a4, 50).unwrap();
        bc1.accounts.get_or_create(a4).balance = 0;
        bc1.accounts.get_or_create(a4).nonce = 0;

        // Export once.
        let mut snap1 = bc1.export_state().unwrap();

        // Import into a fresh chain.
        let mut bc2 = Blockchain::new("0x9999999999999999999999999999999999999999".into());
        bc2.import_state(&snap1).unwrap();

        // Export the imported chain.
        let mut snap2 = bc2.export_state().unwrap();

        normalize_for_compare(&mut snap1);
        normalize_for_compare(&mut snap2);

        let s1 = serde_json::to_value(&snap1).unwrap();
        let s2 = serde_json::to_value(&snap2).unwrap();

        assert_eq!(
            s1, s2,
            "export → import → export must be byte-identical (determinism)\n\
             original: {}\n\
             after round-trip: {}",
            serde_json::to_string_pretty(&s1).unwrap(),
            serde_json::to_string_pretty(&s2).unwrap(),
        );
    }

    /// The drained-sender corner case: account with balance=0, nonce>0
    /// must survive export/import. Sender address that sent everything
    /// away still has consumed nonces that affect future tx validation.
    #[test]
    fn test_drained_sender_preserved_through_roundtrip() {
        let mut bc1 = setup();

        let sender = "0x1111111111111111111111111111111111111111";
        let recv = "0x2222222222222222222222222222222222222222";

        bc1.accounts.credit(sender, 500).unwrap();
        bc1.accounts.transfer(sender, recv, 500, 0).unwrap();

        // Sender balance now 0, but has consumed nonce.
        assert_eq!(bc1.accounts.get_balance(sender), 0);
        let sender_nonce = bc1.accounts.get_nonce(sender);
        assert!(sender_nonce > 0, "drained sender must have non-zero nonce");

        let snap = bc1.export_state().unwrap();
        let mut bc2 = Blockchain::new("0x9999999999999999999999999999999999999999".into());
        bc2.import_state(&snap).unwrap();

        // The drained sender's nonce must have round-tripped — otherwise
        // any future tx from this address would be replayable or bounce
        // with a bad-nonce error after recovery.
        assert_eq!(
            bc2.accounts.get_nonce(sender),
            sender_nonce,
            "sender nonce must survive import (anti-replay invariant)"
        );
    }

    /// Credit-then-fully-drain-including-nonce leaves an account that is
    /// genuinely indistinguishable from a pristine address. The filter
    /// should NOT include such accounts (nothing to preserve) so the
    /// receiver, who would lazy-create on first touch, ends up in the
    /// same state. This pins that specific corner as INTENTIONAL drop.
    #[test]
    fn test_genuinely_empty_account_is_dropped_and_that_is_ok() {
        let mut bc1 = setup();

        let addr = "0x1111111111111111111111111111111111111111";
        // Credit and deliberately reset both balance AND nonce (not
        // reachable via normal tx flow — this is the "truly empty"
        // variant a zero-entry in HashMap can land in via direct reset).
        bc1.accounts.credit(addr, 100).unwrap();
        bc1.accounts.get_or_create(addr).balance = 0;
        bc1.accounts.get_or_create(addr).nonce = 0;

        let snap = bc1.export_state().unwrap();

        // Export dropped the address: filter is balance>0 OR nonce>0 OR is_contract.
        assert!(
            !snap.accounts.iter().any(|a| a.address == addr),
            "account with balance=0 AND nonce=0 AND not-contract must NOT appear in export"
        );

        // Import into fresh chain: address is NOT created. Lookups return
        // default (balance=0, nonce=0) — indistinguishable from the
        // original where the address DOES exist in the HashMap with those
        // same zero values. Balance/nonce getters both return 0.
        let mut bc2 = Blockchain::new("0x9999999999999999999999999999999999999999".into());
        bc2.import_state(&snap).unwrap();
        assert_eq!(bc2.accounts.get_balance(addr), 0);
        assert_eq!(bc2.accounts.get_nonce(addr), 0);
    }

    /// ROOT CAUSE regression (issue #186): `import_state` rewrites
    /// `accounts` but leaves the trie storage (trie_nodes, trie_values,
    /// trie_roots) intact. If a chain has an existing trie at height H
    /// pre-import, init_trie on restart finds the old root still valid
    /// and keeps using it — the trie now DOESN'T match the imported
    /// accounts. Every subsequent block applies to that stale trie and
    /// produces a state_root that diverges from peers whose trie does
    /// match their accounts.
    ///
    /// This test sets up the EXACT production scenario: chain with
    /// committed trie → import snapshot from a peer → restart init_trie
    /// → MUST rebuild trie so it matches imported accounts. Fails on
    /// main (uses stale trie), passes once `cmd_state_import` calls
    /// `storage.reset_trie()` after `bc.import_state()`.
    #[test]
    fn test_import_then_init_trie_rebuilds_from_imported_accounts() {
        // Shared storage dir so we can simulate a restart (re-open the
        // same MDBX database after dropping the first handle).
        let tmp = tempfile::TempDir::new().unwrap();
        let mdbx_path = tmp.path();

        // Chain 1: build a committed trie at height 1.
        let (accounts_a, accounts_b) = {
            let mdbx = std::sync::Arc::new(
                sentrix_storage::MdbxStorage::open(mdbx_path).unwrap(),
            );
            let mut bc = setup();
            bc.init_trie(mdbx.clone()).unwrap();

            let a1 = "0x1111111111111111111111111111111111111111";
            let a2 = "0x2222222222222222222222222222222222222222";
            bc.accounts.credit(a1, 10_000).unwrap();
            bc.accounts.credit(a2, 20_000).unwrap();

            // Force a trie write so there's a committed root at some height.
            // update_trie_for_block needs touched addresses in the last
            // block; easiest is to drive the insert path directly.
            let height = bc.height();
            if let Some(trie) = bc.state_trie.as_mut() {
                use sentrix_trie::address::{account_value_bytes, address_to_key};
                trie.insert(&address_to_key(a1), &account_value_bytes(10_000, 0))
                    .unwrap();
                trie.insert(&address_to_key(a2), &account_value_bytes(20_000, 0))
                    .unwrap();
                let _ = trie.commit(height).unwrap();
            }

            ((a1, 10_000u64), (a2, 20_000u64))
        };

        // Build a snapshot that represents a DIFFERENT canonical state
        // (as if from a peer who had slightly different balances for
        // the same addresses). Simulates the drift-recovery scenario.
        let snapshot = {
            let mdbx = std::sync::Arc::new(
                sentrix_storage::MdbxStorage::open(mdbx_path).unwrap(),
            );
            let mut other = setup();
            other.init_trie(mdbx).unwrap();
            // Same addresses but different balances — the "canonical" version
            other.accounts.credit(accounts_a.0, 9_999).unwrap();
            other.accounts.credit(accounts_b.0, 19_998).unwrap();
            other.export_state().unwrap()
        };

        // Chain 1 again: import the snapshot. This mimics what
        // cmd_state_import does in the CLI: import_state + reset_trie.
        //
        // The reset_trie call is the fix — without it (pre-fix), the
        // stale trie tables in MDBX survive the import and init_trie on
        // restart reads them as if valid. This test would fail on that.
        // We apply the reset here so the next open sees empty trie tables
        // → init_trie backfills from imported accounts.
        {
            let mdbx = std::sync::Arc::new(
                sentrix_storage::MdbxStorage::open(mdbx_path).unwrap(),
            );
            let mut bc = setup();
            bc.init_trie(mdbx.clone()).unwrap();
            bc.import_state(&snapshot).unwrap();

            // The post-fix CLI calls storage.reset_trie() here. Mimic
            // that with the underlying MDBX table clears so the test
            // does not depend on the Storage wrapper.
            for table in [
                "trie_nodes",
                "trie_values",
                "trie_roots",
                "trie_committed_roots",
            ] {
                mdbx.clear_table(table).unwrap();
            }

            // Drop bc so MDBX flushes. Re-open fresh to simulate the
            // next `sentrix start`.
        }

        // Chain 1 restart: init_trie should rebuild from the imported
        // accounts. If the fix is in place, the trie tables are empty
        // and init_trie triggers the backfill path.
        let mdbx_reopen = std::sync::Arc::new(
            sentrix_storage::MdbxStorage::open(mdbx_path).unwrap(),
        );
        let mut bc_reopened = setup();
        // Re-apply the imported accounts (save_blockchain/load_blockchain
        // would persist+restore them in production; here we set them
        // directly to keep the test at unit level).
        bc_reopened.import_state(&snapshot).unwrap();
        bc_reopened.init_trie(mdbx_reopen).unwrap();
        let root_after_restart = bc_reopened
            .trie_root_at(bc_reopened.height())
            .or_else(|| bc_reopened.state_trie.as_ref().map(|t| t.root_hash()));

        // bc_reference = fresh chain that imported the same snapshot
        // onto a pristine MDBX (no prior trie). This is what an
        // untouched peer would look like.
        let tmp_ref = tempfile::TempDir::new().unwrap();
        let mdbx_ref = std::sync::Arc::new(
            sentrix_storage::MdbxStorage::open(tmp_ref.path()).unwrap(),
        );
        let mut bc_reference = setup();
        bc_reference.import_state(&snapshot).unwrap();
        bc_reference.init_trie(mdbx_ref).unwrap();
        let root_reference = bc_reference
            .trie_root_at(bc_reference.height())
            .or_else(|| bc_reference.state_trie.as_ref().map(|t| t.root_hash()));

        assert_eq!(
            root_after_restart, root_reference,
            "After state import + restart, the restored trie root MUST equal what an \
             untouched peer would compute from the same imported accounts. If they \
             differ, the stale trie from pre-import state is still in use — exactly \
             the bug that halted mainnet 2026-04-21.\n\
             restarted chain root:   {:?}\n\
             untouched-peer root:    {:?}",
            root_after_restart.map(hex::encode),
            root_reference.map(hex::encode),
        );
    }

    /// State-trie equality is the consensus-critical invariant. After
    /// export → import, the rebuilt trie's state_root must match the
    /// original's. If this test fails, two validators running the same
    /// binary and starting from the "same" state can disagree on
    /// state_root → #1e strict-reject → chain halt. Exactly the incident
    /// we hit on 2026-04-21.
    #[test]
    fn test_state_root_identical_after_import() {
        // Set up chain with a MIX of account types that exercises the full trie:
        let mut bc1 = setup();

        let addrs = [
            "0x1111111111111111111111111111111111111111",
            "0x2222222222222222222222222222222222222222",
            "0x3333333333333333333333333333333333333333",
            "0x4444444444444444444444444444444444444444",
            "0x5555555555555555555555555555555555555555",
        ];
        for (i, a) in addrs.iter().enumerate() {
            bc1.accounts.credit(a, (i as u64 + 1) * 100_000).unwrap();
            bc1.accounts.get_or_create(a).nonce = i as u64;
        }
        // Drain one so it has balance=0 nonce>0.
        bc1.accounts
            .transfer(addrs[2], addrs[0], 300_000, 0)
            .unwrap();

        // Build trie on bc1 by simulating a block apply that touches every
        // modified address. We go through init_trie + explicit inserts so
        // the trie root stabilizes deterministically.
        let mdbx1 = std::sync::Arc::new(
            sentrix_storage::MdbxStorage::open(tempfile::TempDir::new().unwrap().path()).unwrap(),
        );
        bc1.init_trie(mdbx1.clone()).unwrap();
        let root1 = bc1.trie_root_at(bc1.height()).or_else(|| {
            bc1.state_trie.as_ref().map(|t| t.root_hash())
        });
        assert!(root1.is_some(), "bc1 must have a committed trie root");

        // Export + import into fresh chain.
        let snap = bc1.export_state().unwrap();
        let mut bc2 = Blockchain::new("0x9999999999999999999999999999999999999999".into());
        bc2.import_state(&snap).unwrap();

        // Build trie on bc2 the same way.
        let mdbx2 = std::sync::Arc::new(
            sentrix_storage::MdbxStorage::open(tempfile::TempDir::new().unwrap().path()).unwrap(),
        );
        bc2.init_trie(mdbx2.clone()).unwrap();
        let root2 = bc2.trie_root_at(bc2.height()).or_else(|| {
            bc2.state_trie.as_ref().map(|t| t.root_hash())
        });
        assert!(root2.is_some(), "bc2 must have a committed trie root");

        assert_eq!(
            root1, root2,
            "state_root must be IDENTICAL between original chain and chain rebuilt from export\n\
             original root = {:?}\n\
             restored root = {:?}\n\
             \n\
             If this fails, the export→import path is non-deterministic and any \
             single-validator state import in a multi-validator deployment WILL cause \
             the #1e state_root mismatch guard to fire (proven in production 2026-04-21).",
            root1.map(hex::encode),
            root2.map(hex::encode),
        );
    }
}
