// chain.rs — High-level chain storage API wrapping MdbxStorage.
//
// Drop-in replacement for the current sled-based Storage in sentrix-core.
// Same method signatures so migration is a find-replace on the type.

use crate::error::{StorageError, StorageResult};
use crate::mdbx::{MdbxStorage, height_key, key_to_height};
use crate::tables::*;
use sentrix_primitives::block::Block;
use serde::{Serialize, de::DeserializeOwned};
use std::path::Path;
use std::sync::Arc;

/// High-level chain storage — wraps MdbxStorage with blockchain-specific methods.
///
/// Clone is cheap — `Arc<MdbxStorage>` is reference-counted.
/// Thread-safe: libmdbx `Database` supports concurrent read transactions.
#[derive(Clone)]
pub struct ChainStorage {
    mdbx: Arc<MdbxStorage>,
}

impl ChainStorage {
    /// Open the chain database at the given path.
    /// Applies the same disk-encryption checks as the old sled Storage.
    pub fn open(path: &str) -> StorageResult<Self> {
        let encrypted = std::env::var("SENTRIX_ENCRYPTED_DISK").as_deref() == Ok("true");
        let dev_override = std::env::var("SENTRIX_ALLOW_UNENCRYPTED_DISK").as_deref() == Ok("true");
        if !encrypted {
            if dev_override {
                tracing::warn!(
                    "SECURITY WARNING: opening chain DB at '{}' without disk encryption \
                     confirmation (SENTRIX_ALLOW_UNENCRYPTED_DISK=true).",
                    path
                );
            } else {
                tracing::error!(
                    "REFUSING TO OPEN chain DB at '{}': SENTRIX_ENCRYPTED_DISK is not 'true'.",
                    path
                );
                return Err(StorageError::Other(
                    "disk encryption not confirmed: set SENTRIX_ENCRYPTED_DISK=true \
                     (or SENTRIX_ALLOW_UNENCRYPTED_DISK=true for dev/test)"
                        .to_string(),
                ));
            }
        }

        let mdbx = Arc::new(MdbxStorage::open(Path::new(path))?);

        let storage = Self { mdbx };
        storage.ensure_hash_index()?;
        Ok(storage)
    }

    // ── Generic put/get (JSON encoding, matching old sled Storage) ───

    fn put<T: Serialize>(&self, key: &str, value: &T) -> StorageResult<()> {
        self.mdbx.put_json(TABLE_META, key.as_bytes(), value)
    }

    fn get<T: DeserializeOwned>(&self, key: &str) -> StorageResult<Option<T>> {
        self.mdbx.get_json(TABLE_META, key.as_bytes())
    }

    // ── Hash index migration (same as old sled Storage) ─────

    pub fn ensure_hash_index(&self) -> StorageResult<()> {
        if self.mdbx.has(TABLE_META, b"hash_index_complete")? {
            return Ok(());
        }

        let height = self.load_height()?;
        let mut indexed_any = false;
        for i in 0..=height {
            if let Some(block) = self.load_block(i)? {
                indexed_any = true;
                if !self.mdbx.has(TABLE_BLOCK_HASHES, block.hash.as_bytes())? {
                    self.mdbx.put(
                        TABLE_BLOCK_HASHES,
                        block.hash.as_bytes(),
                        &height_key(block.index),
                    )?;
                }
            }
        }

        if indexed_any {
            self.put("hash_index_complete", &true)?;
        }
        Ok(())
    }

    // ── Blockchain state ────────────────────────────────────

    pub fn save_blockchain<T: Serialize>(
        &self,
        blockchain: &T,
        chain: &[Block],
    ) -> StorageResult<()> {
        // Save state (accounts, authority, etc.)
        self.mdbx.put_json(TABLE_STATE, b"state", blockchain)?;

        // Save each block + hash index
        let batch = self.mdbx.begin_write()?;
        for block in chain {
            let key = format!("block:{}", block.index);
            let block_json = serde_json::to_vec(block)?;
            batch.put(TABLE_META, key.as_bytes(), &block_json)?;

            batch.put(
                TABLE_BLOCK_HASHES,
                block.hash.as_bytes(),
                &height_key(block.index),
            )?;
        }
        batch.commit()?;

        if let Some(last) = chain.last() {
            self.save_height(last.index)?;
        }
        self.mdbx.sync()?;
        Ok(())
    }

    pub fn load_state<T: DeserializeOwned>(&self) -> StorageResult<Option<T>> {
        self.mdbx.get_json(TABLE_STATE, b"state")
    }

    // ── Per-block operations ────────────────────────────────

    pub fn save_block(&self, block: &Block) -> StorageResult<()> {
        let key = format!("block:{}", block.index);
        let block_json = serde_json::to_vec(block)?;
        self.mdbx.put(TABLE_META, key.as_bytes(), &block_json)?;
        self.mdbx.put(
            TABLE_BLOCK_HASHES,
            block.hash.as_bytes(),
            &height_key(block.index),
        )?;
        self.save_height(block.index)?;
        self.mdbx.sync()?;
        Ok(())
    }

    pub fn load_block(&self, index: u64) -> StorageResult<Option<Block>> {
        let key = format!("block:{}", index);
        self.get(&key)
    }

    pub fn load_block_by_hash(&self, hash: &str) -> StorageResult<Option<Block>> {
        if let Some(height_bytes) = self.mdbx.get(TABLE_BLOCK_HASHES, hash.as_bytes())? {
            let index = key_to_height(&height_bytes);
            return self.load_block(index);
        }
        Ok(None)
    }

