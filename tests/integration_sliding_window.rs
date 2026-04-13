// integration_sliding_window.rs — Sliding window (CHAIN_WINDOW_SIZE) tests
//
// Sentrix keeps only the last CHAIN_WINDOW_SIZE (1_000) blocks in RAM.
// Older blocks are evicted and only accessible via sled storage.
// This test verifies that:
//  - height() returns the true chain height (not the window size)
//  - chain_window_start() advances correctly as blocks are evicted
//  - get_block() returns None for evicted blocks
//  - get_block() returns Some for blocks in the window
//  - After a restart, the sliding window is correctly reconstructed

mod common;

use sentrix::core::blockchain::CHAIN_WINDOW_SIZE;
use sentrix::storage::db::Storage;

const EXTRA_BLOCKS: u64 = 100; // blocks beyond the window size
const TOTAL_BLOCKS: u64 = CHAIN_WINDOW_SIZE as u64 + EXTRA_BLOCKS;

/// After producing CHAIN_WINDOW_SIZE + EXTRA_BLOCKS blocks:
/// - height() == TOTAL_BLOCKS
/// - chain_window_start() == EXTRA_BLOCKS + 1
/// - Blocks 0..=EXTRA_BLOCKS are evicted (get_block returns None)
/// - Block EXTRA_BLOCKS+1 is the window start (get_block returns Some)
/// - Latest block is in window
#[test]
fn test_sliding_window_eviction() {
    let (mut bc, val) = common::setup_single_validator();

    for _ in 0..TOTAL_BLOCKS {
        common::mine_empty_block(&mut bc, &val.address);
    }

    assert_eq!(bc.height(), TOTAL_BLOCKS, "height must equal total blocks mined");

    let window_start = bc.chain_window_start();
    let expected_window_start = TOTAL_BLOCKS - CHAIN_WINDOW_SIZE as u64 + 1;
    assert_eq!(
        window_start,
        expected_window_start,
        "window_start should be height - CHAIN_WINDOW_SIZE + 1"
    );

    // Genesis and early blocks must be evicted
    assert!(
        bc.get_block(0).is_none(),
        "genesis block (0) must be evicted from window"
    );
    assert!(
        bc.get_block(EXTRA_BLOCKS - 1).is_none(),
        "block EXTRA_BLOCKS-1 must be evicted"
    );
    assert!(
        bc.get_block(EXTRA_BLOCKS).is_none(),
        "block EXTRA_BLOCKS must be evicted"
    );

    // Window start block must be present
    assert!(
        bc.get_block(window_start).is_some(),
        "window_start block must be in window"
    );

    // Latest block must be present
    assert!(
        bc.get_block(TOTAL_BLOCKS).is_some(),
        "latest block must be in window"
    );

    // A block in the middle of the window must be present
    let mid_window = window_start + CHAIN_WINDOW_SIZE as u64 / 2;
    assert!(
        bc.get_block(mid_window).is_some(),
        "mid-window block {mid_window} must be accessible"
    );

    // Window size = height - chain_window_start + 1 == CHAIN_WINDOW_SIZE
    let inferred_window_size = bc.height() - bc.chain_window_start() + 1;
    assert_eq!(
        inferred_window_size,
        CHAIN_WINDOW_SIZE as u64,
        "window size must equal CHAIN_WINDOW_SIZE"
    );
}

/// chain_stats() must reflect the sliding window metadata correctly.
#[test]
fn test_chain_stats_window_metadata() {
    let (mut bc, val) = common::setup_single_validator();

    // Before window fills: is_partial should be false
    for _ in 0..10 {
        common::mine_empty_block(&mut bc, &val.address);
    }
    let stats = bc.chain_stats();
    assert_eq!(stats["window_start_block"].as_u64().unwrap(), 0);
    assert!(!stats["window_is_partial"].as_bool().unwrap());

    // After window fills: is_partial should be true and window_start > 0
    for _ in 0..TOTAL_BLOCKS {
        common::mine_empty_block(&mut bc, &val.address);
    }
    let stats = bc.chain_stats();
    let window_start = stats["window_start_block"].as_u64().unwrap();
    let is_partial = stats["window_is_partial"].as_bool().unwrap();
    assert!(window_start > 0, "window_start must advance after CHAIN_WINDOW_SIZE blocks");
    assert!(is_partial, "is_partial must be true when window has advanced");
}

/// After saving to storage and reloading, height is correct and the sliding
/// window is properly restored.
#[test]
fn test_sliding_window_survives_restart() {
    let (mut bc, val) = common::setup_single_validator();

    for _ in 0..TOTAL_BLOCKS {
        common::mine_empty_block(&mut bc, &val.address);
    }

    let height_before = bc.height();
    let window_start_before = bc.chain_window_start();
    let latest_hash = bc.get_block(height_before).expect("latest block").hash.clone();

    // Save to storage
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let path = tmp.path().to_str().expect("path").to_string();
    let storage = Storage::open(&path).expect("storage open");
    storage.save_blockchain(&bc).expect("save");
    drop(storage);
    drop(bc);

    // Reload
    let storage2 = Storage::open(&path).expect("storage reopen");
    let loaded = storage2.load_blockchain().expect("load").expect("no state");

    // Height must be preserved exactly
    assert_eq!(loaded.height(), height_before, "height must survive restart");
    assert_eq!(loaded.height(), TOTAL_BLOCKS);

    // Window start must be restored
    assert_eq!(
        loaded.chain_window_start(),
        window_start_before,
        "window_start must survive restart"
    );

    // Evicted blocks are still evicted after reload
    assert!(
        loaded.get_block(0).is_none(),
        "genesis still evicted after restart"
    );

    // Latest block hash must match
    let reloaded_latest = loaded.get_block(height_before).expect("latest in window");
    assert_eq!(reloaded_latest.hash, latest_hash, "latest block hash must survive restart");

    assert!(loaded.is_valid_chain(), "chain must be valid after reload");
}

/// Blocks added after a window overflow do not appear in get_block() for evicted indices.
#[test]
fn test_evicted_block_returns_none() {
    let (mut bc, val) = common::setup_single_validator();

    // Mine exactly CHAIN_WINDOW_SIZE + 1 blocks to trigger the first eviction
    for _ in 0..=CHAIN_WINDOW_SIZE as u64 {
        common::mine_empty_block(&mut bc, &val.address);
    }

    // height = CHAIN_WINDOW_SIZE + 1; window_start = height - CHAIN_WINDOW_SIZE + 1 = 2
    let expected_ws = bc.height() - CHAIN_WINDOW_SIZE as u64 + 1;
    let ws = bc.chain_window_start();
    assert_eq!(ws, expected_ws, "window_start = height - CHAIN_WINDOW_SIZE + 1");

    // Blocks 0 and 1 (both before window_start=2) must be evicted
    assert!(
        bc.get_block(0).is_none(),
        "genesis must be evicted after CHAIN_WINDOW_SIZE+1 blocks"
    );
    assert!(
        bc.get_block(1).is_none(),
        "block 1 must be evicted (window_start = {ws})"
    );

    // Block at window_start must be accessible
    assert!(
        bc.get_block(ws).is_some(),
        "block at window_start ({ws}) must be accessible"
    );
}
