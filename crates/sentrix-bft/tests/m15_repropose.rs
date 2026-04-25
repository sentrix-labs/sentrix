// m15_repropose.rs
//
// V2 M-15 locked-block re-propose: integration tests that simulate
// today's 2026-04-25 mainnet livelock pattern.
//
// Scenario being pinned: round R forms a 2/3+ prevote supermajority
// on hash H but precommits don't reach supermajority (network drop,
// timing, signature path bug). Round R+1 advances. The new round's
// proposer is locked on H. With M-15 wired correctly, that proposer
// re-broadcasts the cached block bytes for H — peers prevote H (lock
// matches), precommit succeeds, chain finalizes block H at round R+1.
//
// Without M-15 wiring, the proposer builds a fresh block H' instead.
// Peers (locked on H) prevote nil because H' != H. Nil-supermajority
// → SkipRound → next round repeats. Livelock.
//
// This file tests the engine in isolation. If the integration tests
// pass on `main`, the engine's lock-cache lifecycle is sound and a
// production livelock root-causes elsewhere (validator-loop wiring,
// libp2p, signature handling, BFT signer-set composition post-DPoS
// migration — all out of scope for this harness).
//
// Reference: internal design doc §9

use sentrix_bft::{BftAction, BftEngine, BftPhase, Precommit, Prevote};
use sentrix_staking::StakeRegistry;
use sentrix_staking::staking::MIN_SELF_STAKE;

/// 4-validator stake registry, equal stakes, all active.
fn setup_four_val_registry() -> (StakeRegistry, Vec<String>, u64, u64) {
    let mut reg = StakeRegistry::new();
    let addresses: Vec<String> = (1..=4).map(|i| format!("0xval{:03}", i)).collect();
    for addr in &addresses {
        reg.register_validator(addr, MIN_SELF_STAKE, 1000, 0).unwrap();
    }
    reg.update_active_set();

    let total_stake: u64 = reg
        .active_set
        .iter()
        .filter_map(|a| reg.get_validator(a))
        .map(|v| v.total_stake())
        .sum();

    let per_val = total_stake / addresses.len() as u64;
    (reg, addresses, per_val, total_stake)
}

fn setup_four_engines(addresses: &[String], total_stake: u64, height: u64) -> Vec<BftEngine> {
    addresses
        .iter()
        .map(|addr| BftEngine::new(height, addr.clone(), total_stake))
        .collect()
}

fn mk_prevote(height: u64, round: u32, block_hash: Option<String>, validator: &str) -> Prevote {
    Prevote {
        height,
        round,
        block_hash,
        validator: validator.to_string(),
        signature: vec![],
    }
}

/// Drive each engine through round 0 up to (but not past) precommit
/// supermajority: stash bytes, deliver proposal, fan 4 prevotes.
/// On return every engine has `locked_hash = Some(block_hash)`,
/// `locked_block = Some(block_bytes)`, phase = Precommit, but no
/// FinalizeBlock has been emitted (caller controls precommit fan-in).
fn drive_round_to_lock(
    engines: &mut [BftEngine],
    reg: &StakeRegistry,
    addresses: &[String],
    height: u64,
    round: u32,
    block_hash: &str,
    block_bytes: &[u8],
    per_val: u64,
) {
    let proposer = reg
        .weighted_proposer(height, round)
        .expect("active set has 4 validators");
    let proposer_idx = addresses.iter().position(|a| a == &proposer).unwrap();

    // Every engine stashes the proposal's bytes (validator loop does this
    // at on_proposal entry + at the proposer's build_or_reuse_proposal exit).
    for engine in engines.iter_mut() {
        engine.stash_proposal_bytes(block_hash, block_bytes.to_vec());
    }

    // Drive the proposal into each engine: own for proposer, peer for others.
    for (i, engine) in engines.iter_mut().enumerate() {
        let action = if i == proposer_idx {
            engine.on_own_proposal(block_hash)
        } else {
            engine.on_proposal(block_hash, &proposer, reg)
        };
        assert!(
            matches!(action, BftAction::BroadcastPrevote(_)),
            "engine {} ({}): expected BroadcastPrevote, got {:?}",
            i,
            addresses[i],
            action
        );
    }

    // Fan 4 prevotes for `block_hash` into every engine. Supermajority
    // forms → each engine flips Prevote → Precommit AND promotes
    // staging → locked_block.
    for sender in addresses {
        let pv = mk_prevote(height, round, Some(block_hash.to_string()), sender);
        for engine in engines.iter_mut() {
            let _ = engine.on_prevote_weighted(&pv, per_val);
        }
    }

    // Post-condition: every engine is locked on `block_hash` with bytes
    // promoted into locked_block, and phase has advanced to Precommit.
    for (i, engine) in engines.iter().enumerate() {
        assert_eq!(
            engine.locked_proposal_bytes(),
            Some((block_hash.to_string(), block_bytes.to_vec())),
            "engine {} ({}): expected locked cache after prevote supermajority",
            i,
            addresses[i]
        );
        assert_eq!(
            engine.phase(),
            BftPhase::Precommit,
            "engine {} should be in Precommit phase after prevote supermajority",
            i
        );
    }
}

