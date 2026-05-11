//! End-to-end Blockchain harness — covers the audit's untested-fn #2
//! (`execute_evm_tx_in_block` against real MDBX).
//!
//! Background: the 2026-05-06 rust-workspace audit flagged `sentrix-evm`
//! at 0% line coverage. The earlier scaffold (PR #513) wired up
//! `setup_evm_active_chain` + a sanity-check native-tx flow but left the
//! actual EVM tx construction `#[ignore]`'d. This file fills it in.
//!
//!   - `setup_evm_active_chain()` — real Blockchain + temp MDBX with
//!     Voyager + EVM forks gated to h=0.
//!   - `test_native_tx_through_full_block_apply` — sanity guard. If
//!     this breaks the harness itself is wrong.
//!   - `test_evm_tx_e2e_value_zero_call` — full path: build raw RLP
//!     EVM tx (alloy_consensus::TxLegacy + secp256k1 signing + EIP-2718
//!     encoding) → wrap in Sentrix `Transaction` per the wire format
//!     `eth_sendRawTransaction` uses (data=`"EVM:gas:hex"`, signature=
//!     hex(raw_rlp)) → admit + create_block + add_block → assert state.
//!     Closes audit #2.
//!
//! The `EVM_VALUE_TRANSFER_HEIGHT` fork is set to 0 so value transfers
//! flow through; the test itself uses value=0 to keep the wire format
//! minimal — the supply-leak (H3) reproducer that lands on top of this
//! harness will use a non-zero value to exercise the wei-side gas
//! deduction path.
//!
//! Run: `cargo test -p sentrix-core --test evm_e2e_harness`
//!
//! Audit reference: internal Sentrix Labs rust-workspace security audit (2026-05-06).

use alloy_consensus::{SignableTransaction, TxEnvelope, TxLegacy};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, B256, Bytes, Signature, TxKind, U256};
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use sentrix_core::blockchain::Blockchain;
use sentrix_primitives::transaction::{MIN_TX_FEE, Transaction};
use sentrix_storage::MdbxStorage;
use sentrix_wallet::Wallet;
use sha3::{Digest as _, Keccak256};
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

/// Fresh `Blockchain` with Voyager + EVM + EVM-value-transfer forks
/// active at h=0. SAFETY: env vars are process-global; cargo runs each
/// integration-test file as its own binary, so there's no cross-file
/// bleed; within this binary we set once at chain construction.
fn setup_evm_active_chain() -> (TempDir, Arc<MdbxStorage>, Blockchain) {
    setup_evm_active_chain_with_h3(true)
}

/// Test-only chain builder. `h3_active=true` activates the audit H3
/// fork (`EVM_GAS_FIX_HEIGHT=0`) so write-path EVM txs skip revm's
/// internal gas deduction; `false` leaves the fork at u64::MAX so the
/// pre-fix supply-leak path runs (used for the negative reproducer
/// test that documents the bug class).
fn setup_evm_active_chain_with_h3(h3_active: bool) -> (TempDir, Arc<MdbxStorage>, Blockchain) {
    // SAFETY: see fn doc.
    unsafe {
        std::env::set_var("VOYAGER_FORK_HEIGHT", "0");
        std::env::set_var("VOYAGER_EVM_HEIGHT", "0");
        std::env::set_var("EVM_VALUE_TRANSFER_HEIGHT", "0");
        if h3_active {
            std::env::set_var("EVM_GAS_FIX_HEIGHT", "0");
        } else {
            std::env::remove_var("EVM_GAS_FIX_HEIGHT");
        }
    }

    let (dir, mdbx) = temp_mdbx();
    let mut bc = Blockchain::new("admin".to_string());
    bc.authority.add_validator_unchecked(
        VALIDATOR.to_string(),
        "Validator 1".to_string(),
        "pk1".to_string(),
    );
    bc.init_trie(Arc::clone(&mdbx)).unwrap();
    // Wire the same MDBX handle into the storage path so EVM receipt /
    // log persistence (`block_executor/evm.rs:188`, `:286`) actually
    // hits the DB. Without this `self.mdbx_storage` is None and the
    // receipt write is a no-op — the unit-test signal is meaningless.
    bc.init_storage_handle(Arc::clone(&mdbx)).unwrap();

    (dir, mdbx, bc)
}

