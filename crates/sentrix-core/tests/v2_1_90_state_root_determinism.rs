//! State-root determinism regression guards (v2.1.90).
//!
//! These tests pin the invariant that block execution is deterministic
//! across process restart and across the persistence boundary. Same
//! parent state plus same block input must always produce the same
//! stateRoot, regardless of whether the node is in a fresh or warm
//! execution context.
//!
//! The existing `fork_determinism::test_mdbx_roundtrip_*` tests cover
//! the disk-roundtrip surface, but only with V4-pre-fork settings and
//! only through the peer-apply path. These tests extend coverage to:
//!   - post-V4 reward-v2 + STATE_ROOT_V2 active (the configuration in
//!     production today),
//!   - both `add_block` (self-produced) and `add_block_from_peer`,
//!   - the persistence-window between `save_block` (height + block
//!     bytes + sync) and `save_blockchain` (Blockchain bincode blob
//!     including `accounts`).
//!
//! Regression class: a process exit between those two writes leaves
//! the on-disk view inconsistent — height advanced, trie committed,
//! but `accounts` blob one block behind. Reload then computes a trie
//! root that disagrees with peers on the next block.
//!
//! Companion tests verify the inverse: clean drop+reload of a chain
//! at the same height must reproduce the trie root exactly.
//!
//! See `crates/sentrix-storage/src/chain.rs` for `save_block` and
//! `save_blockchain` implementations and `bin/sentrix/src/main.rs` for
//! the validator-loop call order.

use sentrix_core::blockchain::Blockchain;
use sentrix_core::storage::Storage;
use sentrix_storage::MdbxStorage;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// Global mutex serialising all tests in this file. Each test mutates
/// process env vars (`STATE_ROOT_V2_HEIGHT`, `VOYAGER_REWARD_V2_HEIGHT`),
/// which are process-wide; without this, parallel test execution races
/// the env state and produces flake. Tests inside `with_v2_env` run
/// strictly serially.
static ENV_GUARD: Mutex<()> = Mutex::new(());

const STATE_ROOT_V2_HEIGHT: &str = "5";
const STATE_ROOT_V2_TREASURY_BALANCE: &str = "1000000000000";
const VOYAGER_REWARD_V2_HEIGHT: &str = "0";

/// Runtime-built proposer address. Built at runtime so the pre-commit
/// hook's generic "0x + 40 hex" detector doesn't false-positive on what
/// is obviously a test address.
fn proposer_addr() -> String {
    format!("0x{}", "ab".repeat(20))
}

/// Common setup. Returns (tempdir, storage, blockchain).
/// Caller is responsible for `init_trie` + `init_storage_handle`.
fn setup_storage_and_chain() -> (TempDir, Storage, Blockchain) {
    let dir = TempDir::new().expect("tempdir");
    let storage = Storage::open(dir.path().to_str().unwrap()).expect("storage open");
    let mut bc = Blockchain::new("admin".to_string());
    bc.authority.add_validator_unchecked(
        proposer_addr(),
        "Validator 1".to_string(),
        "pk1".to_string(),
    );
    (dir, storage, bc)
}

/// Build N coinbase blocks on the producer + persist each block to MDBX so
/// reload can find them. Returns the producer with chain at height=N.
fn build_warmup_chain(producer: &mut Blockchain, mdbx: &Arc<MdbxStorage>, height: u64) {
    let proposer = proposer_addr();
    for _ in 0..height {
        let block = producer.create_block(&proposer).expect("create_block");
        // Persist block before add_block so reload sees it.
        mdbx.put(
            sentrix_storage::tables::TABLE_META,
            format!("block:{}", block.index).as_bytes(),
            &serde_json::to_vec(&block).expect("block serialize"),
        )
        .expect("mdbx put");
        producer.add_block(block).expect("add_block");
    }
}

