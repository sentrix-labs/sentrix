// trie/node.rs - Sentrix — TrieNode types and cryptographic hash functions

use std::sync::OnceLock;
use sha2::{Sha256, Digest};
use serde::{Serialize, Deserialize};

/// 32-byte content-addressed hash — the fundamental unit of the SMT.
pub type NodeHash = [u8; 32];

/// All-zeros sentinel: stored in parent slots that point to an empty subtree.
/// Distinct from any real hash (BLAKE3 of non-empty input is never all-zeros).
pub const NULL_HASH: NodeHash = [0u8; 32];

// ── Precomputed empty-subtree hashes ────────────────────────
// EMPTY_HASH[d] = hash of a completely empty subtree at depth d.
// EMPTY_HASH[256] = SHA-256([0u8;32])          (canonical empty-leaf sentinel)
// EMPTY_HASH[d]   = hash_internal(EMPTY_HASH[d+1], EMPTY_HASH[d+1])  for d < 256
static EMPTY_HASHES: OnceLock<Vec<NodeHash>> = OnceLock::new();

fn compute_empty_hashes() -> Vec<NodeHash> {
    // index in vec = depth (0 = root, 256 = leaf)
    let mut table = vec![NULL_HASH; 257];

    // Leaf sentinel: SHA-256 of 32 zero bytes
    table[256] = {
        let mut h = Sha256::new();
        h.update([0u8; 32]);
        h.finalize().into()
    };

    // Build upward: table[d] = hash_internal(table[d+1], table[d+1])
    for d in (0..256).rev() {
        table[d] = hash_internal_inner(&table[d + 1], &table[d + 1]);
    }
    table
}

/// Return the canonical hash for a completely empty subtree at `depth` (0–256).
/// Depth 0 = root level, depth 256 = single leaf level.
pub fn empty_hash(depth: usize) -> NodeHash {
    EMPTY_HASHES
        .get_or_init(compute_empty_hashes)
        .get(depth)
        .copied()
        .unwrap_or(NULL_HASH)
}

// ── Hash functions ───────────────────────────────────────────

/// Domain-separated internal-node hash: SHA-256(0x01 || left || right).
#[inline]
fn hash_internal_inner(left: &NodeHash, right: &NodeHash) -> NodeHash {
    let mut h = Sha256::new();
    h.update([0x01u8]);
    h.update(left);
    h.update(right);
    h.finalize().into()
}

/// Hash an internal SMT node from its two children.
pub fn hash_internal(left: &NodeHash, right: &NodeHash) -> NodeHash {
    hash_internal_inner(left, right)
}

/// Hash a leaf: BLAKE3(0x00 || key || value).
/// The 0x00 domain separator distinguishes leaf hashes from internal hashes.
pub fn hash_leaf(key: &[u8; 32], value: &[u8]) -> NodeHash {
    let mut h = blake3::Hasher::new();
    h.update(&[0x00u8]);
    h.update(key);
    h.update(value);
    *h.finalize().as_bytes()
}

/// Extract bit `i` from a 32-byte key, MSB-first.
/// Bit 0 = MSB of byte 0, bit 7 = LSB of byte 0, bit 8 = MSB of byte 1, etc.
#[inline]
pub fn get_bit(key: &[u8; 32], i: usize) -> bool {
    let byte_idx = i / 8;
    let bit_idx = 7 - (i % 8);
    (key[byte_idx] >> bit_idx) & 1 == 1
}

// ── TrieNode ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TrieNode {
    /// A short-circuit leaf that can appear at any depth (0–255).
    /// `value_hash` = BLAKE3(0x00 || key || value) — also the node's address in storage.
    Leaf {
        key: [u8; 32],
        value_hash: NodeHash,
    },
    /// An internal node with left (bit=0) and right (bit=1) children.
    /// `hash` = SHA-256(0x01 || left || right).
    Internal {
        left: NodeHash,
        right: NodeHash,
        hash: NodeHash,
    },
}

impl TrieNode {
    /// The hash that identifies this node in content-addressed storage.
    pub fn node_hash(&self) -> NodeHash {
        match self {
            TrieNode::Leaf { value_hash, .. } => *value_hash,
            TrieNode::Internal { hash, .. } => *hash,
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_hashes_cascade() {
        // empty_hash(256) must be SHA-256([0;32])
        let expected: NodeHash = {
            let mut h = Sha256::new();
            h.update([0u8; 32]);
            h.finalize().into()
        };
        assert_eq!(empty_hash(256), expected);

        // empty_hash(255) = hash_internal(empty_hash(256), empty_hash(256))
        let expected_255 = hash_internal(&expected, &expected);
        assert_eq!(empty_hash(255), expected_255);
    }

    #[test]
    fn test_hash_leaf_domain_separated() {
        let key = [1u8; 32];
        let h1 = hash_leaf(&key, b"hello");
        let h2 = hash_leaf(&key, b"world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_hash_internal_domain_separated() {
        // Internal hash must differ from leaf hash of same bytes
        let a = [1u8; 32];
        let b = [2u8; 32];
        let internal = hash_internal(&a, &b);
        let leaf = hash_leaf(&a, &b);
        assert_ne!(internal, leaf);
    }

    #[test]
    fn test_get_bit_msb_first() {
        let mut key = [0u8; 32];
        key[0] = 0b1000_0000; // bit 0 = 1
        assert!(get_bit(&key, 0));
        assert!(!get_bit(&key, 1));

        key[0] = 0b0000_0001; // bit 7 = 1
        assert!(!get_bit(&key, 0));
        assert!(get_bit(&key, 7));
    }

    #[test]
    fn test_null_hash_not_valid_leaf() {
        // hash_leaf must never produce NULL_HASH for any real input
        let leaf_hash = hash_leaf(&[0u8; 32], &[]);
        assert_ne!(leaf_hash, NULL_HASH);
    }
}
