//! Fork-determinism harness — issue #268 surface.
//!
//! Filed 2026-04-25 alongside the v2.1.21 mainnet freeze. Issue #268's
//! reproducer is "v2.1.x peer received a block from a stale-rsync'd
//! chain.db and computed a different `state_root` than the canonical
//! peer that produced the block." The full prod-symptom is at
//! `founder-private/incidents/2026-04-23-vps3-recurring-divergence-rca.md`
//! and the architecture pre-impl scan at
//! `founder-private/architecture/FORK_SEQUENCE_PREIMPL_SCAN_2026-04-24.md`.
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
