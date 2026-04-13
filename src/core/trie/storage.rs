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

    // ── Roots ─────────────────────────────────────────────

    pub fn store_root(&self, version: u64, root: &NodeHash) -> SentrixResult<()> {
        self.roots
            .insert(version.to_be_bytes(), root.as_slice())
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        // Flush to guarantee crash-safety for the version→root mapping
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
}
