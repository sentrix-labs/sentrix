#![allow(missing_docs)]
// integration_sync.rs — Two-node block synchronisation tests
// Verifies that Node B can reconstruct identical chain state by applying
// blocks received from Node A one by one.

mod common;

use sentrix::core::blockchain::Blockchain;
use sentrix::wallet::wallet::Wallet;

/// Helper: create a second, fresh Blockchain with the same validator registered.
/// This simulates a newly joined peer that knows the validator set.
fn setup_node_b(val: &Wallet) -> Blockchain {
    let mut bc = Blockchain::new(common::ADMIN.to_string());
    bc.authority
        .add_validator(common::ADMIN, val.address.clone(), "Test Validator".to_string(), val.public_key.clone())
        .expect("add_validator node B");
    bc
}

/// After Node A produces N blocks, Node B can apply them sequentially and reach
/// identical height, block hashes, and account balances.
#[test]
fn test_two_node_sync_empty_blocks() {
    let (mut bc_a, val) = common::setup_single_validator();
    let mut bc_b = setup_node_b(&val);

    // Node A produces 5 blocks
    for _ in 0..5 {
        common::mine_empty_block(&mut bc_a, &val.address);
    }

    // Sync: apply blocks from A to B one at a time
    for i in 1..=5 {
        let block = bc_a.get_block(i).expect("block in node A").clone();
        bc_b.add_block(block).expect("add_block to node B");
    }

    // ── Assert both nodes are at identical state ───────────────────────────────
    assert_eq!(bc_b.height(), bc_a.height(), "heights must match after sync");
    assert_eq!(bc_b.height(), 5);

    // All block hashes must be identical
    for i in 1..=5 {
        let hash_a = bc_a.get_block(i).expect("block a").hash.clone();
        let hash_b = bc_b.get_block(i).expect("block b").hash.clone();
        assert_eq!(hash_a, hash_b, "hash mismatch at block {}", i);
    }

    // Account balances must be identical (validator earned block rewards on both)
    let bal_a = bc_a.accounts.get_balance(&val.address);
    let bal_b = bc_b.accounts.get_balance(&val.address);
    assert_eq!(bal_a, bal_b, "validator balances must match after sync");

    // Both chains must be valid
    assert!(bc_a.is_valid_chain(), "node A chain invalid");
    assert!(bc_b.is_valid_chain(), "node B chain invalid");
}

/// Blocks containing user transactions must sync correctly: balances and tx history
/// must be identical on both nodes after sync.
#[test]
fn test_two_node_sync_with_transactions() {
    let (mut bc_a, val) = common::setup_single_validator();
    let mut bc_b = setup_node_b(&val);

    // Mine 3 empty blocks to fund the validator
    for _ in 0..3 {
        common::mine_empty_block(&mut bc_a, &val.address);
    }

    // Fund a sender independently on both nodes (simulates genesis allocation parity)
    let sender = common::funded_wallet(&mut bc_a, 500_000_000); // 5 SRX
    bc_b.accounts.credit(&sender.address, 500_000_000).expect("fund sender node B");

    // Block 4: include a TX from sender → RECV
    let tx = common::make_tx(&bc_a, &sender, common::RECV, 100_000_000, sentrix::core::transaction::MIN_TX_FEE);
    bc_a.add_to_mempool(tx).expect("add_to_mempool");
    common::mine_block_with_mempool(&mut bc_a, &val.address);

    // Block 5: empty
    common::mine_empty_block(&mut bc_a, &val.address);

    // Sync blocks 1..5 to Node B
    for i in 1..=5 {
        let block = bc_a.get_block(i).expect("block in a").clone();
        bc_b.add_block(block).expect("add_block to b");
    }

    // Heights and hashes must match
    assert_eq!(bc_b.height(), bc_a.height());
    for i in 1..=5 {
        let hash_a = bc_a.get_block(i).unwrap().hash.clone();
        let hash_b = bc_b.get_block(i).unwrap().hash.clone();
        assert_eq!(hash_a, hash_b, "hash mismatch block {}", i);
    }

    // Balances must match
    assert_eq!(
        bc_a.accounts.get_balance(&sender.address),
        bc_b.accounts.get_balance(&sender.address),
        "sender balance mismatch after sync"
    );
    assert_eq!(
        bc_a.accounts.get_balance(common::RECV),
        bc_b.accounts.get_balance(common::RECV),
        "receiver balance mismatch after sync"
    );

    assert!(bc_b.is_valid_chain(), "node B chain invalid after tx sync");
}

/// Applying the same block twice must be rejected (duplicate block).
#[test]
fn test_duplicate_block_rejected() {
    let (mut bc_a, val) = common::setup_single_validator();
    let mut bc_b = setup_node_b(&val);

    common::mine_empty_block(&mut bc_a, &val.address);

    // Apply block 1 to B → should succeed
    let block = bc_a.get_block(1).expect("block 1").clone();
    bc_b.add_block(block.clone()).expect("first apply should succeed");

    // Apply block 1 again → must fail (wrong height: expected 2, got 1)
    let result = bc_b.add_block(block);
    assert!(result.is_err(), "duplicate block must be rejected");
}

/// Node B receiving blocks out of order must reject them.
#[test]
fn test_out_of_order_block_rejected() {
    let (mut bc_a, val) = common::setup_single_validator();
    let mut bc_b = setup_node_b(&val);

    // Produce 3 blocks on A
    for _ in 0..3 {
        common::mine_empty_block(&mut bc_a, &val.address);
    }

    // Try to apply block 2 before block 1
    let block2 = bc_a.get_block(2).expect("block 2").clone();
    let result = bc_b.add_block(block2);
    assert!(result.is_err(), "out-of-order block must be rejected");
}
