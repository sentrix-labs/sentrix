// db.rs - Sentrix — Per-block persistent storage

use sentrix_primitives::block::Block;
use crate::blockchain::{Blockchain, CHAIN_WINDOW_SIZE};
use sentrix_primitives::error::{SentrixError, SentrixResult};
use serde::{Serialize, de::DeserializeOwned};
use sled::Db;

pub struct Storage {
    db: Db,
}

impl Storage {
    pub fn open(path: &str) -> SentrixResult<Self> {
        // A10: Disk encryption is enforced in production-like envs by default.
        // The operator MUST set SENTRIX_ENCRYPTED_DISK=true to confirm the
        // mount holding `path` is encrypted (LUKS/dm-crypt/BitLocker/FDE).
        //
        // Escape hatch for test/dev runs: SENTRIX_ALLOW_UNENCRYPTED_DISK=true
        // downgrades the failure to a logged warning. This flag should never
        // be set on the validator VPS — it is intended for laptops, CI
        // runners, and ephemeral integration tests only.
        let encrypted = std::env::var("SENTRIX_ENCRYPTED_DISK").as_deref() == Ok("true");
        let dev_override = std::env::var("SENTRIX_ALLOW_UNENCRYPTED_DISK").as_deref() == Ok("true");
        if !encrypted {
            if dev_override {
                tracing::warn!(
                    "SECURITY WARNING: opening chain DB at '{}' without disk \
                     encryption confirmation (SENTRIX_ALLOW_UNENCRYPTED_DISK=true). \
                     Do NOT use this flag on validator nodes.",
                    path
                );
            } else {
                tracing::error!(
                    "REFUSING TO OPEN chain DB at '{}': SENTRIX_ENCRYPTED_DISK is not 'true'. \
                     Confirm the underlying volume is encrypted (LUKS/dm-crypt/BitLocker/FDE) \
                     and re-launch with SENTRIX_ENCRYPTED_DISK=true. \
                     For ephemeral test runs only, set SENTRIX_ALLOW_UNENCRYPTED_DISK=true.",
                    path
                );
                return Err(SentrixError::StorageError(
                    "disk encryption not confirmed: set SENTRIX_ENCRYPTED_DISK=true \
                     (or SENTRIX_ALLOW_UNENCRYPTED_DISK=true for dev/test)"
                        .to_string(),
                ));
            }
        }

        let db = sled::open(path).map_err(|e| SentrixError::StorageError(e.to_string()))?;
        let storage = Self { db };
        // Re-index blocks missing a hash→index entry (one-time migration for pre-index data)
        storage.ensure_hash_index()?;
        Ok(storage)
    }

    // Scan all stored blocks and write missing hash→index entries.
    // O(1) check via sentinel key — skip the full O(n) scan on subsequent opens.
    pub fn ensure_hash_index(&self) -> SentrixResult<()> {
        // Fast path: if hash_index_complete marker exists, all blocks are already indexed.
        if self.db.contains_key("hash_index_complete").unwrap_or(false) {
            return Ok(());
        }

        let height = self.load_height()?;
        let mut indexed_any = false;
        for i in 0..=height {
            let key = format!("block:{}", i);
            if let Some(block) = self.get::<Block>(&key)? {
                indexed_any = true;
                let hash_key = format!("hash:{}", block.hash);
                if self.get::<u64>(&hash_key)?.is_none() {
                    self.put(&hash_key, &block.index)?;
                }
            }
        }

        // Mark indexing as complete so future opens skip the scan.
        // Only set sentinel if blocks actually exist — prevents false-positive
        // on empty DBs (e.g. first open before any blocks are stored).
        if indexed_any {
            self.put("hash_index_complete", &true)?;
        }
        Ok(())
    }

    // ── Generic put/get ──────────────────────────────────

