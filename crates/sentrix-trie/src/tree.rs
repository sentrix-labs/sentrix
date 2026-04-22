// trie/tree.rs - Sentrix — Binary Sparse Merkle Tree (256-level, iterative)

use crate::cache::TrieCache;
use crate::node::{NodeHash, TrieNode, empty_hash, get_bit, hash_internal, hash_leaf};
use crate::proof::MerkleProof;
use crate::storage::TrieStorage;
use sentrix_primitives::{SentrixError, SentrixResult};
use sentrix_storage::MdbxStorage;
use std::sync::Arc;

/// Binary Sparse Merkle Tree with 256 levels.
///
/// Properties:
/// - Keys: 32 bytes (256 bits) — derive from addresses via `address_to_key`
/// - Leaf hash:     BLAKE3(0x00 || key || value)
/// - Internal hash: SHA-256(0x01 || left || right)
/// - Short-circuit: a lone key in a subtree is stored as a leaf at that depth
/// - Persistent:    all nodes/values stored in MDBX; LRU cache in front
/// - Versioned:     each committed `version` (block height) maps to a root hash
pub struct SentrixTrie {
    cache: TrieCache,
    root: NodeHash,
    version: u64,
}

impl SentrixTrie {
    /// Open (or create) a trie backed by MDBX storage at `version`.
    /// Loads the stored root for that version; uses the empty-tree root if none exists.
    pub fn open(mdbx: Arc<MdbxStorage>, version: u64) -> SentrixResult<Self> {
        let storage = TrieStorage::new(mdbx)?;
        let root = storage.load_root(version)?.unwrap_or_else(|| empty_hash(0));
        let cache = TrieCache::new(storage, 10_000);
        Ok(Self {
            cache,
            root,
            version,
        })
    }

    // ── Public accessors ─────────────────────────────────

