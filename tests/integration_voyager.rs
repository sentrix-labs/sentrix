#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]
// integration_voyager.rs — Integration tests for Voyager DPoS + BFT
//
// Tests the full flow: stake → epoch transition → BFT round → finalize → rewards

use sentrix::core::bft::{BftAction, BftEngine, BftPhase};
use sentrix::core::bft_messages::{Precommit, Prevote, supermajority_threshold};
use sentrix::core::epoch::{EPOCH_LENGTH, EpochManager};
use sentrix::core::slashing::{LIVENESS_WINDOW, SlashingEngine};
use sentrix::core::staking::{MIN_SELF_STAKE, StakeRegistry};

fn setup_21_validators() -> StakeRegistry {
    let mut reg = StakeRegistry::new();
    for i in 0..21 {
        let addr = format!("0xval{:03}", i);
        let stake = MIN_SELF_STAKE + (i as u64) * 100_000_000;
        reg.register_validator(&addr, stake, 1000, 0).unwrap();
    }
    reg.update_active_set();
    reg
}

fn total_active_stake(reg: &StakeRegistry) -> u64 {
    reg.active_set
        .iter()
        .filter_map(|a| reg.get_validator(a))
        .map(|v| v.total_stake())
        .sum()
}

#[test]
fn test_full_stake_delegate_epoch_cycle() {
    let mut reg = setup_21_validators();
    let mut epoch = EpochManager::new();
    epoch.initialize(&reg, 0);

    assert_eq!(epoch.current_epoch.validator_set.len(), 21);

    // Delegator stakes to val000
    reg.delegate("0xdelegator1", "0xval000", 50_000_000_000, 100)
        .unwrap();

    // Epoch transition — delegation takes effect
    let released = epoch.transition(&mut reg, EPOCH_LENGTH - 1).unwrap();
    assert!(released.is_empty());
    assert_eq!(epoch.current_epoch.epoch_number, 1);

    // val000 should still be in active set with higher stake now
    assert!(reg.is_active("0xval000"));
    let v = reg.get_validator("0xval000").unwrap();
    assert_eq!(v.total_delegated, 50_000_000_000);
}

#[test]
fn test_undelegate_unbonding_release() {
    let mut reg = setup_21_validators();
    let mut epoch = EpochManager::new();
    epoch.initialize(&reg, 0);

    // Delegate then undelegate
    reg.delegate("0xdel1", "0xval000", 10_000_000, 100).unwrap();
    reg.undelegate("0xdel1", "0xval000", 5_000_000, 200)
        .unwrap();

    // Unbonding not matured yet at normal epoch boundary
    let released = epoch.transition(&mut reg, EPOCH_LENGTH - 1).unwrap();
    assert!(released.is_empty());

    // Transition well past unbonding period
    let released = epoch.transition(&mut reg, 300_000).unwrap();
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].0, "0xdel1");
    assert_eq!(released[0].1, 5_000_000);
}

#[test]
fn test_slash_removes_from_active_set() {
    let mut reg = setup_21_validators();
    let mut slashing = SlashingEngine::new();

    // Make val000 miss all blocks in liveness window
    for h in 0..LIVENESS_WINDOW {
        slashing.liveness.record("0xval000", h, false);
        // Everyone else signs
        for i in 1..21 {
            slashing.liveness.record(&format!("0xval{:03}", i), h, true);
        }
    }

    let active: Vec<String> = reg.active_set.clone();
    let slashed = slashing.check_liveness(&mut reg, &active, LIVENESS_WINDOW);

    assert_eq!(slashed.len(), 1);
    assert_eq!(slashed[0].0, "0xval000");
    assert!(reg.get_validator("0xval000").unwrap().is_jailed);

    // After updating active set, jailed validator should be excluded
    reg.update_active_set();
    assert!(!reg.is_active("0xval000"));
    assert_eq!(reg.active_count(), 20);
}

#[test]
fn test_bft_full_round_with_21_validators() {
    let reg = setup_21_validators();
    let total = total_active_stake(&reg);
    let per_val = total / 21;

    let mut engine = BftEngine::new(1000, "0xval000".into(), total);

    // 1. Proposal
    let proposer = reg.weighted_proposer(1000, 0).unwrap();
    let action = engine.on_proposal("block_1000", &proposer, &reg);

    // Should get a prevote (either for block or nil depending on if we're the proposer)
    match action {
        BftAction::BroadcastPrevote(pv) => {
            assert_eq!(pv.height, 1000);
        }
        BftAction::Wait => {} // we already voted or wrong phase
        _ => panic!("unexpected action after proposal"),
    }

    // 2. Collect 15+ prevotes
    for i in 1..=16 {
        let pv = Prevote {
            height: 1000,
            round: 0,
            block_hash: Some("block_1000".into()),
            validator: format!("0xval{:03}", i),
            signature: vec![],
        };
        engine.on_prevote_weighted(&pv, per_val);
    }

    assert_eq!(engine.phase(), BftPhase::Precommit);

    // 3. Collect precommits → finalize
    let mut finalized = false;
    for i in 0..=16 {
        let pc = Precommit {
            height: 1000,
            round: 0,
            block_hash: Some("block_1000".into()),
            validator: format!("0xval{:03}", i),
            signature: vec![],
        };
        let action = engine.on_precommit_weighted(&pc, per_val);
        if let BftAction::FinalizeBlock {
            height,
            block_hash,
            justification,
            ..
        } = action
        {
            assert_eq!(height, 1000);
            assert_eq!(block_hash, "block_1000");
            assert!(justification.has_supermajority(total));
            finalized = true;
        }
    }

    assert!(finalized, "block should have been finalized");
}