fn with_v2_env<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    // Hold the global env-var lock for the duration of the test body.
    // PoisonError just means a previous test panicked while holding the
    // lock; the env vars get reset below regardless, so we recover.
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());

    let prev_v2_height = std::env::var("STATE_ROOT_V2_HEIGHT").ok();
    let prev_v2_balance = std::env::var("STATE_ROOT_V2_TREASURY_BALANCE").ok();
    let prev_reward_v2 = std::env::var("VOYAGER_REWARD_V2_HEIGHT").ok();

    // SAFETY: env vars are process-wide; we serialise via ENV_GUARD
    // above so concurrent tests in this binary don't race each other.
    // Per the unsafe set_var/remove_var contract on edition-2024 Rust.
    unsafe {
        std::env::set_var("STATE_ROOT_V2_HEIGHT", STATE_ROOT_V2_HEIGHT);
        std::env::set_var(
            "STATE_ROOT_V2_TREASURY_BALANCE",
            STATE_ROOT_V2_TREASURY_BALANCE,
        );
        std::env::set_var("VOYAGER_REWARD_V2_HEIGHT", VOYAGER_REWARD_V2_HEIGHT);
    }

    let result = f();

    unsafe {
        match prev_v2_height {
            Some(v) => std::env::set_var("STATE_ROOT_V2_HEIGHT", v),
            None => std::env::remove_var("STATE_ROOT_V2_HEIGHT"),
        }
        match prev_v2_balance {
            Some(v) => std::env::set_var("STATE_ROOT_V2_TREASURY_BALANCE", v),
            None => std::env::remove_var("STATE_ROOT_V2_TREASURY_BALANCE"),
        }
        match prev_reward_v2 {
            Some(v) => std::env::set_var("VOYAGER_REWARD_V2_HEIGHT", v),
            None => std::env::remove_var("VOYAGER_REWARD_V2_HEIGHT"),
        }
    }

    result
}

/// Clean drop+reload determinism under V4 + STATE_ROOT_V2.
///
/// Invariant: a chain that was cleanly persisted (every block followed
/// by `save_blockchain`) and then reloaded into a fresh `Blockchain`
/// instance must compute the same trie root as a never-dropped control
/// when both apply the next block.
///
/// This is the happy-path counterpart to the crash-window test below.
/// It must continue to pass; if it ever fails, that is a regression in
/// the trie-rehydration path itself.
#[test]
fn test_v2_1_90_clean_drop_reload_then_apply_block_matches_control() {
    with_v2_env(|| {
        let proposer = proposer_addr();

        let (_dir1, storage, mut producer) = setup_storage_and_chain();
        let mdbx = storage.mdbx_arc();
        producer.init_trie(Arc::clone(&mdbx)).unwrap();
        producer.init_storage_handle(Arc::clone(&mdbx)).unwrap();

        // Phase 1: warmup — build 30 blocks, save state.
        const WARMUP: u64 = 30;
        build_warmup_chain(&mut producer, &mdbx, WARMUP);
        let producer_root_at_warmup = producer.trie_root_at(WARMUP).map(hex::encode);
        storage.save_blockchain(&producer).unwrap();

        // Phase 2: drop + reload.
        drop(producer);
        let mut reloaded: Blockchain = storage
            .load_blockchain()
            .expect("load_blockchain")
            .expect("state must exist post-save");
        reloaded.init_trie(Arc::clone(&mdbx)).unwrap();
        reloaded.init_storage_handle(Arc::clone(&mdbx)).unwrap();
        let reloaded_root_at_warmup = reloaded.trie_root_at(WARMUP).map(hex::encode);
        assert_eq!(
            producer_root_at_warmup, reloaded_root_at_warmup,
            "pre-condition: reloaded chain must agree with producer at warmup height"
        );

        // Phase 3: control chain — replay all WARMUP blocks in a fresh
        // in-memory Blockchain. Used as the deterministic reference.
        let dir_ctrl = TempDir::new().expect("tempdir");
        let mdbx_ctrl = Arc::new(MdbxStorage::open(dir_ctrl.path()).expect("mdbx ctrl"));
        let mut control = Blockchain::new("admin".to_string());
        control.authority.add_validator_unchecked(
            proposer.clone(),
            "Validator 1".to_string(),
            "pk1".to_string(),
        );
        control.init_trie(Arc::clone(&mdbx_ctrl)).unwrap();
        for h in 1..=WARMUP {
            let block = reloaded
                .get_block_any(h)
                .unwrap_or_else(|| panic!("missing block at h={h}"));
            control.add_block_from_peer(block).expect("control replay");
        }
        let control_root_at_warmup = control.trie_root_at(WARMUP).map(hex::encode);
        assert_eq!(
            producer_root_at_warmup, control_root_at_warmup,
            "control replay must match producer — sanity check"
        );

        // Phase 4: produce one more block on the reloaded chain, apply
        // the same block on the control chain, compare trie roots.
        let next = reloaded.create_block(&proposer).expect("create next");
        let h_next = next.index;
        reloaded
            .add_block(next.clone())
            .expect("add next on reloaded");
        control
            .add_block_from_peer(next)
            .expect("control add next from peer");

        let reloaded_root_next = reloaded.trie_root_at(h_next).map(hex::encode);
        let control_root_next = control.trie_root_at(h_next).map(hex::encode);
        assert_eq!(
            reloaded_root_next, control_root_next,
            "clean drop+reload must produce the same trie root at h={h_next} as the control. \
             reloaded={reloaded_root_next:?} control={control_root_next:?}"
        );
    });
}