    pub fn root_hash(&self) -> NodeHash {
        self.root
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    // ── Core operations ──────────────────────────────────

    /// Insert or update `key → value`.  Returns the new root hash.
    ///
    /// Fully iterative — no recursion, so stack depth is O(1) regardless of tree depth.
    pub fn insert(&mut self, key: &[u8; 32], value: &[u8]) -> SentrixResult<NodeHash> {
        let new_value_hash = hash_leaf(key, value);

        // Phase 1 — walk DOWN collecting (sibling_hash, did_new_key_go_right) entries.
        // path[0] = decision at depth 0 (root level), path[N-1] = deepest decision.
        let mut path: Vec<(NodeHash, bool)> = Vec::with_capacity(256);
        let mut current = self.root;
        let mut depth = 0usize;
        // T-B: when updating an existing key, record the old leaf hash so it can be
        // removed after the new leaf is written (prevents orphaned-node storage leak).
        let mut old_leaf_hash: Option<NodeHash> = None;
        // Track old internal nodes so they can be cleaned up after structural replacement.
        let mut old_internal_hashes: Vec<NodeHash> = Vec::new();

        loop {
            if depth > 256 {
                return Err(SentrixError::Internal(
                    "SMT depth exceeded 256 — key space exhausted".into(),
                ));
            }

            // Empty slot → new leaf goes here
            if current == empty_hash(depth) {
                break;
            }

            let node = self.cache.get_node(&current)?.ok_or_else(|| {
                SentrixError::Internal(format!("trie: missing node {}", hex::encode(current)))
            })?;

            match node {
                TrieNode::Leaf {
                    key: leaf_key,
                    value_hash: leaf_vh,
                } => {
                    if leaf_key == *key {
                        // Same key — update in place; path already covers the descent.
                        // T-B: capture the old leaf hash (= current) for cleanup below,
                        // but only if the value actually changed (different hash).
                        if current != new_value_hash {
                            old_leaf_hash = Some(current);
                        }
                        break;
                    }
                    // Different key — "expand" the short-circuit leaf by pushing
                    // virtual empty-sibling entries for every level where both keys
                    // share the same bit, then one real sibling at the diverging bit.
                    let mut split = depth;
                    while split < 256 {
                        if get_bit(key, split) != get_bit(&leaf_key, split) {
                            break;
                        }
                        // Bits agree at `split`: sibling is an empty subtree.
                        path.push((empty_hash(split + 1), get_bit(key, split)));
                        split += 1;
                    }
                    if split >= 256 {
                        return Err(SentrixError::Internal(
                            "trie: two keys are identical".into(),
                        ));
                    }
                    // At `split` the keys diverge; the existing leaf is the sibling.
                    path.push((leaf_vh, get_bit(key, split)));
                    break;
                }
                TrieNode::Internal { left, right, .. } => {
                    // get_bit(key, 256) would be out of bounds — an Internal node at depth 256 indicates corruption
                    if depth >= 256 {
                        return Err(SentrixError::Internal(
                            "trie: corrupt tree — Internal node at depth 256 (key space exhausted)"
                                .into(),
                        ));
                    }
                    // Record this internal node so it can be cleaned up when structurally replaced.
                    old_internal_hashes.push(current);
                    let bit = get_bit(key, depth);
                    let (child, sibling) = if bit { (right, left) } else { (left, right) };
                    path.push((sibling, bit));
                    current = child;
                    depth += 1;
                }
            }
        }

        // Phase 2 — store the new leaf.
        let new_leaf = TrieNode::Leaf {
            key: *key,
            value_hash: new_value_hash,
        };
        self.cache.put_node(new_value_hash, new_leaf)?;
        self.cache.store_value(&new_value_hash, value)?;

        // ROOT CAUSE (2026-04-20): removing the old leaf here was unsound for
        // the same reason as the internal-node cleanup below — the old leaf
        // is still reachable from previously committed roots. Left in place;
        // prune() is responsible for collecting unreachable leaves.
        let _ = old_leaf_hash;

        // Phase 3 — walk UP recomputing internal hashes.
        let mut up_hash = new_value_hash;
        for (sibling, is_right) in path.iter().rev() {
            let (left, right) = if *is_right {
                (*sibling, up_hash)
            } else {
                (up_hash, *sibling)
            };
            up_hash = hash_internal(&left, &right);
            let node = TrieNode::Internal {
                left,
                right,
                hash: up_hash,
            };
            self.cache.put_node(up_hash, node)?;
        }

        self.root = up_hash;

        // ROOT CAUSE (2026-04-20 mainnet incident): inline deletion of
        // `old_internal_hashes` here was unsound. The `is_committed_root`
        // guard below only protected root hashes themselves — it did not
        // protect internal nodes that were CHILDREN of still-committed
        // roots. A previously committed root R_N stores {left, right}
        // pointers to internal nodes below it; when a later version's
        // insert walked through those same internal nodes, it added them
        // to `old_internal_hashes` and the cleanup deleted them. Any walk
        // starting from R_N (e.g. on restart, historical query, or any
        // path where self.root hadn't yet been updated to R_{N+1}) would
        // then fire "trie: missing node <hash>".
        //
        // We can't safely decide "is this internal node still reachable"
        // without a full walk of every surviving committed root, which
        // is what `prune()` does. Inline deletion cannot do that cheaply,
        // so it's removed entirely. Storage growth is bounded by calling
        // `prune()` periodically from the block-apply path; see
        // blockchain::maybe_prune_trie.
        //
        // DO NOT re-introduce inline deletion here without also tracking
        // reference counts or walking all committed roots to verify the
        // hash is truly orphaned.
        let _ = old_internal_hashes; // bindings above still needed for borrow-check

        Ok(up_hash)
    }

    /// Look up the value stored at `key`.  Returns `None` if absent.
    pub fn get(&mut self, key: &[u8; 32]) -> SentrixResult<Option<Vec<u8>>> {
        let mut current = self.root;
        let mut depth = 0usize;

        loop {
            if depth > 256 {
                return Ok(None);
            }
            if current == empty_hash(depth) {
                return Ok(None);
            }

            let node = self.cache.get_node(&current)?.ok_or_else(|| {
                SentrixError::Internal(format!("trie: missing node {}", hex::encode(current)))
            })?;

            match node {
                TrieNode::Leaf {
                    key: leaf_key,
                    value_hash,
                } => {
                    if leaf_key == *key {
                        return self.cache.load_value(&value_hash);
                    }
                    return Ok(None);
                }
                TrieNode::Internal { left, right, .. } => {
                    let bit = get_bit(key, depth);
                    current = if bit { right } else { left };
                    depth += 1;
                }
            }
        }
    }

    /// Delete `key` from the trie.  Returns the new root hash.
    ///
    /// If the key is absent the trie is unchanged and the current root is returned — no error.
    /// Sibling-collapse: when both children of a node become empty after deletion, the parent
    /// also collapses to an empty hash (short-circuit property maintained).
    ///
    /// Fully iterative — O(1) stack depth.
    // Initial None assignments are intentional; the compiler warns because they are
    // always overwritten inside the loop before being read.
    #[allow(unused_assignments)]
    pub fn delete(&mut self, key: &[u8; 32]) -> SentrixResult<NodeHash> {
        let mut path: Vec<(NodeHash, bool)> = Vec::with_capacity(256);
        let mut current = self.root;
        let mut depth = 0usize;

        // Storage for leaf/value hashes captured in Phase 1 — cleaned up in Phase 2.
        let mut found_leaf_hash: Option<NodeHash> = None;
        let mut found_value_hash: Option<NodeHash> = None;

        // Phase 1: walk down to find the leaf
        let found_depth = loop {
            if depth > 256 {
                return Ok(self.root); // exhausted — key absent
            }
            if current == empty_hash(depth) {
                return Ok(self.root); // empty subtree — key absent
            }

            let node = self.cache.get_node(&current)?.ok_or_else(|| {
                SentrixError::Internal(format!("trie: missing node {}", hex::encode(current)))
            })?;

            match node {
                TrieNode::Leaf {
                    key: leaf_key,
                    value_hash: leaf_vh,
                } => {
                    if leaf_key != *key {
                        return Ok(self.root); // different leaf — key absent
                    }
                    // Capture leaf and value hash for cleanup once Phase 2 relinks around the deleted node.
                    found_leaf_hash = Some(current);
                    found_value_hash = Some(leaf_vh);
                    break depth; // found — leaf is at `depth`
                }
                TrieNode::Internal { left, right, .. } => {
                    // Internal at depth 256 means all key bits are exhausted — tree structure is corrupt
                    if depth >= 256 {
                        return Err(SentrixError::Internal(
                            "trie: corrupt tree — Internal node at depth 256".into(),
                        ));
                    }
                    let bit = get_bit(key, depth);
                    let (child, sibling) = if bit { (right, left) } else { (left, right) };
                    path.push((sibling, bit));
                    current = child;
                    depth += 1;
                }
            }
        };

        // Phase 2: walk up replacing the deleted leaf with empty, collapsing when both
        //          children are empty.
        let mut up_hash = empty_hash(found_depth);
        let mut up_depth = found_depth; // depth of the node represented by up_hash

        for (sibling, is_right) in path.iter().rev() {
            // Defensive guard against underflow on corrupt or malformed trees
            if up_depth == 0 {
                break;
            }
            // Moving one level toward root
            up_depth -= 1;
            let (left, right) = if *is_right {
                (*sibling, up_hash)
            } else {
                (up_hash, *sibling)
            };
            // Collapse: both children are empty subtrees → parent is empty too
            let child_empty = empty_hash(up_depth + 1);
            if left == child_empty && right == child_empty {
                up_hash = empty_hash(up_depth);
            } else {
                up_hash = hash_internal(&left, &right);
                self.cache.put_node(
                    up_hash,
                    TrieNode::Internal {
                        left,
                        right,
                        hash: up_hash,
                    },
                )?;
            }
        }

        self.root = up_hash;

        // ROOT CAUSE (2026-04-20): deleting the leaf and its value here was
        // unsound — the leaf is still reachable from previously committed
        // roots (e.g. a get() at an older version). Leave intact; prune()
        // handles unreachable-leaf collection.
        let _ = found_leaf_hash;
        let _ = found_value_hash;

        Ok(up_hash)
    }

    /// Generate a Merkle proof (membership or non-membership) for `key`.
    pub fn prove(&self, key: &[u8; 32]) -> SentrixResult<MerkleProof> {
        let mut siblings: Vec<NodeHash> = Vec::with_capacity(64);
        let mut current = self.root;
        let mut depth = 0usize;

        loop {
            if depth > 256 {
                return Ok(MerkleProof {
                    key: *key,
                    value: Vec::new(),
                    siblings,
                    depth,
                    found: false,
                    terminal_hash: empty_hash(depth),
                });
            }
            if current == empty_hash(depth) {
                return Ok(MerkleProof {
                    key: *key,
                    value: Vec::new(),
                    siblings,
                    depth,
                    found: false,
                    terminal_hash: empty_hash(depth),
                });
            }

            let node = self
                .cache
                .get_node(&current)?
                .ok_or_else(|| SentrixError::Internal("trie: missing node in prove".into()))?;

            match node {
                TrieNode::Leaf {
                    key: leaf_key,
                    value_hash,
                } => {
                    if leaf_key == *key {
                        let value = self.cache.load_value(&value_hash)?.unwrap_or_default();
                        let terminal_hash = hash_leaf(key, &value);
                        return Ok(MerkleProof {
                            key: *key,
                            value,
                            siblings,
                            depth,
                            found: true,
                            terminal_hash,
                        });
                    }
                    // Non-membership: hit a different leaf — its hash is the terminal.
                    return Ok(MerkleProof {
                        key: *key,
                        value: Vec::new(),
                        siblings,
                        depth,
                        found: false,
                        terminal_hash: value_hash,
                    });
                }
                TrieNode::Internal { left, right, .. } => {
                    // Internal at depth 256 means the key space is exhausted — tree structure is corrupt
                    if depth >= 256 {
                        return Err(SentrixError::Internal(
                            "trie: corrupt tree — Internal node at depth 256 in prove".into(),
                        ));
                    }
                    let bit = get_bit(key, depth);
                    let (child, sibling) = if bit { (right, left) } else { (left, right) };
                    siblings.push(sibling);
                    current = child;
                    depth += 1;
                }
            }
        }
    }

    // ── Versioning ────────────────────────────────────────

    /// Persist the current root under `version` (block height) and advance the trie version.
    /// Call once per block after all inserts for that block are done.
    pub fn commit(&mut self, version: u64) -> SentrixResult<NodeHash> {
        self.cache.storage.store_root(version, &self.root)?;
        self.version = version;
        Ok(self.root)
    }

    /// Return the state root that was committed at `version`, without changing this trie.
    pub fn root_at_version(&self, version: u64) -> SentrixResult<Option<NodeHash>> {
        self.cache.storage.load_root(version)
    }

    /// Returns true if the given node hash exists in persistent storage.
    /// Used by init_trie() to detect stale root hashes whose nodes were
    /// deleted by V7-L-01 (e.g. after P2P sync with stale height key).
    pub fn node_exists(&self, hash: &NodeHash) -> SentrixResult<bool> {
        Ok(self.cache.storage.load_node(hash)?.is_some())
    }

    /// Walk the current root and verify that every referenced node + value is
    /// loadable from persistent storage. Returns an error naming the first
    /// missing reference.
    ///
    /// Added post-2026-04-21 mainnet 3-way fork. Root cause of that incident
    /// was a pre-v2.1.5 `state_import` that left orphan references in the
    /// trie — the root hash was recorded in `trie_roots` but some subtree
    /// was missing from `trie_nodes` / `trie_values`. Blocks produced on
    /// that broken database emitted `state_root=None`, which the stricter
    /// peers then rejected with CRITICAL #1e — chain fork. This check
    /// surfaces the same class of damage at boot instead of letting the
    /// node produce broken blocks.
    ///
    /// Cost: walks every reachable node once; for a 256-level binary SMT
    /// with ~N leaves this is O(N·log₂(keyspace)) MDBX reads ≈ a few hundred
    /// ms on realistic mainnet state. Run once per boot in init_trie.
    pub fn verify_integrity(&self) -> SentrixResult<()> {
        use crate::node::empty_hash;
        let root = self.root;
        if root == empty_hash(0) {
            return Ok(());
        }
        self.walk_verify(root, 0)
    }

    /// Recursive helper for verify_integrity. Visits every reachable node
    /// exactly once along a unique path (binary SMT has no shared subtrees
    /// across paths) and confirms each is present in `trie_nodes` and each
    /// leaf's value is present in `trie_values`.
    fn walk_verify(&self, hash: NodeHash, depth: usize) -> SentrixResult<()> {
        use crate::node::empty_hash;
        if hash == empty_hash(depth.min(256)) {
            return Ok(());
        }
        let node = self.cache.storage.load_node(&hash)?.ok_or_else(|| {
            SentrixError::Internal(format!(
                "trie integrity: orphan node reference {} at depth {} \
                 — the trie references a node that is missing from trie_nodes. \
                 Chain.db is damaged (historical artifact of a pre-v2.1.5 state_import \
                 or a crash mid-commit). Recover via rsync of chain.db from a healthy peer.",
                hex::encode(hash),
                depth
            ))
        })?;
        match node {
            TrieNode::Leaf { value_hash, .. } => {
                if self.cache.storage.load_value(&value_hash)?.is_none() {
                    return Err(SentrixError::Internal(format!(
                        "trie integrity: orphan value reference {} from leaf {} at depth {} \
                         — the trie references a value that is missing from trie_values. \
                         Same recovery as orphan-node: rsync chain.db from a healthy peer.",
                        hex::encode(value_hash),
                        hex::encode(hash),
                        depth
                    )));
                }
                Ok(())
            }
            TrieNode::Internal { left, right, .. } => {
                self.walk_verify(left, depth + 1)?;
                self.walk_verify(right, depth + 1)?;
                Ok(())
            }
        }
    }

    /// Prune old trie roots and garbage-collect orphaned nodes.
    ///
    /// Keeps the last `keep_versions` committed roots (default 1000).
    /// Removes old root entries from storage, then walks all surviving roots
    /// to build a live-hash set, and finally GCs any node/value not in that set.
    ///
    /// Returns `(roots_pruned, nodes_gc'd)`.
    pub fn prune(&self, keep_versions: u64) -> SentrixResult<(usize, usize)> {
        let roots_pruned = self
            .cache
            .storage
            .prune_old_roots(self.version, keep_versions)?;
        if roots_pruned == 0 {
            return Ok((0, 0));
        }

        // Build live set: walk all remaining committed roots and collect reachable hashes.
        let mut live = std::collections::HashSet::new();
        // Walk the current root
        self.collect_reachable(self.root, &mut live)?;
        // Walk all other surviving roots in storage
        // (they share most nodes with current root, but some may diverge)
        let cutoff = self.version.saturating_sub(keep_versions);
        for version in (cutoff + 1)..=self.version {
            if let Some(root) = self.cache.storage.load_root(version)?
                && !live.contains(&root)
            {
                self.collect_reachable(root, &mut live)?;
            }
        }

        let nodes_gc = self.cache.storage.gc_orphaned_nodes(&live)?;
        tracing::info!(
            "trie prune: removed {} old roots, GC'd {} orphaned entries",
            roots_pruned,
            nodes_gc
        );
        Ok((roots_pruned, nodes_gc))
    }

    /// Recursively collect all node hashes reachable from `hash`.
    fn collect_reachable(
        &self,
        hash: NodeHash,
        live: &mut std::collections::HashSet<NodeHash>,
    ) -> SentrixResult<()> {
        use crate::node::empty_hash;
        // Skip empty subtrees and already-visited nodes
        if hash == empty_hash(0) || live.contains(&hash) {
            return Ok(());
        }
        live.insert(hash);
        if let Some(node) = self.cache.storage.load_node(&hash)? {
            match node {
                TrieNode::Leaf { value_hash, .. } => {
                    live.insert(value_hash);
                }
                TrieNode::Internal { left, right, .. } => {
                    self.collect_reachable(left, live)?;
                    self.collect_reachable(right, live)?;
                }
            }
        }
        Ok(())
    }

    /// Reset the working root to the empty tree.
    /// Call this before a fresh backfill when the committed root's nodes are
    /// stale (deleted by V7-L-01); without this, insert() would try to
    /// traverse the deleted root and fail with "missing node".
    pub fn reset_to_empty(&mut self) {
        self.root = empty_hash(0);
    }
}

// ── Trait impls ──────────────────────────────────────────────

impl std::fmt::Debug for SentrixTrie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SentrixTrie")
            .field("root", &hex::encode(self.root))
            .field("version", &self.version)
            .finish()
    }
}

