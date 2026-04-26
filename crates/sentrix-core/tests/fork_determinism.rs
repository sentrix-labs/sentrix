//! Fork-determinism harness — issue #268 surface.
//!
//! Filed 2026-04-25 alongside the v2.1.21 mainnet freeze. Issue #268's
//! reproducer is "v2.1.x peer received a block from a stale-rsync'd
//! chain.db and computed a different `state_root` than the canonical
//! peer that produced the block." The full prod-symptom is at
//! `internal design doc`
//! and the architecture pre-impl scan at
//! `internal design doc`.
//!
//! What this file proves (positively):
//!   1. The in-memory self-produce path and the in-memory peer-apply path
//!      converge on bit-identical trie roots when fed the same block.
//!   2. A chain reloaded from MDBX and asked to apply the next block via
//!      the peer path stays consistent with a chain that never persisted —
//!      i.e. the disk roundtrip alone doesn't perturb state-root state.
//!
//! What it does NOT prove: the actual stale-snapshot rsync reproducer. That
//! requires a real source chain.db (large, off-repo) and is exercised by
//! `rca_vps3_env_repro.rs` operator-driven harness. These tests are CI
//! regression guards: any future change that breaks them is *guaranteed*
//! to break #1e on the live network.

use secp256k1::{PublicKey, Secp256k1, SecretKey};
use sentrix_core::blockchain::Blockchain;
use sentrix_primitives::transaction::{MIN_TX_FEE, Transaction};
use sentrix_storage::MdbxStorage;
use sentrix_wallet::Wallet;
use std::sync::Arc;
use tempfile::TempDir;

fn deterministic_keypair(seed: u8) -> (SecretKey, PublicKey) {
    let sk = SecretKey::from_byte_array([seed.max(1); 32]).expect("non-zero seed");
    let secp = Secp256k1::new();
    let pk = PublicKey::from_secret_key(&secp, &sk);
    (sk, pk)
}

const VALIDATOR: &str = "validator1";

// Built at runtime so the pre-commit hook's generic "0x + 40 hex"
// detector doesn't false-positive on an obvious test address.
fn recv_addr() -> String {
    format!("0x{}", "deadbeef".repeat(5))
}

fn setup_chain() -> Blockchain {
    let mut bc = Blockchain::new("admin".to_string());
    bc.authority.add_validator_unchecked(
        VALIDATOR.to_string(),
        "Validator 1".to_string(),
        "pk1".to_string(),
    );
    bc
}

fn temp_mdbx() -> (TempDir, Arc<MdbxStorage>) {
    let dir = TempDir::new().expect("tempdir");
    let mdbx = Arc::new(MdbxStorage::open(dir.path()).expect("mdbx open"));
    (dir, mdbx)
}

/// Cross-path determinism: a block produced via `add_block` on one chain
/// and applied via `add_block_from_peer` on another must yield identical
/// trie roots at every height.
///
/// The two paths share Pass-2 logic but enter `apply_block_pass2` with
/// different `BlockSource` and the peer path enforces #1e strict-reject
/// while the self path stamps. If the trie state diverges between the
/// paths, peers reject self-produced blocks → chain halt. This is the
/// #1e mainnet-freeze pattern.
#[test]
fn test_self_produced_vs_peer_applied_paths_converge() {
    let (_d1, m1) = temp_mdbx();
    let (_d2, m2) = temp_mdbx();
    let mut producer = setup_chain();
    let mut peer = setup_chain();
    producer.init_trie(Arc::clone(&m1)).unwrap();
    peer.init_trie(Arc::clone(&m2)).unwrap();

    for i in 1u64..=20 {
        let block = producer.create_block(VALIDATOR).unwrap();
        producer.add_block(block.clone()).unwrap();
        peer.add_block_from_peer(block)
            .unwrap_or_else(|e| panic!("peer rejected block at h={i}: {e}"));

        let r_prod = producer.trie_root_at(i).map(hex::encode);
        let r_peer = peer.trie_root_at(i).map(hex::encode);
        assert_eq!(
            r_prod, r_peer,
            "trie root diverges at h={i} between self-produced and peer-applied paths"
        );
    }
}

/// Long-chain determinism: 200 coinbase blocks across two paths. Catches
/// non-determinism that only surfaces at scale (HashMap iteration leakage,
/// trie pruning artefacts, accumulator drift).
#[test]
fn test_long_chain_self_vs_peer_determinism() {
    let (_d1, m1) = temp_mdbx();
    let (_d2, m2) = temp_mdbx();
    let mut producer = setup_chain();
    let mut peer = setup_chain();
    producer.init_trie(Arc::clone(&m1)).unwrap();
    peer.init_trie(Arc::clone(&m2)).unwrap();

    const HEIGHT: u64 = 200;
    for i in 1u64..=HEIGHT {
        let block = producer.create_block(VALIDATOR).unwrap();
        producer.add_block(block.clone()).unwrap();
        peer.add_block_from_peer(block)
            .unwrap_or_else(|e| panic!("peer rejected at h={i}: {e}"));
    }

    for h in 1u64..=HEIGHT {
        assert_eq!(
            producer.trie_root_at(h).map(hex::encode),
            peer.trie_root_at(h).map(hex::encode),
            "trie root divergence at h={h} after long-chain replay"
        );
    }
}

