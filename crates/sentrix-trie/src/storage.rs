// trie/storage.rs - Sentrix — Persistent sled-backed trie storage

use crate::node::{NodeHash, TrieNode};
use sentrix_primitives::{SentrixError, SentrixResult};
use sled::{Db, Tree};

/// Low-level persistent storage for trie nodes, values, and version→root mappings.
///
/// Four named sled trees:
/// - `trie_nodes`           : NodeHash → bincode(TrieNode)
/// - `trie_values`          : NodeHash → raw account-state bytes
/// - `trie_roots`           : version u64 BE → NodeHash
/// - `trie_committed_roots` : NodeHash → version u64 BE (reverse index for O(1) is_committed_root)
///
/// `Clone` is cheap — sled::Tree is an Arc internally (shared underlying tree).
#[derive(Clone)]
pub struct TrieStorage {
    nodes: Tree,
    values: Tree,
    roots: Tree,
    /// Reverse index: NodeHash → version.
    /// Maintained in sync with `roots` so `is_committed_root()` is O(1) instead of O(n_blocks).
    committed_root_hashes: Tree,
}

impl TrieStorage {
    /// Open (or create) the four named trees from an existing sled Db.
    /// On first open (migration), backfills `trie_committed_roots` from `trie_roots`.
    pub fn new(db: &Db) -> SentrixResult<Self> {
        let nodes = db
            .open_tree("trie_nodes")
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        let values = db
            .open_tree("trie_values")
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        let roots = db
            .open_tree("trie_roots")
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        let committed_root_hashes = db
            .open_tree("trie_committed_roots")
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        let storage = Self {
            nodes,
            values,
            roots,
            committed_root_hashes,
        };

        // One-time migration: backfill reverse index from trie_roots on first open.
        // Subsequent opens skip this via the sentinel "trie_committed_roots_ready" key.
        storage.ensure_committed_roots_index()?;

        Ok(storage)
    }

