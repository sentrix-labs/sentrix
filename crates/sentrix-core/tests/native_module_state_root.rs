//! Native-module state-root commitment tests (feat/native-module-state-root-commitment).
//!
//! Verifies that, post `NATIVE_STATE_IN_TRIE_HEIGHT` fork, the SRC-20
//! `ContractRegistry` and NFT `NftRegistry` are committed into the state trie
//! so their state is reflected in `state_root` — and that pre-fork behavior is
//! bit-identical to before (native state stays off-trie).
//!
//! The native registries are mutated directly (no tx) so each test isolates
//! the *commitment* path from the *apply* path and from account/fee changes:
//! two chains share identical account state and differ only in native-module
//! state, so any state_root difference is attributable to the commitment.

use sentrix_core::blockchain::Blockchain;
use sentrix_core::storage::Storage;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// Serialises the `NATIVE_STATE_IN_TRIE_HEIGHT` env var across tests in this
/// binary (process-wide state would otherwise race under the parallel runner).
static ENV_GUARD: Mutex<()> = Mutex::new(());

fn proposer_addr() -> String {
    format!("0x{}", "ab".repeat(20))
}

fn deployer_addr() -> String {
    format!("0x{}", "cd".repeat(20))
}

/// Fresh chain with an initialised trie + one authorised validator.
fn fresh_chain() -> (TempDir, Storage, Blockchain) {
    let dir = TempDir::new().expect("tempdir");
    let storage = Storage::open(dir.path().to_str().unwrap()).expect("storage open");
    let mut bc = Blockchain::new("admin".to_string());
    bc.authority
        .add_validator_unchecked(proposer_addr(), "V1".to_string(), "pk1".to_string());
    let mdbx = storage.mdbx_arc();
    bc.init_trie(Arc::clone(&mdbx)).unwrap();
    bc.init_storage_handle(Arc::clone(&mdbx)).unwrap();
    (dir, storage, bc)
}

/// Produce `n` coinbase blocks; returns the trie root at the final height.
fn mine_n(bc: &mut Blockchain, n: u64) -> Option<[u8; 32]> {
    let proposer = proposer_addr();
    let mut root = None;
    for _ in 0..n {
        let block = bc.create_block(&proposer).expect("create_block");
        bc.add_block(block).expect("add_block");
        root = bc.trie_root_at(bc.height());
    }
    root
}

/// Run `f` with `NATIVE_STATE_IN_TRIE_HEIGHT` set to `value` (or removed when
/// `None`), restoring the previous value afterward. Serialised via ENV_GUARD.
fn with_native_fork<F: FnOnce()>(value: Option<&str>, f: F) {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let prev = std::env::var("NATIVE_STATE_IN_TRIE_HEIGHT").ok();
    // SAFETY: process-wide env, serialised by ENV_GUARD (edition-2024 contract).
    unsafe {
        match value {
            Some(v) => std::env::set_var("NATIVE_STATE_IN_TRIE_HEIGHT", v),
            None => std::env::remove_var("NATIVE_STATE_IN_TRIE_HEIGHT"),
        }
    }
    f();
    unsafe {
        match prev {
            Some(v) => std::env::set_var("NATIVE_STATE_IN_TRIE_HEIGHT", v),
            None => std::env::remove_var("NATIVE_STATE_IN_TRIE_HEIGHT"),
        }
    }
}

fn deploy_src20(bc: &mut Blockchain, supply: u64, seed: &str) {
    bc.contracts
        .deploy(&deployer_addr(), "Tok", "TOK", 8, supply, 0, seed)
        .expect("src20 deploy");
}

fn deploy_nft(bc: &mut Blockchain, owner: &str, seed: &str) {
    let (cid, _) = bc
        .nft_registry
        .deploy_collection(&deployer_addr(), "C", "C", "u", None, true, true, seed)
        .expect("nft deploy");
    bc.nft_registry
        .get_collection_mut(&cid)
        .unwrap()
        .mint(&deployer_addr(), owner, 1, "", None)
        .expect("nft mint");
}

// ── 1. Pre-fork state_root behaviour is preserved ────────────

#[test]
fn pre_fork_native_state_does_not_affect_state_root() {
    with_native_fork(None, || {
        // Chain A mutates native state; chain B does not. Pre-fork the roots
        // must be identical — native state is not committed.
        let (_da, _sa, mut a) = fresh_chain();
        let (_db, _sb, mut b) = fresh_chain();
        mine_n(&mut a, 3);
        mine_n(&mut b, 3);

        deploy_src20(&mut a, 1_000, "s1");
        deploy_nft(&mut a, &proposer_addr(), "n1");

        let ra = mine_n(&mut a, 1);
        let rb = mine_n(&mut b, 1);
        assert_eq!(ra, rb, "pre-fork: native state must not change state_root");
    });
}