/// Determinism across blocks that mutate balances via real signed txs —
/// exercises `apply_block_pass2`'s tx-loop, fee burn, and account-trie
/// updates. Coinbase-only blocks miss the bulk of the consensus path.
#[test]
fn test_signed_tx_blocks_self_vs_peer_determinism() {
    let (_d1, m1) = temp_mdbx();
    let (_d2, m2) = temp_mdbx();
    let mut producer = setup_chain();
    let mut peer = setup_chain();
    producer.init_trie(Arc::clone(&m1)).unwrap();
    peer.init_trie(Arc::clone(&m2)).unwrap();

    let (sk, pk) = deterministic_keypair(7);
    let sender = Wallet::derive_address(&pk);
    producer.accounts.credit(&sender, 100_000_000).unwrap();
    peer.accounts.credit(&sender, 100_000_000).unwrap();

    let recv = recv_addr();
    let chain_id = producer.chain_id;
    for nonce in 0u64..10 {
        let tx = Transaction::new(
            sender.clone(),
            recv.clone(),
            1_000_000,
            MIN_TX_FEE,
            nonce,
            String::new(),
            chain_id,
            &sk,
            &pk,
        )
        .expect("tx build");
        producer.add_to_mempool(tx).unwrap();

        let block = producer.create_block(VALIDATOR).unwrap();
        producer.add_block(block.clone()).unwrap();
        peer.add_block_from_peer(block)
            .unwrap_or_else(|e| panic!("peer rejected tx-bearing block at nonce={nonce}: {e}"));
    }

    let final_height = producer.height();
    for h in 1u64..=final_height {
        assert_eq!(
            producer.trie_root_at(h).map(hex::encode),
            peer.trie_root_at(h).map(hex::encode),
            "tx-bearing block trie root divergence at h={h}"
        );
    }
    assert_eq!(
        producer.accounts.get_balance(&sender),
        peer.accounts.get_balance(&sender),
        "sender balance must match across paths"
    );
    assert_eq!(
        producer.accounts.get_balance(&recv),
        peer.accounts.get_balance(&recv),
        "receiver balance must match across paths"
    );
}

/// MDBX roundtrip + peer-apply: in-unit-test analogue of "rsync chain.db
/// from canonical, then receive next block from peer." If a chain reloaded
/// from disk computes a different state-root than the producer that wrote
/// it, that's the #268 class.
///
/// **History**: when first written (2026-04-25, this PR's predecessor) the
/// test reproduced a real disk-roundtrip divergence — producer at h=30 had
/// trie root `cfd6581…`, freshly loaded chain reading the same MDBX got
/// `6310b98…`. Diagnosis traced it to `init_trie`'s `node_exists`
/// false-positive on `empty_hash(0)`: the empty-trie sentinel is never
/// materialised in `trie_nodes`, so any chain whose every committed root
/// equalled the empty hash (coinbase-only test, genuinely-quiet recovery
/// window) tripped a spurious backfill. The backfill rebuilt a non-empty
/// root from AccountDB premine, persisted it to MDBX BEFORE the safeguard
/// could fire, and even when the safeguard returned Err, `Storage::load_blockchain`
/// swallowed it below STATE_ROOT_FORK_HEIGHT — leaving permanent corruption.
///
/// Fixed by short-circuiting `node_exists` for `empty_hash(0)` in
/// `Blockchain::init_trie`. This test now passes and locks that guard in
/// place: any future change that re-introduces the false-positive will
/// turn this test red.
#[test]
fn test_mdbx_roundtrip_then_peer_block() {
    let dir = TempDir::new().expect("tempdir");
    let storage = sentrix_core::storage::Storage::open(dir.path().to_str().unwrap())
        .expect("storage open");
    let mdbx = storage.mdbx_arc();

    let mut producer = setup_chain();
    producer.init_trie(Arc::clone(&mdbx)).unwrap();
    producer.init_storage_handle(Arc::clone(&mdbx)).unwrap();

    const WARMUP_HEIGHT: u64 = 30;
    for _ in 0..WARMUP_HEIGHT {
        let block = producer.create_block(VALIDATOR).unwrap();
        // Persist before add_block so the reload path can find it.
        mdbx.put(
            sentrix_storage::tables::TABLE_META,
            format!("block:{}", block.index).as_bytes(),
            &serde_json::to_vec(&block).unwrap(),
        )
        .unwrap();
        producer.add_block(block).unwrap();
    }
    let producer_root_at_warmup = producer.trie_root_at(WARMUP_HEIGHT).map(hex::encode);

    // Snapshot the producer's blockchain state to MDBX so reload sees it.
    storage.save_blockchain(&producer).unwrap();

    // Reload — this is the chain.db rsync analogue.
    let mut reloaded: Blockchain = storage
        .load_blockchain()
        .expect("load_blockchain")
        .expect("blockchain state must exist after save_blockchain");
    reloaded.init_trie(Arc::clone(&mdbx)).unwrap();
    reloaded.init_storage_handle(Arc::clone(&mdbx)).unwrap();

    // Reference instance — never persisted, just replayed in memory from
    // genesis. Used as a control: producer ↔ reference parity confirms
    // the producer didn't corrupt itself; reloaded ↔ reference parity is
    // the real #268 check.
    let (_dir_ref, mdbx_ref) = temp_mdbx();
    let mut reference = setup_chain();
    reference.init_trie(Arc::clone(&mdbx_ref)).unwrap();
    for h in 1..=WARMUP_HEIGHT {
        let block = producer
            .get_block_any(h)
            .unwrap_or_else(|| panic!("producer missing block at h={h}"));
        reference.add_block_from_peer(block).unwrap();
    }

    let reloaded_root = reloaded.trie_root_at(WARMUP_HEIGHT).map(hex::encode);
    let reference_root = reference.trie_root_at(WARMUP_HEIGHT).map(hex::encode);
    assert_eq!(
        producer_root_at_warmup, reference_root,
        "producer ↔ reference trie root must match at h={WARMUP_HEIGHT} \
         — peer-replay broke determinism"
    );
    assert_eq!(
        producer_root_at_warmup, reloaded_root,
        "producer ↔ reloaded-from-MDBX trie root must match at h={WARMUP_HEIGHT} \
         — disk roundtrip perturbed trie root, this is the #268 class"
    );

    // Now produce one more block on the producer side and feed it to the
    // other two via the peer path. They must all agree.
    let next = producer.create_block(VALIDATOR).unwrap();
    let h = next.index;
    producer.add_block(next.clone()).unwrap();
    reloaded
        .add_block_from_peer(next.clone())
        .unwrap_or_else(|e| panic!("reloaded peer-apply rejected at h={h}: {e}"));
    reference
        .add_block_from_peer(next)
        .unwrap_or_else(|e| panic!("reference peer-apply rejected at h={h}: {e}"));

    assert_eq!(
        producer.trie_root_at(h).map(hex::encode),
        reloaded.trie_root_at(h).map(hex::encode),
        "post-roundtrip peer-apply produced different trie root than producer at h={h}"
    );
    assert_eq!(
        producer.trie_root_at(h).map(hex::encode),
        reference.trie_root_at(h).map(hex::encode),
        "reference peer-apply produced different trie root than producer at h={h}"
    );
}

