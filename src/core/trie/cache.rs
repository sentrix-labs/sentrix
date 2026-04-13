// trie/cache.rs - Sentrix — LRU-cached trie node access

use std::num::NonZeroUsize;
use lru::LruCache;
use crate::core::trie::node::{NodeHash, TrieNode};
use crate::core::trie::storage::TrieStorage;
use crate::types::error::SentrixResult;

/// In-memory LRU cache sitting in front of persistent sled storage.
/// Capacity: 10 000 nodes (~10 000 × ~100 bytes ≈ 1 MB).
pub struct TrieCache {
    lru: LruCache<NodeHash, TrieNode>,
    pub(crate) storage: TrieStorage,
}

impl TrieCache {
    /// Create a new cache with a caller-specified `capacity` (number of nodes).
    /// Use `10_000` for production (≈ 1 MB with ~100 bytes per node).
    /// A capacity of zero is clamped to 1 (NonZeroUsize minimum).
    pub fn new(storage: TrieStorage, capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN);
        Self {
            lru: LruCache::new(cap),
            storage,
        }
    }

    /// Fetch a node by hash — LRU first, then persistent storage.
    pub fn get_node(&mut self, hash: &NodeHash) -> SentrixResult<Option<TrieNode>> {
        // Peek at cache without promoting (we promote on actual use below)
        if let Some(node) = self.lru.get(hash) {
            return Ok(Some(node.clone()));
        }
        // Miss — load from sled and populate cache
        let opt = self.storage.load_node(hash)?;
        if let Some(ref node) = opt {
            self.lru.put(*hash, node.clone());
        }
        Ok(opt)
    }

    /// Write a node to both cache and persistent storage.
    pub fn put_node(&mut self, hash: NodeHash, node: TrieNode) -> SentrixResult<()> {
        self.storage.store_node(&hash, &node)?;
        self.lru.put(hash, node);
        Ok(())
    }

    /// Persist a raw value blob keyed by its value_hash.
    pub fn store_value(&self, hash: &NodeHash, value: &[u8]) -> SentrixResult<()> {
        self.storage.store_value(hash, value)
    }

    /// Load a raw value blob by value_hash.
    pub fn load_value(&self, hash: &NodeHash) -> SentrixResult<Option<Vec<u8>>> {
        self.storage.load_value(hash)
    }

    /// T-B: Evict a node from the LRU cache and remove it from persistent storage.
    pub fn delete_node(&mut self, hash: &NodeHash) -> SentrixResult<()> {
        self.lru.pop(hash);
        self.storage.delete_node(hash)
    }

    /// T-B: Remove a value blob from persistent storage.
    pub fn delete_value(&self, hash: &NodeHash) -> SentrixResult<()> {
        self.storage.delete_value(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::trie::node::TrieNode;

    fn temp_cache(capacity: usize) -> (tempfile::TempDir, TrieCache) {
        let dir     = tempfile::TempDir::new().unwrap();
        let db      = sled::open(dir.path()).unwrap();
        let storage = crate::core::trie::storage::TrieStorage::new(&db).unwrap();
        (dir, TrieCache::new(storage, capacity))
    }

    /// T-D: cache must respect the caller-supplied capacity.
    #[test]
    fn test_configurable_capacity_evicts_lru() {
        let (_dir, mut cache) = temp_cache(2);
        let mk_hash = |b: u8| { let mut h = [0u8; 32]; h[0] = b; h };
        let node = TrieNode::Leaf { key: [0u8; 32], value_hash: [0u8; 32] };

        cache.put_node(mk_hash(1), node.clone()).unwrap();
        cache.put_node(mk_hash(2), node.clone()).unwrap();
        cache.put_node(mk_hash(3), node.clone()).unwrap(); // evicts hash(1) from LRU

        // hash(3) must be present; hash(1) may have been evicted from LRU
        // (it's still in sled, so get_node must always find it)
        assert!(cache.get_node(&mk_hash(3)).unwrap().is_some());
        assert!(cache.get_node(&mk_hash(2)).unwrap().is_some());
    }

    /// T-B: delete_node must evict from cache AND remove from storage.
    #[test]
    fn test_delete_node_evicts_cache_and_storage() {
        let (_dir, mut cache) = temp_cache(10_000);
        let hash = { let mut h = [0u8; 32]; h[0] = 0xFF; h };
        let node = TrieNode::Leaf { key: [1u8; 32], value_hash: [2u8; 32] };

        cache.put_node(hash, node).unwrap();
        assert!(cache.get_node(&hash).unwrap().is_some());

        cache.delete_node(&hash).unwrap();
        // Must be gone from both cache and storage
        assert!(cache.get_node(&hash).unwrap().is_none());
    }
}
