//! sentrix-storage — libmdbx-based storage layer for Sentrix blockchain.
//!
//! Replaces sled with libmdbx (used by Reth/Erigon) for better performance:
//! - ACID transactions (proper commit/rollback)
//! - Ordered iteration via B+ tree
//! - Memory-mapped I/O (zero-copy reads)
//! - Battle-tested in production blockchains
//!
//! # Tables
//!
//! - `blocks`: height → Block (bincode)
//! - `block_hashes`: hash → height
//! - `state`: chain state (JSON, backward compat)
//! - `tx_index`: tx_hash → block_height
//! - `trie_nodes`: node_hash → TrieNode
//! - `trie_values`: leaf_key → value
//! - `trie_roots`: height → root_hash
//! - `trie_committed_roots`: height → root_hash
//! - `meta`: metadata key-value pairs

#![allow(missing_docs)]

pub mod chain;
pub mod error;
pub mod mdbx;
pub mod tables;

pub use chain::ChainStorage;
pub use error::{StorageError, StorageResult};
pub use mdbx::{MdbxStorage, WriteBatch, height_key, key_to_height};
pub use tables::*;