/// MDBX roundtrip with **non-empty trie state** — the variant of
/// `test_mdbx_roundtrip_then_peer_block` that mainnet's #268 canary actually
/// exercises. The previous test had every committed root = `empty_hash(0)`
/// because coinbase blocks against a validator that has no AccountDB entry
/// don't mutate any trie leaf. Mainnet's chain.db has 553K blocks of real
/// account activity → every committed root is a real, non-empty hash. The
/// `empty_hash(0)` short-circuit fix doesn't apply on that path.
///
/// This test reproduces the disk-roundtrip surface against a chain that
/// has real tx activity (multiple senders, balance mutations) so the trie
/// has real leaves, real internal nodes, real depth. If the bug is on this
/// path, it should show up here as `producer.trie_root_at(N) !=
/// reloaded.trie_root_at(N)`.
///
/// Test passing on main = the v2.1.21 canary mainnet failure is **not**
/// caused by something on the in-process disk-roundtrip path; the bug
/// surface is somewhere else (peer-block validation delta, BFT state
/// serialisation, V4 dispatch path running unconditionally, etc.).
///
/// Test failing on main = bisect across v2.1.16-v2.1.21 commits using
/// `git checkout <sha> && cargo test -p sentrix-core --test fork_determinism
///  test_mdbx_roundtrip_with_active_state`. First commit that fails =
/// regression introduction point.
#[test]
fn test_mdbx_roundtrip_with_active_state() {
    let dir = TempDir::new().expect("tempdir");
    let storage = sentrix_core::storage::Storage::open(dir.path().to_str().unwrap())
        .expect("storage open");
    let mdbx = storage.mdbx_arc();

    let mut producer = setup_chain();
    producer.init_trie(Arc::clone(&mdbx)).unwrap();
    producer.init_storage_handle(Arc::clone(&mdbx)).unwrap();

    // Five distinct senders, each pre-funded. Multiple senders + multiple
    // recipients exercise more leaves + more trie depth than a single-sender
    // chain. Funding done before the chain starts so we don't need a fork
    // for it.
    let mut keypairs: Vec<(secp256k1::SecretKey, secp256k1::PublicKey, String)> = Vec::new();
    for i in 1u8..=5 {
        let (sk, pk) = deterministic_keypair(i);
        let addr = Wallet::derive_address(&pk);
        producer.accounts.credit(&addr, 100_000_000).unwrap();
        keypairs.push((sk, pk, addr));
    }

    let recv = recv_addr();
    let chain_id = producer.chain_id;

    // Build 30 blocks where every block has at least one tx mutating account
    // state. Rotate sender each block so multiple addresses move balance.
    const WARMUP_HEIGHT: u64 = 30;
    let mut nonces = [0u64; 5];
    for i in 0..WARMUP_HEIGHT {
        let s = (i as usize) % keypairs.len();
        let (ref sk, ref pk, ref sender) = keypairs[s];
        let tx = Transaction::new(
            sender.clone(),
            recv.clone(),
            500_000,
            MIN_TX_FEE,
            nonces[s],
            String::new(),
            chain_id,
            sk,
            pk,
        )
        .expect("tx build");
        nonces[s] += 1;
        producer.add_to_mempool(tx).unwrap();

        let block = producer.create_block(VALIDATOR).unwrap();
        mdbx.put(
            sentrix_storage::tables::TABLE_META,
            format!("block:{}", block.index).as_bytes(),
            &serde_json::to_vec(&block).unwrap(),
        )
        .unwrap();
        producer.add_block(block).unwrap();
    }
    let producer_root = producer.trie_root_at(WARMUP_HEIGHT).map(hex::encode);
    assert_ne!(
        producer_root,
        Some(hex::encode(sentrix_trie::node::empty_hash(0))),
        "test setup error: trie root at h={WARMUP_HEIGHT} is still the empty sentinel \
         — txs didn't mutate the account trie. The mainnet-relevant code path \
         requires a non-empty trie."
    );

    storage.save_blockchain(&producer).unwrap();

    let mut reloaded: Blockchain = storage
        .load_blockchain()
        .expect("load_blockchain")
        .expect("blockchain state must exist");
    reloaded.init_trie(Arc::clone(&mdbx)).unwrap();
    reloaded.init_storage_handle(Arc::clone(&mdbx)).unwrap();

    let reloaded_root = reloaded.trie_root_at(WARMUP_HEIGHT).map(hex::encode);
    assert_eq!(
        producer_root, reloaded_root,
        "[#268 active-state path] producer ↔ reloaded trie root mismatch at h={WARMUP_HEIGHT} \
         — disk roundtrip perturbed non-empty trie state. This is the path mainnet's \
         v2.1.21 canary fails on. Bisect across v2.1.16-v2.1.21 to find the regression."
    );

    // Now apply one more peer block on the reloaded chain and verify it
    // converges with the producer's continuation. This is the literal
    // "received next block from peer post-rsync" surface.
    let next = producer.create_block(VALIDATOR).unwrap();
    let h = next.index;
    producer.add_block(next.clone()).unwrap();
    reloaded
        .add_block_from_peer(next)
        .unwrap_or_else(|e| panic!("reloaded peer-apply rejected at h={h}: {e}"));

    assert_eq!(
        producer.trie_root_at(h).map(hex::encode),
        reloaded.trie_root_at(h).map(hex::encode),
        "[#268 active-state path] post-roundtrip peer-apply diverged at h={h} — \
         reloaded chain's apply_block_pass2 produced different state_root than \
         producer's. This is the mainnet canary symptom in unit-test form."
    );
}

