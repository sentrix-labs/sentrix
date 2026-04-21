//! evm/writeback.rs — Apply a revm EvmState diff back to Sentrix's AccountDB.
//!
//! Called by the block-apply path after a SUCCESSFUL EVM tx execution.
//! Skipped when the tx reverted (caller checks `receipt.success` first);
//! on revert the gas fee is already debited by the native Pass-1 transfer
//! and the EVM state diff is dropped per EVM spec.
//!
//! Rules enforced by this module:
//!
//!   1. Balance wei → sentri: round DOWN. EVM gas accounting at
//!      INITIAL_BASE_FEE = 1e9 wei/unit produces sub-sentri remainders
//!      (e.g. `gas_used=21001 × 1e9 = 2.1001e13 wei`), so the RPC-ingress
//!      rule `!value.is_multiple_of(1e10) → Err` (rpc/jsonrpc/eth.rs:313,
//!      shipped v2.1.6) CANNOT apply here — it'd reject every tx with odd
//!      gas usage. Max precision loss per account per tx: 9_999_999_999
//!      wei = ~1e-9 SRX, which is negligible at any realistic SRX price.
//!
//!   2. Storage slot key: U256 → 64-char lowercase hex via `format!("{:064x}")`.
//!      AccountDB's `store_contract_storage` keys as `"{address}:{slot_hex}"`;
//!      the 64-char padding keeps string comparisons sortable and matches
//!      existing SRC-20 key format.
//!
//!   3. Storage value: U256 → 32-byte big-endian `Vec<u8>`. Reversible with
//!      `U256::from_be_bytes(&value_bytes)` on read. Padding is always 32
//!      bytes so downstream readers can slice without length checks.
//!
//!   4. Only process accounts where `status.is_touched() == true`.
//!      SentrixEvmDb::from_account_db loads ALL accounts into revm's map,
//!      but revm only marks the ones execution actually reads/writes. This
//!      filter prevents spurious AccountDB rewrites.
//!
//!   5. SELFDESTRUCT: observed via `is_selfdestructed()` flag. Writeback
//!      zeros the balance and clears code_hash to EMPTY_CODE_HASH. The
//!      beneficiary's funds transfer is reflected in the beneficiary's
//!      OWN state diff (revm already moved them). Storage slots of the
//!      destroyed contract are not explicitly purged — they become
//!      unreachable because the code pointer is gone; a future prune pass
//!      reclaims. Also emits a `tracing::warn!` for ops visibility.
//!
//!   6. New contract code: present on CREATE txs as `account.info.code = Some(bytecode)`.
//!      Stored via `store_contract_code(code_hash_hex, bytes)` + account
//!      marked as contract via `set_contract(addr, code_hash)`.
//!      Existing contracts whose code didn't change have `code = None`
//!      post-execution (revm doesn't re-load code into the state diff) —
//!      nothing to persist.
//!
//! Determinism: iteration order is NOT the bit-for-bit identity of the
//! output (AccountDB reads are by-key), but sorted iteration prevents
//! log/trace output divergence across validators and matches the pattern
//! used elsewhere (`init_trie` backfill at blockchain.rs:583).
//!
//! Performance note: this function is O(touched_accounts × avg_slots_per_account)
//! per tx. Acceptable at current testnet scale. Future optimization: batch
//! writes through WriteBatch instead of per-call HashMap inserts.

use alloy_primitives::{Address, U256};
use revm::state::EvmState;
use sentrix_primitives::{AccountDB, EMPTY_CODE_HASH, SentrixResult};

use crate::database::address_to_sentrix;

