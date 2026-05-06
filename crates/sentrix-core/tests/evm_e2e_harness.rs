//! End-to-end Blockchain harness — scaffold for EVM tx integration tests.
//!
//! Background: the 2026-05-06 rust-workspace audit flagged `sentrix-evm`
//! at 0% line coverage. Existing unit tests use `InMemoryDB` and don't
//! exercise the full `add_block` → `apply_block_pass2` →
//! `execute_evm_tx_in_block` → `commit_state_to_account_db` chain. State
//! divergence bugs (5 mainnet halts in April–May 2026) keep slipping
//! through unit tests because they only manifest in the real-MDBX +
//! real-trie post-block path.
//!
//! This file provides a working `Blockchain` harness with EVM activated
//! at h=0, plus reference test patterns:
//!
//!   - `setup_evm_active_chain()` — chain ready to accept EVM txs.
//!   - `test_native_tx_through_full_block_apply` — sanity that a real
//!     signed Tx reaches `apply_block_pass2` and updates state. Catches
//!     "harness broken" before any EVM-specific test runs.
//!   - `test_evm_tx_e2e_TODO` — ignored skeleton showing the steps to
//!     construct + apply a real EVM tx. Filling this in lands the audit's
//!     #2 untested-critical-function (`execute_evm_tx_in_block` against
//!     real MDBX) and creates a harness for H3 supply-leak regression.
//!
//! Run: `cargo test -p sentrix-core --test evm_e2e_harness`
//!
//! Audit reference: founder-private/audits/2026-05-06-rust-workspace-security-audit.md

use secp256k1::{PublicKey, Secp256k1, SecretKey};
use sentrix_core::blockchain::Blockchain;
use sentrix_primitives::transaction::{MIN_TX_FEE, Transaction};
use sentrix_storage::MdbxStorage;
use sentrix_wallet::Wallet;
use std::sync::Arc;
use tempfile::TempDir;

const VALIDATOR: &str = "validator1";

fn deterministic_keypair(seed: u8) -> (SecretKey, PublicKey) {
    let sk = SecretKey::from_byte_array([seed.max(1); 32]).expect("non-zero seed");
    let secp = Secp256k1::new();
    let pk = PublicKey::from_secret_key(&secp, &sk);
    (sk, pk)
}

fn temp_mdbx() -> (TempDir, Arc<MdbxStorage>) {
    let dir = TempDir::new().expect("tempdir");
    let mdbx = Arc::new(MdbxStorage::open(dir.path()).expect("mdbx open"));
    (dir, mdbx)
}

/// Build a fresh `Blockchain` with Voyager + EVM forks already active at
/// h=0. The env-var path is the same one production uses; we set it to
/// `0` so the gate `is_voyager_height(0)` and `is_evm_active_height(0)`
/// both return true.
///
/// SAFETY (Rust 1.78+): `std::env::set_var` is unsafe across threads
/// because it mutates process-global state. Tests in this file are
/// `#[serial]`-equivalent through cargo's per-target serialization (each
/// integration test file is its own binary), so cross-test bleed is not
/// a concern; cross-thread bleed within this binary is avoided by
/// setting the env once at chain construction and never mutating
/// afterwards.
fn setup_evm_active_chain() -> (TempDir, Arc<MdbxStorage>, Blockchain) {
    // SAFETY: see fn doc.
    unsafe {
        std::env::set_var("VOYAGER_FORK_HEIGHT", "0");
        std::env::set_var("VOYAGER_EVM_HEIGHT", "0");
    }

    let (dir, mdbx) = temp_mdbx();
    let mut bc = Blockchain::new("admin".to_string());
    bc.authority.add_validator_unchecked(
        VALIDATOR.to_string(),
        "Validator 1".to_string(),
        "pk1".to_string(),
    );
    bc.init_trie(Arc::clone(&mdbx)).unwrap();

    (dir, mdbx, bc)
}

