#![allow(missing_docs)]
// integration_supply.rs — Conservation of value (supply invariant) tests
//
// The fundamental invariant of the Sentrix chain:
//   sum(all_account_balances) + total_burned == total_minted
//
// where total_minted = TOTAL_PREMINE + height * BLOCK_REWARD
// (valid for heights << HALVING_INTERVAL = 42,000,000 — trivially true in tests)
//
// IMPORTANT: funded_wallet() (direct credit) bypasses total_minted tracking, so
// all funds in these tests come exclusively from genesis premine or coinbase rewards.
// No bc.accounts.credit() is used here.

mod common;

use sentrix::core::blockchain::{BLOCK_REWARD, TOTAL_PREMINE};
use sentrix::core::transaction::MIN_TX_FEE;

/// Assert the supply invariant at the current chain state.
fn assert_invariant(bc: &sentrix::core::blockchain::Blockchain) {
    let height = bc.height();
    let sum = bc.accounts.total_supply();
    let burned = bc.accounts.total_burned;
    let expected = TOTAL_PREMINE + height * BLOCK_REWARD;
    assert_eq!(
        sum + burned,
        expected,
        "supply invariant broken at height {height}: sum={sum} + burned={burned} = {} ≠ expected={expected}",
        sum + burned
    );
}

/// Verify the supply invariant holds at genesis (block 0).
#[test]
fn test_supply_invariant_at_genesis() {
    let (bc, _val) = common::setup_single_validator();
    assert_invariant(&bc);
    assert_eq!(bc.accounts.total_supply(), TOTAL_PREMINE);
    assert_eq!(bc.accounts.total_burned, 0);
}

/// The invariant must hold after every one of 100 empty blocks.
#[test]
fn test_supply_invariant_after_empty_blocks() {
    let (mut bc, val) = common::setup_single_validator();
    for _ in 0..100 {
        common::mine_empty_block(&mut bc, &val.address);
        assert_invariant(&bc);
    }
}

/// The invariant must hold after blocks containing transactions funded solely
/// through coinbase rewards (no direct credit() calls).
#[test]
fn test_supply_invariant_with_transactions() {
    let (mut bc, val) = common::setup_single_validator();

    // Mine 100 blocks to accumulate validator balance (100 SRX in coinbase rewards)
    for _ in 0..100 {
        common::mine_empty_block(&mut bc, &val.address);
    }
    assert_invariant(&bc);

    // Now use the validator's accumulated balance as sender for the next 30 blocks
    for _ in 0..30 {
        let val_balance = bc.accounts.get_balance(&val.address);
        let needed = MIN_TX_FEE + MIN_TX_FEE; // amount + fee (both equal MIN_TX_FEE)
        if val_balance >= needed {
            let nonce = bc.accounts.get_nonce(&val.address);
            let sk = val.get_secret_key().expect("sk");
            let pk = val.get_public_key().expect("pk");
            let tx = sentrix::core::transaction::Transaction::new(
                val.address.clone(),
                common::RECV.to_string(),
                MIN_TX_FEE, // non-zero amount required
                MIN_TX_FEE,
                nonce,
                String::new(),
                sentrix::core::blockchain::CHAIN_ID,
                &sk,
                &pk,
            ).expect("Transaction::new");
            bc.add_to_mempool(tx).expect("add_to_mempool");
        }
        common::mine_block_with_mempool(&mut bc, &val.address);
        assert_invariant(&bc);
    }
}

/// Invariant holds even with high fees.
#[test]
fn test_supply_invariant_with_high_fees() {
    let (mut bc, val) = common::setup_single_validator();

    // Fund validator with 200 blocks worth of rewards
    for _ in 0..200 {
        common::mine_empty_block(&mut bc, &val.address);
    }
    assert_invariant(&bc);

    let high_fee = MIN_TX_FEE * 100; // 1_000_000 sentri fee

    for _ in 0..10 {
        let val_balance = bc.accounts.get_balance(&val.address);
        let needed = MIN_TX_FEE + high_fee;
        if val_balance >= needed {
            let nonce = bc.accounts.get_nonce(&val.address);
            let sk = val.get_secret_key().expect("sk");
            let pk = val.get_public_key().expect("pk");
            let tx = sentrix::core::transaction::Transaction::new(
                val.address.clone(),
                common::RECV.to_string(),
                MIN_TX_FEE,
                high_fee,
                nonce,
                String::new(),
                sentrix::core::blockchain::CHAIN_ID,
                &sk,
                &pk,
            ).expect("Transaction::new");
            bc.add_to_mempool(tx).expect("add_to_mempool");
        }
        common::mine_block_with_mempool(&mut bc, &val.address);
        assert_invariant(&bc);
    }
}

/// total_minted increases by exactly BLOCK_REWARD per block.
#[test]
fn test_total_minted_increases_by_block_reward() {
    let (mut bc, val) = common::setup_single_validator();

    assert_eq!(bc.accounts.total_supply() + bc.accounts.total_burned, TOTAL_PREMINE);

    for n in 1..=5u64 {
        common::mine_empty_block(&mut bc, &val.address);
        let expected = TOTAL_PREMINE + n * BLOCK_REWARD;
        let actual = bc.accounts.total_supply() + bc.accounts.total_burned;
        assert_eq!(actual, expected, "incorrect total at block {n}");
    }
}

/// Invariant holds at genesis block 0 with no validators (just premine).
#[test]
fn test_genesis_premine_is_total_initial_supply() {
    let (bc, _val) = common::setup_single_validator();
    let genesis_supply = bc.accounts.total_supply();
    assert_eq!(genesis_supply, TOTAL_PREMINE, "genesis supply must equal TOTAL_PREMINE");
}