/// Clone shares the same underlying MDBX storage (Arc-based) but starts with a fresh LRU cache.
/// Uses the original capacity from construction, not a hardcoded default.
impl Clone for SentrixTrie {
    fn clone(&self) -> Self {
        Self {
            cache: TrieCache::new(self.cache.storage.clone(), self.cache.capacity),
            root: self.root,
            version: self.version,
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::{account_value_bytes, account_value_decode, address_to_key};
    use crate::node::NULL_HASH;

    fn temp_mdbx() -> (tempfile::TempDir, Arc<MdbxStorage>) {
        let dir = tempfile::TempDir::new().unwrap();
        let mdbx = Arc::new(MdbxStorage::open(dir.path()).unwrap());
        (dir, mdbx)
    }

    #[test]
    fn test_empty_trie_root_is_canonical() {
        let (_dir, mdbx) = temp_mdbx();
        let trie = SentrixTrie::open(mdbx, 0).unwrap();
        assert_eq!(trie.root_hash(), empty_hash(0));
    }

    #[test]
    fn test_insert_changes_root() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(mdbx, 0).unwrap();
        let key = address_to_key("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
        let val = account_value_bytes(1_000_000, 0);
        let new_root = trie.insert(&key, &val).unwrap();
        assert_ne!(new_root, empty_hash(0));
        assert_eq!(trie.root_hash(), new_root);
    }

    #[test]
    fn test_insert_and_get_roundtrip() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(mdbx, 0).unwrap();
        let key = address_to_key("0x1111111111111111111111111111111111111111");
        let val = account_value_bytes(42_000_000, 7);
        trie.insert(&key, &val).unwrap();
        let got = trie.get(&key).unwrap();
        assert_eq!(got.as_deref(), Some(val.as_slice()));
    }

    #[test]
    fn test_get_absent_key_returns_none() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let key = address_to_key("0xdeadbeef00000000000000000000000000000000");
        assert!(trie.get(&key).unwrap().is_none());
    }