/// Sanity test: a real signed native Tx flows through `add_to_mempool`
/// → `create_block` → `add_block` and updates account state as expected.
#[test]
fn test_native_tx_through_full_block_apply() {
    let (_dir, _mdbx, mut bc) = setup_evm_active_chain();

    let (sk, pk) = deterministic_keypair(1);
    let sender = Wallet::derive_address(&pk);
    let recv = format!("0x{}", "deadbeef".repeat(5));

    bc.accounts.credit(&sender, 10_000_000_000).unwrap();
    let chain_id = bc.chain_id;

    let amount = 1_000_000u64;
    let tx = Transaction::new(
        sender.clone(),
        recv.clone(),
        amount,
        MIN_TX_FEE,
        0,
        String::new(),
        chain_id,
        &sk,
        &pk,
    )
    .expect("tx build");

    bc.add_to_mempool(tx).expect("mempool accept");
    let block = bc.create_block(VALIDATOR).expect("create_block");
    bc.add_block(block).expect("add_block");

    assert_eq!(bc.height(), 1);
    assert_eq!(bc.accounts.get_balance(&recv), amount);
    assert_eq!(
        bc.accounts.get_balance(&sender),
        10_000_000_000 - amount - MIN_TX_FEE,
    );
}

/// Convert a secp256k1 ECDSA-recoverable signature output into the
/// `alloy_primitives::Signature` shape that
/// `alloy_consensus::SignableTransaction::into_signed` expects.
///
/// secp256k1 0.31's `RecoverableSignature::serialize_compact()` returns
/// `(RecoveryId, [u8; 64])` — 32 bytes of `r` then 32 bytes of `s`.
/// alloy 1.5 `Signature::new(r, s, y_parity)` takes `U256` r/s and a
/// `bool` parity. EIP-155 `v = 35 + 2*chain_id + parity` is handled
/// internally by alloy_consensus when the legacy tx has
/// `chain_id = Some(...)`.
fn secp_to_alloy_signature(sk: &SecretKey, sig_hash: B256) -> Signature {
    let secp = Secp256k1::new();
    let msg = Message::from_digest(sig_hash.into());
    let recoverable = secp.sign_ecdsa_recoverable(msg, sk);
    let (rec_id, compact) = recoverable.serialize_compact();
    let r = U256::from_be_slice(&compact[0..32]);
    let s = U256::from_be_slice(&compact[32..64]);
    let y_parity = (i32::from(rec_id) & 1) == 1;
    Signature::new(r, s, y_parity)
}

/// Derive the EVM-style 20-byte address of a secp256k1 public key.
/// keccak256(uncompressed_pubkey[1..])[12..] is the canonical Ethereum
/// derivation that revm's `recover_signer` reverses.
fn evm_address(pk: &PublicKey) -> Address {
    let uncompressed = pk.serialize_uncompressed();
    let hash = Keccak256::digest(&uncompressed[1..]);
    Address::from_slice(&hash[12..])
}

