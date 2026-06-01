//! sentrix-trie — Binary Sparse Merkle Tree (256-level) with MDBX persistence.
//!
//! Provides:
//! - `SentrixTrie` — the main tree API (insert, get, prove, commit)
//! - `TrieStorage` — MDBX-backed persistent node storage
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
pub use address::{
    account_value_bytes, account_value_decode, address_to_key, liveness_value_bytes,
    liveness_value_decode, pending_rewards_value_bytes, pending_rewards_value_decode,
    total_minted_key, total_minted_value_bytes, total_minted_value_decode,
    validator_liveness_key, validator_pending_rewards_key,
};
pub use node::{NodeHash, TrieNode};
pub use proof::MerkleProof;
pub use tree::SentrixTrie;