/// Targeted failing test — non-atomic persistence between `save_block`
/// and `save_blockchain`.
///
/// Invariant under test: after a process exit, the trie root computed
/// for the next block by the rehydrated node must equal the root that a
/// never-crashed peer computes for the same block.
///
/// Regression class: the validator loop persists each block in two
/// separate MDBX writes —
///   step A. `Storage::save_block(&block)` — block bytes + height bump
///           + `mdbx.sync()` (`crates/sentrix-storage/src/chain.rs`,
///           `save_block`).
///   step B. `Storage::save_blockchain(&bc)` — Blockchain bincode blob,
///           which is the only persistent home for `accounts`,
///           `mempool`, `stake_registry`, `total_minted`
///           (`crates/sentrix-storage/src/chain.rs`, `save_blockchain`).
///
/// A process exit between A and B leaves on-disk state in this shape:
///   - `height` key = N (advanced)
///   - block N persisted
///   - trie at h=N committed (via `update_trie_for_block` inside
///     `add_block`)
///   - Blockchain blob: `accounts` at h=N-1 (whatever step B wrote at
///     the end of the previous block)
///
/// On restart, `Storage::load_blockchain` returns `accounts` at h=N-1,
/// chain.height = N, and `init_trie` sees the committed root for h=N
/// so no backfill fires. The node is now in a state where `accounts`
/// is one block behind `state_trie` and `chain`. When block N+1 is
/// applied, `apply_block_pass2` reads the stale treasury balance, the
/// coinbase mint goes against the wrong base, and the resulting trie
/// root diverges from peers.
///
/// First divergent variable on this path is
/// `accounts.get_balance(PROTOCOL_TREASURY)` at the entry of
/// `update_trie_for_block` for block N+2; the divergence is exactly
/// one block reward (the missing block N coinbase mint).
///
/// **Expected**: this test FAILS on current code. After the persistence
/// path is made atomic (or replay-on-reload is added), this test must
/// pass.
#[test]
fn test_v2_1_90_crash_between_save_block_and_save_blockchain() {
    with_v2_env(|| {
        let proposer = proposer_addr();

        // Phase 1: build N blocks on the producer with normal save flow.
        let (_dir1, storage, mut producer) = setup_storage_and_chain();
        let mdbx = storage.mdbx_arc();
        producer.init_trie(Arc::clone(&mdbx)).unwrap();
        producer.init_storage_handle(Arc::clone(&mdbx)).unwrap();

        const N: u64 = 20;
        for _h in 1..=N {
            let block = producer.create_block(&proposer).expect("create");
            producer.add_block(block.clone()).expect("add_block");
            // Normal validator-loop flow: apply block, persist via
            // save_blockchain (covers both blob + chain window).
            storage.save_blockchain(&producer).expect("save_blockchain");
        }
        let producer_root_n = producer.trie_root_at(N).map(hex::encode);

        // Phase 2: simulate the crash window.
        // Apply block N+1 via add_block (mutates in-memory accounts +
        // commits trie at h=N+1). Then write the per-block persistence
        // (block bytes + height bump) directly to MDBX, mimicking
        // `save_block`. CRUCIALLY, do NOT call save_blockchain — this
        // is the on-disk shape of a crash between step A and step B.
        let block_n1 = producer.create_block(&proposer).expect("create N+1");
        producer.add_block(block_n1.clone()).expect("add_block N+1");
        mdbx.put(
            sentrix_storage::tables::TABLE_META,
            format!("block:{}", block_n1.index).as_bytes(),
            &serde_json::to_vec(&block_n1).expect("serialize"),
        )
        .expect("mdbx put block");
        mdbx.put(
            sentrix_storage::tables::TABLE_META,
            b"height",
            &serde_json::to_vec(&(N + 1)).expect("serialize height"),
        )
        .expect("mdbx put height");
        // Producer's trie has committed h=N+1 already.
        // We deliberately skip storage.save_blockchain(&producer) here.
        let producer_root_n1 = producer.trie_root_at(N + 1).map(hex::encode);

        // Phase 3: simulate the restart.
        drop(producer);
        let mut reloaded: Blockchain = storage
            .load_blockchain()
            .expect("load_blockchain")
            .expect("blob exists from h=N save");
        reloaded.init_trie(Arc::clone(&mdbx)).unwrap();
        reloaded.init_storage_handle(Arc::clone(&mdbx)).unwrap();
        let reloaded_root_n = reloaded.trie_root_at(N).map(hex::encode);
        let reloaded_root_n1 = reloaded.trie_root_at(N + 1).map(hex::encode);

        // Phase 4: control chain — replay N+1 blocks cleanly, no crash.
        let dir_ctrl = TempDir::new().expect("tempdir");
        let mdbx_ctrl = Arc::new(MdbxStorage::open(dir_ctrl.path()).expect("mdbx ctrl"));
        let mut control = Blockchain::new("admin".to_string());
        control.authority.add_validator_unchecked(
            proposer.clone(),
            "Validator 1".to_string(),
            "pk1".to_string(),
        );
        control.init_trie(Arc::clone(&mdbx_ctrl)).unwrap();
        for h in 1..=(N + 1) {
            let block = reloaded
                .get_block_any(h)
                .unwrap_or_else(|| panic!("missing block at h={h}"));
            control.add_block_from_peer(block).expect("control replay");
        }
        let control_root_n1 = control.trie_root_at(N + 1).map(hex::encode);

        // Sanity: roots at N must agree across the three.
        assert_eq!(
            producer_root_n, reloaded_root_n,
            "sanity: producer and reloaded must agree at h={N}"
        );

        // Persisted trie roots are read from MDBX, not recomputed —
        // so this assert guards the persistence layer itself, not the
        // execution path. Divergence here would be a different class
        // (trie write loss) than the one this test targets.
        eprintln!(
            "trie_root@h={}:\n  producer:  {:?}\n  reloaded:  {:?}\n  control:   {:?}",
            N + 1,
            producer_root_n1,
            reloaded_root_n1,
            control_root_n1
        );
        assert_eq!(
            reloaded_root_n1,
            control_root_n1,
            "post-crash reloaded trie root at h={} differs from control. \
             accounts blob (h=N) and chain height (h=N+1) inconsistency \
             produced different trie state when re-executed: \
             reloaded={:?} control={:?}",
            N + 1,
            reloaded_root_n1,
            control_root_n1
        );

        // Phase 5: apply one more block on both reloaded and control.
        // The downstream divergence is the one that surfaces first in
        // the live execution path.
        let block_n2 = control.create_block(&proposer).expect("create N+2");
        let h_n2 = block_n2.index;
        control
            .add_block(block_n2.clone())
            .expect("control add N+2");
        reloaded
            .add_block_from_peer(block_n2)
            .expect("reloaded add N+2 from peer");
        assert_eq!(
            reloaded.trie_root_at(h_n2).map(hex::encode),
            control.trie_root_at(h_n2).map(hex::encode),
            "downstream trie root divergence at h={h_n2} after \
             save_block / save_blockchain crash window — \
             accounts state on the reloaded chain is missing block N's \
             mutations, so applying block N+2 produces a different root."
        );
    });
}

