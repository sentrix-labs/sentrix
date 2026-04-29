// engine/timeouts.rs — BFT phase timeout constants + helpers.
//
// Timeouts tuned for 4-validator testnet with ~100ms localhost latency.
// Round 0 must give enough time for all validators to start up + exchange
// their first proposal.
//
// 2026-04-20 #1d diagnosis (`BFT #1d:` tally logging added in PR #171):
// the livelock fires when the proposer's request-response Proposal
// message doesn't reach all peers within the 10s propose timeout.
// Typically a peer has just reconnected and isn't yet in the proposer's
// `verified_peers` set at proposal time — the message is silently
// dropped. The peer's own propose-phase timeout fires with no proposal
// received, it nil-prevotes, the proposer's supermajority for block
// fails → nil-precommit chain reaction → skip round, repeat forever.
// Bumping propose_timeout 10s → 20s widens the window the proposer has
// to include a freshly-reconnected peer in the outbound send list.
// It's not a root-cause fix (that needs proposer re-broadcast or
// verified-peer stability, backlog #1d follow-up) but empirically
// stops the stall. prevote/precommit also nudged up to 12s so phase
// transitions don't race the wider propose window.

use std::time::Duration;

pub const PROPOSE_TIMEOUT_MS: u64 = 20_000;
pub const PREVOTE_TIMEOUT_MS: u64 = 12_000;
pub const PRECOMMIT_TIMEOUT_MS: u64 = 12_000;
pub const TIMEOUT_INCREMENT_MS: u64 = 1_000; // +1s per round for propose
pub const VOTE_TIMEOUT_INCREMENT_MS: u64 = 2_000; // +2s per round for votes
pub const MAX_TIMEOUT_MS: u64 = 30_000;
pub const MAX_ROUND: u32 = 100;

pub fn propose_timeout(round: u32) -> Duration {
    let ms = PROPOSE_TIMEOUT_MS
        .saturating_add((round as u64).saturating_mul(TIMEOUT_INCREMENT_MS))
        .min(MAX_TIMEOUT_MS);
    Duration::from_millis(ms)
}

pub fn prevote_timeout(round: u32) -> Duration {
    let ms = PREVOTE_TIMEOUT_MS
        .saturating_add((round as u64).saturating_mul(VOTE_TIMEOUT_INCREMENT_MS))
        .min(MAX_TIMEOUT_MS);
    Duration::from_millis(ms)
}

pub fn precommit_timeout(round: u32) -> Duration {
    let ms = PRECOMMIT_TIMEOUT_MS
        .saturating_add((round as u64).saturating_mul(VOTE_TIMEOUT_INCREMENT_MS))
        .min(MAX_TIMEOUT_MS);
    Duration::from_millis(ms)
}
