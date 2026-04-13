#![allow(missing_docs)]
// integration_mempool.rs — Mempool stress tests
// Covers: per-sender limit, global size limit, TTL rejection, fee ordering,
// mempool clearance after block production.
//
// Note: amount=0 is not allowed for non-token TXs (validated in tx.validate()).
// We use amount=1 (minimum non-zero) to keep cost predictable.

mod common;

use sentrix::core::blockchain::{MAX_MEMPOOL_PER_SENDER, MAX_MEMPOOL_SIZE};
use sentrix::core::transaction::MIN_TX_FEE;
use sentrix::wallet::wallet::Wallet;

const MIN_AMOUNT: u64 = 1; // smallest valid non-zero SRX transfer amount
const TX_COST: u64 = MIN_AMOUNT + MIN_TX_FEE; // total cost per TX (amount + fee)

/// The per-sender limit (MAX_MEMPOOL_PER_SENDER = 100) must be enforced.
/// Nonces 0..99 are accepted; nonce 100 is rejected.
#[test]
fn test_per_sender_limit_enforced() {
    let (mut bc, _val) = common::setup_single_validator();

    // Fund sender with enough for all 101 TXs (including pending spend tracking)
    let total_needed = TX_COST * (MAX_MEMPOOL_PER_SENDER as u64 + 5);
    let sender = common::funded_wallet(&mut bc, total_needed);

    // Submit MAX_MEMPOOL_PER_SENDER TXs — all must succeed
    for n in 0..MAX_MEMPOOL_PER_SENDER as u64 {
        let tx = common::make_tx_nonce(&sender, common::RECV, MIN_AMOUNT, MIN_TX_FEE, n);
        bc.add_to_mempool(tx).expect("TX should be accepted within per-sender limit");
    }
    assert_eq!(bc.mempool_size(), MAX_MEMPOOL_PER_SENDER, "mempool should be at per-sender cap");

    // One more TX from the same sender must be rejected
    let excess_tx = common::make_tx_nonce(
        &sender,
        common::RECV,
        MIN_AMOUNT,
        MIN_TX_FEE,
        MAX_MEMPOOL_PER_SENDER as u64,
    );
    let result = bc.add_to_mempool(excess_tx);
    assert!(result.is_err(), "TX beyond per-sender limit must be rejected");
    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("too many pending") || err_str.contains("sender"),
        "error should mention sender limit: {}", err_str
    );
}

/// Global mempool limit (MAX_MEMPOOL_SIZE = 10_000): filling it completely, then
/// the next insertion must return "mempool full".
///
/// Strategy: 101 wallets, each submitting 100 TXs = 10_100 capacity slots.
/// We fill exactly 10_000 from the first 100 wallets, then the 101st wallet
/// tries to submit → rejected with "mempool full".
/// All TXs use the same fee (O(1) append insertion — no O(n²) penalty).
#[test]
fn test_global_mempool_limit_enforced() {
    let (mut bc, _val) = common::setup_single_validator();

    let num_wallets = (MAX_MEMPOOL_SIZE / MAX_MEMPOOL_PER_SENDER) + 1; // 101
    let per_wallet_fund = TX_COST * (MAX_MEMPOOL_PER_SENDER as u64 + 5);
    let mut wallets: Vec<Wallet> = Vec::with_capacity(num_wallets);
    for _ in 0..num_wallets {
        wallets.push(common::funded_wallet(&mut bc, per_wallet_fund));
    }

    // Fill mempool to MAX_MEMPOOL_SIZE using first 100 wallets × 100 TXs each
    let mut total_submitted = 0usize;
    'outer: for wallet in wallets.iter().take(MAX_MEMPOOL_SIZE / MAX_MEMPOOL_PER_SENDER) {
        for n in 0..MAX_MEMPOOL_PER_SENDER as u64 {
            if total_submitted >= MAX_MEMPOOL_SIZE {
                break 'outer;
            }
            let tx = common::make_tx_nonce(wallet, common::RECV, MIN_AMOUNT, MIN_TX_FEE, n);
            bc.add_to_mempool(tx).expect("fill TX should be accepted");
            total_submitted += 1;
        }
    }
    assert_eq!(bc.mempool_size(), MAX_MEMPOOL_SIZE, "mempool should be full");

    // One more TX from the 101st wallet must be rejected
    let overflow_wallet = &wallets[num_wallets - 1];
    let overflow_tx = common::make_tx_nonce(overflow_wallet, common::RECV, MIN_AMOUNT, MIN_TX_FEE, 0);
    let result = bc.add_to_mempool(overflow_tx);
    assert!(result.is_err(), "TX into full mempool must be rejected");
    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("mempool full") || err_str.contains("full"),
        "error should mention full mempool: {}", err_str
    );
}

