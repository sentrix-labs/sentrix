// mdbx.rs — libmdbx wrapper for Sentrix chain storage.
//
// API: Database<NoWriteMap> → Transaction<RO/RW> → Table → Cursor.

use crate::error::{StorageError, StorageResult};
use crate::tables::ALL_TABLES;
use libmdbx::{
    Database, DatabaseOptions, NoWriteMap, TableFlags, Transaction, WriteFlags, RO, RW,
};
use serde::{Serialize, de::DeserializeOwned};
use std::path::Path;

/// Sentrix storage backed by libmdbx.
///
/// Thread-safe: libmdbx `Database` can be shared across threads.
/// Read transactions are lock-free. Write transactions are serialized.
pub struct MdbxStorage {
    db: Database<NoWriteMap>,
}

impl MdbxStorage {
    /// Open (or create) the MDBX database at the given path.
    /// Pre-creates all Sentrix tables on first open.
    pub fn open(path: &Path) -> StorageResult<Self> {
        std::fs::create_dir_all(path).map_err(|e| StorageError::Other(e.to_string()))?;

        let mut opts = DatabaseOptions::default();
        opts.max_tables = Some(16);

        let db = Database::<NoWriteMap>::open_with_options(path, opts)
            .map_err(|e| StorageError::Mdbx(format!("open: {e}")))?;

        // Pre-create all named tables
        {
            let tx = db.begin_rw_txn()?;
            for &table_name in ALL_TABLES {
                tx.create_table(Some(table_name), TableFlags::default())?;
            }
            tx.commit()?;
        }

        tracing::info!("MDBX storage opened at {:?} ({} tables)", path, ALL_TABLES.len());
        Ok(Self { db })
    }

    // ── Raw key-value operations ────────────────────────────

    /// Put a raw key-value pair into the given table.
    pub fn put(&self, table: &str, key: &[u8], value: &[u8]) -> StorageResult<()> {
        let tx = self.db.begin_rw_txn()?;
        let tbl = tx.open_table(Some(table))?;
        tx.put(&tbl, key, value, WriteFlags::default())?;
        tx.commit()?;
        Ok(())
    }

    /// Get a raw value from the given table. Returns None if key not found.
    pub fn get(&self, table: &str, key: &[u8]) -> StorageResult<Option<Vec<u8>>> {
        let tx = self.db.begin_ro_txn()?;
        let tbl = tx.open_table(Some(table))?;
        match tx.get::<Vec<u8>>(&tbl, key) {
            Ok(Some(val)) => Ok(Some(val)),
            Ok(None) => Ok(None),
            Err(libmdbx::Error::NotFound) => Ok(None),
            Err(e) => Err(StorageError::Mdbx(format!("get: {e}"))),
        }
    }

    /// Delete a key from the given table.
    pub fn delete(&self, table: &str, key: &[u8]) -> StorageResult<bool> {
        let tx = self.db.begin_rw_txn()?;
        let tbl = tx.open_table(Some(table))?;
        let deleted = tx.del(&tbl, key, None).is_ok();
        tx.commit()?;
        Ok(deleted)
    }

    /// Check if a key exists in the given table.
    pub fn has(&self, table: &str, key: &[u8]) -> StorageResult<bool> {
        Ok(self.get(table, key)?.is_some())
    }

    // ── Typed operations (bincode encoding) ─────────────────

    /// Put a serializable value into the given table (bincode).
    pub fn put_bincode<V: Serialize>(&self, table: &str, key: &[u8], value: &V) -> StorageResult<()> {
        let encoded = bincode::serialize(value)?;
        self.put(table, key, &encoded)
    }

