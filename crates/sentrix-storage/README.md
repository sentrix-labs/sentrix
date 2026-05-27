# sentrix-storage

[![crates.io](https://img.shields.io/crates/v/sentrix-storage.svg)](https://crates.io/crates/sentrix-storage)
[![docs.rs](https://docs.rs/sentrix-storage/badge.svg)](https://docs.rs/sentrix-storage)

libmdbx-backed persistence layer for Sentrix Chain blocks, transactions, trie nodes, and metadata.

## What it's good for

A typed wrapper over raw libmdbx with batch / table semantics on top. Useful
beyond Sentrix for any Rust project that needs:
- Embedded ACID key-value storage with ordered iteration (same engine as Reth
  and Erigon — battle-tested under blockchain write loads)
- A familiar table → key → value layout without hand-rolling cursors and txn
  lifetimes every call site
- A drop-in replacement for `sled` workloads that outgrew its lack of proper
  transaction commit / rollback semantics

## Why this crate exists

The chain was originally on `sled`; libmdbx (used by Reth and Erigon) gives ACID
transactions with proper commit/rollback semantics, ordered B+ tree iteration, and
memory-mapped reads — important for a chain.db that grows past a few GB. This crate
wraps libmdbx in a typed `MdbxStorage` + `WriteBatch` API and defines the table
layout the rest of the workspace persists into.

[sentrix-trie](../sentrix-trie) writes its node + leaf + root tables here;
[sentrix-core](../sentrix-core) uses `ChainStorage` for blocks, hash→height index,
tx index, and the metadata key-value pairs that drive recovery.

## Usage

```toml
[dependencies]
sentrix-storage = { path = "../sentrix-storage" }
```

```rust
use std::path::Path;
use sentrix_storage::{MdbxStorage, ChainStorage, height_key, key_to_height};

// MdbxStorage is the raw typed wrapper around the libmdbx env;
// ChainStorage layers block-aware semantics on top of it.
let storage = MdbxStorage::open(Path::new("data/chain.db"))?;

// Write transaction — atomic across multiple `put` / `delete` calls.
let batch = storage.begin_write()?;
batch.put("trie_nodes", &node_hash, &serialized_node)?;
batch.commit()?;

// Read path (auto-managed RO txn per call).
if let Some(bytes) = storage.get("trie_nodes", &node_hash)? {
    // ...
}
```

## Tables

| Table | Key | Value |
|---|---|---|
| `blocks` | height (BE u64) | `Block` (bincode) |
| `block_hashes` | hash | height |
| `state` | name | chain state JSON (backward compat) |
| `tx_index` | tx_hash | block_height |
| `trie_nodes` | node_hash | `TrieNode` |
| `trie_values` | leaf_key | value bytes |
| `trie_roots` | height | root_hash |
| `trie_committed_roots` | height | root_hash |
| `meta` | name | metadata bytes |

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual-licensed as above, without any additional terms or conditions.