/// Commit a revm `EvmState` diff back to Sentrix's `AccountDB`.
///
/// **Caller must check `receipt.success == true` BEFORE calling this.**
/// On revert, the diff must be dropped per EVM spec; this function applies
/// unconditionally whatever is passed in.
///
/// Returns `Err` only if the AccountDB setters fail (currently they don't,
/// but the return type is `SentrixResult<()>` to preserve the option for
/// future overflow / validation checks).
pub fn commit_state_to_account_db(
    state: &EvmState,
    account_db: &mut AccountDB,
) -> SentrixResult<()> {
    // Sort touched addresses for deterministic iteration order. HashMap
    // iteration is non-deterministic across processes; sorting removes a
    // potential source of cross-validator log divergence. Matches the
    // init_trie backfill pattern at blockchain.rs:583.
    let mut touched: Vec<(&Address, &revm::state::Account)> = state
        .iter()
        .filter(|(_, acc)| acc.is_touched())
        .collect();
    touched.sort_by_key(|(addr, _)| *addr);

    for (addr, evm_account) in touched {
        let addr_str = address_to_sentrix(addr);

        // ── SELFDESTRUCT fast-path ───────────────────────────────────
        // Post-Cancun (EIP-6780), SELFDESTRUCT only destroys the contract
        // if invoked in the same tx that CREATED it. revm already enforces
        // that; when we see is_selfdestructed() == true it IS a legitimate
        // destroy. See founder-private/security/SECURITY_NOTES.md for
        // partial-support scope (balance zero + code clear; storage slots
        // linger but are unreachable).
        if evm_account.is_selfdestructed() {
            tracing::warn!(
                "evm/writeback: SELFDESTRUCT observed on {} — zeroing balance + code_hash. \
                 Beneficiary transfer is reflected in their own state diff.",
                addr_str
            );
            account_db.set_balance(&addr_str, 0);
            account_db.set_contract(&addr_str, EMPTY_CODE_HASH);
            continue;
        }

        // ── Balance ─────────────────────────────────────────────────
        // Rule: wei → sentri, round DOWN. See module-doc rule (1) for why
        // the v2.1.6 RPC-ingress `is_multiple_of(1e10)` cannot apply here.
        let balance_wei: U256 = evm_account.info.balance;
        let balance_sentri = (balance_wei / U256::from(10_000_000_000u64))
            .try_into()
            .unwrap_or(u64::MAX); // U256→u64 saturation — u64::MAX sentri is 184B SRX, far above any realistic balance
        account_db.set_balance(&addr_str, balance_sentri);

        // ── Nonce ──────────────────────────────────────────────────
        // Direct copy. revm returns the post-execution nonce; for the
        // sender this is `pre_execute_nonce + 1`, which equals the value
        // AccountDB already has (native Pass-1 already incremented, then
        // the block_executor shim subtracts 1 before passing to revm).
        // For the sender this is effectively a no-op; for CREATEd
        // contracts revm sets nonce=1 (EIP-161).
        account_db.set_nonce(&addr_str, evm_account.info.nonce);

        // ── Contract code (new deployments only) ──────────────────
        // revm populates `info.code = Some(bytecode)` on CREATE; None
        // for existing-contract calls (code was loaded from DB, not
        // re-materialised into the state diff). Only persist when
        // actually new. EIP-170 24KB cap is enforced at block_executor
        // level BEFORE this commit — oversized CREATE flips the tx to
        // failed and this writeback is skipped entirely.
        if let Some(bytecode) = &evm_account.info.code {
            let code_bytes = bytecode.original_byte_slice();
            if !code_bytes.is_empty() {
                let code_hash_bytes: [u8; 32] = evm_account.info.code_hash.into();
                let code_hash_hex = hex::encode(code_hash_bytes);
                account_db.store_contract_code(&code_hash_hex, code_bytes.to_vec());
                account_db.set_contract(&addr_str, code_hash_bytes);
            }
        }

        // ── Storage slots ──────────────────────────────────────────
        // Only persist slots where is_changed() == true (original_value
        // != present_value). Collect + sort by slot for deterministic
        // output ordering even though downstream reads are by-key.
        let mut changed_slots: Vec<(U256, U256)> = evm_account
            .storage
            .iter()
            .filter(|(_, slot)| slot.is_changed())
            .map(|(key, slot)| (*key, slot.present_value))
            .collect();
        changed_slots.sort_by_key(|(k, _)| *k);

        for (slot, value) in changed_slots {
            // 64-char lowercase hex, zero-padded. Left-pads with leading
            // zeros (big-endian high bits on the left, same convention as
            // Ethereum storage slot addressing).
            let slot_hex = format!("{slot:064x}");
            let value_bytes = value.to_be_bytes::<32>().to_vec();
            account_db.store_contract_storage(&addr_str, &slot_hex, value_bytes);
        }
    }

    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use revm::primitives::KECCAK_EMPTY;
    use revm::state::{Account, AccountInfo, AccountStatus, EvmStorageSlot};
    use std::collections::HashMap;

    fn addr(b: u8) -> Address {
        Address::from([b; 20])
    }

    fn touched_account(balance_wei: U256, nonce: u64) -> Account {
        let info = AccountInfo {
            balance: balance_wei,
            nonce,
            code_hash: KECCAK_EMPTY,
            account_id: None,
            code: None,
        };
        Account {
            original_info: Box::new(info.clone()),
            info,
            storage: HashMap::default(),
            transaction_id: 0,
            status: AccountStatus::Touched,
        }
    }

    #[test]
    fn test_commit_balance_rounds_down() {
        let mut state = EvmState::default();
        // 2.1001e13 wei = 21001 × 1e9 — legitimate gas×base_fee remainder
        // that must NOT be rejected. 2.1001e13 / 1e10 = 2100 (floor).
        state.insert(
            addr(0x01),
            touched_account(U256::from(21_001_000_000_000u128), 1),
        );
        let mut db = AccountDB::new();
        commit_state_to_account_db(&state, &mut db).unwrap();
        assert_eq!(db.get_balance("0x0101010101010101010101010101010101010101"), 2100);
    }

    #[test]
    fn test_commit_nonce_direct() {
        let mut state = EvmState::default();
        state.insert(addr(0x02), touched_account(U256::ZERO, 7));
        let mut db = AccountDB::new();
        commit_state_to_account_db(&state, &mut db).unwrap();
        assert_eq!(db.get_nonce("0x0202020202020202020202020202020202020202"), 7);
    }

    #[test]
    fn test_commit_single_storage_slot() {
        let mut state = EvmState::default();
        let mut account = touched_account(U256::from(10_000_000_000u64), 1);
        // slot 0 → 42
        account.storage.insert(
            U256::ZERO,
            EvmStorageSlot::new_changed(U256::ZERO, U256::from(42u64), 0),
        );
        state.insert(addr(0x03), account);

        let mut db = AccountDB::new();
        commit_state_to_account_db(&state, &mut db).unwrap();

        let addr_str = "0x0303030303030303030303030303030303030303";
        let slot_hex = format!("{:064x}", 0);
        let stored = db.get_contract_storage(addr_str, &slot_hex);
        assert!(stored.is_some(), "slot 0 must be persisted");
        // 42 big-endian in 32 bytes: 31 zeros then 0x2a.
        let bytes = stored.unwrap();
        assert_eq!(bytes.len(), 32);
        assert_eq!(bytes[31], 42);
        assert!(bytes[..31].iter().all(|b| *b == 0));
    }

    #[test]
    fn test_commit_new_contract_code_persists() {
        use revm::state::Bytecode;
        let mut state = EvmState::default();
        let runtime = vec![0x60, 0x00, 0x60, 0x00, 0xf3]; // dummy RETURN 0x00
        let bytecode = Bytecode::new_raw(alloy_primitives::Bytes::from(runtime.clone()));
        let code_hash = bytecode.hash_slow();

        let mut account = touched_account(U256::ZERO, 1);
        account.info.code = Some(bytecode);
        account.info.code_hash = code_hash;
        state.insert(addr(0x04), account);

        let mut db = AccountDB::new();
        commit_state_to_account_db(&state, &mut db).unwrap();

        let code_hash_bytes: [u8; 32] = code_hash.into();
        let code_hash_hex = hex::encode(code_hash_bytes);
        assert_eq!(
            db.get_contract_code(&code_hash_hex).map(|v| v.as_slice()),
            Some(runtime.as_slice()),
            "new contract bytecode must be persisted"
        );
        let addr_str = "0x0404040404040404040404040404040404040404";
        let acct = db.accounts.get(addr_str).expect("contract account must exist");
        assert_eq!(acct.code_hash, code_hash_bytes, "set_contract must mark account");
        assert!(acct.is_contract());
    }

    #[test]
    fn test_commit_selfdestruct_zeroes_account() {
        let mut state = EvmState::default();
        let mut account = touched_account(U256::from(1_000_000_000_000u64), 5);
        account.status |= AccountStatus::SelfDestructed;
        account.info.code_hash = alloy_primitives::B256::from([0xAA; 32]);
        state.insert(addr(0x05), account);

        let mut db = AccountDB::new();
        // Pre-populate so we can see the zero-out.
        db.credit("0x0505050505050505050505050505050505050505", 100).unwrap();
        db.set_contract("0x0505050505050505050505050505050505050505", [0xBB; 32]);

        commit_state_to_account_db(&state, &mut db).unwrap();

        let addr_str = "0x0505050505050505050505050505050505050505";
        assert_eq!(db.get_balance(addr_str), 0, "selfdestructed balance must be zeroed");
        let acct = db.accounts.get(addr_str).unwrap();
        assert_eq!(acct.code_hash, EMPTY_CODE_HASH, "selfdestructed code_hash must be EMPTY");
    }

    #[test]
    fn test_commit_skips_untouched() {
        let mut state = EvmState::default();
        // Un-touched account should NOT influence AccountDB.
        let info = AccountInfo {
            balance: U256::from(999_999u128),
            nonce: 42,
            code_hash: KECCAK_EMPTY,
            account_id: None,
            code: None,
        };
        let account = Account {
            original_info: Box::new(info.clone()),
            info,
            storage: HashMap::default(),
            transaction_id: 0,
            status: AccountStatus::default(), // NOT touched
        };
        state.insert(addr(0x06), account);

        let mut db = AccountDB::new();
        commit_state_to_account_db(&state, &mut db).unwrap();

        // Account should not even exist in AccountDB since we never touched it.
        assert!(!db.accounts.contains_key("0x0606060606060606060606060606060606060606"));
    }

    #[test]
    fn test_commit_deterministic_across_runs() {
        // Two runs over the same logical state must produce bit-identical
        // AccountDB contents, regardless of HashMap iteration order.
        let mut state_a = EvmState::default();
        let mut state_b = EvmState::default();

        for i in 0u8..5 {
            let mut acc_a = touched_account(U256::from((i as u64 + 1) * 10_000_000_000u64), i as u64);
            let mut acc_b = touched_account(U256::from((i as u64 + 1) * 10_000_000_000u64), i as u64);
            acc_a.storage.insert(
                U256::ZERO,
                EvmStorageSlot::new_changed(U256::ZERO, U256::from(i as u64 + 100), 0),
            );
            acc_b.storage.insert(
                U256::ZERO,
                EvmStorageSlot::new_changed(U256::ZERO, U256::from(i as u64 + 100), 0),
            );
            state_a.insert(addr(i + 1), acc_a);
            state_b.insert(addr(i + 1), acc_b);
        }

        let mut db_a = AccountDB::new();
        let mut db_b = AccountDB::new();
        commit_state_to_account_db(&state_a, &mut db_a).unwrap();
        commit_state_to_account_db(&state_b, &mut db_b).unwrap();

        // Same 5 addresses committed either order → AccountDB contents match.
        for i in 0u8..5 {
            let addr_str = format!("0x{}", hex::encode([i + 1; 20]));
            assert_eq!(db_a.get_balance(&addr_str), db_b.get_balance(&addr_str));
            assert_eq!(db_a.get_nonce(&addr_str), db_b.get_nonce(&addr_str));
            let slot_hex = format!("{:064x}", 0);
            assert_eq!(
                db_a.get_contract_storage(&addr_str, &slot_hex),
                db_b.get_contract_storage(&addr_str, &slot_hex)
            );
        }
    }
}
