//! `Blockchain` block + chain-state accessors. Seven `impl Blockchain`
//! methods extracted from `blockchain.rs` so all the read-side
//! "where am I / give me block N" queries live in one focused file.
//!
//! Methods covered:
//! - `height` / `chain_window_start` — sliding-window position.
//! - `latest_block` / `get_block` / `get_block_by_hash` — in-memory
//!   window lookups, return `None` (or `Err`) on miss.
//! - `get_block_any` — transparent MDBX fallback for blocks evicted
//!   from the in-memory window (BACKLOG #14 — used by the `GetBlocks`
//!   peer handler so fresh / forensic-restored peers can deep-history
//!   backfill).
//! - `get_block_reward` — fork-aware halving + remaining-supply cap;
//!   reads via the tokenomics-math delegators.
//!
//! Rust permits splitting `impl T { … }` across multiple modules in
//! the same crate, so call sites stay `bc.height()` / `bc.get_block(i)`
//! / `bc.get_block_reward()` regardless of which file the impl lives in.

use sentrix_primitives::block::Block;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use sentrix_storage::tables;

use crate::blockchain::Blockchain;
use crate::tokenomics::BLOCK_REWARD;

impl Blockchain {
    // Height is derived from the last block's index, not chain.len()-1.
    // The chain is a sliding window so chain.len() ≤ CHAIN_WINDOW_SIZE.
    pub fn height(&self) -> u64 {
        self.chain.last().map(|b| b.index).unwrap_or(0)
    }

    /// First block index currently held in the in-memory window.
    /// Blocks with index < `chain_window_start()` are only in MDBX storage.
    pub fn chain_window_start(&self) -> u64 {
        self.chain.first().map(|b| b.index).unwrap_or(0)
    }

    /// Returns `Err` instead of panicking when the chain is empty.
    pub fn latest_block(&self) -> SentrixResult<&Block> {
        self.chain
            .last()
            .ok_or_else(|| SentrixError::NotFound("chain is empty".to_string()))
    }

    /// In-memory window lookup. Returns `None` for blocks outside the
    /// window — historical access goes through [`Blockchain::get_block_any`]
    /// which falls back to MDBX storage.
    pub fn get_block(&self, index: u64) -> Option<&Block> {
        let window_start = self.chain_window_start();
        if index < window_start {
            return None;
        }
        self.chain.get((index - window_start) as usize)
    }

    pub fn get_block_by_hash(&self, hash: &str) -> Option<&Block> {
        self.chain.iter().find(|b| b.hash == hash)
    }

    /// Block lookup that transparently falls back to MDBX storage for
    /// blocks evicted from the in-memory sliding window. Returns an
    /// owned `Block` (cloning in the window case, fresh deserialise in
    /// the storage case).
    ///
    /// Added for BACKLOG #14: the `GetBlocks` request-response handler
    /// used to call `get_block` directly and silently dropped every
    /// request for blocks older than `CHAIN_WINDOW_SIZE`, stranding
    /// fresh or forensic-restored peers that needed a deep history
    /// back-fill. Live validators keep full history in MDBX, so this
    /// fallback just serves what already exists on disk.
    ///
    /// Returns `None` only when both the in-memory window misses AND the
    /// MDBX store has no block at that index (i.e. the block was never
    /// produced or the storage handle was never bound — fresh test
    /// Blockchains with no `mdbx_storage` hit the latter).
    pub fn get_block_any(&self, index: u64) -> Option<Block> {
        if let Some(b) = self.get_block(index) {
            return Some(b.clone());
        }
        let mdbx = self.mdbx_storage.as_ref()?;
        let key = format!("block:{}", index);
        // Pre-fix this chain was `mdbx.get(...).ok().flatten()?` +
        // `serde_json::from_slice(&bytes).ok()` which lumped MDBX I/O
        // errors and JSON decode failures into the same silent-None
        // path as a legitimate miss. CR on PR #660 flagged that —
        // an operator with a corrupt MDBX row or a binary skew (block
        // JSON shape change without migration) had no observable
        // signal. Keep the `Option<Block>` signature so the ~10
        // callers (libp2p_node `GetBlocks`, explorer.rs, eth.rs) don't
        // all need to change; surface errors via tracing::warn before
        // collapsing to None.
        let bytes = match mdbx.get(tables::TABLE_META, key.as_bytes()) {
            Ok(Some(b)) => b,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(
                    "get_block_any({}): MDBX read error on TABLE_META: {}",
                    index,
                    e
                );
                return None;
            }
        };
        match serde_json::from_slice::<Block>(&bytes) {
            Ok(b) => Some(b),
            Err(e) => {
                tracing::warn!(
                    "get_block_any({}): block JSON decode error: {}",
                    index,
                    e
                );
                None
            }
        }
    }

    /// Fork-aware block reward at the current height. Reads through the
    /// tokenomics-math delegators (`max_supply_for`, `halvings_at`) so the
    /// v1 / v2 schedule split (210M / 42M halving vs 315M / 126M halving)
    /// is centralised in `crate::tokenomics`.
    pub fn get_block_reward(&self) -> u64 {
        let h = self.height();
        let max_supply = self.max_supply_for(h);
        let remaining = max_supply.saturating_sub(self.total_minted);
        if remaining == 0 {
            return 0;
        }

        // P1: halving bit-shift overflow guard. `u64 >> 64+` is undefined
        // in Rust (panics in debug, implementation-defined in release).
        // `halvings_at` clamps to u32::MAX so `checked_shr` returns None
        // at ≥64 and the reward is zero (matching "halved to nothing"
        // semantics). Pre-fork: ~21×42M blocks (~28 years at 1s) to reach
        // 64 halvings. Post-fork: ~21×126M blocks (~84 years).
        //
        // Call the free function in `crate::tokenomics` directly — the
        // `Blockchain::halvings_at` thin wrapper is `fn` (crate-private)
        // and not visible across module boundaries.
        let halvings: u32 = crate::tokenomics::halvings_at(h);
        let reward = BLOCK_REWARD.checked_shr(halvings).unwrap_or(0);

        if reward == 0 {
            return 0;
        }

        reward.min(remaining)
    }
}