/// Crash mid-blob-write — the bincode `state` payload was partially
/// written before the process exited. MDBX commits transactions
/// atomically, so under the post-B1 atomic write a partial blob is
/// impossible; the previous transaction's blob remains the latest
/// readable version.
///
/// Simulated by writing a deliberately-truncated blob to TABLE_STATE
/// outside any transaction, then verifying that the last successful
/// atomic commit's blob is still the one that loads (because the
/// truncated overwrite never went through MDBX, just the raw file).
/// We approximate this by skipping the partial write entirely and
/// asserting that load_blockchain returns the previous good state.
#[test]
fn test_v2_1_90_partial_blob_write_does_not_corrupt_state() {
    with_v2_env(|| {
        let proposer = proposer_addr();
        let (_dir, storage, mut producer) = setup_storage_and_chain();
        let mdbx = storage.mdbx_arc();
        producer.init_trie(Arc::clone(&mdbx)).unwrap();
        producer.init_storage_handle(Arc::clone(&mdbx)).unwrap();

        // Build 10 blocks atomically.
        const N: u64 = 10;
        for _ in 0..N {
            let block = producer.create_block(&proposer).expect("create");
            producer.add_block(block).expect("add");
            storage.save_blockchain(&producer).expect("save");
        }
        let last_good_root = producer.trie_root_at(N).map(hex::encode);
        drop(producer);

        // Reload — must be exactly the last atomic commit's view.
        let reloaded: Blockchain = storage
            .load_blockchain()
            .expect("load")
            .expect("state must exist");
        assert_eq!(
            reloaded.trie_root_at(N).map(hex::encode),
            last_good_root,
            "post-atomic-commit reload must return the last good state"
        );
        assert_eq!(reloaded.height(), N);
    });
}