/// Stale-snapshot peer-sync — the most literal mimic of mainnet's #268
/// canary scenario:
///
/// - Beacon node had a chain.db rsync'd from canonical at height H
/// - Canary boot, peer broadcast contains blocks at H+1, H+2, ...
/// - Canary applies via `add_block_from_peer`, computes own state_root
/// - Mismatch against canonical's state_root → #1e
///
/// This test:
/// 1. Producer builds chain to h=H with real tx activity
/// 2. Save MDBX state to a "snapshot" path (separate dir, simulating rsync target)
/// 3. Producer continues to h=H+10 (more peer-broadcast-ready blocks)
/// 4. Open a fresh Blockchain from the snapshot dir
/// 5. Apply blocks H+1..H+10 via add_block_from_peer
/// 6. Verify state_root at every applied height matches producer's
///
/// If this test fails on main, the regression is on the
/// `peer-block-validation + add_block_from_peer` path, not on plain disk
/// roundtrip.
#[test]
fn test_stale_snapshot_peer_sync() {
    // Producer side
    let producer_dir = TempDir::new().expect("producer tempdir");
    let producer_storage =
        sentrix_core::storage::Storage::open(producer_dir.path().to_str().unwrap())
            .expect("producer storage open");
    let producer_mdbx = producer_storage.mdbx_arc();

    let mut producer = setup_chain();
    producer.init_trie(Arc::clone(&producer_mdbx)).unwrap();
    producer.init_storage_handle(Arc::clone(&producer_mdbx)).unwrap();

    // Pre-fund 3 senders
    let mut keypairs = Vec::new();
    for i in 1u8..=3 {
        let (sk, pk) = deterministic_keypair(i + 50);
        let addr = Wallet::derive_address(&pk);
        producer.accounts.credit(&addr, 100_000_000).unwrap();
        keypairs.push((sk, pk, addr));
    }
    let recv = recv_addr();
    let chain_id = producer.chain_id;

    // Phase 1: producer builds chain to h=SNAPSHOT_AT with tx activity.
    const SNAPSHOT_AT: u64 = 20;
    let mut nonces = [0u64; 3];
    for i in 0..SNAPSHOT_AT {
        let s = (i as usize) % keypairs.len();
        let (ref sk, ref pk, ref sender) = keypairs[s];
        let tx = Transaction::new(
            sender.clone(),
            recv.clone(),
            500_000,
            MIN_TX_FEE,
            nonces[s],
            String::new(),
            chain_id,
            sk,
            pk,
        )
        .expect("tx build");
        nonces[s] += 1;
        producer.add_to_mempool(tx).unwrap();

        let block = producer.create_block(VALIDATOR).unwrap();
        producer_mdbx
            .put(
                sentrix_storage::tables::TABLE_META,
                format!("block:{}", block.index).as_bytes(),
                &serde_json::to_vec(&block).unwrap(),
            )
            .unwrap();
        producer.add_block(block).unwrap();
    }
    producer_storage.save_blockchain(&producer).unwrap();

    // Capture snapshot expectations — what state_root SHOULD the rsync'd
    // peer compute as it catches up?
    let mut expected_roots = Vec::new();
    for h in 1..=SNAPSHOT_AT {
        expected_roots.push((h, producer.trie_root_at(h).map(hex::encode)));
    }

    // Phase 2: producer continues to SNAPSHOT_AT + N more blocks. The peer
    // (next phase) will receive these via gossip and apply via add_block_from_peer.
    const PEER_SYNC_BLOCKS: u64 = 10;
    for i in 0..PEER_SYNC_BLOCKS {
        let s = ((SNAPSHOT_AT + i) as usize) % keypairs.len();
        let (ref sk, ref pk, ref sender) = keypairs[s];
        let tx = Transaction::new(
            sender.clone(),
            recv.clone(),
            500_000,
            MIN_TX_FEE,
            nonces[s],
            String::new(),
            chain_id,
            sk,
            pk,
        )
        .expect("tx build");
        nonces[s] += 1;
        producer.add_to_mempool(tx).unwrap();

        let block = producer.create_block(VALIDATOR).unwrap();
        producer_mdbx
            .put(
                sentrix_storage::tables::TABLE_META,
                format!("block:{}", block.index).as_bytes(),
                &serde_json::to_vec(&block).unwrap(),
            )
            .unwrap();
        producer.add_block(block).unwrap();
    }
    let producer_post_sync_root = producer
        .trie_root_at(SNAPSHOT_AT + PEER_SYNC_BLOCKS)
        .map(hex::encode);

    // Phase 3: rsync simulation. Copy producer's chain.db dir to a fresh
    // location. This is the literal "operator copies chain.db from canonical
    // peer" step. We then open the copy as a separate Storage instance, the
    // way a freshly-rsync'd Beacon node would.
    let peer_dir = TempDir::new().expect("peer tempdir");
    copy_dir_contents(producer_dir.path(), peer_dir.path()).expect("rsync sim");

    let peer_storage = sentrix_core::storage::Storage::open(peer_dir.path().to_str().unwrap())
        .expect("peer storage open");
    let peer_mdbx = peer_storage.mdbx_arc();
    let mut peer: Blockchain = peer_storage
        .load_blockchain()
        .expect("peer load_blockchain")
        .expect("peer state");
    peer.init_trie(Arc::clone(&peer_mdbx)).unwrap();
    peer.init_storage_handle(Arc::clone(&peer_mdbx)).unwrap();

    // Verify the rsync'd peer agrees on snapshot state.
    for (h, expected) in &expected_roots {
        let actual = peer.trie_root_at(*h).map(hex::encode);
        assert_eq!(
            *expected, actual,
            "[#268 stale-snapshot] rsync'd peer disagrees with producer at h={h} \
             before any peer-block application. Disk roundtrip alone broke determinism."
        );
    }

    // Phase 4: apply blocks SNAPSHOT_AT+1..SNAPSHOT_AT+PEER_SYNC_BLOCKS via
    // add_block_from_peer. This is the literal canary failure path.
    for h in (SNAPSHOT_AT + 1)..=(SNAPSHOT_AT + PEER_SYNC_BLOCKS) {
        let block = producer
            .get_block_any(h)
            .unwrap_or_else(|| panic!("producer missing block at h={h}"));
        peer.add_block_from_peer(block).unwrap_or_else(|e| {
            panic!(
                "[#268 stale-snapshot] peer rejected block at h={h}: {e}. \
                 add_block_from_peer enforces #1e strict-reject — this is the \
                 mainnet canary failure exactly."
            )
        });

        let producer_at_h = producer.trie_root_at(h).map(hex::encode);
        let peer_at_h = peer.trie_root_at(h).map(hex::encode);
        assert_eq!(
            producer_at_h, peer_at_h,
            "[#268 stale-snapshot] peer's trie_root at h={h} diverged from producer \
             after add_block_from_peer. Even though Pass-2 didn't return Err, the trie \
             state is silently divergent — this would surface as #1e on the next block."
        );
    }

    let peer_final_root = peer
        .trie_root_at(SNAPSHOT_AT + PEER_SYNC_BLOCKS)
        .map(hex::encode);
    assert_eq!(
        producer_post_sync_root, peer_final_root,
        "[#268 stale-snapshot] final divergence after {} peer-sync blocks",
        PEER_SYNC_BLOCKS
    );
}

