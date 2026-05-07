//! LastSignBytes / privval guard — prevents double-voting across validator
//! restarts (Tendermint canonical pattern).
//!
//! Every prevote / precommit signing operation records the highest
//! `(height, round, step)` it has signed at, persisted to disk with
//! fsync before the message is broadcast. On boot, the next sign
//! refuses to operate at any `(h, r, s) <= last_signed`.
//!
//! Closes the cascade-jail-at-restart class: pre-fix, a validator that
//! crashed mid-round would re-vote inconsistently after recovery — same
//! height + round but different block hash — which (under a Byzantine
//! interpretation) looks like equivocation. CometBFT's privval daemon
//! has had this guard since 2018; Sentrix was missing it.
//!
//! Activation: operator sets `LAST_SIGN_GUARD_PATH=/var/lib/sentrix/last-sign.json`
//! in each validator's env file. With the env unset, the guard is a
//! no-op (matches pre-fix behaviour bit-identically) so chain history
//! stays valid + new operators don't get a hard requirement.
//!
//! File format: small JSON blob, atomic-rename-on-write so a partial
//! write can't corrupt the file mid-restart.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Vote step. Step ordering matters for the comparison: at the same
/// (height, round) signing a precommit AFTER a prevote is allowed
/// (precommit > prevote in step), but going back from precommit to
/// prevote is double-vote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VoteStep {
    Prevote = 1,
    Precommit = 2,
    /// Proposal — counted as step 0 because proposing happens before
    /// any voting at the same (height, round).
    Proposal = 0,
    /// RoundStatus / multiaddr advertisements / non-consensus messages.
    /// These are NOT guarded — they don't risk double-vote-style
    /// equivocation slashing.
    Other = 99,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LastSignState {
    pub height: u64,
    pub round: u32,
    pub step: u32, // serialized as numeric so we don't break on enum changes
    /// Optional — last bytes the signer hashed. Lets a future
    /// `same-bytes-replay` exemption recognise a duplicate sign request
    /// for an identical message and re-emit the same signature instead
    /// of refusing. Not implemented yet but field reserved.
    #[serde(default)]
    pub last_sign_bytes_hex: String,
}

/// Whole-process guard state. `None` means "guard not initialised" —
/// every guarded_check returns Ok(()) so the call site behaves as if
/// the guard didn't exist. Operator opts in by calling `init(path)`
/// once at startup.
struct Guard {
    path: PathBuf,
    state: Mutex<LastSignState>,
}

static GUARD: OnceLock<Guard> = OnceLock::new();

/// Initialise the guard. Called once by the validator binary at
/// startup if `LAST_SIGN_GUARD_PATH` env var is set. Reads the
/// existing state file if present; creates an empty one if not.
/// Idempotent — second call is a no-op.
pub fn init(path: PathBuf) -> std::io::Result<()> {
    if GUARD.get().is_some() {
        return Ok(());
    }
    let state = if path.exists() {
        let bytes = std::fs::read(&path)?;
        serde_json::from_slice::<LastSignState>(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
    } else {
        // Initial state — every (h, r, s) >= (0, 0, 0) is allowed.
        let st = LastSignState::default();
        write_atomic(&path, &st)?;
        st
    };
    let _ = GUARD.set(Guard {
        path,
        state: Mutex::new(state),
    });
    tracing::info!(target: "sentrix_bft::last_sign_guard", "LastSignBytes guard initialised");
    Ok(())
}

/// Read the persisted last-signed state. Returns None if the guard
/// hasn't been initialised (operator opted out via unset env var).
///
/// Added in v2.1.85 so the BFT engine init path can resume at
/// `last_signed.round + 1` for the same height after a mid-round
/// crash, instead of restarting at round 0 and getting refused by
/// the guard. See `bin/sentrix/src/main.rs` validator-loop for the
/// caller; without this the v2.1.84 guard caused chicken-egg halts
/// where post-restart engine couldn't make progress.
pub fn current_state() -> Option<LastSignState> {
    let g = GUARD.get()?;
    let st = g.state.lock().ok()?;
    Some(st.clone())
}

/// Check + record a sign-attempt. Returns Ok(()) if the guard isn't
/// initialised (legacy behaviour) OR if `(height, round, step)` strictly
/// exceeds the persisted last-signed tuple. Returns Err if the request
/// would re-sign at-or-below the last-signed point — indicating a
/// double-vote attempt.
///
/// On Ok, the new tuple is fsync'd to disk BEFORE returning, so a
/// post-broadcast crash leaves the on-disk state already advanced.
pub fn check_and_record(height: u64, round: u32, step: VoteStep) -> Result<(), DoubleSignAttempt> {
    let g = match GUARD.get() {
        Some(g) => g,
        None => return Ok(()),
    };
    if matches!(step, VoteStep::Other) {
        return Ok(());
    }
    let mut st = g.state.lock().expect("last-sign-state mutex poisoned");
    let new_step_ord = step as u32;
    let cur_tuple = (st.height, st.round, st.step);
    let new_tuple = (height, round, new_step_ord);
    if new_tuple <= cur_tuple {
        return Err(DoubleSignAttempt {
            attempted: new_tuple,
            last_signed: cur_tuple,
        });
    }
    st.height = height;
    st.round = round;
    st.step = new_step_ord;
    let snapshot = st.clone();
    drop(st);
    if let Err(e) = write_atomic(&g.path, &snapshot) {
        tracing::error!(
            target: "sentrix_bft::last_sign_guard",
            "FATAL: last-sign-state persist failed at {:?}: {} — refusing to sign, validator should halt",
            g.path,
            e
        );
        return Err(DoubleSignAttempt {
            attempted: new_tuple,
            last_signed: cur_tuple,
        });
    }
    Ok(())
}

/// Atomic write: writes to a `<path>.tmp` file, fsyncs, then renames.
/// Avoids the half-written-JSON-on-power-loss class.
fn write_atomic(path: &Path, state: &LastSignState) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        let bytes = serde_json::to_vec(state)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        // Best-effort directory fsync (POSIX recommendation after rename).
        if let Ok(d) = std::fs::File::open(parent) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct DoubleSignAttempt {
    pub attempted: (u64, u32, u32),
    pub last_signed: (u64, u32, u32),
}

impl std::fmt::Display for DoubleSignAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "DoubleSignAttempt: attempted (h={}, r={}, s={}) <= last_signed (h={}, r={}, s={})",
            self.attempted.0, self.attempted.1, self.attempted.2,
            self.last_signed.0, self.last_signed.1, self.last_signed.2,
        )
    }
}