    /// Get a deserializable value from the given table (bincode).
    pub fn get_bincode<V: DeserializeOwned>(&self, table: &str, key: &[u8]) -> StorageResult<Option<V>> {
        match self.get(table, key)? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => Ok(None),
        }
    }

    /// Put a JSON-serializable value (for backward compat with sled's JSON storage).
    pub fn put_json<V: Serialize>(&self, table: &str, key: &[u8], value: &V) -> StorageResult<()> {
        let encoded = serde_json::to_vec(value)?;
        self.put(table, key, &encoded)
    }

    /// Get a JSON-deserializable value.
    pub fn get_json<V: DeserializeOwned>(&self, table: &str, key: &[u8]) -> StorageResult<Option<V>> {
        match self.get(table, key)? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    // ── Batch write transaction ─────────────────────────────

    /// Begin a batch write. All operations are committed atomically.
    pub fn begin_write(&self) -> StorageResult<WriteBatch<'_>> {
        let tx = self.db.begin_rw_txn()?;
        Ok(WriteBatch { tx })
    }

    // ── Iteration ───────────────────────────────────────────

    /// Iterate all key-value pairs in a table (ordered by key).
    pub fn iter(&self, table: &str) -> StorageResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let tx = self.db.begin_ro_txn()?;
        let tbl = tx.open_table(Some(table))?;
        let cursor = tx.cursor(&tbl)?;
        let mut results = Vec::new();
        for item in cursor {
            let (key, value) = item?;
            results.push((key.to_vec(), value.to_vec()));
        }
        Ok(results)
    }

    /// Count entries in a table.
    pub fn count(&self, table: &str) -> StorageResult<usize> {
        let tx = self.db.begin_ro_txn()?;
        let tbl = tx.open_table(Some(table))?;
        let stat = tx.table_stat(&tbl)?;
        Ok(stat.entries())
    }

    // ── Utility ─────────────────────────────────────────────

    /// Clear all data in a specific table.
    pub fn clear_table(&self, table: &str) -> StorageResult<()> {
        let tx = self.db.begin_rw_txn()?;
        let tbl = tx.open_table(Some(table))?;
        tx.clear_table(&tbl)?;
        tx.commit()?;
        Ok(())
    }

    /// Force sync to disk.
    pub fn sync(&self) -> StorageResult<()> {
        self.db.sync(true)?;
        Ok(())
    }
}

/// Batch write transaction — all operations commit or rollback atomically.
pub struct WriteBatch<'env> {
    tx: Transaction<'env, RW, NoWriteMap>,
}

impl WriteBatch<'_> {
    /// Put a raw key-value pair.
    pub fn put(&self, table: &str, key: &[u8], value: &[u8]) -> StorageResult<()> {
        let tbl = self.tx.open_table(Some(table))?;
        self.tx.put(&tbl, key, value, WriteFlags::default())?;
        Ok(())
    }

    /// Put a bincode-encoded value.
    pub fn put_bincode<V: Serialize>(&self, table: &str, key: &[u8], value: &V) -> StorageResult<()> {
        let encoded = bincode::serialize(value)?;
        self.put(table, key, &encoded)
    }

    /// Put a JSON-encoded value.
    pub fn put_json<V: Serialize>(&self, table: &str, key: &[u8], value: &V) -> StorageResult<()> {
        let encoded = serde_json::to_vec(value)?;
        self.put(table, key, &encoded)
    }

    /// Delete a key.
    pub fn delete(&self, table: &str, key: &[u8]) -> StorageResult<()> {
        let tbl = self.tx.open_table(Some(table))?;
        let _ = self.tx.del(&tbl, key, None); // ignore NotFound
        Ok(())
    }

    /// Commit all batched operations atomically.
    pub fn commit(self) -> StorageResult<()> {
        self.tx.commit()?;
        Ok(())
    }
}

// ── Height key helper ───────────────────────────────────────

/// Convert a block height to big-endian bytes for ordered MDBX storage.
pub fn height_key(height: u64) -> [u8; 8] {
    height.to_be_bytes()
}

