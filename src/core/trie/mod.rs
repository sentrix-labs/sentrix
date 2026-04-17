// trie/mod.rs — Re-export from sentrix-trie crate for backward compatibility.

pub use sentrix_trie::address;
pub use sentrix_trie::cache;
pub use sentrix_trie::node;
pub use sentrix_trie::proof;
pub use sentrix_trie::storage;
pub use sentrix_trie::tree;

pub use sentrix_trie::address::{account_value_bytes, account_value_decode, address_to_key};
pub use sentrix_trie::node::{NodeHash, TrieNode};
pub use sentrix_trie::node::{NULL_HASH, empty_hash, get_bit, hash_internal, hash_leaf};
pub use sentrix_trie::proof::MerkleProof;
pub use sentrix_trie::tree::SentrixTrie;
