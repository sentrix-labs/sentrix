//! sentrix-trie — Binary Sparse Merkle Tree (256-level) with sled persistence.
//!
//! Provides:
//! - `SentrixTrie` — the main tree API (insert, get, prove, commit)
//! - `TrieStorage` — sled-backed persistent node storage
//! - `TrieCache` — LRU cache in front of TrieStorage
//! - `MerkleProof` — inclusion proof generation + verification
//! - Address helpers for account state encoding

#![allow(missing_docs)]

pub mod address;
pub mod cache;
pub mod node;
pub mod proof;
pub mod storage;
pub mod tree;

// Re-export commonly used types
pub use tree::SentrixTrie;
pub use address::{account_value_bytes, account_value_decode, address_to_key};
pub use node::{NodeHash, TrieNode};
pub use proof::MerkleProof;
