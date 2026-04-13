// trie/storage.rs - Sentrix — Persistent sled-backed trie storage

use sled::{Db, Tree};
use crate::core::trie::node::{NodeHash, TrieNode};
use crate::types::error::{SentrixError, SentrixResult};

/// Low-level persistent storage for trie nodes, values, and version→root mappings.
///
/// Three named sled trees:
/// - `trie_nodes`  : NodeHash → bincode(TrieNode)
/// - `trie_values` : NodeHash → raw account-state bytes
/// - `trie_roots`  : version u64 BE → NodeHash
///
/// `Clone` is cheap — sled::Tree is an Arc internally (shared underlying tree).
#[derive(Clone)]
pub struct TrieStorage {
    nodes: Tree,
    values: Tree,
    roots: Tree,
}

impl TrieStorage {
    /// Open (or create) the three named trees from an existing sled Db.
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
        Ok(Self { nodes, values, roots })
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
        self.roots
            .insert(version.to_be_bytes(), root.as_slice())
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        // V7-H-02: flush all three trees in order (nodes → values → roots) so that
        // node/value writes that preceded this root are durable before the root pointer
        // is committed.  Flushing only `roots` would leave nodes/values in the OS page
        // cache — a crash could produce a valid root pointing to missing nodes.
        self.nodes
            .flush()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        self.values
            .flush()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        self.roots
            .flush()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
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

    /// T-F: Garbage-collect node and value entries not present in `live_hashes`.
    ///
    /// Scans both `trie_nodes` and `trie_values`, collecting every hash not in the live
    /// set, then deletes them.  Returns the total count of entries removed across both trees.
    ///
    /// V7-M-02: previously only GC'd `trie_nodes`; orphaned value blobs (from delete()
    /// calls) were never cleaned.  Now both trees are scanned.
    ///
    /// Callers must supply a complete set of hashes reachable from all committed roots
    /// they wish to preserve.  Nodes referenced only by un-committed (in-flight) mutations
    /// are safe to include — but omitting them will cause those nodes to be deleted.
    pub fn gc_orphaned_nodes(
        &self,
        live_hashes: &std::collections::HashSet<NodeHash>,
    ) -> SentrixResult<usize> {
        let node_count = self.gc_tree(&self.nodes, live_hashes)?;
        // V7-M-02: also GC value blobs — leaf value_hash == leaf node_hash, same live set.
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
    use crate::core::trie::node::{TrieNode, empty_hash};
    use std::collections::HashSet;

    fn temp_storage() -> (tempfile::TempDir, TrieStorage) {
        let dir = tempfile::TempDir::new().unwrap();
        let db  = sled::open(dir.path()).unwrap();
        let storage = TrieStorage::new(&db).unwrap();
        (dir, storage)
    }

    fn dummy_hash(byte: u8) -> NodeHash {
        let mut h = [0u8; 32];
        h[0] = byte;
        h
    }

    #[test]
    fn test_delete_node_removes_entry() {
        let (_dir, storage) = temp_storage();
        let hash = dummy_hash(0xAB);
        let node = TrieNode::Leaf { key: [1u8; 32], value_hash: [2u8; 32] };

        storage.store_node(&hash, &node).unwrap();
        assert!(storage.load_node(&hash).unwrap().is_some(), "node must exist after store");

        storage.delete_node(&hash).unwrap();
        assert!(storage.load_node(&hash).unwrap().is_none(), "node must be absent after delete");
    }

    #[test]
    fn test_delete_value_removes_entry() {
        let (_dir, storage) = temp_storage();
        let hash = dummy_hash(0xCD);
        let val  = b"balance_data";

        storage.store_value(&hash, val).unwrap();
        assert!(storage.load_value(&hash).unwrap().is_some(), "value must exist after store");

        storage.delete_value(&hash).unwrap();
        assert!(storage.load_value(&hash).unwrap().is_none(), "value must be absent after delete");
    }

    #[test]
    fn test_gc_orphaned_nodes_removes_unlisted() {
        let (_dir, storage) = temp_storage();
        let live_hash   = dummy_hash(0x01);
        let orphan_hash = dummy_hash(0x02);

        let node = TrieNode::Leaf { key: [0u8; 32], value_hash: empty_hash(0) };
        storage.store_node(&live_hash,   &node).unwrap();
        storage.store_node(&orphan_hash, &node).unwrap();

        let mut live: HashSet<NodeHash> = HashSet::new();
        live.insert(live_hash);

        let removed = storage.gc_orphaned_nodes(&live).unwrap();
        assert_eq!(removed, 1, "exactly one orphan must be removed");
        assert!(storage.load_node(&live_hash).unwrap().is_some(),   "live node must survive GC");
        assert!(storage.load_node(&orphan_hash).unwrap().is_none(), "orphan must be removed by GC");
    }

    #[test]
    fn test_gc_empty_live_set_removes_all() {
        let (_dir, storage) = temp_storage();
        let node = TrieNode::Leaf { key: [0u8; 32], value_hash: empty_hash(0) };
        for i in 0u8..5 {
            storage.store_node(&dummy_hash(i), &node).unwrap();
        }
        let removed = storage.gc_orphaned_nodes(&HashSet::new()).unwrap();
        assert_eq!(removed, 5, "all 5 nodes must be removed when live set is empty");
    }

    #[test]
    fn test_gc_also_removes_orphan_values() {
        let (_dir, storage) = temp_storage();
        let live_hash   = dummy_hash(0x01);
        let orphan_hash = dummy_hash(0x02);

        let node = TrieNode::Leaf { key: [0u8; 32], value_hash: empty_hash(0) };
        storage.store_node(&live_hash,   &node).unwrap();
        storage.store_node(&orphan_hash, &node).unwrap();
        // Also store value blobs (as if delete() leaked them)
        storage.store_value(&live_hash,   b"live_data").unwrap();
        storage.store_value(&orphan_hash, b"orphan_data").unwrap();

        let mut live: std::collections::HashSet<NodeHash> = std::collections::HashSet::new();
        live.insert(live_hash);

        let removed = storage.gc_orphaned_nodes(&live).unwrap();
        // 1 orphan node + 1 orphan value = 2 removed
        assert_eq!(removed, 2, "GC must remove both orphan node and orphan value");
        assert!(storage.load_value(&live_hash).unwrap().is_some(),   "live value must survive GC");
        assert!(storage.load_value(&orphan_hash).unwrap().is_none(), "orphan value must be removed");
    }
}