#[test]
fn test_bft_timeout_nil_round() {
    let reg = setup_21_validators();
    let total = total_active_stake(&reg);
    let per_val = total / 21;

    let mut engine = BftEngine::new(500, "0xval000".into(), total);

    // Propose timeout → nil prevote
    let action = engine.on_timeout();
    assert!(matches!(action, BftAction::BroadcastPrevote(_)));
    assert_eq!(engine.phase(), BftPhase::Prevote);

    // Collect nil prevotes
    for i in 1..=16 {
        let pv = Prevote {
            height: 500,
            round: 0,
            block_hash: None,
            validator: format!("0xval{:03}", i),
            signature: vec![],
        };
        engine.on_prevote_weighted(&pv, per_val);
    }
    assert_eq!(engine.phase(), BftPhase::Precommit);

    // Collect nil precommits → skip round
    let mut skipped = false;
    for i in 0..=16 {
        let pc = Precommit {
            height: 500,
            round: 0,
            block_hash: None,
            validator: format!("0xval{:03}", i),
            signature: vec![],
        };
        let action = engine.on_precommit_weighted(&pc, per_val);
        if matches!(action, BftAction::SkipRound) {
            skipped = true;
        }
    }
    assert!(skipped, "should have skipped round on nil supermajority");
}

#[test]
fn test_reward_distribution_with_commission() {
    let mut reg = StakeRegistry::new();
    reg.register_validator("0xproposer", MIN_SELF_STAKE, 1000, 0)
        .unwrap(); // 10% commission
    reg.delegate("0xdel1", "0xproposer", MIN_SELF_STAKE, 0)
        .unwrap(); // equal delegation
    reg.update_active_set();

    let block_reward = 100_000_000; // 1 SRX
    let fee_share = 50_000_000; // 0.5 SRX from fees

    // V4 Step 2: Pioneer fallback path (empty signers) preserves legacy behaviour.
    reg.distribute_reward("0xproposer", &[], block_reward, fee_share)
        .unwrap();

    let v = reg.get_validator("0xproposer").unwrap();
    // Total reward = 150M sentri
    // Commission = 10% = 15M
    // Delegator pool = 135M
    // Proposer's self-stake share = 50% of 135M = 67.5M
    // Total pending = 15M + 67.5M = 82.5M
    assert!(v.pending_rewards > 0);
    // Allow small rounding variance
    let expected = 82_500_000u64;
    assert!(
        v.pending_rewards.abs_diff(expected) < 2,
        "expected ~{}, got {}",
        expected,
        v.pending_rewards
    );
}

#[test]
fn test_supermajority_threshold_21_validators() {
    // 21 validators with equal stake
    let stake_per = MIN_SELF_STAKE;
    let total = stake_per * 21;
    let threshold = supermajority_threshold(total);

    // Need >2/3 of total stake
    assert!(threshold > total * 2 / 3);
    // 15 validators should be enough (15/21 = 71.4% > 66.7%)
    assert!(stake_per * 15 >= threshold);
    // 14 validators should NOT be enough
    assert!(stake_per * 14 < threshold);
}

#[test]
fn test_redelegate_between_validators() {
    let mut reg = setup_21_validators();

    reg.delegate("0xdel1", "0xval000", 10_000_000_000, 100)
        .unwrap();

    // Redelegate half to val001
    reg.redelegate("0xdel1", "0xval000", "0xval001", 5_000_000_000, 200)
        .unwrap();

    let v0 = reg.get_validator("0xval000").unwrap();
    let v1 = reg.get_validator("0xval001").unwrap();
    assert_eq!(v0.total_delegated, 5_000_000_000);
    assert_eq!(v1.total_delegated, 5_000_000_000);
}

#[test]
fn test_double_sign_tombstones_validator() {
    let mut reg = setup_21_validators();
    let mut slashing = SlashingEngine::new();

    let evidence = sentrix::core::slashing::DoubleSignEvidence {
        validator: "0xval005".into(),
        height: 1000,
        block_hash_a: "hash_a".into(),
        block_hash_b: "hash_b".into(),
        signature_a: "sig_a".into(),
        signature_b: "sig_b".into(),
    };

    let amount = slashing.process_double_sign(&mut reg, &evidence).unwrap();
    assert!(amount > 0);

    let v = reg.get_validator("0xval005").unwrap();
    assert!(v.is_tombstoned);
    assert!(v.is_jailed);

    // Tombstoned validator excluded from active set
    reg.update_active_set();
    assert!(!reg.is_active("0xval005"));
}

#[test]
fn test_epoch_validator_rotation() {
    let mut reg = setup_21_validators();
    let mut epoch = EpochManager::new();
    epoch.initialize(&reg, 0);

    // Register a new whale validator that should enter top 21
    reg.register_validator("0xwhale", MIN_SELF_STAKE * 100, 500, 1000)
        .unwrap();

    // Epoch transition
    epoch.transition(&mut reg, EPOCH_LENGTH - 1).unwrap();

    // Whale should now be in active set (top by stake)
    assert!(reg.is_active("0xwhale"));
    assert_eq!(reg.active_count(), 21); // still capped at 21

    // One of the lower-staked validators should be pushed out
    // val000 had the lowest stake (MIN_SELF_STAKE + 0)
    assert!(!reg.is_active("0xval000"));
}