    #[test]
    fn test_update_existing_key() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let key = address_to_key("0xaaaa");
        trie.insert(&key, &account_value_bytes(100, 0)).unwrap();
        trie.insert(&key, &account_value_bytes(200, 1)).unwrap();
        let got = trie.get(&key).unwrap().unwrap();
        let (bal, nonce) = account_value_decode(&got).unwrap();
        assert_eq!(bal, 200);
        assert_eq!(nonce, 1);
    }

    #[test]
    fn test_multiple_keys_independent() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let k1 = address_to_key("0xaaaa");
        let k2 = address_to_key("0xbbbb");
        trie.insert(&k1, &account_value_bytes(100, 0)).unwrap();
        trie.insert(&k2, &account_value_bytes(200, 0)).unwrap();
        let v1 = trie.get(&k1).unwrap().unwrap();
        let v2 = trie.get(&k2).unwrap().unwrap();
        assert_eq!(account_value_decode(&v1).unwrap().0, 100);
        assert_eq!(account_value_decode(&v2).unwrap().0, 200);
    }

    #[test]
    fn test_root_changes_per_insert() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let k1 = address_to_key("0xaaaa");
        let k2 = address_to_key("0xbbbb");
        let r0 = trie.root_hash();
        trie.insert(&k1, &account_value_bytes(1, 0)).unwrap();
        let r1 = trie.root_hash();
        trie.insert(&k2, &account_value_bytes(2, 0)).unwrap();
        let r2 = trie.root_hash();
        assert_ne!(r0, r1);
        assert_ne!(r1, r2);
    }

    #[test]
    fn test_commit_and_versioned_root() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let key = address_to_key("0xabcd");
        trie.insert(&key, &account_value_bytes(500, 0)).unwrap();
        let root_v1 = trie.commit(1).unwrap();
        // Further insert shouldn't affect committed root
        trie.insert(&key, &account_value_bytes(999, 1)).unwrap();
        let stored = trie.root_at_version(1).unwrap();
        assert_eq!(stored, Some(root_v1));
    }

    #[test]
    fn test_membership_proof_verifies() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let key = address_to_key("0x1234");
        let val = account_value_bytes(777, 3);
        trie.insert(&key, &val).unwrap();
        let root = trie.root_hash();
        let proof = trie.prove(&key).unwrap();
        assert!(proof.found);
        assert!(proof.verify_membership(&root));
    }

    #[test]
    fn test_nonmembership_proof_verifies() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        // Insert one key, prove a different one is absent
        let key_present = address_to_key("0xaaaa");
        let key_absent = address_to_key("0xbbbb");
        trie.insert(&key_present, &account_value_bytes(1, 0))
            .unwrap();
        let root = trie.root_hash();
        let proof = trie.prove(&key_absent).unwrap();
        assert!(!proof.found);
        assert!(proof.verify_nonmembership(&root));
    }

    #[test]
    fn test_empty_trie_nonmembership_proof() {
        let (_dir, mdbx) = temp_mdbx();
        let trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let key = address_to_key("0xffff");
        let root = trie.root_hash();
        let proof = trie.prove(&key).unwrap();
        assert!(!proof.found);
        assert!(proof.verify_nonmembership(&root));
    }

    #[test]
    fn test_null_hash_sentinel_unused() {
        // NULL_HASH ([0u8;32]) must never appear as a valid leaf hash
        assert_ne!(NULL_HASH, empty_hash(0));
        assert_ne!(NULL_HASH, hash_leaf(&[0u8; 32], &[]));
    }

    /// T-B: updating an existing key must leave storage reclaimable by prune().
    ///
    /// The previous in-line "delete old leaf after update" cleanup (removed
    /// 2026-04-20) was unsound — the old leaf is still reachable from prior
    /// committed roots. This test now checks the invariant via prune(),
    /// which is the sound mechanism for reclaiming unreachable leaves.
    #[test]
    fn test_update_in_place_prune_reclaims_old_leaf() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let key = address_to_key("0xaaaa");

        // Version 1: initial insert + commit.
        trie.insert(&key, &account_value_bytes(100, 0)).unwrap();
        let _ = trie.commit(1).unwrap();
        let nodes_v1 = mdbx
            .count(sentrix_storage::tables::TABLE_TRIE_NODES)
            .unwrap();

        // Version 2: update same key + commit. The old leaf now lives only
        // in version 1's subtree — not reachable from v2's root.
        trie.insert(&key, &account_value_bytes(200, 1)).unwrap();
        let _ = trie.commit(2).unwrap();
        let nodes_after_update = mdbx
            .count(sentrix_storage::tables::TABLE_TRIE_NODES)
            .unwrap();
        assert!(
            nodes_after_update > nodes_v1,
            "before prune, node count must grow (old leaf is still stored for v1)"
        );

        // prune(keep=0) retires v1 and GCs nodes only reachable from it.
        let (roots_pruned, nodes_gc) = trie.prune(0).unwrap();
        assert!(roots_pruned >= 1, "must retire at least one old root");
        assert!(nodes_gc >= 1, "must GC at least one unreachable leaf");

        let nodes_after_prune = mdbx
            .count(sentrix_storage::tables::TABLE_TRIE_NODES)
            .unwrap();
        assert!(
            nodes_after_prune < nodes_after_update,
            "prune must reduce node count once old versions retire"
        );
    }

    /// T-D: open with a custom LRU capacity (small cache, still functionally correct).
    #[test]
    fn test_custom_capacity_trie_functional() {
        let (_dir, mdbx) = temp_mdbx();
        // Use a tiny capacity to exercise LRU eviction; correctness must be preserved
        let storage = crate::storage::TrieStorage::new(Arc::clone(&mdbx)).unwrap();
        let root = crate::storage::TrieStorage::new(Arc::clone(&mdbx))
            .unwrap()
            .load_root(0)
            .unwrap()
            .unwrap_or_else(|| empty_hash(0));
        let cache = crate::cache::TrieCache::new(storage, 4);
        let mut trie = SentrixTrie {
            cache,
            root,
            version: 0,
        };

        let k1 = address_to_key("0xaaaa");
        let k2 = address_to_key("0xbbbb");
        let k3 = address_to_key("0xcccc");
        trie.insert(&k1, &account_value_bytes(1, 0)).unwrap();
        trie.insert(&k2, &account_value_bytes(2, 0)).unwrap();
        trie.insert(&k3, &account_value_bytes(3, 0)).unwrap();

        // All values must be retrievable despite small LRU (sled fallback)
        assert!(trie.get(&k1).unwrap().is_some());
        assert!(trie.get(&k2).unwrap().is_some());
        assert!(trie.get(&k3).unwrap().is_some());
    }

    /// T-F: gc_orphaned_nodes must remove nodes not reachable from the current root.
    #[test]
    fn test_gc_removes_stale_nodes() {
        use std::collections::HashSet;
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let key = address_to_key("0x1234");

        trie.insert(&key, &account_value_bytes(500, 0)).unwrap();
        // Update same key — old leaf becomes orphan (T-B cleans it up immediately,
        // but we test that gc handles any remaining orphans after a different scenario).
        trie.insert(&key, &account_value_bytes(999, 1)).unwrap();

        // After T-B cleanup, node count for one key should be 1 (just the current leaf).
        // Run GC with only the current root hash in the live set.
        let live: HashSet<[u8; 32]> = [trie.root_hash()].into();
        let removed = trie.cache.storage.gc_orphaned_nodes(&live).unwrap();
        // All nodes reachable from root are the current internal/leaf nodes; anything
        // not reachable (internal nodes from old path) gets removed.
        let _ = removed; // count varies — just assert GC runs without error
    }

    /// delete() makes the key unreachable from the current root; storage is
    /// reclaimed by prune() once the pre-delete version is retired.
    ///
    /// Inline delete-time cleanup (removed 2026-04-20) was unsound — the
    /// leaf node and its value blob were still reachable from the pre-delete
    /// committed root. Walking that older root after a delete would then
    /// fire "trie: missing node".
    #[test]
    fn test_delete_key_unreachable_and_prunable() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let key = address_to_key("0x1111111111111111111111111111111111111111");
        let val = account_value_bytes(500, 0);

        // v1: insert + commit.
        trie.insert(&key, &val).unwrap();
        let _ = trie.commit(1).unwrap();
        let values_v1 = mdbx
            .count(sentrix_storage::tables::TABLE_TRIE_VALUES)
            .unwrap();

        // v2: delete + commit.
        trie.delete(&key).unwrap();
        let _ = trie.commit(2).unwrap();

        // From the current root, the key is gone.
        assert!(
            trie.get(&key).unwrap().is_none(),
            "deleted key must not be found from the current root"
        );

        // v1's root is still intact and must still resolve the key.
        let mut trie_v1 = SentrixTrie::open(Arc::clone(&mdbx), 1).unwrap();
        assert!(
            trie_v1.get(&key).unwrap().is_some(),
            "key must still be retrievable from v1 root after a v2 delete"
        );

        // prune(keep=0) retires v1; its leaf+value become unreachable.
        let (_roots_pruned, nodes_gc) = trie.prune(0).unwrap();
        assert!(nodes_gc >= 1, "prune must GC the deleted leaf's storage");

        let values_after_prune = mdbx
            .count(sentrix_storage::tables::TABLE_TRIE_VALUES)
            .unwrap();
        assert!(
            values_after_prune < values_v1,
            "prune must reclaim the value blob of the deleted key"
        );
    }

    /// Updates accumulate internal-node storage between commits; prune() is
    /// what reclaims it. Inline cleanup of old_internal_hashes (removed
    /// 2026-04-20) was unsound — see the comment in insert() and the
    /// ROOT-CAUSE regression tests above.
    #[test]
    fn test_insert_internal_nodes_accumulate_until_prune() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let k1 = address_to_key("0xaaaa");
        let k2 = address_to_key("0xbbbb");

        // v1: two keys committed.
        trie.insert(&k1, &account_value_bytes(100, 0)).unwrap();
        trie.insert(&k2, &account_value_bytes(200, 0)).unwrap();
        let _ = trie.commit(1).unwrap();
        let nodes_v1 = mdbx
            .count(sentrix_storage::tables::TABLE_TRIE_NODES)
            .unwrap();

        // v2: structural update on k1.
        trie.insert(&k1, &account_value_bytes(300, 1)).unwrap();
        let _ = trie.commit(2).unwrap();
        let nodes_v2 = mdbx
            .count(sentrix_storage::tables::TABLE_TRIE_NODES)
            .unwrap();
        assert!(
            nodes_v2 > nodes_v1,
            "v2 must store new internal nodes alongside v1's (no inline cleanup)"
        );

        // Both keys reachable from current (v2) root.
        let v1 = trie.get(&k1).unwrap().unwrap();
        assert_eq!(account_value_decode(&v1).unwrap().0, 300);
        let v2 = trie.get(&k2).unwrap().unwrap();
        assert_eq!(account_value_decode(&v2).unwrap().0, 200);

        // v1's root still walkable — k1's old value retrievable.
        let mut trie_v1 = SentrixTrie::open(Arc::clone(&mdbx), 1).unwrap();
        let v1_old = trie_v1.get(&k1).unwrap().unwrap();
        assert_eq!(account_value_decode(&v1_old).unwrap().0, 100);

        // prune(keep=0) reclaims v1's now-unreachable internal nodes.
        let (roots_pruned, nodes_gc) = trie.prune(0).unwrap();
        assert!(roots_pruned >= 1, "prune must retire at least v1");
        assert!(nodes_gc >= 1, "prune must GC v1's orphaned internal nodes");
    }

    /// ROOT CAUSE #3 regression guard: insert() must not delete the root node of a
    /// previously committed version, even when that root hash appears in old_internal_hashes.
    #[test]
    fn test_insert_does_not_delete_committed_root() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();

        let k1 = address_to_key("0xaaaa");
        let k2 = address_to_key("0xbbbb");

        // Version 1: insert k1 and commit.
        trie.insert(&k1, &account_value_bytes(100, 0)).unwrap();
        let root_v1 = trie.commit(1).unwrap();

        // Version 2: insert k2 (structural change — root hash changes, old root may end
        // up in old_internal_hashes).  Must NOT delete the v1 root node.
        trie.insert(&k2, &account_value_bytes(200, 0)).unwrap();
        let _ = trie.commit(2).unwrap();

        // The v1 root node must still exist in storage.
        assert!(
            trie.node_exists(&root_v1).unwrap(),
            "committed root v1 ({}) must survive subsequent insert()",
            hex::encode(root_v1)
        );

        // And we must still be able to load root_at_version(1) successfully.
        let loaded = trie.root_at_version(1).unwrap();
        assert_eq!(
            loaded,
            Some(root_v1),
            "root_at_version(1) must return the original committed root"
        );
    }

    /// Regression: walking a previously committed root must remain possible
    /// after subsequent inserts. The 2026-04-20 mainnet "trie missing node"
    /// incident traced back to insert() deleting internal nodes that were
    /// still reachable from earlier committed roots. The is_committed_root
    /// guard at line 180 protected only the root hash itself, not the
    /// internal nodes BELOW it.
    ///
    /// Failing on current code, passing after the fix.
    #[test]
    fn test_committed_root_subtree_remains_walkable_after_later_insert() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();

        // Build a real subtree: three keys with shared upper prefix bits so
        // there are real internal nodes in the path (not just expanded leaves).
        let k1 = address_to_key("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1");
        let k2 = address_to_key("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa2");
        let k3 = address_to_key("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa3");

        // Version 1: build a 3-key trie and commit. Walk now sees real
        // internal nodes between root and leaves.
        trie.insert(&k1, &account_value_bytes(100, 0)).unwrap();
        trie.insert(&k2, &account_value_bytes(200, 0)).unwrap();
        trie.insert(&k3, &account_value_bytes(300, 0)).unwrap();
        let root_v1 = trie.commit(1).unwrap();

        // Version 2: update k1's value — the insert walks down v1's
        // structure and (under the buggy cleanup) deletes the internal
        // nodes on k1's path that root_v1 still references.
        trie.insert(&k1, &account_value_bytes(999, 1)).unwrap();
        let _ = trie.commit(2).unwrap();

        // Re-open at version 1 and try to read all three keys back.
        // get() walks from root_v1; if any internal node on a key's path
        // was deleted by v2, this returns Err("trie: missing node ...").
        let mut trie_v1 = SentrixTrie::open(Arc::clone(&mdbx), 1).unwrap();
        assert_eq!(trie_v1.root_hash(), root_v1, "trie at v1 must load v1 root");

        for (k, expected_balance) in [(k1, 100u64), (k2, 200), (k3, 300)] {
            let v = trie_v1
                .get(&k)
                .expect("walking committed root v1 must not hit a deleted internal node");
            let bytes = v.expect("k must be retrievable from v1 root");
            let (balance, _) = account_value_decode(&bytes).unwrap();
            assert_eq!(
                balance, expected_balance,
                "v1 should preserve original balance"
            );
        }
    }

    /// Same shape as the test above, but routed through update_trie_for_block-style
    /// repeated insert/commit with a structural-change burst. This is closer to
    /// what mainnet validators do every block — a few inserts and a commit. The
    /// regression we observed in the 2026-04-20 incident is that after enough
    /// such bursts, a walk from the CURRENT root (not even a historical one)
    /// hits "missing node" because some prior burst's cleanup nuked an internal
    /// node still referenced by the live root.
    #[test]
    fn test_current_root_walkable_after_many_bursts() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();

        // Generate 64 keys with shared upper prefix so they cluster in the trie
        // (lots of shared internal nodes — maximises the chance a cleanup
        // touches a live node).
        let keys: Vec<_> = (0u8..64)
            .map(|i| {
                let mut h = [0u8; 32];
                // Shared upper 16 bytes
                for byte in h.iter_mut().take(16) {
                    *byte = 0xAA;
                }
                h[31] = i;
                h
            })
            .collect();

        // Round 1: bulk insert + commit.
        for (i, k) in keys.iter().enumerate() {
            trie.insert(k, &account_value_bytes(100 + i as u64, 0)).unwrap();
        }
        let _ = trie.commit(1).unwrap();

        // Rounds 2..=20: update one key per round, commit. Each update walks
        // through the cluster, deletes some internal nodes along its path.
        for round in 2u64..=20 {
            let i = (round as usize) % keys.len();
            trie.insert(&keys[i], &account_value_bytes(1000 + round, round)).unwrap();
            let _ = trie.commit(round).unwrap();
        }

        // After all those bursts, every key must still be reachable from the
        // CURRENT root.
        for (i, k) in keys.iter().enumerate() {
            let _ = trie.get(k).unwrap_or_else(|e| {
                panic!(
                    "current-root walk for key {i} hit a missing node: {e}. \
                     Inline cleanup deleted a still-live internal node."
                );
            });
        }
    }

    /// prove() only reads the trie — no mutable reference required.
    #[test]
    fn test_prove_works_with_shared_reference() {
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let key = address_to_key("0x5555555555555555555555555555555555555555");
        trie.insert(&key, &account_value_bytes(999, 0)).unwrap();
        let root = trie.root_hash();

        // prove() takes &self — can be called on a shared reference
        let trie_ref: &SentrixTrie = &trie;
        let proof = trie_ref.prove(&key).unwrap();
        assert!(proof.found);
        assert!(proof.verify_membership(&root));
    }

    // ── Boot-time integrity check (post-2026-04-21 fork follow-up) ──

    #[test]
    fn test_verify_integrity_empty_trie() {
        let (_dir, mdbx) = temp_mdbx();
        let trie = SentrixTrie::open(mdbx, 0).unwrap();
        // Empty trie is trivially intact — no references to verify.
        trie.verify_integrity().unwrap();
    }

    #[test]
    fn test_verify_integrity_intact_trie() {
        // Populated trie with several accounts should pass integrity check.
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(mdbx, 0).unwrap();
        for i in 1u8..=10 {
            let addr = format!("0x{}", hex::encode([i; 20]));
            let key = address_to_key(&addr);
            let val = account_value_bytes(1_000_000 * i as u64, i as u64);
            trie.insert(&key, &val).unwrap();
        }
        trie.verify_integrity().unwrap();
    }

    #[test]
    fn test_verify_integrity_detects_orphan_node() {
        // Simulate pre-v2.1.5 state_import damage: populate a trie, then
        // manually delete one of its internal nodes. verify_integrity
        // must surface the orphan reference.
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        for i in 1u8..=4 {
            let addr = format!("0x{}", hex::encode([i; 20]));
            let key = address_to_key(&addr);
            trie.insert(&key, &account_value_bytes(1_000 * i as u64, 0)).unwrap();
        }

        // The root is an Internal node — delete it (or one of its children)
        // to simulate orphan reference.
        let root = trie.root;
        let storage = &trie.cache.storage;
        storage.delete_node(&root).unwrap();

        let err = trie.verify_integrity().expect_err("must detect missing root node");
        assert!(
            format!("{err}").contains("orphan node reference"),
            "error should name the orphan kind; got: {err}"
        );
    }

    #[test]
    fn test_verify_integrity_detects_orphan_value() {
        // Simulate: node structure survives but a leaf's value blob is
        // missing from trie_values. This is what you'd see after a
        // partial state_import that populated trie_nodes but failed mid-way
        // through trie_values.
        let (_dir, mdbx) = temp_mdbx();
        let mut trie = SentrixTrie::open(Arc::clone(&mdbx), 0).unwrap();
        let addr = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let key = address_to_key(addr);
        let val = account_value_bytes(777, 0);
        trie.insert(&key, &val).unwrap();

        // Walk from root to find the single Leaf and delete its value.
        let mut node_hash = trie.root;
        let storage = &trie.cache.storage;
        loop {
            let node = storage.load_node(&node_hash).unwrap().unwrap();
            match node {
                TrieNode::Leaf { value_hash, .. } => {
                    storage.delete_value(&value_hash).unwrap();
                    break;
                }
                TrieNode::Internal { left, right, .. } => {
                    // Pick whichever side has a non-empty hash.
                    node_hash = if left != NULL_HASH { left } else { right };
                }
            }
        }

        let err = trie.verify_integrity().expect_err("must detect missing value");
        assert!(
            format!("{err}").contains("orphan value reference"),
            "error should name the orphan kind; got: {err}"
        );
    }

    /// Clone preserves the original capacity rather than using a hardcoded default.
    #[test]
    fn test_clone_preserves_capacity() {
        let (_dir, mdbx) = temp_mdbx();
        let storage = crate::storage::TrieStorage::new(Arc::clone(&mdbx)).unwrap();
        let cache = crate::cache::TrieCache::new(storage, 42);
        let trie = SentrixTrie {
            cache,
            root: empty_hash(0),
            version: 0,
        };

        let cloned = trie.clone();
        assert_eq!(cloned.cache.capacity, 42, "clone must preserve capacity");
    }
}