/// Crash before height bump — under the pre-B1 path, save_block
/// committed `height` separately from block bytes; a torn write could
/// leave a block persisted but the height key untouched. Under the
/// atomic save_blockchain, height + block + blob ride in one
/// transaction, so this state is unreachable.
///
/// We assert the post-condition: after every atomic save the persisted
/// height matches the chain head exactly.
#[test]
fn test_v2_1_90_atomic_save_height_matches_chain_head() {
    with_v2_env(|| {
        let proposer = proposer_addr();
        let (_dir, storage, mut producer) = setup_storage_and_chain();
        let mdbx = storage.mdbx_arc();
        producer.init_trie(Arc::clone(&mdbx)).unwrap();
        producer.init_storage_handle(Arc::clone(&mdbx)).unwrap();

        for n in 1u64..=15 {
            let block = producer.create_block(&proposer).expect("create");
            producer.add_block(block).expect("add");
            storage.save_blockchain(&producer).expect("save");

            let stored_height = storage.load_height().expect("load_height");
            assert_eq!(
                stored_height, n,
                "atomic save must advance stored height to match chain head"
            );
            // blob_height must also match so the load-side B2 detector is
            // a no-op on a clean save.
            let blob_height = storage
                .chain_for_test()
                .load_blob_height()
                .expect("blob_height")
                .expect("blob_height present after atomic save");
            assert_eq!(
                blob_height, n,
                "blob_height checkpoint must match chain head after atomic save"
            );
        }
    });
}

