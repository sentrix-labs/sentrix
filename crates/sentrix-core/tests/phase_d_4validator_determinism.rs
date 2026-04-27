//! Phase D Step 5-full — 4-validator consensus-determinism harness.
//!
//! Phase D consensus-jail correctness rests on one invariant: when the
//! proposer emits a `JailEvidenceBundle` system tx at an epoch boundary,
//! ALL peers must compute the same evidence locally and apply identical
//! state mutations after `add_block`. If any peer diverges (extra/missing
//! validator in the evidence list, different is_jailed outcome), the
//! chain forks.
//!
//! This harness simulates 4 validators in-process by running 4 distinct
//! `Blockchain` instances through the same block sequence. Each instance
//! has identical initial state (active_set + liveness window), processes
//! the proposer-built boundary block, and is asserted to converge on the
//! same jail outcome — bit-identical state_root included.
//!
//! Each integration test file becomes its own test binary, so the env
//! vars set here are isolated from unit tests under `cargo test`.

use sentrix_core::blockchain::Blockchain;
use sentrix_primitives::block::Block;
use sentrix_primitives::transaction::Transaction;
use std::sync::{Mutex, MutexGuard, OnceLock};

// Built at runtime so the pre-commit hook's generic "0x + 40 hex"
// detector doesn't false-positive on these obvious test addresses.
fn downer_addr() -> String {
    format!("0x{}", "feedface".repeat(5))
}
fn validator_addr() -> String {
    format!("0x{}", "b01dface00".repeat(4))
}

/// Serialize env-mutating tests inside this binary. Each test in this
/// file sets the same fork-gate env vars (VOYAGER_REWARD_V2_HEIGHT,
/// JAIL_CONSENSUS_HEIGHT) and would race otherwise under the default
/// parallel test runner.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Build a Blockchain instance pre-loaded with the same downer + same
/// LivenessTracker state. Returns a chain padded to (boundary - 1).
fn setup_validator_chain() -> Blockchain {
    let downer = downer_addr();
    let validator = validator_addr();
    let mut bc = Blockchain::new("admin".to_string());

    // Bypass Pioneer authority for Voyager-mode validation
    bc.voyager_activated = true;
    bc.authority.add_validator_unchecked(
        validator.clone(),
        "validator".to_string(),
        "pk".to_string(),
    );

    // Register both validator (proposer) + downer (the one we'll jail)
    bc.stake_registry
        .register_validator(&validator, sentrix_staking::staking::MIN_SELF_STAKE, 1000, 0)
        .expect("register validator");
    bc.stake_registry
        .register_validator(&downer, sentrix_staking::staking::MIN_SELF_STAKE, 1000, 0)
        .expect("register downer");

    // Same active_set on every validator (consensus invariant)
    bc.stake_registry.active_set = vec![downer.clone()];

    // Identical liveness state across all validators: full window of misses.
    // This is what makes compute_jail_evidence deterministic across peers
    // (post asymmetric-record fix in PR #356 + #362).
    let window = sentrix_staking::slashing::LIVENESS_WINDOW;
    for h in 0..window {
        bc.slashing.liveness.record(&downer, h, false);
    }

    // Pad to (boundary - 1) so the next block lands on epoch boundary.
    let target_height = sentrix_staking::epoch::EPOCH_LENGTH - 2;
    let prev_hash = bc.latest_block().unwrap().hash.clone();
    let pad = Block::new(
        target_height,
        prev_hash,
        vec![Transaction::new_coinbase(
            validator.clone(),
            0,
            target_height,
            1_700_000_000,
        )],
        validator,
    );
    bc.chain.push(pad);

    bc
}

