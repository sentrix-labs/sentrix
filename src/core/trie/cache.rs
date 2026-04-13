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
    pub fn new(storage: TrieStorage) -> Self {
        // NonZeroUsize::new(10_000) never returns None for a non-zero literal,
        // but use unwrap_or to stay panic-free.
        let cap = NonZeroUsize::new(10_000).unwrap_or(NonZeroUsize::MIN);
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
}
