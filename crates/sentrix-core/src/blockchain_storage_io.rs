//! `Blockchain` MDBX storage I/O — durable block persistence + txid
//! index management. Five `impl Blockchain` methods extracted from
//! `blockchain.rs` so the storage seam lives in one focused file.
//!
//! Methods covered:
//! - `init_storage_handle` — bind an MdbxStorage Arc at boot.
//! - `persist_block_durable` — atomic 4-op write (block bytes + hash
//!   index + height marker + sync). Paired with the BlockchainSnapshot
//!   rollback path in `add_block_impl` so a partial write rewinds the
//!   in-memory chain.
//! - `record_tx_in_index` — fire-and-warn write of one tx→height entry.
//!   No-op when `mdbx_storage` is None (unit tests).
//! - `lookup_tx_in_storage` — read-side fallback for `get_transaction`
//!   when the tx's block has been evicted from the in-memory window.
//! - `backfill_txid_index` — one-shot startup walk with a fast-path
//!   sample on the latest block so warm chains skip the full scan.
//!
//! Rust supports splitting `impl T { … }` across multiple files within
//! the same crate. The struct fields all stay accessible since
//! `blockchain.rs` declares them `pub` / `pub(crate)`.

use sentrix_primitives::block::Block;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use sentrix_storage::{MdbxStorage, height_key, key_to_height, tables};
use std::sync::Arc;

use crate::blockchain::Blockchain;

impl Blockchain {
    /// Bind MDBX storage handle so `get_transaction()` can resolve txids that
    /// fall outside the in-memory chain window. Cheap clone — `Arc<MdbxStorage>`.
    pub fn init_storage_handle(&mut self, mdbx: Arc<MdbxStorage>) -> SentrixResult<()> {
        self.mdbx_storage = Some(mdbx);
        Ok(())
    }

    /// BACKLOG #16 durable fix: atomically persist a block's MDBX record
    /// (TABLE_META + TABLE_BLOCK_HASHES + height bump + sync). Returns Ok
    /// only if all four mutations committed cleanly. Called from
    /// `add_block_impl` AFTER Pass-2 commit — paired with the existing
    /// `BlockchainSnapshot` rollback path so a persist failure here
    /// triggers in-memory rollback, keeping chain state and disk in
    /// lock-step.
    ///
    /// Returns `Err(StorageNotInitialised)` if `mdbx_storage` was never
    /// bound (unit tests with no storage backing). Callers should treat
    /// that as "skip persist" — no gap risk because there's no disk at
    /// all. A real production path always has `mdbx_storage` set via
    /// `init_storage_handle`, so any Err here is a real MDBX failure
    /// (disk full, lock contention, permissions, corruption).
    pub fn persist_block_durable(&self, block: &Block) -> SentrixResult<()> {
        let mdbx = self.mdbx_storage.as_ref().ok_or_else(|| {
            SentrixError::Internal("persist_block_durable: mdbx_storage not initialised".into())
        })?;

        let key = format!("block:{}", block.index);
        let block_json = serde_json::to_vec(block).map_err(|e| {
            SentrixError::Internal(format!("persist_block_durable: serialize block: {e}"))
        })?;

        // Same byte layout as `Storage::save_block` in sentrix-storage: (1)
        // block bytes in TABLE_META, (2) reverse hash→height index in
        // TABLE_BLOCK_HASHES, (3) height marker in TABLE_META, (4) sync.
        mdbx.put(tables::TABLE_META, key.as_bytes(), &block_json)
            .map_err(|e| {
                SentrixError::Internal(format!("persist_block_durable: TABLE_META put: {e}"))
            })?;
        mdbx.put(
            tables::TABLE_BLOCK_HASHES,
            block.hash.as_bytes(),
            &height_key(block.index),
        )
        .map_err(|e| {
            SentrixError::Internal(format!(
                "persist_block_durable: TABLE_BLOCK_HASHES put: {e}"
            ))
        })?;
        mdbx.put(tables::TABLE_META, b"height", &block.index.to_be_bytes())
            .map_err(|e| {
                SentrixError::Internal(format!("persist_block_durable: height put: {e}"))
            })?;
        mdbx.sync()
            .map_err(|e| SentrixError::Internal(format!("persist_block_durable: sync: {e}")))?;
        Ok(())
    }