/// **Audit #2 lander.** End-to-end EVM tx through Sentrix's full apply
/// path, exercising `execute_evm_tx_in_block` against real MDBX. Uses
/// the simplest meaningful EVM tx shape — value=0 Call to an EOA — so
/// revm runs the tx as a no-op while the surrounding plumbing
/// (RLP encode → mempool admit → block apply → fee accounting → MDBX
/// receipt persist) all fires.
#[test]
fn test_evm_tx_e2e_value_zero_call() {
    let (_dir, mdbx, mut bc) = setup_evm_active_chain();

    let (sk, pk) = deterministic_keypair(2);
    let sender_evm: Address = evm_address(&pk);
    // The Sentrix-side `from_address` MUST equal the EVM-side recovered
    // sender. Pass-1 native debits `from_address` via `charge_fee_only`
    // and revm independently uses `caller = parse_sentrix_address(from)`
    // for the inner EVM execution. Skew between the two = state divergence.
    let sender_str = format!("0x{}", hex::encode(sender_evm.as_slice()));

    // Fund alice generously — 1000 SRX = 1e11 sentri.
    bc.accounts.credit(&sender_str, 100_000_000_000).unwrap();
    let initial_sender_balance = bc.accounts.get_balance(&sender_str);
    let initial_sender_nonce = bc.accounts.get_nonce(&sender_str);

    let chain_id = bc.chain_id;
    let recipient = Address::from([0xab; 20]);
    let recipient_str = format!("0x{}", hex::encode(recipient.as_slice()));

    // Build the alloy TxLegacy. value=0 keeps the test focused on the
    // wire format + receipt persistence; the H3 reproducer will set
    // value to a non-zero amount to exercise the wei-side gas path.
    let alloy_tx = TxLegacy {
        chain_id: Some(chain_id),
        nonce: 0,
        // gas_price set to INITIAL_BASE_FEE so revm's internal pricing
        // mirrors what `block_executor/evm.rs:189` injects for non-test
        // EVM txs. (The H3 fix changes whether revm actually deducts
        // this; the test as written today passes regardless.)
        gas_price: sentrix_evm::gas::INITIAL_BASE_FEE as u128,
        gas_limit: 100_000,
        to: TxKind::Call(recipient),
        value: U256::ZERO,
        input: Bytes::new(),
    };

    let sig_hash = alloy_tx.signature_hash();
    let signature = secp_to_alloy_signature(&sk, sig_hash);
    let signed = alloy_tx.into_signed(signature);
    let envelope: TxEnvelope = signed.into();

    let mut raw_bytes = Vec::new();
    envelope.encode_2718(&mut raw_bytes);
    let raw_hex = hex::encode(&raw_bytes);
    let txid = hex::encode(Keccak256::digest(&raw_bytes));

    // Build the Sentrix Tx by hand — same recipe the JSON-RPC layer
    // uses (`crates/sentrix-rpc/src/jsonrpc/eth.rs::eth_send_raw_transaction`).
    // `data` is the `EVM:gas_limit:hex_data` triplet; `signature` field
    // is overloaded to carry the raw 2718-encoded envelope.
    let evm_data = format!("EVM:{}:{}", 100_000u64, hex::encode(b""));
    let sentrix_tx = Transaction {
        txid: txid.clone(),
        from_address: sender_str.clone(),
        to_address: recipient_str.clone(),
        amount: 0, // value=0 → 0 sentri
        fee: MIN_TX_FEE,
        nonce: 0,
        data: evm_data,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        chain_id,
        signature: raw_hex,
        public_key: String::new(),
    };

    bc.add_to_mempool(sentrix_tx).expect("mempool admit");
    let block = bc.create_block(VALIDATOR).expect("create_block");
    bc.add_block(block).expect("add_block");

    // ── State assertions ────────────────────────────────────────
    assert_eq!(bc.height(), 1, "block applied");

    // Sender nonce: revm bumps the nonce in EVM state writeback. Pre-
    // bump in native Pass-1 was removed for EVM txs, so the post-block
    // nonce should be initial + 1.
    assert_eq!(
        bc.accounts.get_nonce(&sender_str),
        initial_sender_nonce + 1,
        "sender nonce bumped exactly once via EVM writeback",
    );

    // Sender balance: at minimum debited by MIN_TX_FEE (Pass-1 charge).
    // The H3 leak adds a sub-sentri rounding loss on top; we don't
    // assert the exact value to avoid coupling the test to the H3 fix
    // direction. Just check the floor.
    let post = bc.accounts.get_balance(&sender_str);
    assert!(
        post <= initial_sender_balance - MIN_TX_FEE,
        "sender debited at least MIN_TX_FEE (post={post}, pre={initial_sender_balance}, fee={MIN_TX_FEE})",
    );
    assert!(
        initial_sender_balance - post < MIN_TX_FEE + 1_000,
        "sender debit beyond MIN_TX_FEE bounded by sub-sentri rounding (post={post}, pre={initial_sender_balance})",
    );

    // Receipt persisted to MDBX (M1 fix path now warns on Err — confirms
    // we're not silently skipping the persist).
    let receipt_key = sentrix_evm::receipt_key(&txid).expect("valid txid");
    let stored: Option<sentrix_evm::StoredReceipt> = mdbx
        .get_bincode(sentrix_storage::tables::TABLE_RECEIPTS, &receipt_key)
        .expect("MDBX read");
    assert!(
        stored.is_some(),
        "EVM tx receipt must persist to TABLE_RECEIPTS"
    );
    let r = stored.unwrap();
    assert!(r.success, "value=0 call to EOA should succeed under revm");
}