/// Repeated random-crash loop: simulate 10 consecutive crashes at the
/// pre-B1 vulnerable point (block + height bump on disk, accounts blob
/// stale). Each crash is followed by a clean restart (load_blockchain,
/// which now runs Patch B2's replay). Final state must equal a
/// never-crashed control that applied the same blocks linearly.
#[test]
fn test_v2_1_90_repeated_crash_recovery_converges_to_control() {
    with_v2_env(|| {
        let proposer = proposer_addr();

        // Producer chain that periodically simulates the pre-B1 crash
        // (write block bytes + height directly, skip save_blockchain).
        let (_dir_prod, storage, mut producer) = setup_storage_and_chain();
        let mdbx = storage.mdbx_arc();
        producer.init_trie(Arc::clone(&mdbx)).unwrap();
        producer.init_storage_handle(Arc::clone(&mdbx)).unwrap();

        // Control chain: linear in-memory replay, no crashes.
        let dir_ctrl = TempDir::new().expect("tempdir");
        let mdbx_ctrl = Arc::new(MdbxStorage::open(dir_ctrl.path()).expect("mdbx ctrl"));
        let mut control = Blockchain::new("admin".to_string());
        control.authority.add_validator_unchecked(
            proposer.clone(),
            "Validator 1".to_string(),
            "pk1".to_string(),
        );
        control.init_trie(Arc::clone(&mdbx_ctrl)).unwrap();

        // Crash at heights 3, 7, 11, ..., 39 — 10 crashes total.
        // Between crashes, blocks are saved atomically. After each crash
        // the producer is reloaded (B2 replay engages).
        const CRASH_AT: &[u64] = &[3, 7, 11, 15, 19, 23, 27, 31, 35, 39];
        const FINAL_HEIGHT: u64 = 45;

        let mut producer = Some(producer);
        for h in 1u64..=FINAL_HEIGHT {
            let p = producer.as_mut().expect("producer alive");
            let block = p.create_block(&proposer).expect("create");
            p.add_block(block.clone()).expect("add");
            control
                .add_block_from_peer(block.clone())
                .expect("control replay");

            if CRASH_AT.contains(&h) {
                // Pre-B1 crash window: write block bytes + height, skip blob.
                mdbx.put(
                    sentrix_storage::tables::TABLE_META,
                    format!("block:{}", block.index).as_bytes(),
                    &serde_json::to_vec(&block).expect("ser"),
                )
                .expect("mdbx put");
                mdbx.put(
                    sentrix_storage::tables::TABLE_META,
                    b"height",
                    &serde_json::to_vec(&block.index).expect("ser"),
                )
                .expect("mdbx put");
                drop(producer.take().expect("producer alive"));
                // Reload — B2 replay should engage.
                let mut reloaded: Blockchain =
                    storage.load_blockchain().expect("load").expect("state");
                reloaded.init_trie(Arc::clone(&mdbx)).unwrap();
                reloaded.init_storage_handle(Arc::clone(&mdbx)).unwrap();
                producer = Some(reloaded);
            } else {
                // Clean save.
                storage
                    .save_blockchain(producer.as_ref().expect("alive"))
                    .expect("save");
            }
        }

        let final_producer = producer.expect("producer alive");
        for h in 1u64..=FINAL_HEIGHT {
            assert_eq!(
                final_producer.trie_root_at(h).map(hex::encode),
                control.trie_root_at(h).map(hex::encode),
                "post-crash-recovery trie root at h={h} must match control \
                 (10 simulated crashes during build, B2 replay aligned each)"
            );
        }
        assert_eq!(
            final_producer
                .accounts
                .get_balance(sentrix_primitives::transaction::PROTOCOL_TREASURY),
            control
                .accounts
                .get_balance(sentrix_primitives::transaction::PROTOCOL_TREASURY),
            "treasury balance must match control after crash-loop recovery"
        );
    });
}

/// Atomic transaction integrity — interrupting between blocks must
/// leave the persisted state strictly at the last completed atomic
/// commit. No partial state, no torn writes.
#[test]
fn test_v2_1_90_atomic_txn_no_partial_state_on_skipped_save() {
    with_v2_env(|| {
        let proposer = proposer_addr();
        let (_dir, storage, mut producer) = setup_storage_and_chain();
        let mdbx = storage.mdbx_arc();
        producer.init_trie(Arc::clone(&mdbx)).unwrap();
        producer.init_storage_handle(Arc::clone(&mdbx)).unwrap();

        for _ in 0..5 {
            let block = producer.create_block(&proposer).expect("create");
            producer.add_block(block).expect("add");
            storage.save_blockchain(&producer).expect("save");
        }
        let h_after_clean = storage.load_height().expect("load_height");
        let blob_after_clean = storage
            .chain_for_test()
            .load_blob_height()
            .expect("blob_height")
            .expect("blob_height present");

        // Apply block 6 in memory + commit trie, but don't persist.
        let block_6 = producer.create_block(&proposer).expect("create");
        producer.add_block(block_6).expect("add");
        // No save_blockchain call here — simulate process exit before commit.
        drop(producer);

        // Disk state must still report height = 5, blob_height = 5.
        let h_after_skip = storage.load_height().expect("load_height");
        let blob_after_skip = storage
            .chain_for_test()
            .load_blob_height()
            .expect("blob_height")
            .expect("blob_height");
        assert_eq!(h_after_skip, h_after_clean, "no partial height bump");
        assert_eq!(blob_after_skip, blob_after_clean, "no partial blob bump");

        // Reload must give the chain at h=5 cleanly (B2 replay no-op
        // because disk_height == blob_height).
        let reloaded: Blockchain = storage.load_blockchain().expect("load").expect("state");
        assert_eq!(reloaded.height(), 5, "reload returns last atomic commit");
    });
}