    pub fn load_blocks_range(&self, from: u64, to: u64) -> StorageResult<Vec<Block>> {
        let mut blocks = Vec::new();
        for i in from..=to {
            if let Some(block) = self.load_block(i)? {
                blocks.push(block);
            }
        }
        Ok(blocks)
    }

    // ── Height ──────────────────────────────────────────────

    pub fn save_height(&self, height: u64) -> StorageResult<()> {
        self.put("height", &height)
    }

    pub fn load_height(&self) -> StorageResult<u64> {
        Ok(self.get::<u64>("height")?.unwrap_or(0))
    }

    // ── Trie tree access (returns raw MdbxStorage for trie backend) ──

    /// Get a reference to the underlying MdbxStorage for trie operations.
    /// The trie backend needs direct table access for nodes/values/roots.
    pub fn mdbx(&self) -> &MdbxStorage {
        &self.mdbx
    }

    /// Get a cloneable Arc handle to MdbxStorage (for sharing with trie/blockchain).
    pub fn mdbx_arc(&self) -> Arc<MdbxStorage> {
        Arc::clone(&self.mdbx)
    }

    // ── Tx index ────────────────────────────────────────────

    pub fn index_tx(&self, tx_hash: &str, block_index: u64) -> StorageResult<()> {
        self.mdbx
            .put(TABLE_TX_INDEX, tx_hash.as_bytes(), &height_key(block_index))
    }

    pub fn find_tx_block(&self, tx_hash: &str) -> StorageResult<Option<u64>> {
        match self.mdbx.get(TABLE_TX_INDEX, tx_hash.as_bytes())? {
            Some(bytes) => Ok(Some(key_to_height(&bytes))),
            None => Ok(None),
        }
    }

    // ── Utility ─────────────────────────────────────────────

    pub fn has_blockchain(&self) -> bool {
        self.mdbx.has(TABLE_STATE, b"state").unwrap_or(false)
            || self.mdbx.has(TABLE_META, b"blockchain").unwrap_or(false)
    }

    pub fn clear(&self) -> StorageResult<()> {
        for table in ALL_TABLES {
            self.mdbx.clear_table(table)?;
        }
        Ok(())
    }

    pub fn reset_trie(&self) -> StorageResult<()> {
        self.mdbx.clear_table(TABLE_TRIE_NODES)?;
        self.mdbx.clear_table(TABLE_TRIE_VALUES)?;
        self.mdbx.clear_table(TABLE_TRIE_ROOTS)?;
        self.mdbx.clear_table(TABLE_TRIE_COMMITTED)?;
        self.mdbx.sync()?;
        Ok(())
    }

    pub fn db_size_bytes(&self) -> u64 {
        self.mdbx.db_size_bytes().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> String {
        let dir = std::env::temp_dir().join(format!("sentrix_mdbx_test_{}", uuid::Uuid::new_v4()));
        dir.to_str().unwrap().to_string()
    }

    #[test]
    fn test_open_chain_storage() {
        let path = temp_path();
        let storage = ChainStorage::open(&path).unwrap();
        assert!(!storage.has_blockchain());
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_save_and_load_block() {
        let path = temp_path();
        let storage = ChainStorage::open(&path).unwrap();

        let block = Block::genesis();
        storage.save_block(&block).unwrap();

        let loaded = storage.load_block(0).unwrap().unwrap();
        assert_eq!(loaded.index, 0);
        assert_eq!(loaded.hash, block.hash);

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_load_block_by_hash() {
        let path = temp_path();
        let storage = ChainStorage::open(&path).unwrap();

        let block = Block::genesis();
        storage.save_block(&block).unwrap();

        let found = storage.load_block_by_hash(&block.hash).unwrap().unwrap();
        assert_eq!(found.index, 0);

        assert!(storage.load_block_by_hash("nonexistent").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_height() {
        let path = temp_path();
        let storage = ChainStorage::open(&path).unwrap();

        storage.save_height(42).unwrap();
        assert_eq!(storage.load_height().unwrap(), 42);

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_tx_index() {
        let path = temp_path();
        let storage = ChainStorage::open(&path).unwrap();

        storage.index_tx("abc123", 10).unwrap();
        assert_eq!(storage.find_tx_block("abc123").unwrap(), Some(10));
        assert_eq!(storage.find_tx_block("nonexistent").unwrap(), None);

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_persistence() {
        let path = temp_path();
        {
            let storage = ChainStorage::open(&path).unwrap();
            let block = Block::genesis();
            storage.save_block(&block).unwrap();
        }
        {
            let storage = ChainStorage::open(&path).unwrap();
            let loaded = storage.load_block(0).unwrap().unwrap();
            assert_eq!(loaded.index, 0);
        }
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_clear_and_reset_trie() {
        let path = temp_path();
        let storage = ChainStorage::open(&path).unwrap();

        storage.save_block(&Block::genesis()).unwrap();
        storage
            .mdbx()
            .put(TABLE_TRIE_NODES, b"node1", b"data")
            .unwrap();
        assert!(storage.load_block(0).unwrap().is_some());

        storage.reset_trie().unwrap();
        // Trie cleared but blocks still exist
        assert!(
            storage
                .mdbx()
                .get(TABLE_TRIE_NODES, b"node1")
                .unwrap()
                .is_none()
        );
        assert!(storage.load_block(0).unwrap().is_some());

        let _ = std::fs::remove_dir_all(&path);
    }
}