// ── 2 & 3. Post-fork SRC-20 / NFT changes affect state_root ──

#[test]
fn post_fork_src20_change_affects_state_root() {
    with_native_fork(Some("0"), || {
        let (_da, _sa, mut a) = fresh_chain();
        let (_db, _sb, mut b) = fresh_chain();
        mine_n(&mut a, 3);
        mine_n(&mut b, 3);

        deploy_src20(&mut a, 1_000, "s1"); // only A

        let ra = mine_n(&mut a, 1);
        let rb = mine_n(&mut b, 1);
        assert_ne!(ra, rb, "post-fork: SRC-20 state must change state_root");
    });
}

#[test]
fn post_fork_nft_change_affects_state_root() {
    with_native_fork(Some("0"), || {
        let (_da, _sa, mut a) = fresh_chain();
        let (_db, _sb, mut b) = fresh_chain();
        mine_n(&mut a, 3);
        mine_n(&mut b, 3);

        deploy_nft(&mut a, &proposer_addr(), "n1"); // only A

        let ra = mine_n(&mut a, 1);
        let rb = mine_n(&mut b, 1);
        assert_ne!(ra, rb, "post-fork: NFT state must change state_root");
    });
}

// ── 4. Replay determinism ────────────────────────────────────

#[test]
fn post_fork_replay_produces_identical_state_root() {
    with_native_fork(Some("0"), || {
        let build = || {
            let (_d, _s, mut bc) = fresh_chain();
            mine_n(&mut bc, 3);
            deploy_src20(&mut bc, 1_000, "s1");
            deploy_nft(&mut bc, &proposer_addr(), "n1");
            mine_n(&mut bc, 1)
        };
        assert_eq!(
            build(),
            build(),
            "same native ops replayed → identical root"
        );
    });
}

// ── 5 & 6. Different native states → different state_root ─────

#[test]
fn post_fork_different_src20_supply_differs() {
    with_native_fork(Some("0"), || {
        let make = |supply: u64| {
            let (_d, _s, mut bc) = fresh_chain();
            mine_n(&mut bc, 3);
            deploy_src20(&mut bc, supply, "s1");
            mine_n(&mut bc, 1)
        };
        assert_ne!(make(1_000), make(2_000));
    });
}

#[test]
fn post_fork_different_nft_owner_differs() {
    with_native_fork(Some("0"), || {
        let make = |owner: &str| {
            let (_d, _s, mut bc) = fresh_chain();
            mine_n(&mut bc, 3);
            deploy_nft(&mut bc, owner, "n1");
            mine_n(&mut bc, 1)
        };
        let alice = format!("0x{}", "11".repeat(20));
        let bob = format!("0x{}", "22".repeat(20));
        assert_ne!(make(&alice), make(&bob));
    });
}

// ── 7. HashMap insertion order does not change state_root ─────

#[test]
fn post_fork_src20_deploy_order_independent() {
    with_native_fork(Some("0"), || {
        let (_da, _sa, mut a) = fresh_chain();
        let (_db, _sb, mut b) = fresh_chain();
        mine_n(&mut a, 3);
        mine_n(&mut b, 3);

        // Same two contracts, opposite deploy order.
        deploy_src20(&mut a, 1_000, "s1");
        deploy_src20(&mut a, 2_000, "s2");
        deploy_src20(&mut b, 2_000, "s2");
        deploy_src20(&mut b, 1_000, "s1");

        let ra = mine_n(&mut a, 1);
        let rb = mine_n(&mut b, 1);
        assert_eq!(ra, rb, "deploy order must not change state_root");
    });
}

// ── 8. Empty native registry commitment is stable ────────────

#[test]
fn post_fork_empty_registry_commitment_stable() {
    with_native_fork(Some("0"), || {
        // Two fresh chains with empty registries must agree post-fork, and the
        // empty-registry root must differ from a populated one.
        let (_da, _sa, mut a) = fresh_chain();
        let (_db, _sb, mut b) = fresh_chain();
        let ra = mine_n(&mut a, 3);
        let rb = mine_n(&mut b, 3);
        assert_eq!(ra, rb, "empty native commitment must be deterministic");

        deploy_src20(&mut a, 1, "s1");
        let ra2 = mine_n(&mut a, 1);
        let rb2 = mine_n(&mut b, 1);
        assert_ne!(ra2, rb2, "empty vs populated must differ post-fork");
    });
}