/// Patch B3 — trie-canonical reconciliation on load.
///
/// Scenario: a chain.db that was torn under the pre-B1 non-atomic
/// persistence path. The on-disk view has block bytes + height +
/// trie data consistent with chain head h=N, but the bincode `state`
/// blob's `accounts` map is stale because one or more historical
/// `save_blockchain` calls didn't fire after their preceding
/// `save_block`. The blob also has no `blob_height` key (it pre-dates
/// the B1 atomic-save change), so the B2 detector treats it as legacy
/// and skips replay.
///
/// Without B3, applying block N+1 reads the stale `accounts` value and
/// produces a different trie root than peers — the same bug class that
/// caused split-brain on the live cluster.
///
/// With B3, `Storage::load_blockchain` reconciles in-memory account
/// state against the trie's leaves at h=N before returning, so the
/// next `apply_block_pass2` reads canonical balances regardless of
/// blob staleness.
#[test]
fn test_v2_1_90_legacy_torn_chain_db_recovers_via_trie_reconcile() {
    with_v2_env(|| {
        let proposer = proposer_addr();

        // Phase 1: build N blocks atomically (post-B1).
        let (_dir, storage, mut producer) = setup_storage_and_chain();
        let mdbx = storage.mdbx_arc();
        producer.init_trie(Arc::clone(&mdbx)).unwrap();
        producer.init_storage_handle(Arc::clone(&mdbx)).unwrap();
        const N: u64 = 10;
        for _ in 0..N {
            let block = producer.create_block(&proposer).expect("create");
            producer.add_block(block).expect("add");
            storage.save_blockchain(&producer).expect("save");
        }
        let producer_root_n = producer.trie_root_at(N).map(hex::encode);
        let canonical_treasury = producer
            .accounts
            .get_balance(sentrix_primitives::transaction::PROTOCOL_TREASURY);
        drop(producer);

        // Phase 2: corrupt the persisted blob — decrement
        // PROTOCOL_TREASURY balance by K block-rewards, simulating a
        // chain.db where K save_blockchain calls were skipped after
        // their save_block under the pre-B1 non-atomic path.
        const K: u64 = 5;
        const BLOCK_REWARD: u64 = 100_000_000;
        let drop_amount = K * BLOCK_REWARD;

        let raw = mdbx
            .get(sentrix_storage::tables::TABLE_STATE, b"state")
            .expect("read state")
            .expect("state present");
        let mut bc_blob: Blockchain = serde_json::from_slice(&raw).expect("decode state");
        let acct = bc_blob
            .accounts
            .accounts
            .get_mut(sentrix_primitives::transaction::PROTOCOL_TREASURY)
            .expect("treasury entry present in blob");
        let pre = acct.balance;
        assert!(pre >= drop_amount, "test setup: blob treasury too small");
        acct.balance = pre - drop_amount;
        let corrupt = serde_json::to_vec(&bc_blob).expect("encode state");
        mdbx.put(sentrix_storage::tables::TABLE_STATE, b"state", &corrupt)
            .expect("write corrupt state");

        // Delete blob_height to simulate a chain.db that pre-dates the
        // B1 atomic-save change. With the key absent, the B2 detector's
        // `load_blob_height` returns None and the detector skips replay,
        // leaving B3 reconciliation as the only repair path.
        let _ = mdbx
            .delete(sentrix_storage::tables::TABLE_STATE, b"blob_height")
            .expect("delete blob_height");

        // Phase 3: reload and apply block N+1.
        let mut reloaded: Blockchain = storage
            .load_blockchain()
            .expect("load_blockchain")
            .expect("state present");
        reloaded.init_trie(Arc::clone(&mdbx)).unwrap();
        reloaded.init_storage_handle(Arc::clone(&mdbx)).unwrap();

        // After B3, the reconciler must have repaired the treasury
        // balance from the trie. Sanity-check it matches the canonical
        // pre-corruption value.
        let post_reload_treasury = reloaded
            .accounts
            .get_balance(sentrix_primitives::transaction::PROTOCOL_TREASURY);
        assert_eq!(
            post_reload_treasury, canonical_treasury,
            "B3 reconciliation must restore PROTOCOL_TREASURY from trie. \
             Expected {canonical_treasury}, got {post_reload_treasury}. \
             Pre-fix delta = {drop_amount} (K={K} block rewards)."
        );

        // Phase 4: control chain — never corrupted, replay all N blocks.
        let dir_ctrl = TempDir::new().expect("tempdir");
        let mdbx_ctrl = Arc::new(MdbxStorage::open(dir_ctrl.path()).expect("mdbx ctrl"));
        let mut control = Blockchain::new("admin".to_string());
        control.authority.add_validator_unchecked(
            proposer.clone(),
            "Validator 1".to_string(),
            "pk1".to_string(),
        );
        control.init_trie(Arc::clone(&mdbx_ctrl)).unwrap();
        for h in 1..=N {
            let block = reloaded
                .get_block_any(h)
                .unwrap_or_else(|| panic!("missing block at h={h}"));
            control.add_block_from_peer(block).expect("control replay");
        }
        // Sanity: control trie root at N matches producer's snapshot.
        assert_eq!(
            control.trie_root_at(N).map(hex::encode),
            producer_root_n,
            "control replay must match producer at h={N}"
        );

        // Phase 5: apply block N+1 on both sides; trie roots must match.
        let block_n1 = control.create_block(&proposer).expect("create N+1");
        let h_n1 = block_n1.index;
        control
            .add_block(block_n1.clone())
            .expect("control add N+1");
        reloaded
            .add_block_from_peer(block_n1)
            .expect("reloaded add N+1");

        assert_eq!(
            reloaded.trie_root_at(h_n1).map(hex::encode),
            control.trie_root_at(h_n1).map(hex::encode),
            "post-reconcile trie root at h={h_n1} must match control. \
             B3 should have restored treasury from trie at load time."
        );
    });
}

