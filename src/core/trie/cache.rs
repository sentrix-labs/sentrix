// trie/cache.rs - Sentrix — LRU-cached trie node access

use std::num::NonZeroUsize;
use std::sync::Mutex;
use lru::LruCache;
use crate::core::trie::node::{NodeHash, TrieNode};
use crate::core::trie::storage::TrieStorage;
use crate::types::error::SentrixResult;

/// In-memory LRU cache sitting in front of persistent sled storage.
/// The LRU is wrapped in a Mutex to allow shared-reference access (V7-M-03).
/// This enables `prove()` on SentrixTrie to take `&self` (read lock on blockchain
/// is sufficient for proof generation — no write lock needed).
pub struct TrieCache {
    lru: Mutex<LruCache<NodeHash, TrieNode>>,
    pub(crate) storage: TrieStorage,
    /// Stored for use by Clone (V7-I-03).
    pub(crate) capacity: usize,
}

impl TrieCache {
    /// Create a new cache with a caller-specified `capacity` (number of nodes).
    /// Use `10_000` for production (≈ 1 MB with ~100 bytes per node).
    /// A capacity of zero is clamped to 1 (NonZeroUsize minimum).
    pub fn new(storage: TrieStorage, capacity: usize) -> Self {
        let cap = NonZeroUsize::new(capacity).unwrap_or(NonZeroUsize::MIN);
        Self {
            lru: Mutex::new(LruCache::new(cap)),
            storage,
            capacity,
        }
    }

    /// Acquire the LRU lock, mapping a poison error to SentrixError::Internal.
    fn lock_lru(&self) -> SentrixResult<std::sync::MutexGuard<'_, LruCache<NodeHash, TrieNode>>> {
        self.lru.lock().map_err(|e| {
            crate::types::error::SentrixError::Internal(
                format!("trie LRU lock poisoned: {e}")
            )
        })
    }

    /// Fetch a node by hash — LRU first, then persistent storage.
    /// Takes `&self` to allow shared access (V7-M-03).
    pub fn get_node(&self, hash: &NodeHash) -> SentrixResult<Option<TrieNode>> {
        {
            let mut lru = self.lock_lru()?;
            if let Some(node) = lru.get(hash) {
                return Ok(Some(node.clone()));
            }
        }
        // Miss — load from sled (released lock above to avoid holding during I/O)
        let opt = self.storage.load_node(hash)?;
        if let Some(ref node) = opt {
            let mut lru = self.lock_lru()?;
            lru.put(*hash, node.clone());
        }
        Ok(opt)
    }

    /// Write a node to both cache and persistent storage.
    pub fn put_node(&self, hash: NodeHash, node: TrieNode) -> SentrixResult<()> {
        self.storage.store_node(&hash, &node)?;
        let mut lru = self.lock_lru()?;
        lru.put(hash, node);
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
    pub fn delete_node(&self, hash: &NodeHash) -> SentrixResult<()> {
        {
            let mut lru = self.lock_lru()?;
            lru.pop(hash);
        }
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
        let (_dir, cache) = temp_cache(2);
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
        let (_dir, cache) = temp_cache(10_000);
        let hash = { let mut h = [0u8; 32]; h[0] = 0xFF; h };
        let node = TrieNode::Leaf { key: [1u8; 32], value_hash: [2u8; 32] };

        cache.put_node(hash, node).unwrap();
        assert!(cache.get_node(&hash).unwrap().is_some());

        cache.delete_node(&hash).unwrap();
        // Must be gone from both cache and storage
        assert!(cache.get_node(&hash).unwrap().is_none());
    }

    /// V7-I-03: clone uses original capacity, not hardcoded 10_000.
    #[test]
    fn test_cache_capacity_stored() {
        let (_dir, cache) = temp_cache(42);
        assert_eq!(cache.capacity, 42);
    }
}
