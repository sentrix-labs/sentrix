// trie/cache.rs - Sentrix — LRU-cached trie node access

use crate::node::{NodeHash, TrieNode};
use crate::storage::TrieStorage;
use lru::LruCache;
use sentrix_primitives::{SentrixError, SentrixResult};
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Mutex;

/// In-memory LRU cache sitting in front of persistent MDBX storage.
/// The LRU is wrapped in a Mutex to allow shared-reference access (V7-M-03).
/// This enables `prove()` on SentrixTrie to take `&self` (read lock on blockchain
/// is sufficient for proof generation — no write lock needed).
///
/// `pending_nodes` / `pending_values` buffer the trie writes for the
/// current uncommitted version. `put_node` / `store_value` write here +
/// to the LRU, never to MDBX directly. `flush_pending(version, root)`
/// drains both buffers in a single MDBX transaction (batched write),
/// avoiding the per-op transaction overhead that dominated the trie
/// phase of block apply (~340 ms → ~15 ms expected on mainnet).
///
/// Reads hit LRU → pending → MDBX in order. A buffered write is always
/// also in the LRU, so the pending lookup only matters if LRU evicted
/// the entry before flush — at the 10 k default capacity this is
/// extremely rare, but the path covers it for correctness.
pub struct TrieCache {
    lru: Mutex<LruCache<NodeHash, TrieNode>>,
    pending_nodes: Mutex<HashMap<NodeHash, TrieNode>>,
    pending_values: Mutex<HashMap<NodeHash, Vec<u8>>>,
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
            pending_nodes: Mutex::new(HashMap::new()),
            pending_values: Mutex::new(HashMap::new()),
            storage,
            capacity,
        }
    }

    /// Acquire the LRU lock, mapping a poison error to SentrixError::Internal.
    fn lock_lru(&self) -> SentrixResult<std::sync::MutexGuard<'_, LruCache<NodeHash, TrieNode>>> {
        self.lru
            .lock()
            .map_err(|e| SentrixError::Internal(format!("trie LRU lock poisoned: {e}")))
    }

    fn lock_pending_nodes(
        &self,
    ) -> SentrixResult<std::sync::MutexGuard<'_, HashMap<NodeHash, TrieNode>>> {
        self.pending_nodes
            .lock()
            .map_err(|e| SentrixError::Internal(format!("trie pending_nodes lock poisoned: {e}")))
    }

    fn lock_pending_values(
        &self,
    ) -> SentrixResult<std::sync::MutexGuard<'_, HashMap<NodeHash, Vec<u8>>>> {
        self.pending_values
            .lock()
            .map_err(|e| SentrixError::Internal(format!("trie pending_values lock poisoned: {e}")))
    }

    /// Fetch a node by hash — LRU first, then persistent storage.
    /// Takes `&self` to allow shared access (V7-M-03).
    ///
    /// P1 (trie cache race): the miss path holds the LRU lock across
    /// the MDBX read so a concurrent `delete_node` cannot finish
    /// between our initial miss and our fill. Without this, the
    /// sequence `reader LRU miss → reader MDBX found → writer MDBX
    /// delete + LRU pop → reader LRU insert` would leave a stale
    /// resurrection in the cache indefinitely. Hits still take the
    /// lock only momentarily; only the rare miss path pays the
    /// serialisation cost.
    pub fn get_node(&self, hash: &NodeHash) -> SentrixResult<Option<TrieNode>> {
        let mut lru = self.lock_lru()?;
        if let Some(node) = lru.get(hash) {
            return Ok(Some(node.clone()));
        }
        // LRU miss — try the pending-write buffer (the entry might be a
        // node we just put_node()'d this block, not yet flushed to MDBX).
        // We check pending BEFORE MDBX because the LRU is the primary
        // hot-path; pending is a small fallback when LRU evicted under
        // pressure within the same uncommitted block.
        {
            let pending = self.lock_pending_nodes()?;
            if let Some(node) = pending.get(hash) {
                let cloned = node.clone();
                lru.put(*hash, cloned.clone());
                return Ok(Some(cloned));
            }
        }
        // Miss — load from MDBX while still holding the LRU lock. This
        // serialises miss fills against concurrent deletes, preventing
        // the stale-resurrection race documented above.
        let opt = self.storage.load_node(hash)?;
        if let Some(ref node) = opt {
            lru.put(*hash, node.clone());
        }
        Ok(opt)
    }

    /// Buffer a node write to be flushed in the next commit. Also puts
    /// into the LRU so reads in the current block see it without a
    /// pending-buffer lookup.
    pub fn put_node(&self, hash: NodeHash, node: TrieNode) -> SentrixResult<()> {
        {
            let mut pending = self.lock_pending_nodes()?;
            pending.insert(hash, node.clone());
        }
        let mut lru = self.lock_lru()?;
        lru.put(hash, node);
        Ok(())
    }

    /// Buffer a value-blob write to be flushed in the next commit.
    pub fn store_value(&self, hash: &NodeHash, value: &[u8]) -> SentrixResult<()> {
        let mut pending = self.lock_pending_values()?;
        pending.insert(*hash, value.to_vec());
        Ok(())
    }

    /// Load a raw value blob by value_hash. Check pending buffer first
    /// (current uncommitted block's writes), fall back to MDBX.
    pub fn load_value(&self, hash: &NodeHash) -> SentrixResult<Option<Vec<u8>>> {
        {
            let pending = self.lock_pending_values()?;
            if let Some(v) = pending.get(hash) {
                return Ok(Some(v.clone()));
            }
        }
        self.storage.load_value(hash)
    }

    /// Drain both pending buffers + write the root + reverse-index entry
    /// in a single MDBX transaction. Called by `Trie::commit(version)`.
    /// Atomic by construction — either every buffered write lands together
    /// with the root advance, or nothing changes (MDBX rollback on error).
    pub fn flush_pending(&self, version: u64, root: &NodeHash) -> SentrixResult<()> {
        let nodes: Vec<(NodeHash, TrieNode)> = {
            let pending = self.lock_pending_nodes()?;
            pending.iter().map(|(k, v)| (*k, v.clone())).collect()
        };
        let values: Vec<(NodeHash, Vec<u8>)> = {
            let pending = self.lock_pending_values()?;
            pending.iter().map(|(k, v)| (*k, v.clone())).collect()
        };
        self.storage
            .flush_trie_batch(&nodes, &values, version, root)?;
        // Only clear after a successful MDBX commit so a failure leaves
        // the buffers intact for the next retry.
        self.lock_pending_nodes()?.clear();
        self.lock_pending_values()?.clear();
        Ok(())
    }

    /// T-B: Evict a node from the LRU cache and remove it from persistent storage.
    ///
    /// P1 (trie cache race): MDBX delete happens FIRST, LRU pop happens
    /// SECOND — under the SAME lock that get_node's miss-fill takes. The
    /// prior ordering (pop then delete) allowed a concurrent reader that
    /// had just hit MDBX (finding the about-to-be-deleted node) to
    /// insert it back into the cache AFTER our pop, leaving a stale
    /// resurrection. Ordering MDBX-first guarantees that once our MDBX
    /// delete returns, no further reader can observe the node in MDBX;
    /// holding the LRU lock across the pop ensures any get_node that
    /// observed the node in MDBX BEFORE our delete and is now waiting
    /// to insert it cannot race past our pop.
    pub fn delete_node(&self, hash: &NodeHash) -> SentrixResult<()> {
        let mut lru = self.lock_lru()?;
        self.storage.delete_node(hash)?;
        lru.pop(hash);
        // Also drop from the pending buffer, otherwise a node that was
        // put_node'd this block and then delete_node'd would survive in
        // pending and re-land in MDBX at the next flush — defeating the
        // delete.
        self.lock_pending_nodes()?.remove(hash);
        Ok(())
    }

    /// T-B: Remove a value blob from persistent storage + pending buffer.
    pub fn delete_value(&self, hash: &NodeHash) -> SentrixResult<()> {
        self.storage.delete_value(hash)?;
        self.lock_pending_values()?.remove(hash);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::TrieNode;

    fn temp_cache(capacity: usize) -> (tempfile::TempDir, TrieCache) {
        let dir = tempfile::TempDir::new().unwrap();
        let mdbx = std::sync::Arc::new(sentrix_storage::MdbxStorage::open(dir.path()).unwrap());
        let storage = crate::storage::TrieStorage::new(mdbx).unwrap();
        (dir, TrieCache::new(storage, capacity))
    }

    /// T-D: cache must respect the caller-supplied capacity.
    #[test]
    fn test_configurable_capacity_evicts_lru() {
        let (_dir, cache) = temp_cache(2);
        let mk_hash = |b: u8| {
            let mut h = [0u8; 32];
            h[0] = b;
            h
        };
        let node = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: [0u8; 32],
        };

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
        let hash = {
            let mut h = [0u8; 32];
            h[0] = 0xFF;
            h
        };
        let node = TrieNode::Leaf {
            key: [1u8; 32],
            value_hash: [2u8; 32],
        };

        cache.put_node(hash, node).unwrap();
        assert!(cache.get_node(&hash).unwrap().is_some());

        cache.delete_node(&hash).unwrap();
        // Must be gone from both cache and storage
        assert!(cache.get_node(&hash).unwrap().is_none());
    }

    /// Clone preserves the original capacity rather than using a hardcoded default.
    #[test]
    fn test_cache_capacity_stored() {
        let (_dir, cache) = temp_cache(42);
        assert_eq!(cache.capacity, 42);
    }

    /// Regression test for the batched-commit path.
    ///
    /// Invariant: between `put_node` and `flush_pending`, the buffered
    /// nodes are visible via `get_node` (LRU + pending fallback) but NOT
    /// yet present in persistent MDBX. After `flush_pending`, MDBX
    /// contains them and the buffer is empty.
    ///
    /// Locks in the new contract — fails on `main` before this change
    /// (where `put_node` was synchronous-write).
    #[test]
    fn test_pending_buffer_visible_before_flush_persistent_after() {
        let (_dir, cache) = temp_cache(10_000);
        let h1 = {
            let mut h = [0u8; 32];
            h[0] = 0x01;
            h
        };
        let node1 = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: [0u8; 32],
        };

        cache.put_node(h1, node1.clone()).unwrap();
        cache.store_value(&h1, b"hello").unwrap();

        // Visible via cache (LRU hit).
        assert!(cache.get_node(&h1).unwrap().is_some());
        // Visible via cache (load_value checks pending first).
        assert_eq!(cache.load_value(&h1).unwrap(), Some(b"hello".to_vec()));
        // NOT yet in persistent storage.
        assert!(cache.storage.load_node(&h1).unwrap().is_none());
        assert!(cache.storage.load_value(&h1).unwrap().is_none());

        // Flush — single MDBX transaction lands nodes + values + root.
        let root_hash = [0xAB; 32];
        cache.flush_pending(0, &root_hash).unwrap();

        // Now in persistent storage.
        assert!(cache.storage.load_node(&h1).unwrap().is_some());
        assert_eq!(cache.storage.load_value(&h1).unwrap(), Some(b"hello".to_vec()));
        // Root stored too.
        assert_eq!(cache.storage.load_root(0).unwrap(), Some(root_hash));
        // Pending buffers drained.
        assert!(cache.lock_pending_nodes().unwrap().is_empty());
        assert!(cache.lock_pending_values().unwrap().is_empty());
    }

    /// `get_node` falls through LRU → pending → MDBX. If a node is in
    /// pending but LRU has been evicted under capacity pressure, the
    /// pending fallback must still serve the read.
    #[test]
    fn test_get_node_falls_back_to_pending_after_lru_eviction() {
        let (_dir, cache) = temp_cache(2); // tiny LRU
        let mk_hash = |b: u8| {
            let mut h = [0u8; 32];
            h[0] = b;
            h
        };
        let node = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: [0u8; 32],
        };

        cache.put_node(mk_hash(1), node.clone()).unwrap();
        cache.put_node(mk_hash(2), node.clone()).unwrap();
        cache.put_node(mk_hash(3), node.clone()).unwrap(); // evicts h(1) from LRU
        // h(1) was just evicted from LRU but it's still in pending —
        // pre-flush, MDBX doesn't have it yet. get_node must find it
        // via the pending fallback or the test catches the regression.
        assert!(cache.get_node(&mk_hash(1)).unwrap().is_some());
    }
}
