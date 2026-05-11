// mdbx.rs — libmdbx wrapper for Sentrix chain storage.
//
// API: Database<NoWriteMap> → Transaction<RO/RW> → Table → Cursor.

use crate::error::{StorageError, StorageResult};
use crate::tables::ALL_TABLES;
use libmdbx::{
    Database, DatabaseOptions, Mode, NoWriteMap, RW, ReadWriteOptions, TableFlags, Transaction,
    WriteFlags,
};
use serde::{Serialize, de::DeserializeOwned};
use std::path::Path;

/// MDBX geometry — explicit upper bound + growth step. Default libmdbx
/// geometry has a small upper_size and grows in tiny increments, which
/// caused 5 mainnet halts on 2026-05-01 (h≈1.18M / 1.19M / 1.197M):
/// once the file's geometric ceiling is hit, every trie write fails with
/// `MDBX_MAP_FULL`. The validators that hit it can't persist new state,
/// in-memory blockchain advances anyway, they propose blocks with stale
/// state, peers reject → 2v2 split-brain → BFT halt. Independent write
/// histories across the fleet (validator-pair A vs validator-pair B)
/// made the failure deterministically factional even on byte-identical
/// post-rsync chain.db.
///
/// 64 GB upper bound covers ~20× current chain.db size at expected growth
/// rate (3 GB / 1.2M blocks ≈ 2.5 µB / block × 5y projection at 1 block/s
/// = ~400 GB worst case; conservative 64 GB ceiling for now, lift later
/// if validators approach it). 256 MB growth step minimises fragmentation
/// vs the libmdbx default tiny step.
const MAX_DB_SIZE: isize = 64 * 1024 * 1024 * 1024;
const GROWTH_STEP: isize = 256 * 1024 * 1024;

/// Sentrix storage backed by libmdbx.
///
/// Thread-safe: libmdbx `Database` can be shared across threads.
/// Read transactions are lock-free. Write transactions are serialized.
pub struct MdbxStorage {
    db: Database<NoWriteMap>,
}

impl std::fmt::Debug for MdbxStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MdbxStorage").finish_non_exhaustive()
    }
}