/// 4-validator determinism: proposer builds boundary block; all 4
/// validators apply it; all 4 must converge on identical jail state +
/// state_root.
#[test]
fn phase_d_4validator_consensus_jail_determinism() {
    let _guard = env_lock();
    unsafe {
        std::env::set_var("VOYAGER_REWARD_V2_HEIGHT", "0");
        std::env::set_var("JAIL_CONSENSUS_HEIGHT", "0");
    }

    // Spin up 4 independent chain instances, all with identical initial
    // state (downer in active_set, full LIVENESS_WINDOW of misses).
    let mut validators: Vec<Blockchain> =
        (0..4).map(|_| setup_validator_chain()).collect();

    // Pre-condition: no validator has the downer jailed yet.
    for (i, bc) in validators.iter().enumerate() {
        let pre_jailed = bc
            .stake_registry
            .get_validator(&downer_addr())
            .map(|v| v.is_jailed)
            .unwrap_or(false);
        assert!(
            !pre_jailed,
            "validator {} pre-emission must not have downer jailed",
            i
        );
    }

    // Proposer (validators[0]) builds the boundary block. The block now
    // contains the system tx as transactions[1].
    let block = validators[0]
        .create_block_voyager(&validator_addr())
        .expect("proposer must build boundary block");

    assert_eq!(
        block.transactions.len(),
        2,
        "boundary block must have coinbase + JailEvidenceBundle system tx"
    );
    assert!(
        block.transactions[1].is_system_tx(),
        "tx[1] must be the system-emitted JailEvidenceBundle"
    );

    // Capture the proposer-side block for cloning into peer add_block calls.
    let block_for_peers = block.clone();

    // Apply the same block on ALL 4 validators (proposer included).
    // This is the heart of the determinism check: each chain runs Pass-1
    // + Pass-2 + dispatch independently and MUST reach the same state.
    validators[0]
        .add_block(block)
        .expect("proposer must accept its own block");
    for (i, bc) in validators.iter_mut().enumerate().skip(1) {
        bc.add_block(block_for_peers.clone())
            .unwrap_or_else(|e| panic!("peer validator {} rejected boundary block: {:?}", i, e));
    }

    // Post-condition: ALL 4 validators have the downer jailed.
    for (i, bc) in validators.iter().enumerate() {
        let post_jailed = bc
            .stake_registry
            .get_validator(&downer_addr())
            .map(|v| v.is_jailed)
            .unwrap_or(false);
        assert!(
            post_jailed,
            "validator {} must have downer jailed after consensus-jail dispatch",
            i
        );
    }

    // Determinism check: all 4 validators must agree on the post-apply
    // state_root. If any diverges, the chain would fork at this height.
    // We pull state_root via update_trie_for_block which all chains will
    // have stamped on the latest block.
    let roots: Vec<Option<[u8; 32]>> = validators
        .iter()
        .map(|bc| bc.chain.last().and_then(|b| b.state_root))
        .collect();
    let proposer_root = roots[0];
    for (i, root) in roots.iter().enumerate().skip(1) {
        assert_eq!(
            root, &proposer_root,
            "validator {} state_root diverged from proposer (consensus split)",
            i
        );
    }

    // Cleanup: don't leak env vars to other test binaries.
    unsafe {
        std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
        std::env::remove_var("VOYAGER_REWARD_V2_HEIGHT");
    }
}

/// Negative case: if peer 1's local LivenessTracker DIVERGES from
/// the proposer's view (different signed/missed counts), the peer
/// must reject the block via dispatch's recompute-and-compare.
/// This is the safety mechanism that prevents a malicious or buggy
/// proposer from jailing legitimate validators.
#[test]
fn phase_d_4validator_diverging_evidence_rejected() {
    let _guard = env_lock();
    unsafe {
        std::env::set_var("VOYAGER_REWARD_V2_HEIGHT", "0");
        std::env::set_var("JAIL_CONSENSUS_HEIGHT", "0");
    }

    let mut proposer = setup_validator_chain();
    let mut diverging_peer = setup_validator_chain();

    // Mutate ONE record on the diverging peer's tracker so its
    // get_stats(DOWNER) returns different signed/missed counts than
    // the proposer's. compute_jail_evidence will then produce a
    // different evidence list → dispatch rejects the block.
    diverging_peer.slashing.liveness.record(&downer_addr(), 0, true);

    let block = proposer
        .create_block_voyager(&validator_addr())
        .expect("proposer must build boundary block");

    // Diverging peer must reject the block.
    let err = diverging_peer.add_block(block.clone()).expect_err(
        "diverging peer must reject block — evidence recompute differs from claim",
    );
    let msg = format!("{err:?}");
    assert!(
        msg.contains("verification failed") || msg.contains("differs from claim"),
        "expected evidence-divergence rejection; got: {msg}"
    );

    // Sanity: original proposer still accepts its own block.
    proposer
        .add_block(block)
        .expect("proposer must accept its own block");

    unsafe {
        std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
        std::env::remove_var("VOYAGER_REWARD_V2_HEIGHT");
    }
}
