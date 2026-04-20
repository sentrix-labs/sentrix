// db.rs - Sentrix — Per-block persistent storage (MDBX backend)

use crate::blockchain::{Blockchain, CHAIN_WINDOW_SIZE};
use sentrix_primitives::block::Block;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use sentrix_storage::{ChainStorage, MdbxStorage};
use std::sync::Arc;

pub struct Storage {
    chain: ChainStorage,
}

impl Storage {
    pub fn open(path: &str) -> SentrixResult<Self> {
        let chain =
            ChainStorage::open(path).map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(Self { chain })
    }

    pub fn ensure_hash_index(&self) -> SentrixResult<()> {
        self.chain
            .ensure_hash_index()
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    /// Get a cloneable Arc handle to the underlying MdbxStorage.
    /// Used for trie init and txid index binding.
    pub fn mdbx_arc(&self) -> Arc<MdbxStorage> {
        self.chain.mdbx_arc()
    }

    // ── Blockchain state (everything except blocks) ──────

    pub fn save_blockchain(&self, blockchain: &Blockchain) -> SentrixResult<()> {
        self.chain
            .save_blockchain(blockchain, &blockchain.chain)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    pub fn load_blockchain(&self) -> SentrixResult<Option<Blockchain>> {
        // Try loading state
        let mut bc: Blockchain = match self
            .chain
            .load_state()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?
        {
            Some(bc) => bc,
            None => {
                // Fallback: old "blockchain" key in meta table
                // (not expected with fresh MDBX DBs, but handles migration edge cases)
                return Ok(None);
            }
        };

        // Load only the sliding window (last CHAIN_WINDOW_SIZE blocks) into RAM.
        let height = self
            .chain
            .load_height()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        let window_start = height.saturating_sub(CHAIN_WINDOW_SIZE as u64 - 1);
        let mut blocks = Vec::with_capacity((height - window_start + 1) as usize);
        for i in window_start..=height {
            match self.load_block(i)? {
                Some(block) => blocks.push(block),
                None => {
                    let effective = if let Some(last) = blocks.last() {
                        last.index
                    } else {
                        let mut h = window_start.saturating_sub(1);
                        while h > 0 && self.load_block(h)?.is_none() {
                            h = h.saturating_sub(1);
                        }
                        let new_start = h.saturating_sub(CHAIN_WINDOW_SIZE as u64 - 1);
                        for j in new_start..=h {
                            if let Some(b) = self.load_block(j)? {
                                blocks.push(b);
                            }
                        }
                        h
                    };
                    tracing::warn!(
                        "Block {} missing (stored height = {}). \
                         Adjusting height to {}. Node will re-sync from peers.",
                        i,
                        height,
                        effective
                    );
                    self.chain
                        .save_height(effective)
                        .map_err(|e| SentrixError::StorageError(e.to_string()))?;
                    break;
                }
            }
        }
        bc.chain = blocks;

        // M-05: validate the in-memory chain window on load so a
        // corrupted DB is surfaced instead of silently serving stale
        // data via eth_getBlock* / get_transaction until a peer gossip
        // later discovers the divergence. Only warn (do not fail
        // start-up) because live nodes can recover via P2P sync; a
        // hard failure would keep an otherwise functional validator
        // offline when its peers could re-populate the chain window.
        if !bc.is_valid_chain_window() {
            tracing::error!(
                "M-05: loaded chain window failed integrity check (height={}, window_len={}). \
                 Node will continue but is likely to re-sync from peers; investigate \
                 the underlying DB corruption before this recurs.",
                height,
                bc.chain.len()
            );
        }

        // Restore state trie from MDBX.
        //
        // HARD-FAIL on trie init failure above STATE_ROOT_FORK_HEIGHT: past the
        // fork height, `state_root` is part of the block hash, so a node that
        // silently fails trie init would produce blocks with `state_root = None`
        // while peers with working tries produce `state_root = Some(...)`. The
        // hashes diverge → chain fork. Mainnet stall on 2026-04-20 was caused
        // by exactly this silent-warn path. Refusing to start surfaces broken
        // trie state to operators immediately; the chain tolerates one
        // validator offline better than a silently-diverging validator.
        //
        // Below fork height the old hash format ignores state_root entirely, so
        // a failed trie init cannot cause consensus divergence — warn-only is
        // still safe there.
        let mdbx = self.mdbx_arc();
        if let Err(e) = bc.init_trie(mdbx.clone()) {
            let h = bc.height();
            if h >= sentrix_primitives::block::STATE_ROOT_FORK_HEIGHT {
                return Err(SentrixError::Internal(format!(
                    "trie init failed at height {h}: {e}. Past STATE_ROOT_FORK_HEIGHT the trie \
                     must be functional — state_root is part of the block hash and silent trie \
                     failure causes chain forks. Resync from peers or wipe data dir to recover."
                )));
            }
            tracing::warn!(
                "trie init failed at height {} (below fork height — allowed): {}",
                h,
                e
            );
        }

        // Bind storage handles for txid_index lookups
        if let Err(e) = bc.init_storage_handle(mdbx.clone()) {
            tracing::warn!("txid_index init failed: {}", e);
        } else {
            match bc.backfill_txid_index(&mdbx) {
                Ok(0) => {}
                Ok(n) => {
                    tracing::info!("txid_index: backfilled {} tx entries from stored blocks", n)
                }
                Err(e) => tracing::warn!("txid_index backfill failed: {}", e),
            }
        }

        Ok(Some(bc))
    }

    // ── Per-block operations ─────────────────────────────

    pub fn save_block(&self, block: &Block) -> SentrixResult<()> {
        self.chain
            .save_block(block)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn load_block(&self, index: u64) -> SentrixResult<Option<Block>> {
        self.chain
            .load_block(index)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn load_block_by_hash(&self, hash: &str) -> SentrixResult<Option<Block>> {
        self.chain
            .load_block_by_hash(hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn load_blocks_range(&self, from: u64, to: u64) -> SentrixResult<Vec<Block>> {
        self.chain
            .load_blocks_range(from, to)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    // ── Height ───────────────────────────────────────────

    pub fn save_height(&self, height: u64) -> SentrixResult<()> {
        self.chain
            .save_height(height)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn load_height(&self) -> SentrixResult<u64> {
        self.chain
            .load_height()
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    // ── Utility ──────────────────────────────────────────

    pub fn has_blockchain(&self) -> bool {
        self.chain.has_blockchain()
    }

    pub fn clear(&self) -> SentrixResult<()> {
        self.chain
            .clear()
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn reset_trie(&self) -> SentrixResult<()> {
        self.chain
            .reset_trie()
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn db_size_bytes(&self) -> u64 {
        self.chain.db_size_bytes()
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

        let block = bc.create_block("val1").unwrap();
        bc.add_block(block).unwrap();

        storage.save_blockchain(&bc).unwrap();

        let b0 = storage.load_block(0).unwrap().unwrap();
        assert_eq!(b0.index, 0);

        let b1 = storage.load_block(1).unwrap().unwrap();
        assert_eq!(b1.index, 1);
        assert_eq!(b1.validator, "val1");

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

    #[test]
    fn test_save_blockchain_overwrites_stale_block() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let bc = Blockchain::new("admin".to_string());
        let canonical_hash = bc.chain[0].hash.clone();

        let mut stale = bc.chain[0].clone();
        stale.hash = "stale_h1_hash".to_string();
        storage.save_block(&stale).unwrap();

        storage.save_blockchain(&bc).unwrap();

        let stored = storage.load_block(0).unwrap().unwrap();
        assert_eq!(
            stored.hash, canonical_hash,
            "save_blockchain must overwrite stale H1 block with canonical H2 block"
        );

        let _ = std::fs::remove_dir_all(&path);
    }
}