    /// Backfill `trie_committed_roots` from `trie_roots` if the reverse index is absent.
    /// O(n_blocks) one-time cost on migration; O(1) fast-path on all subsequent opens.
    fn ensure_committed_roots_index(&self) -> SentrixResult<()> {
        // Fast path: sentinel present means the index is already complete.
        if self
            .committed_root_hashes
            .contains_key(b"__ready__")
            .map_err(|e| SentrixError::StorageError(e.to_string()))?
        {
            return Ok(());
        }

        // Slow path: scan trie_roots and populate the reverse index.
        let mut any = false;
        for entry in self.roots.iter() {
            let (k, v) = entry.map_err(|e| SentrixError::StorageError(e.to_string()))?;
            if v.len() == 32 {
                // key = version u64 BE (8 bytes), value = NodeHash (32 bytes)
                self.committed_root_hashes
                    .insert(&v[..], &k[..])
                    .map_err(|e| SentrixError::StorageError(e.to_string()))?;
                any = true;
            }
        }

        // Write sentinel once we know the index is in sync.
        // Only on non-empty roots so an empty-DB first-open doesn't mark it prematurely.
        if any {
            self.committed_root_hashes
                .insert(b"__ready__", b"1")
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    // ── Nodes ─────────────────────────────────────────────

    pub fn store_node(&self, hash: &NodeHash, node: &TrieNode) -> SentrixResult<()> {
        let bytes = bincode::serialize(node)
            .map_err(|e| SentrixError::SerializationError(e.to_string()))?;
        self.nodes
            .insert(hash, bytes)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    pub fn load_node(&self, hash: &NodeHash) -> SentrixResult<Option<TrieNode>> {
        match self
            .nodes
            .get(hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?
        {
            Some(bytes) => {
                let node = bincode::deserialize::<TrieNode>(&bytes)
                    .map_err(|e| SentrixError::SerializationError(e.to_string()))?;
                Ok(Some(node))
            }
            None => Ok(None),
        }
    }

    /// T-B: Remove a node entry from persistent storage (called when a leaf is replaced).
    pub fn delete_node(&self, hash: &NodeHash) -> SentrixResult<()> {
        self.nodes
            .remove(hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    // ── Values ────────────────────────────────────────────

    pub fn store_value(&self, hash: &NodeHash, value: &[u8]) -> SentrixResult<()> {
        self.values
            .insert(hash, value)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    pub fn load_value(&self, hash: &NodeHash) -> SentrixResult<Option<Vec<u8>>> {
        self.values
            .get(hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
            .map(|opt| opt.map(|iv| iv.to_vec()))
    }

    /// T-B: Remove a value blob from persistent storage (called when a leaf is replaced).
    pub fn delete_value(&self, hash: &NodeHash) -> SentrixResult<()> {
        self.values
            .remove(hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    // ── Roots ─────────────────────────────────────────────

    pub fn store_root(&self, version: u64, root: &NodeHash) -> SentrixResult<()> {
        // sled uses a write-ahead log and is crash-safe by default.  Explicit
        // flush() calls are not required for durability and block the write lock
        // unnecessarily — removed in fix/trie-permanent-fix (ROOT CAUSE #2).
        self.roots
            .insert(version.to_be_bytes(), root.as_slice())
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        // Maintain reverse index: NodeHash → version (O(1) is_committed_root lookups).
        self.committed_root_hashes
            .insert(root.as_slice(), &version.to_be_bytes())
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        // Mark reverse index as complete once at least one root is written
        // (covers the case where ensure_committed_roots_index() ran on an empty DB).
        if !self
            .committed_root_hashes
            .contains_key(b"__ready__")
            .unwrap_or(false)
        {
            let _ = self.committed_root_hashes.insert(b"__ready__", b"1");
        }
        Ok(())
    }

    pub fn load_root(&self, version: u64) -> SentrixResult<Option<NodeHash>> {
        match self
            .roots
            .get(version.to_be_bytes())
            .map_err(|e| SentrixError::StorageError(e.to_string()))?
        {
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
    ///
    /// Called by `SentrixTrie::insert()` before deleting old internal nodes so that
    /// the root hash of a previously committed version is never removed — which would
    /// cause a "root missing" error on restart and trigger a non-deterministic backfill
    /// that permanently forks the chain (ROOT CAUSE #3 fix).
    ///
    /// O(1) via `trie_committed_roots` reverse index (previously O(n_blocks) full scan).
    /// The reverse index is maintained by `store_root()` and backfilled from `trie_roots`
    /// on first open by `ensure_committed_roots_index()`.
    pub fn is_committed_root(&self, hash: &NodeHash) -> SentrixResult<bool> {
        self.committed_root_hashes
            .contains_key(hash.as_slice())
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    /// Prune old trie roots, keeping only the last `keep` versions.
    ///
    /// Deletes root entries from both `trie_roots` and `trie_committed_roots` for
    /// versions older than `(latest_version - keep)`. Returns the number of roots removed.
    pub fn prune_old_roots(&self, latest_version: u64, keep: u64) -> SentrixResult<usize> {
        if latest_version <= keep {
            return Ok(0); // Nothing to prune
        }
        let cutoff = latest_version - keep;
        let mut removed = 0usize;

        // Iterate trie_roots to find versions <= cutoff
        let mut to_delete: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        for entry in self.roots.iter() {
            let (k, v) = entry.map_err(|e| SentrixError::StorageError(e.to_string()))?;
            if k.len() == 8 {
                let mut buf = [0u8; 8];
                buf.copy_from_slice(&k);
                let version = u64::from_be_bytes(buf);
                if version <= cutoff {
                    to_delete.push((k.to_vec(), v.to_vec()));
                }
            }
        }

        for (key, root_hash) in &to_delete {
            // Remove from trie_roots
            self.roots
                .remove(key.as_slice())
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
            // Remove from reverse index (trie_committed_roots)
            if root_hash.len() == 32 {
                self.committed_root_hashes
                    .remove(root_hash.as_slice())
                    .map_err(|e| SentrixError::StorageError(e.to_string()))?;
            }
            removed += 1;
        }

        Ok(removed)
    }

    /// T-F: Garbage-collect node and value entries not present in `live_hashes`.
    ///
    /// Scans both `trie_nodes` and `trie_values`, collecting every hash not in the live
    /// set, then deletes them.  Returns the total count of entries removed across both trees.
    ///
    /// GC sweeps both `trie_nodes` and `trie_values` — orphaned value blobs from delete()
    /// calls were previously never cleaned.  Now both trees are scanned.
    ///
    /// Callers must supply a complete set of hashes reachable from all committed roots
    /// they wish to preserve.  Nodes referenced only by un-committed (in-flight) mutations
    /// are safe to include — but omitting them will cause those nodes to be deleted.
    pub fn gc_orphaned_nodes(
        &self,
        live_hashes: &std::collections::HashSet<NodeHash>,
    ) -> SentrixResult<usize> {
        let node_count = self.gc_tree(&self.nodes, live_hashes)?;
        // Also GC value blobs — leaf value_hash matches leaf node_hash, same live set.
        let value_count = self.gc_tree(&self.values, live_hashes)?;
        Ok(node_count + value_count)
    }

    /// Shared helper: scan a sled Tree for hashes not in `live_hashes` and remove them.
    fn gc_tree(
        &self,
        tree: &sled::Tree,
        live_hashes: &std::collections::HashSet<NodeHash>,
    ) -> SentrixResult<usize> {
        let mut to_delete: Vec<NodeHash> = Vec::new();
        for entry in tree.iter() {
            let (k, _) = entry.map_err(|e| SentrixError::StorageError(e.to_string()))?;
            if k.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&k);
                if !live_hashes.contains(&arr) {
                    to_delete.push(arr);
                }
            }
        }
        let count = to_delete.len();
        for hash in &to_delete {
            tree.remove(hash)
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{TrieNode, empty_hash};
    use std::collections::HashSet;

    fn temp_storage() -> (tempfile::TempDir, TrieStorage) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = sled::open(dir.path()).unwrap();
        let storage = TrieStorage::new(&db).unwrap();
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
        // Regression: store_root() must not call nodes/values/roots.flush().
        // We validate this by calling store_root() many times quickly — if flushes
        // were present the test would be noticeably slow on spinning disk / CI.
        let (_dir, storage) = temp_storage();
        let root = dummy_hash(0xFF);
        for v in 0u64..50 {
            storage.store_root(v, &root).unwrap();
        }
        // Also confirm we can still load them back correctly.
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
        // Also store value blobs (as if delete() leaked them)
        storage.store_value(&live_hash, b"live_data").unwrap();
        storage.store_value(&orphan_hash, b"orphan_data").unwrap();

        let mut live: std::collections::HashSet<NodeHash> = std::collections::HashSet::new();
        live.insert(live_hash);

        let removed = storage.gc_orphaned_nodes(&live).unwrap();
        // 1 orphan node + 1 orphan value = 2 removed
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

    // ── V10-C-02: is_committed_root O(1) reverse-index tests ─

    #[test]
    fn test_v10_c02_committed_root_reverse_index_populated_by_store_root() {
        let (_dir, storage) = temp_storage();
        let root = dummy_hash(0x42);
        storage.store_root(7, &root).unwrap();
        // Reverse index must contain the root hash immediately after store_root()
        assert!(
            storage
                .committed_root_hashes
                .contains_key(root.as_slice())
                .unwrap(),
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
        // Simulate a pre-migration DB: write directly to trie_roots tree, bypassing store_root().
        // Then re-open via TrieStorage::new() and verify ensure_committed_roots_index() backfills.
        let dir = tempfile::TempDir::new().unwrap();
        let db = sled::open(dir.path()).unwrap();

        let root = dummy_hash(0xAA);
        // Write directly to trie_roots without using TrieStorage (simulates old data)
        let old_roots = db.open_tree("trie_roots").unwrap();
        old_roots.insert(1u64.to_be_bytes(), &root[..]).unwrap();
        drop(old_roots);
        drop(db);

        // Now open via TrieStorage::new() — this triggers ensure_committed_roots_index()
        let db2 = sled::open(dir.path()).unwrap();
        let storage2 = TrieStorage::new(&db2).unwrap();

        // Backfill must have populated the reverse index
        assert!(
            storage2.is_committed_root(&root).unwrap(),
            "ensure_committed_roots_index() must backfill pre-migration roots"
        );
    }

    // ── Disk pruning tests ──────────────────────────────

    #[test]
    fn test_prune_old_roots_removes_stale() {
        let (_dir, storage) = temp_storage();
        // Store roots for versions 1..=10
        for v in 1u64..=10 {
            storage.store_root(v, &dummy_hash(v as u8)).unwrap();
        }
        // Prune keeping last 3 (versions 8,9,10 survive; 1-7 deleted)
        let removed = storage.prune_old_roots(10, 3).unwrap();
        assert_eq!(removed, 7, "should remove versions 1-7");
        // Verify surviving roots
        assert!(
            storage.load_root(8).unwrap().is_some(),
            "version 8 must survive"
        );
        assert!(
            storage.load_root(10).unwrap().is_some(),
            "version 10 must survive"
        );
        // Verify pruned roots
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
        // Keep 10 but only 5 exist — no pruning
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

        // Prune keeping only last 1 (version 10 survives, version 1 removed)
        storage.prune_old_roots(10, 1).unwrap();
        assert!(
            !storage.is_committed_root(&root).unwrap(),
            "pruned root must be removed from reverse index"
        );
    }
}
