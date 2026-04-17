// trie/proof.rs - Sentrix — Merkle proof types and verification

use crate::node::{NodeHash, get_bit, hash_internal, hash_leaf};
use serde::{Deserialize, Serialize};

/// A Merkle proof for a key in the SentrixTrie.
///
/// Covers both membership (found=true) and non-membership (found=false) cases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MerkleProof {
    /// 32-byte trie key (SHA-256 of address bytes)
    pub key: [u8; 32],
    /// Raw account-state bytes (non-empty iff found=true)
    pub value: Vec<u8>,
    /// Sibling hash at each level, from the root down to `depth-1`.
    /// `siblings[i]` is the sibling of the node at depth `i`.
    pub siblings: Vec<NodeHash>,
    /// Number of levels traversed before reaching the terminal node.
    pub depth: usize,
    /// True → membership proof; false → non-membership proof.
    pub found: bool,
    /// The hash at the terminal position in the tree.
    /// - Membership   : hash_leaf(key, value)
    /// - Non-membership hit empty: empty_hash(depth)
    /// - Non-membership hit other leaf: that leaf's value_hash
    pub terminal_hash: NodeHash,
}

impl MerkleProof {
    /// Verify a membership proof against `root`.
    pub fn verify_membership(&self, root: &NodeHash) -> bool {
        if !self.found {
            return false;
        }
        if self.siblings.len() != self.depth {
            return false;
        }
        let leaf_hash = hash_leaf(&self.key, &self.value);
        self.compute_root(leaf_hash) == *root
    }

    /// Verify a non-membership proof against `root`.
    pub fn verify_nonmembership(&self, root: &NodeHash) -> bool {
        if self.found {
            return false;
        }
        if self.siblings.len() != self.depth {
            return false;
        }
        self.compute_root(self.terminal_hash) == *root
    }

    /// Reconstruct the root hash by walking the path upward from `leaf_hash`.
    fn compute_root(&self, leaf_hash: NodeHash) -> NodeHash {
        let mut current = leaf_hash;
        for d in (0..self.depth).rev() {
            let bit = get_bit(&self.key, d);
            let (left, right) = if bit {
                (self.siblings[d], current)
            } else {
                (current, self.siblings[d])
            };
            current = hash_internal(&left, &right);
        }
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{empty_hash, hash_leaf};

    #[test]
    fn test_membership_proof_verify() {
        let key = [42u8; 32];
        let value = b"test_value";
        let leaf_hash = hash_leaf(&key, value);

        // Single-level proof: root = hash_internal(sibling, leaf_hash)
        let sibling = [99u8; 32];
        // key bit 0 = MSB of key[0] = (42 >> 7) & 1 = 0 → goes left → sibling is right
        let bit0 = 42u8 >= 128; // false — MSB of 42 is 0
        let root = if bit0 {
            hash_internal(&sibling, &leaf_hash)
        } else {
            hash_internal(&leaf_hash, &sibling)
        };

        let proof = MerkleProof {
            key,
            value: value.to_vec(),
            siblings: vec![sibling],
            depth: 1,
            found: true,
            terminal_hash: leaf_hash,
        };
        assert!(proof.verify_membership(&root));
        assert!(!proof.verify_nonmembership(&root));
    }

    #[test]
    fn test_nonmembership_empty_proof_verify() {
        let key = [0u8; 32];
        let depth = 2usize;
        let terminal = empty_hash(depth);

        // Build a path of empty siblings → the whole subtree is empty
        let sibling0 = empty_hash(1);
        let sibling1 = empty_hash(2);
        // Reconstruct root manually
        let mut h = terminal;
        // d=1 first (rev order: d=1, d=0)
        let bit1 = get_bit(&key, 1);
        h = if bit1 {
            hash_internal(&sibling1, &h)
        } else {
            hash_internal(&h, &sibling1)
        };
        let bit0 = get_bit(&key, 0);
        let root = if bit0 {
            hash_internal(&sibling0, &h)
        } else {
            hash_internal(&h, &sibling0)
        };

        let proof = MerkleProof {
            key,
            value: Vec::new(),
            siblings: vec![sibling0, sibling1],
            depth,
            found: false,
            terminal_hash: terminal,
        };
        assert!(proof.verify_nonmembership(&root));
        assert!(!proof.verify_membership(&root));
    }
}