/// Test 1 — happy path: locked validators preserve cached bytes
/// across a round advance. After round 0 fails to precommit, every
/// engine's `locked_proposal_bytes()` still returns the cached B(H)
/// in round 1. This is the engine-side precondition that makes
/// re-propose possible.
#[test]
fn test_m15_locked_validator_reproposes_cached_block() {
    let (reg, addresses, per_val, total_stake) = setup_four_val_registry();
    let height: u64 = 100;
    let mut engines = setup_four_engines(&addresses, total_stake, height);

    let block_hash = "hash_R0_winner".to_string();
    let block_bytes = b"opaque-block-bytes-for-R0-winner".to_vec();

    // Round 0: drive to lock (precommits NOT delivered).
    drive_round_to_lock(
        &mut engines,
        &reg,
        &addresses,
        height,
        0,
        &block_hash,
        &block_bytes,
        per_val,
    );

    // Simulate every validator timing out and advancing to round 1.
    // Real validator loop calls advance_round on TimeoutAdvanceRound;
    // the harness mimics that path.
    for engine in engines.iter_mut() {
        engine.advance_round();
    }

    // Post-advance: lock state must persist across the round boundary.
    // This is the M-15 contract: advance_round preserves locked_hash +
    // locked_block, only staging is reset.
    for (i, engine) in engines.iter().enumerate() {
        assert_eq!(
            engine.round(),
            1,
            "engine {} did not advance to round 1",
            i
        );
        assert_eq!(
            engine.phase(),
            BftPhase::Propose,
            "engine {} should be in Propose phase at round 1 start",
            i
        );

        let cached = engine.locked_proposal_bytes();
        assert!(
            cached.is_some(),
            "engine {} ({}) lost cached block across advance_round — \
             this is the M-15 invariant break",
            i,
            addresses[i]
        );
        let (cached_hash, cached_bytes) = cached.unwrap();
        assert_eq!(
            cached_hash, block_hash,
            "engine {} cached_hash drift after advance_round",
            i
        );
        assert_eq!(
            cached_bytes, block_bytes,
            "engine {} cached_bytes drift after advance_round",
            i
        );
    }

    // The validator loop's `build_or_reuse_proposal` would consult
    // `locked_proposal_bytes()` here and re-broadcast the cached
    // block. The engine's contract is satisfied — re-propose can fire.
}

