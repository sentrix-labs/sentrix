// trie/tree.rs - Sentrix — Binary Sparse Merkle Tree (256-level, iterative)

use sled::Db;
use crate::core::trie::cache::TrieCache;
use crate::core::trie::node::{NodeHash, TrieNode, empty_hash, hash_leaf, hash_internal, get_bit};
use crate::core::trie::proof::MerkleProof;
use crate::core::trie::storage::TrieStorage;
use crate::types::error::{SentrixError, SentrixResult};

/// Binary Sparse Merkle Tree with 256 levels.
///
/// Properties:
/// - Keys: 32 bytes (256 bits) — derive from addresses via `address_to_key`
/// - Leaf hash:     BLAKE3(0x00 || key || value)
/// - Internal hash: SHA-256(0x01 || left || right)
/// - Short-circuit: a lone key in a subtree is stored as a leaf at that depth
/// - Persistent:    all nodes/values stored in sled; LRU cache in front
/// - Versioned:     each committed `version` (block height) maps to a root hash
pub struct SentrixTrie {
    cache: TrieCache,
    root: NodeHash,
    version: u64,
}

impl SentrixTrie {
    /// Open (or create) a trie backed by `db` at `version`.
    /// Loads the stored root for that version; uses the empty-tree root if none exists.
    pub fn open(db: &Db, version: u64) -> SentrixResult<Self> {
        let storage = TrieStorage::new(db)?;
        let root = storage
            .load_root(version)?
            .unwrap_or_else(|| empty_hash(0));
        let cache = TrieCache::new(storage);
        Ok(Self { cache, root, version })
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

            let node = self
                .cache
                .get_node(&current)?
                .ok_or_else(|| {
                    SentrixError::Internal(format!(
                        "trie: missing node {}",
                        hex::encode(current)
                    ))
                })?;

            match node {
                TrieNode::Leaf { key: leaf_key, value_hash: leaf_vh } => {
                    if leaf_key == *key {
                        // Same key — update in place; path already covers the descent.
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
                    let bit = get_bit(key, depth);
                    let (child, sibling) = if bit { (right, left) } else { (left, right) };
                    path.push((sibling, bit));
                    current = child;
                    depth += 1;
                }
            }
        }

        // Phase 2 — store the new leaf.
        let new_leaf = TrieNode::Leaf { key: *key, value_hash: new_value_hash };
        self.cache.put_node(new_value_hash, new_leaf)?;
        self.cache.store_value(&new_value_hash, value)?;

        // Phase 3 — walk UP recomputing internal hashes.
        let mut up_hash = new_value_hash;
        for (sibling, is_right) in path.iter().rev() {
            let (left, right) = if *is_right {
                (*sibling, up_hash)
            } else {
                (up_hash, *sibling)
            };
            up_hash = hash_internal(&left, &right);
            let node = TrieNode::Internal { left, right, hash: up_hash };
            self.cache.put_node(up_hash, node)?;
        }

        self.root = up_hash;
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

            let node = self
                .cache
                .get_node(&current)?
                .ok_or_else(|| {
                    SentrixError::Internal(format!(
                        "trie: missing node {}",
                        hex::encode(current)
                    ))
                })?;

            match node {
                TrieNode::Leaf { key: leaf_key, value_hash } => {
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

    /// Generate a Merkle proof (membership or non-membership) for `key`.
    pub fn prove(&mut self, key: &[u8; 32]) -> SentrixResult<MerkleProof> {
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
                TrieNode::Leaf { key: leaf_key, value_hash } => {
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

/// Clone shares the same underlying sled trees (Arc-based) but starts with a fresh LRU cache.
impl Clone for SentrixTrie {
    fn clone(&self) -> Self {
        Self {
            cache: TrieCache::new(self.cache.storage.clone()),
            root: self.root,
            version: self.version,
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::trie::node::NULL_HASH;
    use crate::core::trie::address::{address_to_key, account_value_bytes, account_value_decode};

    fn temp_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = sled::open(dir.path()).unwrap();
        (dir, db)
    }

    #[test]
    fn test_empty_trie_root_is_canonical() {
        let (_dir, db) = temp_db();
        let trie = SentrixTrie::open(&db, 0).unwrap();
        assert_eq!(trie.root_hash(), empty_hash(0));
    }

    #[test]
    fn test_insert_changes_root() {
        let (_dir, db) = temp_db();
        let mut trie = SentrixTrie::open(&db, 0).unwrap();
        let key = address_to_key("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
        let val = account_value_bytes(1_000_000, 0);
        let new_root = trie.insert(&key, &val).unwrap();
        assert_ne!(new_root, empty_hash(0));
        assert_eq!(trie.root_hash(), new_root);
    }

    #[test]
    fn test_insert_and_get_roundtrip() {
        let (_dir, db) = temp_db();
        let mut trie = SentrixTrie::open(&db, 0).unwrap();
        let key = address_to_key("0x1111111111111111111111111111111111111111");
        let val = account_value_bytes(42_000_000, 7);
        trie.insert(&key, &val).unwrap();
        let got = trie.get(&key).unwrap();
        assert_eq!(got.as_deref(), Some(val.as_slice()));
    }

    #[test]
    fn test_get_absent_key_returns_none() {
        let (_dir, db) = temp_db();
        let mut trie = SentrixTrie::open(&db, 0).unwrap();
        let key = address_to_key("0xdeadbeef00000000000000000000000000000000");
        assert!(trie.get(&key).unwrap().is_none());
    }

    #[test]
    fn test_update_existing_key() {
        let (_dir, db) = temp_db();
        let mut trie = SentrixTrie::open(&db, 0).unwrap();
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
        let (_dir, db) = temp_db();
        let mut trie = SentrixTrie::open(&db, 0).unwrap();
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
        let (_dir, db) = temp_db();
        let mut trie = SentrixTrie::open(&db, 0).unwrap();
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
        let (_dir, db) = temp_db();
        let mut trie = SentrixTrie::open(&db, 0).unwrap();
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
        let (_dir, db) = temp_db();
        let mut trie = SentrixTrie::open(&db, 0).unwrap();
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
        let (_dir, db) = temp_db();
        let mut trie = SentrixTrie::open(&db, 0).unwrap();
        // Insert one key, prove a different one is absent
        let key_present = address_to_key("0xaaaa");
        let key_absent  = address_to_key("0xbbbb");
        trie.insert(&key_present, &account_value_bytes(1, 0)).unwrap();
        let root = trie.root_hash();
        let proof = trie.prove(&key_absent).unwrap();
        assert!(!proof.found);
        assert!(proof.verify_nonmembership(&root));
    }

    #[test]
    fn test_empty_trie_nonmembership_proof() {
        let (_dir, db) = temp_db();
        let mut trie = SentrixTrie::open(&db, 0).unwrap();
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
}
