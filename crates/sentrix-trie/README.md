# sentrix-trie

[![crates.io](https://img.shields.io/crates/v/sentrix-trie.svg)](https://crates.io/crates/sentrix-trie)
[![docs.rs](https://docs.rs/sentrix-trie/badge.svg)](https://docs.rs/sentrix-trie)

256-level Binary Sparse Merkle Tree with MDBX persistence for Sentrix Chain state.

## What it's good for

Authenticated key-value state with inclusion / non-inclusion proofs. Useful
beyond Sentrix for any Rust project needing one of:
- L1 / L2 / app-chain account or storage trees committed to a root hash
- zk-rollup state commitments with verifiable proofs
- Airdrop / claim systems that publish a root and serve per-leaf proofs
- Any content-addressed structure where a verifier wants to confirm membership
  without re-reading the full dataset

## Why this crate exists

The chain commits `state_root` into every block header, so the block executor needs
an authenticated key-value store that can produce inclusion proofs (for
`eth_getProof`) and recompute deterministic roots after each transaction. The BSMT
implementation here writes nodes through to [sentrix-storage](../sentrix-storage) and
keeps a hot LRU in front (`TrieCache`) so the trie hot path doesn't hammer mdbx for
every read.

Used by [sentrix-core](../sentrix-core) for account state writeback, and by
[sentrix-rpc](../sentrix-rpc) for `eth_getProof`. Criterion microbenches under
`benches/trie_insert.rs` gate consensus-touching PRs against the main-branch baseline.

## Usage

```toml
[dependencies]
sentrix-trie = { path = "../sentrix-trie" }
```

```rust
use std::path::Path;
use std::sync::Arc;
use sentrix_storage::MdbxStorage;
use sentrix_trie::{SentrixTrie, MerkleProof, account_value_bytes, address_to_key};

// SentrixTrie is the main API — insert/get/prove/commit. Keys are 256-bit
// (typically hashed addresses or storage slots); values are arbitrary bytes.
// `version` is the height / commit counter the trie writes nodes under.
let mdbx = Arc::new(MdbxStorage::open(Path::new("data/chain.db"))?);
let mut trie = SentrixTrie::open(mdbx, 0)?;          // (storage, version)
let key = address_to_key("0x...");
let value = account_value_bytes(/* Account */);
trie.insert(&key, &value)?;
let root = trie.commit(1)?;                          // next version

// MerkleProof generates inclusion / non-inclusion proofs for the RPC layer.
// `verify_membership(&root)` checks the proof matches the trie that committed
// to `root`; `verify_nonmembership(&root)` proves a key is absent.
let proof: MerkleProof = trie.prove(&key)?;
assert!(proof.verify_membership(&root));
```

Key re-exports: `SentrixTrie`, `TrieNode`, `NodeHash`, `MerkleProof`,
`account_value_bytes`, `account_value_decode`, `address_to_key`.

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual-licensed as above, without any additional terms or conditions.