/// Test 2 — unstick path: from the locked-state at round 1, the new
/// round's proposer re-broadcasts cached bytes. Peers (also locked
/// on H) accept the proposal, prevote H, precommit H, finalize H.
/// Chain unsticks at round 1 instead of looping forever.
#[test]
fn test_m15_unlock_after_repropose_quorum() {
    let (reg, addresses, per_val, total_stake) = setup_four_val_registry();
    let height: u64 = 100;
    let mut engines = setup_four_engines(&addresses, total_stake, height);

    let block_hash = "hash_R0_winner".to_string();
    let block_bytes = b"opaque-block-bytes-for-R0-winner".to_vec();

    // Round 0: form lock, precommits dropped.
    drive_round_to_lock(
        &mut engines,
        &reg,
        &addresses,
        height,
        0,
        &block_hash,
        &block_bytes,
        per_val,
    );

    // Round advance — every engine to round 1, locks preserved.
    for engine in engines.iter_mut() {
        engine.advance_round();
    }

    // Round 1: re-propose. Identify the new proposer; pull cached
    // bytes from its engine; replay them to all 4 engines as if the
    // proposer had broadcast them.
    let r1_proposer = reg.weighted_proposer(height, 1).unwrap();
    let r1_proposer_idx = addresses.iter().position(|a| a == &r1_proposer).unwrap();

    let cached = engines[r1_proposer_idx]
        .locked_proposal_bytes()
        .expect("R1 proposer must have cached bytes from R0 lock");
    let (re_hash, re_bytes) = cached;
    assert_eq!(re_hash, block_hash, "R1 proposer cache must match R0 lock");
    assert_eq!(re_bytes, block_bytes, "R1 proposer cache bytes must match R0");

    // Validator loop pattern: every receiving engine stashes bytes
    // BEFORE on_proposal — the engine's `BftMessage::Propose` handler
    // does this in main.rs at line ~1936.
    for engine in engines.iter_mut() {
        engine.stash_proposal_bytes(&re_hash, re_bytes.clone());
    }

    // Drive the re-proposed block into each engine.
    for (i, engine) in engines.iter_mut().enumerate() {
        let action = if i == r1_proposer_idx {
            engine.on_own_proposal(&re_hash)
        } else {
            engine.on_proposal(&re_hash, &r1_proposer, &reg)
        };
        // Locked on this exact hash → prevote should be Some(re_hash),
        // not None (nil). If a peer prevoted nil here it means the
        // lock-match check in accept_proposal mis-fired.
        match action {
            BftAction::BroadcastPrevote(pv) => {
                assert_eq!(
                    pv.block_hash.as_deref(),
                    Some(re_hash.as_str()),
                    "engine {} ({}): locked on {}, should prevote {} not {:?}",
                    i,
                    addresses[i],
                    block_hash,
                    re_hash,
                    pv.block_hash
                );
            }
            other => panic!(
                "engine {} ({}): expected BroadcastPrevote, got {:?}",
                i, addresses[i], other
            ),
        }
    }

    // Fan 4 prevotes for re_hash into every engine — supermajority
    // forms, every engine emits BroadcastPrecommit.
    let mut precommits_seen: Vec<Precommit> = Vec::new();
    for sender in &addresses {
        let pv = mk_prevote(height, 1, Some(re_hash.clone()), sender);
        for engine in engines.iter_mut() {
            if let BftAction::BroadcastPrecommit(pc) = engine.on_prevote_weighted(&pv, per_val) {
                precommits_seen.push(pc);
            }
        }
    }
    assert!(
        precommits_seen.len() >= 4,
        "expected ≥4 BroadcastPrecommit emissions at round 1; got {}",
        precommits_seen.len()
    );
    for pc in &precommits_seen[..4] {
        assert_eq!(
            pc.block_hash.as_deref(),
            Some(re_hash.as_str()),
            "round-1 precommit must target re-proposed hash, not nil"
        );
    }

    // Fan 4 precommits for re_hash → supermajority → every engine
    // emits FinalizeBlock. The chain unsticks at round 1.
    let mut finalized = vec![false; engines.len()];
    for pc in &precommits_seen[..4] {
        for (i, engine) in engines.iter_mut().enumerate() {
            if let BftAction::FinalizeBlock {
                block_hash: bh,
                round,
                ..
            } = engine.on_precommit_weighted(pc, per_val)
            {
                assert_eq!(bh, re_hash, "engine {} finalized wrong hash", i);
                assert_eq!(round, 1, "engine {} finalized at wrong round", i);
                finalized[i] = true;
            }
        }
    }

    for (i, f) in finalized.iter().enumerate() {
        assert!(
            *f,
            "engine {} ({}) did not finalize at round 1 — re-propose path is broken",
            i, addresses[i]
        );
    }
}