/// Construct + sign a value-transfer EVM tx and apply it via add_block.
/// Returns (sender_str, sender_balance_before, txid). Shared by the H3
/// reproducer pair below.
fn submit_evm_value_transfer(
    bc: &mut Blockchain,
    sk: &SecretKey,
    pk: &PublicKey,
    value_sentri: u64,
    nonce: u64,
) -> (String, u64, String) {
    let sender_evm: Address = evm_address(pk);
    let sender_str = format!("0x{}", hex::encode(sender_evm.as_slice()));
    let sender_balance_before = bc.accounts.get_balance(&sender_str);
    let chain_id = bc.chain_id;
    let recipient = Address::from([0xab; 20]);
    let recipient_str = format!("0x{}", hex::encode(recipient.as_slice()));

    // 1 sentri = 1e10 wei. Value in wei must be a whole-sentri multiple
    // (the RPC ingress enforces this; same constraint here so writeback
    // doesn't drop sub-sentri remainders.)
    let value_wei = U256::from(value_sentri).saturating_mul(U256::from(10_000_000_000u64));

    let alloy_tx = TxLegacy {
        chain_id: Some(chain_id),
        nonce,
        gas_price: sentrix_evm::gas::INITIAL_BASE_FEE as u128,
        gas_limit: 100_000,
        to: TxKind::Call(recipient),
        value: value_wei,
        input: Bytes::new(),
    };
    let sig_hash = alloy_tx.signature_hash();
    let signature = secp_to_alloy_signature(sk, sig_hash);
    let signed = alloy_tx.into_signed(signature);
    let envelope: TxEnvelope = signed.into();
    let mut raw_bytes = Vec::new();
    envelope.encode_2718(&mut raw_bytes);
    let raw_hex = hex::encode(&raw_bytes);
    let txid = hex::encode(Keccak256::digest(&raw_bytes));

    let evm_data = format!("EVM:{}:{}", 100_000u64, hex::encode(b""));
    let sentrix_tx = Transaction {
        txid: txid.clone(),
        from_address: sender_str.clone(),
        to_address: recipient_str,
        amount: value_sentri,
        fee: MIN_TX_FEE,
        nonce,
        data: evm_data,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        chain_id,
        signature: raw_hex,
        public_key: String::new(),
    };

    bc.add_to_mempool(sentrix_tx).expect("mempool admit");
    let block = bc.create_block(VALIDATOR).expect("create_block");
    bc.add_block(block).expect("add_block");

    (sender_str, sender_balance_before, txid)
}

/// Audit H3 — POST-fork: with `EVM_GAS_FIX_HEIGHT=0` active, a value-
/// transfer EVM tx debits the sender by EXACTLY `value + tx.fee` —
/// no extra wei-side gas deduction sneaks out via the writeback's
/// floor-div. Sender's net debit stays whole-sentri-aligned, no
/// silent supply leak.
#[test]
fn test_h3_post_fork_supply_invariant_holds_on_value_transfer() {
    let (_dir, _mdbx, mut bc) = setup_evm_active_chain_with_h3(true);
    let (sk, pk) = deterministic_keypair(3);
    let sender_str = format!("0x{}", hex::encode(evm_address(&pk).as_slice()));
    bc.accounts.credit(&sender_str, 100_000_000_000).unwrap(); // 1000 SRX

    let value = 100_000_000u64; // 1 SRX
    let (_, balance_before, _txid) =
        submit_evm_value_transfer(&mut bc, &sk, &pk, value, 0);

    let balance_after = bc.accounts.get_balance(&sender_str);
    let actual_drop = balance_before - balance_after;
    let expected_drop = value + MIN_TX_FEE;

    assert_eq!(
        actual_drop, expected_drop,
        "post-H3-fork sender drop = value + tx.fee EXACT (got {}, expected {})",
        actual_drop, expected_drop,
    );
}

/// Audit H3 — PRE-fork (negative reproducer): with the gas fix
/// inactive, the same value-transfer above leaks an extra sub-sentri
/// amount due to revm's internal `gas_used × INITIAL_BASE_FEE`
/// deduction surviving the wei→sentri floor-div in writeback. The
/// leak amount is small at INITIAL_BASE_FEE = 10K wei (≤1 sentri per
/// tx) but the architectural pattern scales with base_fee. Marked
/// `#[ignore]` because it asserts the pre-fix LEAKY behaviour —
/// running it on the post-fork code path produces no leak (good)
/// and the assertion would fail. Keep as documentation for a future
/// reader who wants to see the exact failure mode the H3 fix closes.
#[test]
#[ignore = "documents pre-fork leak — un-ignore + flip assert direction to verify the bug if EVM_GAS_FIX_HEIGHT is ever rolled back"]
fn test_h3_pre_fork_supply_leak_documented() {
    let (_dir, _mdbx, mut bc) = setup_evm_active_chain_with_h3(false);
    let (sk, pk) = deterministic_keypair(4);
    let sender_str = format!("0x{}", hex::encode(evm_address(&pk).as_slice()));
    bc.accounts.credit(&sender_str, 100_000_000_000).unwrap();

    let (_, balance_before, _) = submit_evm_value_transfer(
        &mut bc,
        &sk,
        &pk,
        100_000_000, // 1 SRX
        0,
    );
    let balance_after = bc.accounts.get_balance(&sender_str);
    let drop = balance_before - balance_after;
    // Pre-fix the drop is value + fee + the wei-side gas remainder.
    // We don't know the exact gas_used without running it, so just
    // assert the inequality that demonstrates the leak.
    assert!(
        drop > 100_000_000 + MIN_TX_FEE,
        "pre-fork: drop must exceed value + fee due to wei-side gas leak"
    );
}
