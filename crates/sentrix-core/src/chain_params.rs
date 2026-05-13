//! Chain parameters that aren't tokenomics or address validation —
//! per-block timing + transaction-count limits, the chain identifier,
//! the hash-algorithm version marker, and the in-memory window size.
//!
//! Each is a compile-time default; the chain ID also accepts a runtime
//! override via `SENTRIX_CHAIN_ID` so testnet (7120) shares one binary
//! with mainnet (7119). Extracted from `crate::blockchain` so the
//! protocol-parameter layer lives in one focused module.

/// Target block time in seconds. The block producer doesn't shorten
/// below this; networks under high load fall toward this floor.
pub const BLOCK_TIME_SECS: u64 = 1;

/// Maximum transactions per block. Bounds worst-case apply duration
/// and serialization size. The mempool can hold more
/// ([`crate::blockchain::MAX_MEMPOOL_SIZE`]); excess waits for the next
/// block.
pub const MAX_TX_PER_BLOCK: usize = 5000;

/// Default chain ID. Mainnet — overridable per-process via the
/// `SENTRIX_CHAIN_ID` env var for testnet (7120), devnet, or local
/// integration runs. Use [`get_chain_id`] to read the effective value
/// at runtime, never refer to this constant directly outside
/// genesis / boot paths.
pub const CHAIN_ID: u64 = 7119;

/// Read the chain ID for this process — `SENTRIX_CHAIN_ID` env var
/// when present and parseable, otherwise [`CHAIN_ID`] (7119 / mainnet).
pub fn get_chain_id() -> u64 {
    std::env::var("SENTRIX_CHAIN_ID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(CHAIN_ID)
}

/// Hash-algorithm version. Reserved for a future migration from
/// SHA-256 (the only allocated value today) to a different family.
/// The chain stamps this into block headers so a future fork can
/// recognise pre-migration vs post-migration blocks.
pub const HASH_VERSION: u8 = 1; // 1 = SHA-256 (current)

/// Sliding-window size for the in-memory block cache. Only the last
/// `CHAIN_WINDOW_SIZE` blocks live in RAM; older blocks remain on
/// disk in MDBX and are read on demand through
/// [`crate::blockchain::Blockchain::get_block_any`] — the variant
/// that falls back to MDBX storage when the in-memory window misses.
/// Plain `get_block` returns `None` for out-of-window heights.
pub const CHAIN_WINDOW_SIZE: usize = 1_000;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::env_test_lock;

    /// Block time floor is exactly 1 second — pinning the literal so a
    /// stray edit is loud at build time. const-assert (not a runtime
    /// test) because the value is compile-time.
    const _: () = assert!(BLOCK_TIME_SECS == 1);

    /// Per-block tx cap must be > 0 and ≤ mempool size cap, otherwise
    /// users can't ever queue ahead. Const-assert so a drift in either
    /// constant fails the build, not just `cargo test`.
    const _: () = assert!(MAX_TX_PER_BLOCK > 0);
    const _: () = assert!(MAX_TX_PER_BLOCK <= crate::blockchain::MAX_MEMPOOL_SIZE);

    #[test]
    fn chain_id_default_is_mainnet() {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("SENTRIX_CHAIN_ID");
            assert_eq!(get_chain_id(), 7119);
            assert_eq!(get_chain_id(), CHAIN_ID);
        }
    }

    #[test]
    fn chain_id_env_override_round_trip() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("SENTRIX_CHAIN_ID", "7120");
            assert_eq!(get_chain_id(), 7120);

            std::env::set_var("SENTRIX_CHAIN_ID", "not_a_number");
            assert_eq!(get_chain_id(), CHAIN_ID);

            std::env::remove_var("SENTRIX_CHAIN_ID");
            assert_eq!(get_chain_id(), CHAIN_ID);
        }
    }

    /// If someone bumps to 2+ they're claiming a hash-algorithm
    /// migration. The chain header decoder enforces this; the const-
    /// assert pins the literal so a stray edit fails the build.
    const _: () = assert!(HASH_VERSION == 1);

    /// Sliding window must be > 0 (used as a slice index) and smaller
    /// than mempool size cap (no point keeping more blocks in RAM than
    /// the mempool can hold pending txs).
    const _: () = assert!(CHAIN_WINDOW_SIZE > 0);
    const _: () = assert!(CHAIN_WINDOW_SIZE <= crate::blockchain::MAX_MEMPOOL_SIZE);
}
