//! Tokenomics constants — max supply, halving intervals, block reward.
//!
//! Tokenomics v1 (genesis-active): 210M cap, 42M-block halving, 1 SRX
//! initial. Geometric series math: 1 × 42M × 2 = 84M from mining + 63M
//! premine = 147M asymptotic — caps unreachable, validator runway era
//! 5 = year 6.66 (cliff).
//!
//! Tokenomics v2 ([`crate::fork_heights::get_tokenomics_v2_height`]
//! fork-gated): 315M cap, 126M-block halving (BTC-parity 4-year), 1 SRX
//! initial unchanged. Geometric: 1 × 126M × 2 = 252M mining + 63M
//! premine = 315M (cap reachable). Premine ratio drops from 30%
//! (intended) → 20% (mining-share dilution from 70% → 80%). Validator
//! runway era 5 = year ~20.
//!
//! Extracted from `crate::blockchain` so the tokenomics layer lives in
//! one focused module. `blockchain.rs` re-exports the constants and the
//! free helper so existing import paths continue to resolve.

/// Maximum supply in sentri (v1 — pre-tokenomics-v2 fork). Equal to
/// 210,000,000 SRX × 100,000,000 sentri/SRX.
pub const MAX_SUPPLY: u64 = 210_000_000 * 100_000_000;

/// Maximum supply in sentri after the tokenomics-v2 fork activates.
/// Equal to 315,000,000 SRX × 100,000,000 sentri/SRX. The cap is reachable
/// under v2 (vs. asymptotic-only under v1) because the longer halving
/// interval lets the geometric series sum to the literal cap.
pub const MAX_SUPPLY_V2: u64 = 315_000_000 * 100_000_000;

/// Era-zero block reward in sentri. Equal to 1 SRX × 100,000,000
/// sentri/SRX. Halving schedule applies per [`HALVING_INTERVAL`] (v1) or
/// [`HALVING_INTERVAL_V2`] (post-fork) — the initial reward itself is
/// unchanged across forks.
pub const BLOCK_REWARD: u64 = 100_000_000;

/// Blocks per halving era (v1). At 1-second block time this is ~1.33y
/// per era. Combined with [`MAX_SUPPLY`] = 210M, era-5 supply asymptote
/// would land at year ~6.66 (operator-flagged as a runway cliff).
pub const HALVING_INTERVAL: u64 = 42_000_000;

/// Blocks per halving era (post-tokenomics-v2 fork). At 1-second block
/// time this is ~4 years per era — BTC-parity. Combined with
/// [`MAX_SUPPLY_V2`] = 315M, the geometric series sums to the literal
/// cap (no runway cliff).
pub const HALVING_INTERVAL_V2: u64 = 126_000_000;

/// [`MAX_SUPPLY`] expressed in whole SRX as f64 — used by RPC/explorer
/// display paths. Single source of truth; do NOT redefine as a local
/// constant elsewhere.
///
/// **Pre-tokenomics-v2 callers:** prefer
/// [`crate::blockchain::Blockchain::max_supply_for`] at runtime to get
/// the fork-aware value. This helper returns the v1 number for backward
/// compatibility with non-`&self` paths only.
pub fn max_supply_srx() -> f64 {
    (MAX_SUPPLY / 100_000_000) as f64
}

// ── Fork-aware emission math ─────────────────────────────
//
// These three functions used to live on `impl Blockchain` (max_supply_for,
// halving_interval_for, halvings_at). Moved here so the tokenomics layer
// owns its math; `Blockchain` keeps thin delegators for API compat with
// existing instance-method callers.

/// Max supply in sentri at the given height (fork-aware). Pre-fork:
/// [`MAX_SUPPLY`] (210M). Post-fork: [`MAX_SUPPLY_V2`] (315M).
pub fn max_supply_for(height: u64) -> u64 {
    if crate::blockchain::Blockchain::is_tokenomics_v2_height(height) {
        MAX_SUPPLY_V2
    } else {
        MAX_SUPPLY
    }
}

/// Halving interval in blocks at the given height (fork-aware).
/// Pre-fork: [`HALVING_INTERVAL`] (42M blocks ≈ 1.33y). Post-fork:
/// [`HALVING_INTERVAL_V2`] (126M blocks ≈ 4y BTC-parity).
pub fn halving_interval_for(height: u64) -> u64 {
    if crate::blockchain::Blockchain::is_tokenomics_v2_height(height) {
        HALVING_INTERVAL_V2
    } else {
        HALVING_INTERVAL
    }
}

/// Halving count at a given height, fork-aware. Pre-fork blocks count
/// halvings against 42M intervals; post-fork blocks count against 126M
/// intervals **starting from the fork height** (so cumulative halvings
/// don't reset at fork moment, and no jump-up in reward).
///
/// Assumes fork is activated while still within v1 era 0
/// (`fork_height < HALVING_INTERVAL` = 42M). At current mainnet height
/// ~600K, this is satisfied for any plausible fork target.
pub fn halvings_at(height: u64) -> u32 {
    let fork = crate::fork_heights::get_tokenomics_v2_height();
    if fork == u64::MAX || height < fork {
        (height / HALVING_INTERVAL).try_into().unwrap_or(u32::MAX)
    } else {
        // Post-fork: count halvings from fork height using v2 interval.
        // Pre-fork halvings = 0 by activation invariant (fork while in era 0).
        let post = height.saturating_sub(fork);
        (post / HALVING_INTERVAL_V2).try_into().unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_cap_arithmetic() {
        assert_eq!(MAX_SUPPLY, 21_000_000_000_000_000);
        assert_eq!(max_supply_srx(), 210_000_000.0);
    }

    #[test]
    fn v2_cap_arithmetic() {
        assert_eq!(MAX_SUPPLY_V2, 31_500_000_000_000_000);
    }

    #[test]
    fn v2_cap_is_v1_cap_one_and_half() {
        // 315 / 210 = 1.5 exactly. Catches typo'd zeros in either constant.
        assert_eq!(MAX_SUPPLY_V2 * 2, MAX_SUPPLY * 3);
    }

    #[test]
    fn block_reward_is_one_srx() {
        // 1 SRX = 10^8 sentri (8 decimals)
        assert_eq!(BLOCK_REWARD, 100_000_000);
    }

    #[test]
    fn halving_intervals_self_consistent() {
        // v2 interval is exactly 3× v1, matching the cap ratio. If
        // either constant drifts, the geometric supply curve breaks.
        assert_eq!(HALVING_INTERVAL_V2, HALVING_INTERVAL * 3);
    }

    #[test]
    fn halving_v2_is_btc_parity() {
        // ~4 years at 1-second block time. 4 × 365.25 × 86400 = 126,230,400.
        // The protocol rounds down to a clean 126M.
        let four_years_secs: u64 = 4 * 365 * 24 * 3600;
        let diff = HALVING_INTERVAL_V2.abs_diff(four_years_secs);
        // Within ~0.2% of the literal 4-year second count.
        assert!(diff < HALVING_INTERVAL_V2 / 500);
    }
}