/// Decode a height from big-endian key bytes.
pub fn key_to_height(key: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&key[..8]);
    u64::from_be_bytes(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::*;
    use tempfile::TempDir;

    fn temp_storage() -> (TempDir, MdbxStorage) {
        let dir = TempDir::new().unwrap();
        let storage = MdbxStorage::open(dir.path()).unwrap();
        (dir, storage)
    }

    #[test]
    fn test_open_creates_tables() {
        let (_dir, storage) = temp_storage();
        for table in ALL_TABLES {
            let count = storage.count(table).unwrap();
            assert_eq!(count, 0, "table {} should be empty", table);
        }
    }

    #[test]
    fn test_put_get_raw() {
        let (_dir, storage) = temp_storage();
        storage.put(TABLE_META, b"test_key", b"test_value").unwrap();
        let val = storage.get(TABLE_META, b"test_key").unwrap();
        assert_eq!(val, Some(b"test_value".to_vec()));
    }

    #[test]
    fn test_get_missing_key() {
        let (_dir, storage) = temp_storage();
        let val = storage.get(TABLE_META, b"nonexistent").unwrap();
        assert!(val.is_none());
    }

    #[test]
    fn test_delete() {
        let (_dir, storage) = temp_storage();
        storage.put(TABLE_META, b"key", b"value").unwrap();
        assert!(storage.has(TABLE_META, b"key").unwrap());
        storage.delete(TABLE_META, b"key").unwrap();
        assert!(!storage.has(TABLE_META, b"key").unwrap());
    }

    #[test]
    fn test_put_get_bincode() {
        let (_dir, storage) = temp_storage();
        let value: u64 = 42;
        storage.put_bincode(TABLE_META, b"height", &value).unwrap();
        let loaded: Option<u64> = storage.get_bincode(TABLE_META, b"height").unwrap();
        assert_eq!(loaded, Some(42));
    }

    #[test]
    fn test_put_get_json() {
        let (_dir, storage) = temp_storage();
        let value = vec!["hello", "world"];
        storage.put_json(TABLE_META, b"list", &value).unwrap();
        let loaded: Option<Vec<String>> = storage.get_json(TABLE_META, b"list").unwrap();
        assert_eq!(loaded, Some(vec!["hello".to_string(), "world".to_string()]));
    }

    #[test]
    fn test_batch_write() {
        let (_dir, storage) = temp_storage();
        let batch = storage.begin_write().unwrap();
        batch.put(TABLE_META, b"a", b"1").unwrap();
        batch.put(TABLE_META, b"b", b"2").unwrap();
        batch.put(TABLE_META, b"c", b"3").unwrap();
        batch.commit().unwrap();

        assert_eq!(storage.count(TABLE_META).unwrap(), 3);
        assert_eq!(storage.get(TABLE_META, b"b").unwrap(), Some(b"2".to_vec()));
    }

    #[test]
    fn test_count() {
        let (_dir, storage) = temp_storage();
        assert_eq!(storage.count(TABLE_META).unwrap(), 0);
        storage.put(TABLE_META, b"k1", b"v1").unwrap();
        storage.put(TABLE_META, b"k2", b"v2").unwrap();
        assert_eq!(storage.count(TABLE_META).unwrap(), 2);
    }

    #[test]
    fn test_clear_table() {
        let (_dir, storage) = temp_storage();
        storage.put(TABLE_META, b"k1", b"v1").unwrap();
        storage.put(TABLE_META, b"k2", b"v2").unwrap();
        assert_eq!(storage.count(TABLE_META).unwrap(), 2);
        storage.clear_table(TABLE_META).unwrap();
        assert_eq!(storage.count(TABLE_META).unwrap(), 0);
    }

    #[test]
    fn test_height_key_ordering() {
        let (_dir, storage) = temp_storage();
        for h in [0u64, 1, 100, 1000, 999999] {
            storage
                .put(TABLE_BLOCKS, &height_key(h), &h.to_le_bytes())
                .unwrap();
        }
        let entries = storage.iter(TABLE_BLOCKS).unwrap();
        let heights: Vec<u64> = entries.iter().map(|(k, _)| key_to_height(k)).collect();
        assert_eq!(heights, vec![0, 1, 100, 1000, 999999]);
    }

    #[test]
    fn test_persistence_across_reopen() {
        let dir = TempDir::new().unwrap();
        {
            let storage = MdbxStorage::open(dir.path()).unwrap();
            storage.put(TABLE_META, b"persist", b"yes").unwrap();
            storage.sync().unwrap();
        }
        {
            let storage = MdbxStorage::open(dir.path()).unwrap();
            let val = storage.get(TABLE_META, b"persist").unwrap();
            assert_eq!(val, Some(b"yes".to_vec()));
        }
    }

    #[test]
    fn test_block_roundtrip() {
        use sentrix_primitives::Block;

        let (_dir, storage) = temp_storage();
        let block = Block::genesis();
        let key = height_key(block.index);
        storage.put_bincode(TABLE_BLOCKS, &key, &block).unwrap();

        let loaded: Block = storage.get_bincode(TABLE_BLOCKS, &key).unwrap().unwrap();
        assert_eq!(loaded.index, 0);
        assert_eq!(loaded.hash, block.hash);
    }
}