    fn put<T: Serialize>(&self, key: &str, value: &T) -> SentrixResult<()> {
        let bytes = serde_json::to_vec(value)?;
        self.db
            .insert(key, bytes)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    fn get<T: DeserializeOwned>(&self, key: &str) -> SentrixResult<Option<T>> {
        match self
            .db
            .get(key)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?
        {
            Some(bytes) => {
                let value = serde_json::from_slice(&bytes)?;
                Ok(Some(value))
            }
            None => Ok(None),
        }
    }

    fn flush(&self) -> SentrixResult<()> {
        self.db
            .flush()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    // ── Blockchain state (everything except blocks) ──────

    pub fn save_blockchain(&self, blockchain: &Blockchain) -> SentrixResult<()> {
        // Save state (accounts, authority, contracts, mempool, metadata)
        self.put("state", blockchain)?;

        // Save each block individually: key = "block:{index}" + hash index.
        // Always overwrite — no contains_key guard — so an H2 block (with recomputed
        // state_root hash) corrects any stale H1 entry that was written before
        // add_block() stamped the state_root (fork-prevention, PR #78).
        for block in &blockchain.chain {
            let key = format!("block:{}", block.index);
            self.put(&key, block)?;
            // Hash → index lookup (old hash entries for the same index become
            // harmless orphans in sled; they are never queried in normal operation)
            let hash_key = format!("hash:{}", block.hash);
            self.put(&hash_key, &block.index)?;
        }

        self.save_height(blockchain.height())?;
        self.flush()?;
        Ok(())
    }

    // chain field is #[serde(skip)] — always reconstructed from individual per-block sled keys
    pub fn load_blockchain(&self) -> SentrixResult<Option<Blockchain>> {
        // Try new format (state + per-block)
        if let Some(mut bc) = self.get::<Blockchain>("state")? {
            // Load only the sliding window (last CHAIN_WINDOW_SIZE blocks) into RAM.
            // Older blocks remain in sled and are accessible on-demand via load_block().
            let height = self.load_height()?;
            let window_start = height.saturating_sub(CHAIN_WINDOW_SIZE as u64 - 1);
            let mut blocks = Vec::with_capacity((height - window_start + 1) as usize);
            for i in window_start..=height {
                match self.load_block(i)? {
                    Some(block) => blocks.push(block),
                    None => {
                        // Missing block detected during window reconstruction — node will re-sync from peers.
                        let effective = if let Some(last) = blocks.last() {
                            last.index
                        } else {
                            // First block in window missing — scan backwards
                            let mut h = window_start.saturating_sub(1);
                            while h > 0 && self.load_block(h)?.is_none() {
                                h = h.saturating_sub(1);
                            }
                            // Reload window from found height
                            let new_start = h.saturating_sub(CHAIN_WINDOW_SIZE as u64 - 1);
                            for j in new_start..=h {
                                if let Some(b) = self.load_block(j)? {
                                    blocks.push(b);
                                }
                            }
                            h
                        };
                        tracing::warn!(
                            "Block {} missing in sled (stored height = {}). \
                             Adjusting height to {}. Node will re-sync from peers.",
                            i,
                            height,
                            effective
                        );
                        self.save_height(effective)?;
                        break;
                    }
                }
            }
            bc.chain = blocks;
            // Restore the state trie from sled after loading blockchain state
            if let Err(e) = bc.init_trie(&self.db) {
                tracing::warn!("trie init failed after blockchain load: {}", e);
            }
            // A5: bind storage handles for txid_index lookups, backfill once.
            if let Err(e) = bc.init_storage_handle(&self.db) {
                tracing::warn!("txid_index init failed: {}", e);
            } else {
                match bc.backfill_txid_index(&self.db) {
                    Ok(0) => {} // already populated
                    Ok(n) => {
                        tracing::info!("txid_index: backfilled {} tx entries from sled blocks", n)
                    }
                    Err(e) => tracing::warn!("txid_index backfill failed: {}", e),
                }
            }
            return Ok(Some(bc));
        }

        // Fallback: old single-blob format (pre-M-04 migration)
        if let Some(mut bc) = self.get::<Blockchain>("blockchain")? {
            // Migrate: save in new format
            self.save_blockchain(&bc)?;
            let _ = self.db.remove("blockchain");
            // Trim to window after migration
            if bc.chain.len() > CHAIN_WINDOW_SIZE {
                let excess = bc.chain.len() - CHAIN_WINDOW_SIZE;
                bc.chain.drain(..excess);
            }
            // Restore the state trie from sled (migration from legacy storage format)
            if let Err(e) = bc.init_trie(&self.db) {
                tracing::warn!("trie init failed after blockchain migration: {}", e);
            }
            // A5: bind storage handles + backfill txid_index after legacy migration.
            if let Err(e) = bc.init_storage_handle(&self.db) {
                tracing::warn!("txid_index init failed: {}", e);
            } else {
                match bc.backfill_txid_index(&self.db) {
                    Ok(0) => {}
                    Ok(n) => {
                        tracing::info!("txid_index: backfilled {} tx entries from sled blocks", n)
                    }
                    Err(e) => tracing::warn!("txid_index backfill failed: {}", e),
                }
            }
            return Ok(Some(bc));
        }

        Ok(None)
    }

    // ── Per-block operations ─────────────────────────────

    pub fn save_block(&self, block: &Block) -> SentrixResult<()> {
        let key = format!("block:{}", block.index);
        self.put(&key, block)?;
        let hash_key = format!("hash:{}", block.hash);
        self.put(&hash_key, &block.index)?;
        // Keep height key current so init_trie() loads the correct trie root on restart.
        // Critical for P2P sync path: save_blockchain() is NOT called per-block there.
        self.save_height(block.index)?;
        self.flush()?;
        Ok(())
    }

    pub fn load_block(&self, index: u64) -> SentrixResult<Option<Block>> {
        let key = format!("block:{}", index);
        self.get(&key)
    }

    pub fn load_block_by_hash(&self, hash: &str) -> SentrixResult<Option<Block>> {
        // O(1) lookup via hash index (ensure_hash_index() at open() guarantees all blocks are indexed)
        let hash_key = format!("hash:{}", hash);
        if let Some(index) = self.get::<u64>(&hash_key)? {
            return self.load_block(index);
        }
        Ok(None)
    }

    pub fn load_blocks_range(&self, from: u64, to: u64) -> SentrixResult<Vec<Block>> {
        let mut blocks = Vec::new();
        for i in from..=to {
            if let Some(block) = self.load_block(i)? {
                blocks.push(block);
            }
        }
        Ok(blocks)
    }

    // ── Height ───────────────────────────────────────────

    pub fn save_height(&self, height: u64) -> SentrixResult<()> {
        self.put("height", &height)
    }

    pub fn load_height(&self) -> SentrixResult<u64> {
        Ok(self.get::<u64>("height")?.unwrap_or(0))
    }

    // ── Utility ──────────────────────────────────────────

    pub fn has_blockchain(&self) -> bool {
        self.db.contains_key("state").unwrap_or(false)
            || self.db.contains_key("blockchain").unwrap_or(false)
    }

    pub fn clear(&self) -> SentrixResult<()> {
        self.db
            .clear()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    /// Drop and re-create the three trie named trees, clearing all trie state.
    /// On next startup init_trie() will detect no committed root and backfill from AccountDB.
    pub fn reset_trie(&self) -> SentrixResult<()> {
        for tree_name in &[
            "trie_nodes",
            "trie_values",
            "trie_roots",
            "trie_committed_roots",
        ] {
            self.db.drop_tree(tree_name).map_err(|e| {
                SentrixError::StorageError(format!("failed to drop {}: {}", tree_name, e))
            })?;
        }
        self.db
            .flush()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    pub fn db_size_bytes(&self) -> u64 {
        self.db.size_on_disk().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockchain::Blockchain;

    fn temp_db_path() -> String {
        let dir = std::env::temp_dir().join(format!("sentrix_test_{}", uuid::Uuid::new_v4()));
        dir.to_str().unwrap().to_string()
    }

    #[test]
    fn test_open_storage() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();
        assert!(!storage.has_blockchain());
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_save_and_load_blockchain() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let mut bc = Blockchain::new("admin".to_string());
        bc.authority.add_validator_unchecked(
            "val1".to_string(),
            "Validator 1".to_string(),
            "pk1".to_string(),
        );

        storage.save_blockchain(&bc).unwrap();
        assert!(storage.has_blockchain());

        let loaded = storage.load_blockchain().unwrap().unwrap();
        assert_eq!(loaded.height(), bc.height());
        assert_eq!(loaded.total_minted, bc.total_minted);
        assert_eq!(loaded.chain_id, bc.chain_id);
        assert_eq!(loaded.chain.len(), bc.chain.len());

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_per_block_storage() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let mut bc = Blockchain::new("admin".to_string());
        bc.authority.add_validator_unchecked(
            "val1".to_string(),
            "V1".to_string(),
            "pk1".to_string(),
        );

        // Produce a block
        let block = bc.create_block("val1").unwrap();
        bc.add_block(block).unwrap();

        storage.save_blockchain(&bc).unwrap();

        // Load individual blocks
        let b0 = storage.load_block(0).unwrap().unwrap();
        assert_eq!(b0.index, 0);

        let b1 = storage.load_block(1).unwrap().unwrap();
        assert_eq!(b1.index, 1);
        assert_eq!(b1.validator, "val1");

        // Block that doesn't exist
        assert!(storage.load_block(99).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_load_block_by_hash() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let bc = Blockchain::new("admin".to_string());
        storage.save_blockchain(&bc).unwrap();

        let genesis_hash = bc.chain[0].hash.clone();
        let found = storage.load_block_by_hash(&genesis_hash).unwrap().unwrap();
        assert_eq!(found.index, 0);

        assert!(storage.load_block_by_hash("nonexistent").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_load_blocks_range() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let mut bc = Blockchain::new("admin".to_string());
        bc.authority.add_validator_unchecked(
            "val1".to_string(),
            "V1".to_string(),
            "pk1".to_string(),
        );

        for _ in 0..3 {
            let block = bc.create_block("val1").unwrap();
            bc.add_block(block).unwrap();
        }
        storage.save_blockchain(&bc).unwrap();

        let range = storage.load_blocks_range(1, 3).unwrap();
        assert_eq!(range.len(), 3);
        assert_eq!(range[0].index, 1);
        assert_eq!(range[2].index, 3);

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_save_and_load_height() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        storage.save_height(42).unwrap();
        let height = storage.load_height().unwrap();
        assert_eq!(height, 42);

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_persistence_across_reopen() {
        let path = temp_db_path();

        {
            let storage = Storage::open(&path).unwrap();
            let bc = Blockchain::new("admin".to_string());
            storage.save_blockchain(&bc).unwrap();
        }

        {
            let storage = Storage::open(&path).unwrap();
            assert!(storage.has_blockchain());
            let loaded = storage.load_blockchain().unwrap().unwrap();
            assert_eq!(loaded.height(), 0);
            // Verify per-block retrieval works after reopen
            let b0 = storage.load_block(0).unwrap().unwrap();
            assert_eq!(b0.index, 0);
        }

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_db_size() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();
        let bc = Blockchain::new("admin".to_string());
        storage.save_blockchain(&bc).unwrap();
        assert!(storage.db_size_bytes() > 0);
        let _ = std::fs::remove_dir_all(&path);
    }

    // ── PR-78: save_blockchain overwrites stale block (H1→H2 correction) ──

    #[test]
    fn test_save_blockchain_overwrites_stale_block() {
        // Regression: save_blockchain must overwrite an existing block so that
        // an H2 block (state_root + recomputed hash) corrects any stale H1 entry.
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let bc = Blockchain::new("admin".to_string());
        let canonical_hash = bc.chain[0].hash.clone();

        // Simulate an H1 block already on disk with a wrong hash.
        let mut stale = bc.chain[0].clone();
        stale.hash = "stale_h1_hash".to_string();
        storage.save_block(&stale).unwrap();

        // save_blockchain carries the canonical (H2) version — must overwrite.
        storage.save_blockchain(&bc).unwrap();

        let stored = storage.load_block(0).unwrap().unwrap();
        assert_eq!(
            stored.hash, canonical_hash,
            "save_blockchain must overwrite stale H1 block with canonical H2 block"
        );

        let _ = std::fs::remove_dir_all(&path);
    }

    // ── L-01: ensure_hash_index migration tests ──────────

    #[test]
    fn test_l01_ensure_hash_index_repairs_missing_entries() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        // Save a block without its hash index (simulate old data)
        let bc = Blockchain::new("admin".to_string());
        let block = &bc.chain[0];
        let key = format!("block:{}", block.index);
        storage.put(&key, block).unwrap();
        storage.save_height(0).unwrap();
        // Don't write hash:{hash} — simulating pre-index old data

        // Hash lookup should fail before migration
        let hash_key = format!("hash:{}", block.hash);
        assert!(storage.get::<u64>(&hash_key).unwrap().is_none());

        // ensure_hash_index() must repair it
        storage.ensure_hash_index().unwrap();
        let index = storage.get::<u64>(&hash_key).unwrap();
        assert_eq!(index, Some(0));

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_l01_load_block_by_hash_returns_none_without_index() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        // Empty DB — no blocks, no hash entries
        let result = storage.load_block_by_hash("nonexistent_hash").unwrap();
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&path);
    }
}
