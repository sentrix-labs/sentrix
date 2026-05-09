//! End-to-end test for the `last_sign_guard` module's global singleton.
//!
//! Lives as an integration test (not a `#[cfg(test)] mod tests`) so its
//! `OnceLock` init doesn't poison the unit-test binary, where
//! `messages::tests::test_proposal_sign_verify` (and friends) exercise
//! `Proposal::sign` / `Prevote::sign` / `Precommit::sign` through the same
//! singleton and rely on the guard being uninitialised.
//!
//! Walks every branch of `check_and_record_with_bytes` end-to-end through
//! the live GUARD: legacy bypass, Other step, advance + persist, replay
//! exemption (same bytes → Ok), equivocation (different bytes → Err),
//! rollback rejection, legacy clear-hash advance, idempotent init.

use sentrix_bft::last_sign_guard::{
    VoteStep, check_and_record, check_and_record_with_bytes, current_state, init,
};

#[test]
fn full_guard_lifecycle_through_global() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("last-sign.json");

    // Pre-init: every call is a no-op pass; current_state returns None.
    assert!(check_and_record_with_bytes(1, 0, VoteStep::Prevote, b"any").is_ok());
    assert!(check_and_record(1, 0, VoteStep::Prevote).is_ok());
    assert!(current_state().is_none());

    init(path).unwrap();

    // VoteStep::Other is never guarded — passes regardless of (h, r).
    assert!(check_and_record_with_bytes(0, 0, VoteStep::Other, b"x").is_ok());
    assert!(check_and_record_with_bytes(u64::MAX, u32::MAX, VoteStep::Other, b"x").is_ok());

    // New tuple > cur → Ok + persist + hash recorded.
    let p1 = b"prevote-payload-h10-r0";
    assert!(check_and_record_with_bytes(10, 0, VoteStep::Prevote, p1).is_ok());
    let st = current_state().unwrap();
    assert_eq!(
        (st.height, st.round, st.step),
        (10, 0, VoteStep::Prevote as u32)
    );
    assert_eq!(st.last_sign_bytes_hex.len(), 64);

    // Same tuple + same bytes → replay exempt (Ok, no error).
    assert!(check_and_record_with_bytes(10, 0, VoteStep::Prevote, p1).is_ok());

    // Same tuple + different bytes → real equivocation (Err).
    let err = check_and_record_with_bytes(10, 0, VoteStep::Prevote, b"DIFFERENT").unwrap_err();
    assert_eq!(err.attempted, (10, 0, VoteStep::Prevote as u32));
    assert_eq!(err.last_signed, (10, 0, VoteStep::Prevote as u32));
    // Display impl renders the tuple legibly.
    let s = format!("{}", err);
    assert!(s.contains("DoubleSignAttempt"));
    assert!(s.contains("h=10"));

    // Advance step (Prevote → Precommit) at same h/r → Ok.
    assert!(check_and_record_with_bytes(10, 0, VoteStep::Precommit, b"precommit-bytes").is_ok());

    // Rollback attempt → Err (legacy strict path, empty payload).
    assert!(check_and_record(10, 0, VoteStep::Prevote).is_err());

    // Legacy callers (no payload bytes) advance forward fine and clear hash.
    assert!(check_and_record(11, 0, VoteStep::Prevote).is_ok());
    let st = current_state().unwrap();
    assert_eq!(st.height, 11);
    assert!(st.last_sign_bytes_hex.is_empty());

    // After legacy advance the persisted cur_hash is empty; replay path
    // requires both non-empty → falls through to strict tuple check → Err.
    assert!(check_and_record_with_bytes(11, 0, VoteStep::Prevote, b"new-bytes").is_err());

    // init() is idempotent — second call when GUARD already set is a no-op.
    init(dir.path().join("ignored.json")).unwrap();
    let st = current_state().unwrap();
    assert_eq!(st.height, 11); // unchanged
}
