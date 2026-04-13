// integration_tx.rs — Transaction lifecycle end-to-end tests
// Covers: mempool acceptance, block inclusion, balance changes, double-spend protection,
// insufficient balance, fee distribution, and nonce sequencing.

mod common;

use sentrix::core::blockchain::BLOCK_REWARD;
use sentrix::core::transaction::MIN_TX_FEE;

const SEND_AMOUNT: u64 = 100_000_000; // 1 SRX in sentri
const INITIAL_FUND: u64 = 500_000_000; // 5 SRX

/// Full TX lifecycle: submit → mine → assert correct balance changes and fee distribution.
#[test]
fn test_tx_lifecycle_full() {
    let (mut bc, val) = common::setup_single_validator();
    let sender = common::funded_wallet(&mut bc, INITIAL_FUND);

    let sender_before = bc.accounts.get_balance(&sender.address);
    let recv_before = bc.accounts.get_balance(common::RECV);
    let burned_before = bc.accounts.total_burned;

    // Submit TX
    let tx = common::make_tx(&bc, &sender, common::RECV, SEND_AMOUNT, MIN_TX_FEE);
    let txid = tx.txid.clone();
    bc.add_to_mempool(tx).expect("add_to_mempool");
    assert_eq!(bc.mempool_size(), 1, "TX should be in mempool");

    // Mine a block that includes the TX
    common::mine_block_with_mempool(&mut bc, &val.address);

    // ── Assert balance changes ─────────────────────────────────────────────────
    let sender_after = bc.accounts.get_balance(&sender.address);
    let recv_after = bc.accounts.get_balance(common::RECV);
    let burned_after = bc.accounts.total_burned;

    // Sender loses amount + fee
    assert_eq!(
        sender_after,
        sender_before - SEND_AMOUNT - MIN_TX_FEE,
        "sender balance incorrect"
    );
    // Receiver gains exactly the amount (not fee)
    assert_eq!(recv_after, recv_before + SEND_AMOUNT, "receiver balance incorrect");

    // Burned = ceil(MIN_TX_FEE / 2)
    let expected_burn = MIN_TX_FEE.div_ceil(2);
    assert_eq!(burned_after - burned_before, expected_burn, "burned amount incorrect");

    // TX is in the block
    let tx_result = bc.get_transaction(&txid);
    assert!(tx_result.is_some(), "TX should be findable after mining");

    // Mempool must be empty after mining
    assert_eq!(bc.mempool_size(), 0, "mempool should be empty after mining");
}

/// Double-spend: submitting the same TX (same nonce) again must be rejected.
#[test]
fn test_double_spend_rejected() {
    let (mut bc, val) = common::setup_single_validator();
    let sender = common::funded_wallet(&mut bc, INITIAL_FUND);

    let tx = common::make_tx(&bc, &sender, common::RECV, SEND_AMOUNT, MIN_TX_FEE);
    bc.add_to_mempool(tx).expect("first TX should be accepted");
    common::mine_block_with_mempool(&mut bc, &val.address);

    // Nonce is now 1 on-chain; re-submitting nonce=0 must fail
    let stale_tx = common::make_tx_nonce(&sender, common::RECV, SEND_AMOUNT, MIN_TX_FEE, 0);
    let result = bc.add_to_mempool(stale_tx);
    assert!(result.is_err(), "double-spend (stale nonce) must be rejected");
}

/// TX with insufficient balance must be rejected at mempool stage.
#[test]
fn test_insufficient_balance_rejected_at_mempool() {
    let (mut bc, _val) = common::setup_single_validator();
    let sender = common::funded_wallet(&mut bc, 10_000); // only 10_000 sentri

    // Trying to send 1 SRX (100_000_000) with no balance for it
    let tx = common::make_tx(&bc, &sender, common::RECV, SEND_AMOUNT, MIN_TX_FEE);
    let result = bc.add_to_mempool(tx);
    assert!(result.is_err(), "TX exceeding balance must be rejected");
    let err_str = result.unwrap_err().to_string();
    assert!(
        err_str.contains("balance") || err_str.contains("InsufficientBalance"),
        "error should mention balance: {}", err_str
    );
}

/// Multiple sequential TXs from the same sender must have correct nonces.
#[test]
fn test_sequential_nonces_accepted() {
    let (mut bc, val) = common::setup_single_validator();
    let sender = common::funded_wallet(&mut bc, 1_000_000_000); // 10 SRX

    // Submit 3 TXs with nonces 0, 1, 2
    for n in 0..3u64 {
        let tx = common::make_tx_nonce(&sender, common::RECV, 10_000_000, MIN_TX_FEE, n);
        bc.add_to_mempool(tx).expect("TX {n} should be accepted");
    }
    assert_eq!(bc.mempool_size(), 3, "3 TXs should be in mempool");

    // Mine the block — all 3 must be included
    common::mine_block_with_mempool(&mut bc, &val.address);
    assert_eq!(bc.mempool_size(), 0, "mempool must be empty after mining");
}

/// TX with a skipped nonce (nonce=1 when account nonce=0 and no pending TXs)
/// must be rejected.
#[test]
fn test_wrong_nonce_rejected() {
    let (mut bc, _val) = common::setup_single_validator();
    let sender = common::funded_wallet(&mut bc, INITIAL_FUND);

    // Nonce 1 when expected is 0
    let tx = common::make_tx_nonce(&sender, common::RECV, 10_000_000, MIN_TX_FEE, 1);
    let result = bc.add_to_mempool(tx);
    assert!(result.is_err(), "wrong nonce must be rejected");
}

/// Validator balance increases by BLOCK_REWARD + fee_share after mining a block with a TX.
#[test]
fn test_validator_receives_reward_and_fee_share() {
    let (mut bc, val) = common::setup_single_validator();
    let sender = common::funded_wallet(&mut bc, INITIAL_FUND);

    let val_before = bc.accounts.get_balance(&val.address);

    let tx = common::make_tx(&bc, &sender, common::RECV, 1_000_000, MIN_TX_FEE);
    bc.add_to_mempool(tx).expect("add_to_mempool");
    common::mine_block_with_mempool(&mut bc, &val.address);

    let val_after = bc.accounts.get_balance(&val.address);
    let fee_share = MIN_TX_FEE / 2; // floor division
    assert_eq!(
        val_after,
        val_before + BLOCK_REWARD + fee_share,
        "validator should receive block reward + floor(fee/2)"
    );
}