/// Test 3 — pin the livelock failure mode where a validator locks on
/// hash H without having stashed the bytes (e.g. their libp2p never
/// delivered the Propose message in round 0, but they still observed
/// peer prevotes and locked).
///
/// In this case `locked_hash = Some(H)` but `locked_block = None` →
/// `locked_proposal_bytes()` returns None. If THAT validator is
/// elected proposer of round 1, the validator loop's
/// `build_or_reuse_proposal` falls through to `create_block_voyager`
/// → builds a fresh B'(H') → peers prevote nil (locked on H ≠ H') →
/// SkipRound → loops.
///
/// This test asserts the engine state matches the bug pattern, which
/// makes the production fix obvious: a locked-but-byteless validator
/// should NOT propose in the first place. They should send a nil
/// prevote and let a peer-with-bytes drive the round.
#[test]
fn test_m15_locked_without_bytes_returns_none_from_accessor() {
    let (reg, addresses, per_val, total_stake) = setup_four_val_registry();
    let height: u64 = 100;
    let mut engines = setup_four_engines(&addresses, total_stake, height);

    let block_hash = "hash_R0_winner".to_string();
    let block_bytes = b"opaque-block-bytes".to_vec();
    let proposer = reg.weighted_proposer(height, 0).unwrap();
    let proposer_idx = addresses.iter().position(|a| a == &proposer).unwrap();

    // Pick a non-proposer validator to be the "byteless" victim. It
    // never stashes bytes — simulates libp2p drop of the Proposal
    // message before stash_proposal_bytes was called.
    let byteless_idx = (proposer_idx + 1) % engines.len();

    // Three of four engines stash bytes (proposer + the two
    // non-byteless peers).
    for (i, engine) in engines.iter_mut().enumerate() {
        if i != byteless_idx {
            engine.stash_proposal_bytes(&block_hash, block_bytes.clone());
        }
    }

    // Drive proposal into each engine. The byteless victim still
    // processes on_proposal (they got the message just AFTER stash
    // was missed — or got it via post-vote gossip), so they prevote.
    // The lock will form via prevote tally regardless.
    for (i, engine) in engines.iter_mut().enumerate() {
        let action = if i == proposer_idx {
            engine.on_own_proposal(&block_hash)
        } else {
            engine.on_proposal(&block_hash, &proposer, &reg)
        };
        assert!(
            matches!(action, BftAction::BroadcastPrevote(_)),
            "engine {} expected BroadcastPrevote, got {:?}",
            i,
            action
        );
    }

    // Fan 4 prevotes → all 4 engines lock on hash. The byteless one
    // promotes nothing into locked_block because its staging is empty.
    for sender in &addresses {
        let pv = mk_prevote(height, 0, Some(block_hash.clone()), sender);
        for engine in engines.iter_mut() {
            let _ = engine.on_prevote_weighted(&pv, per_val);
        }
    }

    // Verify the bug pattern: byteless engine has locked_hash but no
    // locked_block.
    let byteless = &engines[byteless_idx];
    assert!(
        byteless.locked_proposal_bytes().is_none(),
        "byteless engine should have no cached bytes — got {:?}",
        byteless.locked_proposal_bytes().map(|(h, _)| h)
    );

    // Post advance_round, the byteless engine still has no bytes.
    // If that engine is the round-1 proposer in production, the
    // helper falls through to create_block_voyager and builds a
    // wrong-hash block — peers prevote nil, livelock starts.
    for engine in engines.iter_mut() {
        engine.advance_round();
    }
    let byteless = &engines[byteless_idx];
    assert!(
        byteless.locked_proposal_bytes().is_none(),
        "byteless engine still has no cached bytes after advance_round"
    );
}
