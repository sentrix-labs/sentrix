// tables.rs — Table (named database) definitions for Sentrix chain storage.
//
// Each table has a specific key format and value encoding.
// Keys use big-endian u64 for ordered iteration (height scans).

/// Block storage: height (u64 BE) → Block (bincode)
pub const TABLE_BLOCKS: &str = "blocks";

/// Block hash index: block_hash (hex string bytes) → height (u64 BE)
pub const TABLE_BLOCK_HASHES: &str = "block_hashes";

/// Chain state: "state" → Blockchain (serde_json, for backward compat)
pub const TABLE_STATE: &str = "state";

/// Transaction index: tx_hash (hex string bytes) → block_height (u64 BE)
pub const TABLE_TX_INDEX: &str = "tx_index";

/// Trie nodes: node_hash (32 bytes) → TrieNode (bincode)
pub const TABLE_TRIE_NODES: &str = "trie_nodes";

/// Trie leaf values: leaf_key (32 bytes) → value bytes
pub const TABLE_TRIE_VALUES: &str = "trie_values";

/// Trie roots per height: height (u64 BE) → root_hash (32 bytes)
pub const TABLE_TRIE_ROOTS: &str = "trie_roots";

/// Trie committed roots: height (u64 BE) → root_hash (32 bytes)
pub const TABLE_TRIE_COMMITTED: &str = "trie_committed_roots";

/// Chain metadata: key string → value bytes (height, hash_index_complete, etc.)
pub const TABLE_META: &str = "meta";

/// All table names for pre-creation during environment open.
pub const ALL_TABLES: &[&str] = &[
    TABLE_BLOCKS,
    TABLE_BLOCK_HASHES,
    TABLE_STATE,
    TABLE_TX_INDEX,
    TABLE_TRIE_NODES,
    TABLE_TRIE_VALUES,
    TABLE_TRIE_ROOTS,
    TABLE_TRIE_COMMITTED,
    TABLE_META,
];