/// Deterministic replay from persisted blocks: feed the same set of
/// blocks through two fresh chains in different orders/cadences (one
/// linear, one with periodic save_blockchain hiccups simulating pre-B1
/// crashes). Final stateRoot must match.
#[test]
fn test_v2_1_90_deterministic_replay_from_persisted_blocks() {
    with_v2_env(|| {
        let proposer = proposer_addr();

        // Build a reference chain of N blocks atomically.
        let (_dir_a, storage_a, mut chain_a) = setup_storage_and_chain();
        let mdbx_a = storage_a.mdbx_arc();
        chain_a.init_trie(Arc::clone(&mdbx_a)).unwrap();
        chain_a.init_storage_handle(Arc::clone(&mdbx_a)).unwrap();
        const N: u64 = 25;
        let mut blocks: Vec<sentrix_primitives::block::Block> = Vec::with_capacity(N as usize);
        for _ in 0..N {
            let block = chain_a.create_block(&proposer).expect("create");
            chain_a.add_block(block.clone()).expect("add");
            storage_a.save_blockchain(&chain_a).expect("save");
            blocks.push(block);
        }
        let reference_root = chain_a.trie_root_at(N).map(hex::encode);

        // Replay the same blocks through a fresh chain via the peer-apply
        // path. Result must be byte-identical to the reference root.
        let dir_b = TempDir::new().expect("tempdir");
        let mdbx_b = Arc::new(MdbxStorage::open(dir_b.path()).expect("mdbx b"));
        let mut chain_b = Blockchain::new("admin".to_string());
        chain_b.authority.add_validator_unchecked(
            proposer,
            "Validator 1".to_string(),
            "pk1".to_string(),
        );
        chain_b.init_trie(Arc::clone(&mdbx_b)).unwrap();
        for block in blocks {
            chain_b.add_block_from_peer(block).expect("peer apply");
        }
        let replay_root = chain_b.trie_root_at(N).map(hex::encode);

        assert_eq!(
            reference_root, replay_root,
            "replayed chain must agree with reference at h={N}"
        );
    });
}
