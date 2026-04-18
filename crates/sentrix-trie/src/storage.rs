// trie/storage.rs - Sentrix — Persistent MDBX-backed trie storage

use crate::node::{NodeHash, TrieNode};
use sentrix_primitives::{SentrixError, SentrixResult};
use sentrix_storage::{MdbxStorage, tables};
use std::sync::Arc;

/// Low-level persistent storage for trie nodes, values, and version→root mappings.
///
/// Four MDBX tables (same logical layout as the old sled trees):
/// - `trie_nodes`           : NodeHash → bincode(TrieNode)
/// - `trie_values`          : NodeHash → raw account-state bytes
/// - `trie_roots`           : version u64 BE → NodeHash
/// - `trie_committed_roots` : NodeHash → version u64 BE (reverse index for O(1) is_committed_root)
///
/// `Clone` is cheap — `Arc<MdbxStorage>` is reference-counted.
#[derive(Clone)]
pub struct TrieStorage {
    mdbx: Arc<MdbxStorage>,
}

impl TrieStorage {
    /// Open trie storage backed by the given MdbxStorage.
    /// On first open (migration), backfills `trie_committed_roots` from `trie_roots`.
    pub fn new(mdbx: Arc<MdbxStorage>) -> SentrixResult<Self> {
        let storage = Self { mdbx };
        storage.ensure_committed_roots_index()?;
        Ok(storage)
    }