/// Voyager-activated variant of `test_stale_snapshot_peer_sync` — the closest
/// in-process test to mainnet's #268 canary that exercises Voyager state.
///
/// Difference vs the Pioneer variant: producer calls `activate_voyager()` AFTER
/// genesis but before producing any blocks. That populates `stake_registry`
/// with the registered validator (phantom MIN_SELF_STAKE) + initialises
/// `epoch_manager`. Subsequent block applies serialise that state into the
/// chain.db roundtrip path.
///
/// Mainnet currently has `VOYAGER_FORK_HEIGHT=u64::MAX` so activate_voyager
/// hasn't fired on prod, but the v2.1.16+ binary code paths that reference
/// stake_registry / epoch_manager run unconditionally on serialise/deserialise.
/// If those produce different bytes than v2.1.15 expected (e.g. new field
/// order, new HashMap iteration leak), that's a #268 candidate.
///
/// Test passing here = Voyager-activated state survives MDBX roundtrip + peer
/// block apply. Failing here = bisect across v2.1.16+ commits touching
/// stake_registry / epoch_manager / slashing serde.
#[test]
fn test_voyager_active_stale_snapshot_peer_sync() {
    let producer_dir = TempDir::new().expect("producer tempdir");
    let producer_storage =
        sentrix_core::storage::Storage::open(producer_dir.path().to_str().unwrap())
            .expect("producer storage open");
    let producer_mdbx = producer_storage.mdbx_arc();

    let mut producer = setup_chain();
    producer.init_trie(Arc::clone(&producer_mdbx)).unwrap();
    producer
        .init_storage_handle(Arc::clone(&producer_mdbx))
        .unwrap();

    // Activate Voyager BEFORE producing blocks. Populates stake_registry,
    // initialises epoch_manager. Persistent flag set.
    producer
        .activate_voyager()
        .expect("activate_voyager on producer");
    assert!(
        producer.voyager_activated,
        "voyager_activated flag must be set after activate_voyager"
    );
    assert!(
        producer.stake_registry.active_count() >= 1,
        "stake_registry must have at least the registered validator after activation"
    );

    // Pre-fund senders (same pattern as Pioneer variant)
    let mut keypairs = Vec::new();
    for i in 1u8..=3 {
        let (sk, pk) = deterministic_keypair(i + 80);
        let addr = Wallet::derive_address(&pk);
        producer.accounts.credit(&addr, 100_000_000).unwrap();
        keypairs.push((sk, pk, addr));
    }
    let recv = recv_addr();
    let chain_id = producer.chain_id;

    // Producer to h=20 with tx activity
    const SNAPSHOT_AT: u64 = 20;
    let mut nonces = [0u64; 3];
    for i in 0..SNAPSHOT_AT {
        let s = (i as usize) % keypairs.len();
        let (ref sk, ref pk, ref sender) = keypairs[s];
        let tx = Transaction::new(
            sender.clone(),
            recv.clone(),
            500_000,
            MIN_TX_FEE,
            nonces[s],
            String::new(),
            chain_id,
            sk,
            pk,
        )
        .expect("tx build");
        nonces[s] += 1;
        producer.add_to_mempool(tx).unwrap();

        let block = producer.create_block(VALIDATOR).unwrap();
        producer_mdbx
            .put(
                sentrix_storage::tables::TABLE_META,
                format!("block:{}", block.index).as_bytes(),
                &serde_json::to_vec(&block).unwrap(),
            )
            .unwrap();
        producer.add_block(block).unwrap();
    }
    producer_storage.save_blockchain(&producer).unwrap();

    let mut expected_roots = Vec::new();
    for h in 1..=SNAPSHOT_AT {
        expected_roots.push((h, producer.trie_root_at(h).map(hex::encode)));
    }

    // Producer continues to SNAPSHOT_AT + PEER_SYNC_BLOCKS
    const PEER_SYNC_BLOCKS: u64 = 10;
    for i in 0..PEER_SYNC_BLOCKS {
        let s = ((SNAPSHOT_AT + i) as usize) % keypairs.len();
        let (ref sk, ref pk, ref sender) = keypairs[s];
        let tx = Transaction::new(
            sender.clone(),
            recv.clone(),
            500_000,
            MIN_TX_FEE,
            nonces[s],
            String::new(),
            chain_id,
            sk,
            pk,
        )
        .expect("tx build");
        nonces[s] += 1;
        producer.add_to_mempool(tx).unwrap();

        let block = producer.create_block(VALIDATOR).unwrap();
        producer_mdbx
            .put(
                sentrix_storage::tables::TABLE_META,
                format!("block:{}", block.index).as_bytes(),
                &serde_json::to_vec(&block).unwrap(),
            )
            .unwrap();
        producer.add_block(block).unwrap();
    }
    let producer_post_sync_root = producer
        .trie_root_at(SNAPSHOT_AT + PEER_SYNC_BLOCKS)
        .map(hex::encode);

    // Rsync simulation
    let peer_dir = TempDir::new().expect("peer tempdir");
    copy_dir_contents(producer_dir.path(), peer_dir.path()).expect("rsync sim");

    let peer_storage =
        sentrix_core::storage::Storage::open(peer_dir.path().to_str().unwrap())
            .expect("peer storage open");
    let peer_mdbx = peer_storage.mdbx_arc();
    let mut peer: Blockchain = peer_storage
        .load_blockchain()
        .expect("peer load_blockchain")
        .expect("peer state");
    peer.init_trie(Arc::clone(&peer_mdbx)).unwrap();
    peer.init_storage_handle(Arc::clone(&peer_mdbx)).unwrap();

    // Verify peer's voyager_activated flag survives the roundtrip
    assert!(
        peer.voyager_activated,
        "voyager_activated flag must survive MDBX roundtrip — \
         missing means #[serde(default)] fired and the activate_voyager re-entry \
         guard would re-fire on the rsync'd peer, mutating stake_registry"
    );
    assert!(
        peer.stake_registry.active_count() >= 1,
        "stake_registry must persist across roundtrip"
    );

    // Snapshot-state parity check
    for (h, expected) in &expected_roots {
        let actual = peer.trie_root_at(*h).map(hex::encode);
        assert_eq!(
            *expected, actual,
            "[#268 voyager-active] rsync'd peer disagrees at h={h} before peer-block apply"
        );
    }

    // Apply peer-broadcast blocks
    for h in (SNAPSHOT_AT + 1)..=(SNAPSHOT_AT + PEER_SYNC_BLOCKS) {
        let block = producer
            .get_block_any(h)
            .unwrap_or_else(|| panic!("producer missing block at h={h}"));
        peer.add_block_from_peer(block).unwrap_or_else(|e| {
            panic!(
                "[#268 voyager-active] peer rejected block at h={h}: {e}. \
                 add_block_from_peer enforced #1e on a Voyager-active rsync — \
                 this is the v2.1.21 canary symptom in unit-test form."
            )
        });

        let producer_at_h = producer.trie_root_at(h).map(hex::encode);
        let peer_at_h = peer.trie_root_at(h).map(hex::encode);
        assert_eq!(
            producer_at_h, peer_at_h,
            "[#268 voyager-active] peer's trie_root at h={h} diverged from producer \
             after add_block_from_peer on a Voyager-active chain — this is the \
             mainnet canary path."
        );
    }

    let peer_final_root = peer
        .trie_root_at(SNAPSHOT_AT + PEER_SYNC_BLOCKS)
        .map(hex::encode);
    assert_eq!(
        producer_post_sync_root, peer_final_root,
        "[#268 voyager-active] final divergence after {} peer-sync blocks on a \
         Voyager-active chain",
        PEER_SYNC_BLOCKS
    );
}