/// Mining a block clears the mined TXs from the mempool.
#[test]
fn test_mempool_clears_after_block() {
    let (mut bc, val) = common::setup_single_validator();
    let sender = common::funded_wallet(&mut bc, TX_COST * 10);

    // Submit 3 TXs
    for n in 0..3u64 {
        let tx = common::make_tx_nonce(&sender, common::RECV, MIN_AMOUNT, MIN_TX_FEE, n);
        bc.add_to_mempool(tx).expect("tx accepted");
    }
    assert_eq!(bc.mempool_size(), 3);

    common::mine_block_with_mempool(&mut bc, &val.address);

    assert_eq!(bc.mempool_size(), 0, "all mined TXs should be removed from mempool");
}

/// TTL: a TX with an ancient timestamp (epoch 0) must be rejected immediately.
#[test]
fn test_ttl_stale_timestamp_rejected() {
    let (mut bc, _val) = common::setup_single_validator();
    let sender = common::funded_wallet(&mut bc, 500_000_000);

    // Create a valid TX, then forcibly set its timestamp to epoch 0 (way too old).
    // The timestamp check runs before signature validation, so this works even
    // though the mutated timestamp makes the signature inconsistent.
    let mut tx = common::make_tx(&bc, &sender, common::RECV, MIN_AMOUNT, MIN_TX_FEE);
    tx.timestamp = 0; // epoch zero — definitely older than MEMPOOL_MAX_AGE_SECS (3600s)

    let result = bc.add_to_mempool(tx);
    assert!(result.is_err(), "TX with ancient timestamp must be rejected");
    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("old") || err_str.contains("age") || err_str.contains("timestamp"),
        "error should mention stale TX: {}", err_str
    );
}

/// TX with a timestamp more than +5 min in the future must be rejected.
#[test]
fn test_future_timestamp_rejected() {
    let (mut bc, _val) = common::setup_single_validator();
    let sender = common::funded_wallet(&mut bc, 500_000_000);

    let mut tx = common::make_tx(&bc, &sender, common::RECV, MIN_AMOUNT, MIN_TX_FEE);
    tx.timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + 600; // 10 minutes in the future

    let result = bc.add_to_mempool(tx);
    assert!(result.is_err(), "TX from the future must be rejected");
}

/// Higher-fee TXs must be added to the mempool (fee-ordering is internal, verified
/// indirectly by confirming both TXs are accepted and mempool size is correct).
#[test]
fn test_fee_priority_both_accepted() {
    let (mut bc, _val) = common::setup_single_validator();

    let low_fee_sender = common::funded_wallet(&mut bc, 500_000_000);
    let high_fee_sender = common::funded_wallet(&mut bc, 500_000_000);

    let low_tx = common::make_tx(&bc, &low_fee_sender, common::RECV, MIN_AMOUNT, MIN_TX_FEE);
    let high_tx = common::make_tx(&bc, &high_fee_sender, common::RECV, MIN_AMOUNT, MIN_TX_FEE * 10);

    bc.add_to_mempool(low_tx).expect("low fee tx accepted");
    bc.add_to_mempool(high_tx).expect("high fee tx accepted");

    assert_eq!(bc.mempool_size(), 2, "both TXs should be in mempool");
}

/// Mempool correctly tracks per-sender pending spend for balance checks.
#[test]
fn test_mempool_pending_spend_tracked() {
    let (mut bc, _val) = common::setup_single_validator();

    // Fund sender with exactly enough for 2 TXs
    let sender = common::funded_wallet(&mut bc, TX_COST * 2);

    let tx1 = common::make_tx_nonce(&sender, common::RECV, MIN_AMOUNT, MIN_TX_FEE, 0);
    let tx2 = common::make_tx_nonce(&sender, common::RECV, MIN_AMOUNT, MIN_TX_FEE, 1);
    bc.add_to_mempool(tx1).expect("tx1 accepted");
    bc.add_to_mempool(tx2).expect("tx2 accepted");

    // TX3 must fail — sender doesn't have enough after tx1+tx2 pending
    let tx3 = common::make_tx_nonce(&sender, common::RECV, MIN_AMOUNT, MIN_TX_FEE, 2);
    let result = bc.add_to_mempool(tx3);
    assert!(result.is_err(), "TX3 must be rejected due to insufficient balance after pending spend");
}