    /// Record a tx → block_index mapping. Called by `add_block` for each
    /// tx in a freshly committed block. No-op if `init_storage_handle` was
    /// never called (e.g. unit tests with no storage backing).
    pub fn record_tx_in_index(&self, txid: &str, block_index: u64) {
        if let Some(mdbx) = &self.mdbx_storage {
            // Audit M5 (2026-05-06): pre-fix this swallowed put errors,
            // which would silently 404 `eth_getTransactionByHash` for a
            // tx already canonical in chain state. Don't propagate (block
            // apply already succeeded), but surface for ops.
            if let Err(e) = mdbx.put(
                tables::TABLE_TX_INDEX,
                txid.as_bytes(),
                &height_key(block_index),
            ) {
                tracing::warn!(
                    "record_tx_in_index({}, h={}): MDBX put failed: {}",
                    txid,
                    block_index,
                    e,
                );
            }
        }
    }

    /// Resolve a txid to its containing `(Block, block_index)` by
    /// consulting the MDBX txid_index then loading the block. Returns
    /// `None` if the txid is unknown or the storage handle was never
    /// initialised.
    pub fn lookup_tx_in_storage(&self, txid: &str) -> Option<(Block, u64)> {
        let mdbx = self.mdbx_storage.as_ref()?;
        let raw = mdbx
            .get(tables::TABLE_TX_INDEX, txid.as_bytes())
            .ok()
            .flatten()?;
        if raw.len() != 8 {
            return None;
        }
        let block_index = key_to_height(&raw);
        let key = format!("block:{}", block_index);
        let bytes = mdbx
            .get(tables::TABLE_META, key.as_bytes())
            .ok()
            .flatten()?;
        let block: Block = serde_json::from_slice(&bytes).ok()?;
        Some((block, block_index))
    }

    /// One-shot backfill — walk every stored block from genesis to the current
    /// height and populate the txid_index for any tx that does not already
    /// have an entry. Idempotent. Called once at startup.
    ///
    /// Fast path (issue #268): on a warm chain the index is already populated,
    /// so scanning every block is 500K+ redundant MDBX reads with zero writes.
    /// Before committing to the full scan, sample the LATEST block's last tx
    /// and check whether it's already indexed. If yes, assume warm and return
    /// immediately. A single deliberate gap in the tail is vanishingly unlikely
    /// to matter for UX (the next block's txs will be indexed via the regular
    /// `add_block` path); the next restart re-samples and heals any drift.
    ///
    /// Slow path logs progress every 50K blocks so operators see activity
    /// rather than a silent several-minute freeze during a cold-start
    /// backfill on a large chain.
    pub fn backfill_txid_index(&self, mdbx: &MdbxStorage) -> SentrixResult<usize> {
        if self.mdbx_storage.is_none() {
            return Ok(0);
        }
        let height = self.height();

        // Fast path: is the latest block's last tx already indexed?
        if let Some(latest) = self.latest_block().ok()
            && let Some(last_tx) = latest.transactions.last()
            && mdbx
                .get(tables::TABLE_TX_INDEX, last_tx.txid.as_bytes())
                .map_err(|e| SentrixError::StorageError(e.to_string()))?
                .is_some()
        {
            return Ok(0);
        }

        tracing::info!(
            "txid_index: scanning {} blocks for backfill (this can take minutes on large chains)",
            height + 1
        );

        const PROGRESS_STEP: u64 = 50_000;
        let mut written = 0usize;
        for i in 0..=height {
            let key = format!("block:{}", i);
            let bytes = match mdbx
                .get(tables::TABLE_META, key.as_bytes())
                .map_err(|e| SentrixError::StorageError(e.to_string()))?
            {
                Some(b) => b,
                None => continue,
            };
            let block: Block = match serde_json::from_slice(&bytes) {
                Ok(b) => b,
                Err(_) => continue,
            };
            for tx in &block.transactions {
                if mdbx
                    .get(tables::TABLE_TX_INDEX, tx.txid.as_bytes())
                    .map_err(|e| SentrixError::StorageError(e.to_string()))?
                    .is_none()
                {
                    mdbx.put(
                        tables::TABLE_TX_INDEX,
                        tx.txid.as_bytes(),
                        &height_key(block.index),
                    )
                    .map_err(|e| SentrixError::StorageError(e.to_string()))?;
                    written += 1;
                }
            }
            if i > 0 && i.is_multiple_of(PROGRESS_STEP) {
                tracing::info!(
                    "txid_index: scanned {}/{} blocks ({} entries written so far)",
                    i,
                    height + 1,
                    written
                );
            }
        }
        Ok(written)
    }
}