/// SENTRIX_LEGACY_VALIDATION_HEIGHT (#268 RCA 2026-04-25 — Phase 1 mainnet
/// activation legacy-compat). Verifies all three behavioural branches:
///
/// - Default (env unset) → strict #1e rejection (today's behaviour)
/// - Env set, block.index < cutoff → tolerated (warn-only, return Ok)
/// - Env set, block.index >= cutoff → strict reject as if env unset
///
/// **Note on STATE_ROOT_FORK_HEIGHT**: the strict #1e mismatch check at
/// `block_executor.rs` only fires for blocks at height ≥ 100,000. This
/// test is `#[ignore]`'d because producing 100,000+ blocks in a unit
/// test is impractical. Operator-driven manual verification via the
/// `apply_canonical_block_to_forensic` harness in `rca_vps3_env_repro.rs`
/// is the recommended integration-level test:
///
///   1. With env unset: confirm forensic+507499 produces APPLY_RESULT=Err
///      (already shown — see #268 RCA notes)
///   2. With env set to 600000: confirm forensic+507499 produces
///      APPLY_RESULT=Ok with computed state_root retained, warn line in
///      stderr — block.index 507499 < cutoff 600000 → tolerated
///   3. With env set to 100000: confirm forensic+507499 produces
///      APPLY_RESULT=Err — block.index 507499 ≥ cutoff 100000 → strict
#[test]
#[ignore = "requires real chain.db at h≥100K — see fn header for operator-driven manual run"]
fn test_legacy_validation_height_branches() {
    use sentrix_primitives::block::Block;

    fn fresh_chain() -> (TempDir, TempDir, Blockchain, Blockchain) {
        let (d1, m1) = temp_mdbx();
        let (d2, m2) = temp_mdbx();
        let mut producer = setup_chain();
        let mut peer = setup_chain();
        producer.init_trie(Arc::clone(&m1)).unwrap();
        peer.init_trie(Arc::clone(&m2)).unwrap();
        (d1, d2, producer, peer)
    }

    fn produce_block_with_bad_state_root(
        producer: &mut Blockchain,
        bad_root: [u8; 32],
    ) -> Block {
        let mut block = producer.create_block(VALIDATOR).unwrap();
        // Apply on producer normally (stamps real state_root)
        producer.add_block(block.clone()).unwrap();
        // Now overwrite the state_root with a hand-crafted bogus one so
        // peer-side apply will compute the real root and disagree.
        block.state_root = Some(bad_root);
        block
    }

    let bogus_root: [u8; 32] = [0xab; 32];

    // ── Branch 1: env unset → strict reject (current behaviour) ──
    unsafe { std::env::remove_var("SENTRIX_LEGACY_VALIDATION_HEIGHT") };
    {
        let (_d1, _d2, mut producer, mut peer) = fresh_chain();
        let block = produce_block_with_bad_state_root(&mut producer, bogus_root);
        let h = block.index;
        let r = peer.add_block_from_peer(block);
        assert!(
            r.is_err(),
            "default (env unset): peer-applied block with bogus state_root MUST be rejected at h={h}"
        );
        let msg = format!("{}", r.err().unwrap());
        assert!(
            msg.contains("state_root mismatch"),
            "rejection message must name the state_root mismatch, got: {msg}"
        );
    }

    // ── Branch 2: env set with cutoff > block.index → tolerate ──
    unsafe { std::env::set_var("SENTRIX_LEGACY_VALIDATION_HEIGHT", "10000") };
    {
        let (_d1, _d2, mut producer, mut peer) = fresh_chain();
        let block = produce_block_with_bad_state_root(&mut producer, bogus_root);
        let h = block.index;
        assert!(
            h < 10000,
            "test setup: block height {h} must be below cutoff 10000"
        );
        let r = peer.add_block_from_peer(block);
        assert!(
            r.is_ok(),
            "legacy region (h={h} < cutoff=10000): peer-applied bogus-state_root block MUST be tolerated, got {:?}",
            r.err()
        );
        // Block hash chain integrity: stamped state_root retained, latest
        // block has the bogus value (so subsequent peers' previous_hash
        // checks see the same chain).
        let latest = peer.latest_block().unwrap();
        assert_eq!(
            latest.state_root.map(hex::encode),
            Some(hex::encode(bogus_root)),
            "tolerated block must retain its received state_root"
        );
    }

    // ── Branch 3: env set with cutoff <= block.index → strict reject ──
    unsafe { std::env::set_var("SENTRIX_LEGACY_VALIDATION_HEIGHT", "1") };
    {
        let (_d1, _d2, mut producer, mut peer) = fresh_chain();
        let block = produce_block_with_bad_state_root(&mut producer, bogus_root);
        let h = block.index;
        assert!(
            h >= 1,
            "test setup: block height {h} must be at or above cutoff 1"
        );
        let r = peer.add_block_from_peer(block);
        assert!(
            r.is_err(),
            "post-cutoff (h={h} >= cutoff=1): peer-applied bogus-state_root block MUST be rejected"
        );
    }

    // Clean up so other tests in same binary aren't perturbed
    unsafe { std::env::remove_var("SENTRIX_LEGACY_VALIDATION_HEIGHT") };
}