    /// Backfill `trie_committed_roots` from `trie_roots` if the reverse index is absent.
    /// O(n_blocks) one-time cost on migration; O(1) fast-path on all subsequent opens.
    fn ensure_committed_roots_index(&self) -> SentrixResult<()> {
        // Fast path: sentinel present means the index is already complete.
        if self.mdbx.has(tables::TABLE_TRIE_COMMITTED, b"__ready__")
            .map_err(|e| SentrixError::StorageError(e.to_string()))? {
            return Ok(());
        }

        // Slow path: scan trie_roots and populate the reverse index.
        let entries = self.mdbx.iter(tables::TABLE_TRIE_ROOTS)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        let mut any = false;
        for (k, v) in &entries {
            if v.len() == 32 {
                self.mdbx.put(tables::TABLE_TRIE_COMMITTED, v, k)
                    .map_err(|e| SentrixError::StorageError(e.to_string()))?;
                any = true;
            }
        }

        if any {
            self.mdbx.put(tables::TABLE_TRIE_COMMITTED, b"__ready__", b"1")
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    // ── Nodes ─────────────────────────────────────────────

    pub fn store_node(&self, hash: &NodeHash, node: &TrieNode) -> SentrixResult<()> {
        let bytes = bincode::serialize(node)
            .map_err(|e| SentrixError::SerializationError(e.to_string()))?;
        self.mdbx.put(tables::TABLE_TRIE_NODES, hash, &bytes)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    pub fn load_node(&self, hash: &NodeHash) -> SentrixResult<Option<TrieNode>> {
        match self.mdbx.get(tables::TABLE_TRIE_NODES, hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))? {
            Some(bytes) => {
                let node = bincode::deserialize::<TrieNode>(&bytes)
                    .map_err(|e| SentrixError::SerializationError(e.to_string()))?;
                Ok(Some(node))
            }
            None => Ok(None),
        }
    }

    /// Remove a node entry from persistent storage (called when a leaf is replaced).
    pub fn delete_node(&self, hash: &NodeHash) -> SentrixResult<()> {
        self.mdbx.delete(tables::TABLE_TRIE_NODES, hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    // ── Values ────────────────────────────────────────────

    pub fn store_value(&self, hash: &NodeHash, value: &[u8]) -> SentrixResult<()> {
        self.mdbx.put(tables::TABLE_TRIE_VALUES, hash, value)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    pub fn load_value(&self, hash: &NodeHash) -> SentrixResult<Option<Vec<u8>>> {
        self.mdbx.get(tables::TABLE_TRIE_VALUES, hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    /// Remove a value blob from persistent storage (called when a leaf is replaced).
    pub fn delete_value(&self, hash: &NodeHash) -> SentrixResult<()> {
        self.mdbx.delete(tables::TABLE_TRIE_VALUES, hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    // ── Roots ─────────────────────────────────────────────

    pub fn store_root(&self, version: u64, root: &NodeHash) -> SentrixResult<()> {
        self.mdbx.put(tables::TABLE_TRIE_ROOTS, &version.to_be_bytes(), root.as_slice())
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        // Maintain reverse index: NodeHash → version (O(1) is_committed_root lookups).
        self.mdbx.put(tables::TABLE_TRIE_COMMITTED, root.as_slice(), &version.to_be_bytes())
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        if !self.mdbx.has(tables::TABLE_TRIE_COMMITTED, b"__ready__")
            .unwrap_or(false) {
            let _ = self.mdbx.put(tables::TABLE_TRIE_COMMITTED, b"__ready__", b"1");
        }
        Ok(())
    }

    pub fn load_root(&self, version: u64) -> SentrixResult<Option<NodeHash>> {
        match self.mdbx.get(tables::TABLE_TRIE_ROOTS, &version.to_be_bytes())
            .map_err(|e| SentrixError::StorageError(e.to_string()))? {
            Some(bytes) if bytes.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Ok(Some(arr))
            }
            Some(_) => Err(SentrixError::StorageError(
                "corrupt trie root: wrong byte length".to_string(),
            )),
            None => Ok(None),
        }
    }

    /// Check whether `hash` is currently recorded as a committed root for any version.
    /// O(1) via `trie_committed_roots` reverse index.
    pub fn is_committed_root(&self, hash: &NodeHash) -> SentrixResult<bool> {
        self.mdbx.has(tables::TABLE_TRIE_COMMITTED, hash.as_slice())
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    /// Prune old trie roots, keeping only the last `keep` versions.
    pub fn prune_old_roots(&self, latest_version: u64, keep: u64) -> SentrixResult<usize> {
        if latest_version <= keep {
            return Ok(0);
        }
        let cutoff = latest_version - keep;
        let mut removed = 0usize;

        let entries = self.mdbx.iter(tables::TABLE_TRIE_ROOTS)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        let mut to_delete: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for (k, v) in &entries {
            if k.len() == 8 {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(k);
                let version = u64::from_be_bytes(buf);
                if version <= cutoff {
                    to_delete.push((k.clone(), v.clone()));
                }
            }
        }

        for (key, root_hash) in &to_delete {
            self.mdbx.delete(tables::TABLE_TRIE_ROOTS, key.as_slice())
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
            if root_hash.len() == 32 {
                self.mdbx.delete(tables::TABLE_TRIE_COMMITTED, root_hash.as_slice())
                    .map_err(|e| SentrixError::StorageError(e.to_string()))?;
            }
            removed += 1;
        }

        Ok(removed)
    }

    /// Garbage-collect node and value entries not present in `live_hashes`.
    pub fn gc_orphaned_nodes(
        &self,
        live_hashes: &std::collections::HashSet<NodeHash>,
    ) -> SentrixResult<usize> {
        let node_count = self.gc_table(tables::TABLE_TRIE_NODES, live_hashes)?;
        let value_count = self.gc_table(tables::TABLE_TRIE_VALUES, live_hashes)?;
        Ok(node_count + value_count)
    }

    /// Shared helper: scan an MDBX table for hashes not in `live_hashes` and remove them.
    fn gc_table(
        &self,
        table: &str,
        live_hashes: &std::collections::HashSet<NodeHash>,
    ) -> SentrixResult<usize> {
        let entries = self.mdbx.iter(table)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        let mut to_delete: Vec<NodeHash> = Vec::new();
        for (k, _) in &entries {
            if k.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(k);
                if !live_hashes.contains(&arr) {
                    to_delete.push(arr);
                }
            }
        }
        let count = to_delete.len();
        for hash in &to_delete {
            self.mdbx.delete(table, hash)
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        }
        Ok(count)
    }

    /// Count entries in a trie table. Used by tests.
    pub fn count(&self, table: &str) -> SentrixResult<usize> {
        self.mdbx.count(table)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{TrieNode, empty_hash};
    use std::collections::HashSet;

    fn temp_storage() -> (tempfile::TempDir, TrieStorage) {
        let dir = tempfile::TempDir::new().unwrap();
        let mdbx = Arc::new(MdbxStorage::open(dir.path()).unwrap());
        let storage = TrieStorage::new(mdbx).unwrap();
        (dir, storage)
    }

    fn dummy_hash(byte: u8) -> NodeHash {
        let mut h = [0u8; 32];
        h[0] = byte;
        h
    }

    #[test]
    fn test_is_committed_root_true_for_stored() {
        let (_dir, storage) = temp_storage();
        let root = dummy_hash(0x10);
        storage.store_root(1, &root).unwrap();
        assert!(
            storage.is_committed_root(&root).unwrap(),
            "is_committed_root must return true for a stored root"
        );
    }

    #[test]
    fn test_is_committed_root_false_for_unknown() {
        let (_dir, storage) = temp_storage();
        let committed = dummy_hash(0x10);
        let other = dummy_hash(0x20);
        storage.store_root(1, &committed).unwrap();
        assert!(
            !storage.is_committed_root(&other).unwrap(),
            "is_committed_root must return false for a hash not in trie_roots"
        );
    }

    #[test]
    fn test_store_root_no_blocking_flush() {
        let (_dir, storage) = temp_storage();
        let root = dummy_hash(0xFF);
        for v in 0u64..50 {
            storage.store_root(v, &root).unwrap();
        }
        assert_eq!(storage.load_root(0).unwrap(), Some(root));
        assert_eq!(storage.load_root(49).unwrap(), Some(root));
    }

    #[test]
    fn test_delete_node_removes_entry() {
        let (_dir, storage) = temp_storage();
        let hash = dummy_hash(0xAB);
        let node = TrieNode::Leaf {
            key: [1u8; 32],
            value_hash: [2u8; 32],
        };

        storage.store_node(&hash, &node).unwrap();
        assert!(
            storage.load_node(&hash).unwrap().is_some(),
            "node must exist after store"
        );

        storage.delete_node(&hash).unwrap();
        assert!(
            storage.load_node(&hash).unwrap().is_none(),
            "node must be absent after delete"
        );
    }

    #[test]
    fn test_delete_value_removes_entry() {
        let (_dir, storage) = temp_storage();
        let hash = dummy_hash(0xCD);
        let val = b"balance_data";

        storage.store_value(&hash, val).unwrap();
        assert!(
            storage.load_value(&hash).unwrap().is_some(),
            "value must exist after store"
        );

        storage.delete_value(&hash).unwrap();
        assert!(
            storage.load_value(&hash).unwrap().is_none(),
            "value must be absent after delete"
        );
    }

    #[test]
    fn test_gc_orphaned_nodes_removes_unlisted() {
        let (_dir, storage) = temp_storage();
        let live_hash = dummy_hash(0x01);
        let orphan_hash = dummy_hash(0x02);

        let node = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: empty_hash(0),
        };
        storage.store_node(&live_hash, &node).unwrap();
        storage.store_node(&orphan_hash, &node).unwrap();

        let mut live: HashSet<NodeHash> = HashSet::new();
        live.insert(live_hash);

        let removed = storage.gc_orphaned_nodes(&live).unwrap();
        assert_eq!(removed, 1, "exactly one orphan must be removed");
        assert!(
            storage.load_node(&live_hash).unwrap().is_some(),
            "live node must survive GC"
        );
        assert!(
            storage.load_node(&orphan_hash).unwrap().is_none(),
            "orphan must be removed by GC"
        );
    }

    #[test]
    fn test_gc_empty_live_set_removes_all() {
        let (_dir, storage) = temp_storage();
        let node = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: empty_hash(0),
        };
        for i in 0u8..5 {
            storage.store_node(&dummy_hash(i), &node).unwrap();
        }
        let removed = storage.gc_orphaned_nodes(&HashSet::new()).unwrap();
        assert_eq!(
            removed, 5,
            "all 5 nodes must be removed when live set is empty"
        );
    }

    #[test]
    fn test_gc_also_removes_orphan_values() {
        let (_dir, storage) = temp_storage();
        let live_hash = dummy_hash(0x01);
        let orphan_hash = dummy_hash(0x02);

        let node = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: empty_hash(0),
        };
        storage.store_node(&live_hash, &node).unwrap();
        storage.store_node(&orphan_hash, &node).unwrap();
        storage.store_value(&live_hash, b"live_data").unwrap();
        storage.store_value(&orphan_hash, b"orphan_data").unwrap();

        let mut live: std::collections::HashSet<NodeHash> = std::collections::HashSet::new();
        live.insert(live_hash);

        let removed = storage.gc_orphaned_nodes(&live).unwrap();
        assert_eq!(
            removed, 2,
            "GC must remove both orphan node and orphan value"
        );
        assert!(
            storage.load_value(&live_hash).unwrap().is_some(),
            "live value must survive GC"
        );
        assert!(
            storage.load_value(&orphan_hash).unwrap().is_none(),
            "orphan value must be removed"
        );
    }

    #[test]
    fn test_v10_c02_committed_root_reverse_index_populated_by_store_root() {
        let (_dir, storage) = temp_storage();
        let root = dummy_hash(0x42);
        storage.store_root(7, &root).unwrap();
        assert!(
            storage.mdbx.has(tables::TABLE_TRIE_COMMITTED, root.as_slice()).unwrap(),
            "trie_committed_roots must contain the hash after store_root()"
        );
    }

    #[test]
    fn test_v10_c02_is_committed_root_o1_lookup() {
        let (_dir, storage) = temp_storage();
        let r1 = dummy_hash(0x11);
        let r2 = dummy_hash(0x22);
        let r3 = dummy_hash(0x33);
        storage.store_root(1, &r1).unwrap();
        storage.store_root(2, &r2).unwrap();
        assert!(storage.is_committed_root(&r1).unwrap(), "r1 must be found");
        assert!(storage.is_committed_root(&r2).unwrap(), "r2 must be found");
        assert!(
            !storage.is_committed_root(&r3).unwrap(),
            "r3 was never stored"
        );
    }

    #[test]
    fn test_v10_c02_migration_backfills_existing_roots() {
        // Simulate pre-migration DB: write directly to trie_roots, bypassing store_root().
        let dir = tempfile::TempDir::new().unwrap();
        let mdbx = Arc::new(MdbxStorage::open(dir.path()).unwrap());

        let root = dummy_hash(0xAA);
        mdbx.put(tables::TABLE_TRIE_ROOTS, &1u64.to_be_bytes(), &root[..]).unwrap();

        // Re-open via TrieStorage::new() — triggers ensure_committed_roots_index()
        let storage = TrieStorage::new(mdbx).unwrap();

        assert!(
            storage.is_committed_root(&root).unwrap(),
            "ensure_committed_roots_index() must backfill pre-migration roots"
        );
    }

    #[test]
    fn test_prune_old_roots_removes_stale() {
        let (_dir, storage) = temp_storage();
        for v in 1u64..=10 {
            storage.store_root(v, &dummy_hash(v as u8)).unwrap();
        }
        let removed = storage.prune_old_roots(10, 3).unwrap();
        assert_eq!(removed, 7, "should remove versions 1-7");
        assert!(
            storage.load_root(8).unwrap().is_some(),
            "version 8 must survive"
        );
        assert!(
            storage.load_root(10).unwrap().is_some(),
            "version 10 must survive"
        );
        assert!(
            storage.load_root(1).unwrap().is_none(),
            "version 1 must be pruned"
        );
        assert!(
            storage.load_root(7).unwrap().is_none(),
            "version 7 must be pruned"
        );
    }

    #[test]
    fn test_prune_old_roots_noop_when_few_versions() {
        let (_dir, storage) = temp_storage();
        for v in 1u64..=5 {
            storage.store_root(v, &dummy_hash(v as u8)).unwrap();
        }
        let removed = storage.prune_old_roots(5, 10).unwrap();
        assert_eq!(removed, 0, "should not prune when versions < keep");
    }

    #[test]
    fn test_prune_removes_reverse_index() {
        let (_dir, storage) = temp_storage();
        let root = dummy_hash(0x42);
        storage.store_root(1, &root).unwrap();
        storage.store_root(10, &dummy_hash(0xFF)).unwrap();
        assert!(storage.is_committed_root(&root).unwrap());

        storage.prune_old_roots(10, 1).unwrap();
        assert!(
            !storage.is_committed_root(&root).unwrap(),
            "pruned root must be removed from reverse index"
        );
    }
}
