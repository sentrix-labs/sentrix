#![allow(missing_docs)]
// integration_restart.rs — Node restart persistence tests
// Verifies that all chain state survives a full shutdown → storage save → reload cycle.

mod common;

use sentrix::storage::db::Storage;

/// After producing N blocks, saving to disk, and reloading, all state must be
/// byte-for-byte identical: height, balances, burned total, chain hashes, mempool.
#[test]
fn test_restart_preserves_full_state() {
    let (mut bc, val) = common::setup_single_validator();

    // Produce 10 blocks
    for _ in 0..10 {
        common::mine_empty_block(&mut bc, &val.address);
    }

    // Record state before shutdown
    let height_before = bc.height();
    let burned_before = bc.accounts.total_burned;
    let supply_before = bc.accounts.total_supply();
    let validator_balance_before = bc.accounts.get_balance(&val.address);

    // Collect block hashes for all blocks in window
    let hashes_before: Vec<(u64, String)> = (0..=height_before)
        .filter_map(|i| bc.get_block(i).map(|b| (i, b.hash.clone())))
        .collect();

    // Persist to temporary storage
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().to_str().expect("path").to_string();
    let storage = Storage::open(&path).expect("storage open");
    storage.save_blockchain(&bc).expect("save_blockchain");
    drop(storage);
    drop(bc); // simulate node shutdown

    // Reload from storage (simulate restart)
    let storage2 = Storage::open(&path).expect("storage reopen");
    let loaded = storage2.load_blockchain().expect("load_blockchain error").expect("no state found");

    // ── Assert all state is identical ──────────────────────────────────────────
    assert_eq!(loaded.height(), height_before, "height mismatch after restart");
    assert_eq!(loaded.height(), 10, "expected exactly 10 blocks");
    assert_eq!(loaded.accounts.total_burned, burned_before, "total_burned changed after restart");
    assert_eq!(loaded.accounts.total_supply(), supply_before, "total supply changed after restart");
    assert_eq!(
        loaded.accounts.get_balance(&val.address),
        validator_balance_before,
        "validator balance changed after restart"
    );

    // Chain must still be valid
    assert!(loaded.is_valid_chain_window(), "chain invalid after restart");

    // Block hashes must be identical for all blocks in the sliding window
    let window_start = loaded.chain_window_start();
    for (idx, expected_hash) in &hashes_before {
        if *idx >= window_start {
            let block = loaded.get_block(*idx).expect("block should be in window");
            assert_eq!(&block.hash, expected_hash, "hash mismatch for block {}", idx);
        }
    }

    // Mempool must be empty — stale in-flight TXs are not persisted
    assert_eq!(loaded.mempool_size(), 0, "mempool should be empty after restart");
}

/// Non-stale pending transactions survive a restart (mempool is persisted).
/// After restart, the node can continue processing the pending TX in the next block.
#[test]
fn test_restart_preserves_pending_mempool_txs() {
    let (mut bc, val) = common::setup_single_validator();

    // Mine a few blocks to fund the sender
    for _ in 0..3 {
        common::mine_empty_block(&mut bc, &val.address);
    }

    // Fund a sender and submit a TX to mempool (do NOT mine it)
    let sender = common::funded_wallet(&mut bc, 10_000_000);
    let tx = common::make_tx(&bc, &sender, common::RECV, 100_000, sentrix::core::transaction::MIN_TX_FEE);
    bc.add_to_mempool(tx).expect("add_to_mempool");
    assert_eq!(bc.mempool_size(), 1, "mempool should have 1 pending tx");

    // Save and reload
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().to_str().expect("path").to_string();
    let storage = Storage::open(&path).expect("storage open");
    storage.save_blockchain(&bc).expect("save_blockchain");
    drop(storage);
    drop(bc);

    let storage2 = Storage::open(&path).expect("storage reopen");
    let mut loaded = storage2.load_blockchain().expect("load").expect("no state");

    // Non-stale TX survives the restart
    assert_eq!(loaded.mempool_size(), 1, "non-stale pending TX must survive restart");

    // After restart, the node can still mine the pending TX
    common::mine_block_with_mempool(&mut loaded, &val.address);
    assert_eq!(loaded.mempool_size(), 0, "TX must be mined after restart");
    assert_eq!(loaded.accounts.get_balance(common::RECV), 100_000, "receiver must have funds");
}

/// Chain height must survive restart without corruption.
#[test]
fn test_restart_height_integrity() {
    let (mut bc, val) = common::setup_single_validator();

    for _ in 0..5 {
        common::mine_empty_block(&mut bc, &val.address);
    }

    let expected_height = bc.height();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().to_str().expect("path").to_string();
    let storage = Storage::open(&path).expect("storage open");
    storage.save_blockchain(&bc).expect("save");
    drop(storage);
    drop(bc);

    let storage2 = Storage::open(&path).expect("reopen");
    let loaded = storage2.load_blockchain().expect("load").expect("no state");
    assert_eq!(loaded.height(), expected_height, "height must survive restart");
    assert!(loaded.is_valid_chain_window(), "reloaded chain must be valid");
}