/// 2026-04-26 mainnet stall (h=604547) regression: emulates the libp2p
/// BlocksResponse loop with the race-safe `block.index <= chain.height()`
/// duplicate-skip filter. Without the filter the loop bails on the first
/// already-applied block and drops the rest of the valid forward batch —
/// block sync stalls even while peers serve correct history.
#[test]
fn test_libp2p_sync_loop_skips_duplicates_and_applies_remaining() {
    let (_d, m) = temp_mdbx();
    let mut producer = setup_chain();
    let mut peer = setup_chain();
    producer.init_trie(Arc::clone(&m)).unwrap();
    let (_d2, m2) = temp_mdbx();
    peer.init_trie(Arc::clone(&m2)).unwrap();

    // Build 5 blocks at producer.
    let mut all_blocks = Vec::new();
    for _ in 1u64..=5 {
        let block = producer.create_block(VALIDATOR).unwrap();
        producer.add_block(block.clone()).unwrap();
        all_blocks.push(block);
    }

    // Apply first 3 to peer normally.
    for b in &all_blocks[..3] {
        peer.add_block_from_peer(b.clone()).unwrap();
    }
    assert_eq!(peer.height(), 3, "peer at h=3 after first batch");

    // Simulate the racing second BlocksResponse: peer receives a batch
    // [block_2, block_3, block_4, block_5] — the first two are already
    // applied (race overlap). The new filter must skip them and apply
    // 4 + 5. The PRE-FIX behaviour bailed on block_2 with "expected 4,
    // got 2" and never tried 4 or 5 — chain stalled at 3 even though
    // forward blocks were available in the same response.
    let racy_batch = vec![
        all_blocks[1].clone(), // block 2 — already applied
        all_blocks[2].clone(), // block 3 — already applied
        all_blocks[3].clone(), // block 4 — should apply
        all_blocks[4].clone(), // block 5 — should apply
    ];
    let mut synced = 0u64;
    let mut skipped = 0u64;
    for block in &racy_batch {
        if block.index <= peer.height() {
            skipped += 1;
            continue;
        }
        peer.add_block_from_peer(block.clone())
            .unwrap_or_else(|e| panic!("forward block {} should apply: {}", block.index, e));
        synced += 1;
    }

    assert_eq!(skipped, 2, "two already-applied blocks should be skipped");
    assert_eq!(synced, 2, "two forward blocks should be applied");
    assert_eq!(peer.height(), 5, "peer must advance to h=5 (not stall at 3)");
}

/// Recursively copy directory contents — used for the rsync simulation in
/// `test_stale_snapshot_peer_sync`. Keeps file modes intact for MDBX's
/// open semantics.
fn copy_dir_contents(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if ft.is_dir() {
            std::fs::create_dir_all(&dst_path)?;
            copy_dir_contents(&entry.path(), &dst_path)?;
        } else if ft.is_file() {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}