impl MdbxStorage {
    /// Open (or create) the MDBX database at the given path.
    /// Pre-creates all Sentrix tables on first open.
    pub fn open(path: &Path) -> StorageResult<Self> {
        std::fs::create_dir_all(path).map_err(|e| StorageError::Other(e.to_string()))?;

        let db = Database::<NoWriteMap>::open_with_options(
            path,
            DatabaseOptions {
                max_tables: Some(16),
                mode: Mode::ReadWrite(ReadWriteOptions {
                    max_size: Some(MAX_DB_SIZE),
                    growth_step: Some(GROWTH_STEP),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .map_err(|e| StorageError::Mdbx(format!("open: {e}")))?;

        // Pre-create all named tables
        {
            let tx = db.begin_rw_txn()?;
            for &table_name in ALL_TABLES {
                tx.create_table(Some(table_name), TableFlags::default())?;
            }
            tx.commit()?;
        }

        tracing::info!(
            "MDBX storage opened at {:?} ({} tables)",
            path,
            ALL_TABLES.len()
        );
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
    pub fn put_bincode<V: Serialize>(
        &self,
        table: &str,
        key: &[u8],
        value: &V,
    ) -> StorageResult<()> {
        let encoded = bincode::serialize(value)?;
        self.put(table, key, &encoded)
    }

    /// Get a deserializable value from the given table (bincode).
    pub fn get_bincode<V: DeserializeOwned>(
        &self,
        table: &str,
        key: &[u8],
    ) -> StorageResult<Option<V>> {
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
    pub fn get_json<V: DeserializeOwned>(
        &self,
        table: &str,
        key: &[u8],
    ) -> StorageResult<Option<V>> {
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
    ///
    /// Materialises the entire table into a `Vec`. Fine for small admin
    /// tables (table list, validator set, etc.). Do NOT use for
    /// `TABLE_LOGS` / `TABLE_RECEIPTS` / any unbounded-growth table —
    /// see `iter_range` / `iter_from` for cursor-based scans that don't
    /// allocate the world.
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

    /// Cursor-based range scan with a single sequential walk and no
    /// intermediate `Vec` allocation. Calls `f(key, value)` for every
    /// entry whose key starts with `prefix`, in key-sorted order.
    /// Callback returns `false` to break early.
    ///
    /// Replaces the `iter(...).into_iter().filter(...)` pattern that
    /// materialised every entry in the table (and was the root of the
    /// per-block O(total_logs) scan in the bloom builder, `eth_getLogs`,
    /// and `eth_getTransactionReceipt` paths). MDBX `set_range` seeks
    /// directly to the first key >= `prefix`; the walk stops the first
    /// time a key no longer starts with `prefix` (MDBX keys are
    /// lexicographically sorted, so all matches are contiguous).
    pub fn iter_range<F>(&self, table: &str, prefix: &[u8], mut f: F) -> StorageResult<()>
    where
        F: FnMut(&[u8], &[u8]) -> bool,
    {
        let tx = self.db.begin_ro_txn()?;
        let tbl = tx.open_table(Some(table))?;
        let mut cursor = tx.cursor(&tbl)?;
        let first: Option<(Vec<u8>, Vec<u8>)> = cursor.set_range(prefix)?;
        let Some((k, v)) = first else {
            return Ok(());
        };
        if !k.starts_with(prefix) {
            return Ok(());
        }
        if !f(&k, &v) {
            return Ok(());
        }
        while let Some((k, v)) = cursor.next::<Vec<u8>, Vec<u8>>()? {
            if !k.starts_with(prefix) {
                break;
            }
            if !f(&k, &v) {
                break;
            }
        }
        Ok(())
    }

    /// Cursor walk starting at `start_key` (or first key >= `start_key`),
    /// no prefix constraint. Callback returns `false` to break.
    ///
    /// Use this when the stop condition is value-derived rather than a
    /// fixed key prefix — e.g. range-walking by block height where the
    /// caller decodes the height out of the key and stops at an
    /// upper bound.
    pub fn iter_from<F>(&self, table: &str, start_key: &[u8], mut f: F) -> StorageResult<()>
    where
        F: FnMut(&[u8], &[u8]) -> bool,
    {
        let tx = self.db.begin_ro_txn()?;
        let tbl = tx.open_table(Some(table))?;
        let mut cursor = tx.cursor(&tbl)?;
        let first: Option<(Vec<u8>, Vec<u8>)> = cursor.set_range(start_key)?;
        let Some((k, v)) = first else {
            return Ok(());
        };
        if !f(&k, &v) {
            return Ok(());
        }
        while let Some((k, v)) = cursor.next::<Vec<u8>, Vec<u8>>()? {
            if !f(&k, &v) {
                break;
            }
        }
        Ok(())
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

    /// Get approximate database size on disk.
    pub fn db_size_bytes(&self) -> StorageResult<u64> {
        let info = self.db.info()?;
        Ok(info.map_size() as u64)
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
    pub fn put_bincode<V: Serialize>(
        &self,
        table: &str,
        key: &[u8],
        value: &V,
    ) -> StorageResult<()> {
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

    // Regression tests for the audit D-G3 fix (per-block bloom build +
    // eth_getLogs + eth_getTransactionReceipt previously did
    // `storage.iter(TABLE_LOGS)` and pulled every log in the chain into
    // RAM; replaced by `iter_range` / `iter_from` cursor walks).

    #[test]
    fn test_iter_range_walks_prefix_only() {
        let (_dir, storage) = temp_storage();
        // Two heights × two logs each. Keys: 8-byte BE height || 8 bytes
        // of per-tx noise so each entry is unique.
        for height in [1u64, 2, 3] {
            for idx in 0u8..3 {
                let mut k = Vec::with_capacity(16);
                k.extend_from_slice(&height.to_be_bytes());
                k.extend_from_slice(&[idx; 8]);
                storage
                    .put(TABLE_META, &k, &[height as u8, idx])
                    .unwrap();
            }
        }
        let prefix = 2u64.to_be_bytes();
        let mut seen = Vec::new();
        storage
            .iter_range(TABLE_META, &prefix, |k, v| {
                seen.push((k.to_vec(), v.to_vec()));
                true
            })
            .unwrap();
        assert_eq!(seen.len(), 3, "should only see the 3 entries at height=2");
        for (k, _) in &seen {
            assert_eq!(&k[..8], &prefix, "every yielded key must start with prefix");
        }
    }

    #[test]
    fn test_iter_range_no_match_is_no_op() {
        let (_dir, storage) = temp_storage();
        // Insert a few keys at height=1; query height=9 → no walk.
        for idx in 0u8..3 {
            let mut k = Vec::with_capacity(16);
            k.extend_from_slice(&1u64.to_be_bytes());
            k.extend_from_slice(&[idx; 8]);
            storage.put(TABLE_META, &k, &[idx]).unwrap();
        }
        let mut seen = 0;
        storage
            .iter_range(TABLE_META, &9u64.to_be_bytes(), |_k, _v| {
                seen += 1;
                true
            })
            .unwrap();
        assert_eq!(seen, 0);
    }

    #[test]
    fn test_iter_range_early_break() {
        let (_dir, storage) = temp_storage();
        for idx in 0u8..5 {
            let mut k = Vec::with_capacity(16);
            k.extend_from_slice(&5u64.to_be_bytes());
            k.extend_from_slice(&[idx; 8]);
            storage.put(TABLE_META, &k, &[idx]).unwrap();
        }
        let mut count = 0;
        storage
            .iter_range(TABLE_META, &5u64.to_be_bytes(), |_k, _v| {
                count += 1;
                count < 3 // stop after the 3rd entry
            })
            .unwrap();
        assert_eq!(count, 3);
    }

    #[test]
    fn test_iter_from_walks_upward_with_stop_condition() {
        let (_dir, storage) = temp_storage();
        // Heights 1..=10, one entry each. Walk from height=3,
        // stop when height > 7. Expect 5 entries (3, 4, 5, 6, 7).
        for height in 1u64..=10 {
            storage
                .put(TABLE_META, &height.to_be_bytes(), &[height as u8])
                .unwrap();
        }
        let mut heights = Vec::new();
        storage
            .iter_from(TABLE_META, &3u64.to_be_bytes(), |k, _v| {
                let mut h_bytes = [0u8; 8];
                h_bytes.copy_from_slice(&k[..8]);
                let h = u64::from_be_bytes(h_bytes);
                if h > 7 {
                    return false; // stop
                }
                heights.push(h);
                true
            })
            .unwrap();
        assert_eq!(heights, vec![3, 4, 5, 6, 7]);
    }
}
