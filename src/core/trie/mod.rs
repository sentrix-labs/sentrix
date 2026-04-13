// trie/mod.rs - Sentrix — Binary Sparse Merkle Tree module

pub mod address;
pub mod cache;
pub mod node;
pub mod proof;
pub mod storage;
pub mod tree;

pub use address::{address_to_key, account_value_bytes, account_value_decode};
pub use node::{NodeHash, TrieNode, NULL_HASH, empty_hash, hash_leaf, hash_internal, get_bit};
pub use proof::MerkleProof;
pub use tree::SentrixTrie;