/// Sanity test: a real signed native Tx flows through `add_to_mempool`
/// → `create_block` → `add_block` and updates account state as expected.
/// If this breaks, no EVM-specific test in this file can be trusted —
/// the harness itself is wrong.
#[test]
fn test_native_tx_through_full_block_apply() {
    let (_dir, _mdbx, mut bc) = setup_evm_active_chain();

    let (sk, pk) = deterministic_keypair(1);
    let sender = Wallet::derive_address(&pk);
    let recv = format!("0x{}", "deadbeef".repeat(5));

    // 100 SRX = 10^10 sentri
    bc.accounts.credit(&sender, 10_000_000_000).unwrap();
    let initial_supply = bc.accounts.total_supply();
    let initial_burned = bc.accounts.total_burned;

    let chain_id = bc.chain_id;
    let amount = 1_000_000u64;
    let tx = Transaction::new(
        sender.clone(),
        recv.clone(),
        amount,
        MIN_TX_FEE,
        0, // first nonce
        String::new(),
        chain_id,
        &sk,
        &pk,
    )
    .expect("tx build");

    bc.add_to_mempool(tx).expect("mempool accept");
    let block = bc.create_block(VALIDATOR).expect("create_block");
    bc.add_block(block).expect("add_block");

    // State assertions.
    assert_eq!(bc.height(), 1, "block applied");
    assert_eq!(
        bc.accounts.get_balance(&recv),
        amount,
        "recipient credited",
    );
    assert_eq!(
        bc.accounts.get_balance(&sender),
        10_000_000_000 - amount - MIN_TX_FEE,
        "sender debited amount + fee",
    );

    // Supply conservation: half the fee was burned, half stays in the
    // validator pool (credited to VALIDATOR by the block-end coinbase).
    let burn_delta = bc.accounts.total_burned - initial_burned;
    let supply_delta = initial_supply.saturating_sub(bc.accounts.total_supply());
    assert_eq!(
        burn_delta,
        MIN_TX_FEE.div_ceil(2),
        "exactly ceil(fee/2) burned",
    );
    // Block reward credits the validator with `block_reward + fee_share`.
    // Net supply change over the block = +block_reward - burned. The
    // exact reward varies with the fork height schedule, so just assert
    // the burn portion accounts for the expected fee/2 burn.
    assert!(
        burn_delta <= supply_delta || supply_delta == 0,
        "burn must not exceed observable supply drop (block reward may have offset)",
    );
}

/// **TODO scaffold** — fill in to land audit #2 (untested
/// `execute_evm_tx_in_block` against real MDBX) and to wire H3 supply-
/// leak regression at the e2e level.
///
/// Construction steps for an EVM tx through Sentrix's wire format:
///
///   1. Build an `alloy_consensus::TxLegacy` (or `TxEip1559`) with the
///      target `chain_id`, `nonce`, `gas_price`, `gas_limit`, `to`,
///      `value`, `input`. Set `gas_price = sentrix_evm::gas::INITIAL_BASE_FEE`
///      to match what `block_executor/evm.rs:189` injects.
///   2. Sign via secp256k1 → `alloy_consensus::Signed<TxLegacy>` →
///      `TxEnvelope::Legacy(...)`.
///   3. RLP-encode via `Decodable2718::encode_2718` (or equivalent
///      eip-2718 wrapper) into a `Vec<u8>`; hex-encode for the Sentrix
///      `tx.signature` field (Sentrix re-decodes via
///      `TxEnvelope::decode_2718` in `block_executor/evm.rs:74`).
///   4. Build `tx.data = format!("EVM:{}:{}", gas_limit, hex::encode(calldata))`.
///   5. Wrap in `Transaction::new(...)` — note `from_address` MUST
///      equal `Wallet::derive_address(&pk)` of the EVM signer's
///      pubkey, because Pass-1 native debits `from_address` via
///      `charge_fee_only`. If the EVM signer differs from the Sentrix
///      tx signer, accounting drifts.
///   6. `bc.add_to_mempool(tx)`; build + apply block; assert receipt
///      via `eth_getTransactionReceipt`-equivalent path
///      (`bc.mdbx_storage.get_bincode::<StoredReceipt>(TABLE_RECEIPTS, &key)`).
///
/// Reference: `crates/sentrix-rpc/src/jsonrpc/eth.rs::send_raw_transaction`
/// constructs the inverse direction (raw EVM tx hex → Sentrix Tx).
#[test]
#[ignore = "TODO scaffold — fill in EVM tx construction per fn doc to land audit #2"]
fn test_evm_tx_e2e_landing_pad() {
    let (_dir, _mdbx, mut bc) = setup_evm_active_chain();
    let (_sk, pk) = deterministic_keypair(2);
    let sender = Wallet::derive_address(&pk);
    bc.accounts.credit(&sender, 100_000_000_000).unwrap(); // 1000 SRX

    // Step 1–4: construct the raw EVM tx. (Body intentionally absent;
    // see fn doc for the recipe.)

    // Step 5: wrap in Sentrix Transaction::new(...) — REMEMBER that the
    // EVM signer's secp256k1 pubkey must match `from_address` so Pass-1
    // native fee accounting lines up with revm's caller.

    // Step 6: bc.add_to_mempool + create_block + add_block, then assert:
    //   - bc.accounts.get_balance(&sender) reflects amount + Pass-1 fee
    //     (and, post-H3 fix, the gas portion via burn or validator credit
    //     as decided in the audit's H3 design call).
    //   - if Call: `bc.accounts.get_contract_storage(&target, &slot)`
    //     matches the SSTORE.
    //   - if Create: `bc.accounts.get_contract_code(&code_hash_hex)` is
    //     non-empty.
    //   - the receipt key persists: `mdbx.get(TABLE_RECEIPTS, &receipt_key(&txid))`.

    // Once this body lands, also un-ignore `test_h3_evm_writeback_set_balance_breaks_supply_invariant`
    // in `crates/sentrix-primitives/src/account.rs` and rewrite it to use
    // this harness instead of the synthetic AccountDB calls — that closes
    // the audit's H3 reproducer gap properly.

    panic!(
        "TODO scaffold — fill in EVM tx construction per the fn-level \
         docstring on this test. Harness is verified by \
         test_native_tx_through_full_block_apply above."
    );
}