impl std::error::Error for DoubleSignAttempt {}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests can't use the global GUARD safely (singleton). Test the
    // pure logic in isolation via a helper.
    fn check_pure(cur: (u64, u32, u32), new: (u64, u32, u32)) -> Result<(), ()> {
        if new <= cur { Err(()) } else { Ok(()) }
    }

    #[test]
    fn step_ordering_rejects_prevote_after_precommit_same_round() {
        // last signed: (h=10, r=0, s=Precommit=2)
        let cur = (10, 0, VoteStep::Precommit as u32);
        // attempt: (h=10, r=0, s=Prevote=1)
        let new = (10, 0, VoteStep::Prevote as u32);
        assert!(check_pure(cur, new).is_err());
    }

    #[test]
    fn step_ordering_allows_precommit_after_prevote_same_round() {
        let cur = (10, 0, VoteStep::Prevote as u32);
        let new = (10, 0, VoteStep::Precommit as u32);
        assert!(check_pure(cur, new).is_ok());
    }

    #[test]
    fn next_round_allowed() {
        let cur = (10, 0, VoteStep::Precommit as u32);
        let new = (10, 1, VoteStep::Prevote as u32);
        assert!(check_pure(cur, new).is_ok());
    }

    #[test]
    fn next_height_allowed() {
        let cur = (10, 5, VoteStep::Precommit as u32);
        let new = (11, 0, VoteStep::Prevote as u32);
        assert!(check_pure(cur, new).is_ok());
    }

    #[test]
    fn same_tuple_rejected() {
        let cur = (10, 0, VoteStep::Prevote as u32);
        let new = (10, 0, VoteStep::Prevote as u32);
        assert!(check_pure(cur, new).is_err());
    }

    #[test]
    fn old_height_rejected() {
        let cur = (11, 0, VoteStep::Prevote as u32);
        let new = (10, 5, VoteStep::Precommit as u32);
        assert!(check_pure(cur, new).is_err());
    }

    #[test]
    fn write_atomic_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("last-sign.json");
        let st = LastSignState { height: 42, round: 7, step: 2, last_sign_bytes_hex: String::new() };
        write_atomic(&path, &st).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let read: LastSignState = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(read.height, 42);
        assert_eq!(read.round, 7);
        assert_eq!(read.step, 2);
    }

    // Note: no end-to-end test for the global GUARD. It's a process-
    // singleton OnceLock; initializing it in any test poisons the state
    // for every sibling test that goes through `Prevote::sign` /
    // `Precommit::sign` / `Proposal::sign` (those refuse to sign at
    // already-recorded tuples). Algorithm coverage above is enough; the
    // glue (init → state file → mutex → check_and_record) is exercised
    // by the validator binary at startup.
}
