# State Management

Two storage layers: in-memory sliding window (fast, last 1000 blocks) and sled (durable, everything since genesis). Account state goes through a Binary Sparse Merkle Tree that produces a verifiable root per block.

## Sliding Window

```rust
const CHAIN_WINDOW_SIZE: usize = 1000;
```

Only the last 1,000 blocks live in `Vec<Block>`. Older blocks are in sled. RAM stays at ~2 MB regardless of chain height.

## sled

Pure Rust embedded DB. Crash-safe, atomic writes, no external dependencies.

| Key | Value |
|-----|-------|
| `state` | Full chain state (accounts, validators, mempool, contracts) |
| `block:{index}` | Individual block |
| `hash:{hash}` | Block index (O(1) lookup by hash) |
| `height` | Current height |

## SentrixTrie

256-level Binary Sparse Merkle Tree for account state. Every block (height ≥ 100,000) gets a state root stamped into its hash.

### How addresses map to trie paths

```
address → strip 0x → lowercase → hex decode → SHA-256 → 256-bit path
```

Each bit = left (0) or right (1) traversal. Short-circuit leaves — if a subtree has one value, the leaf sits at the highest unique prefix instead of depth 256.

### Hashing

| Node | Hash |
|------|------|
| Leaf | `BLAKE3(0x00 ∥ key ∥ value)` |
| Internal | `SHA-256(0x01 ∥ left ∥ right)` |

The `0x00`/`0x01` prefix + different hash algorithms = a leaf can never collide with an internal node.

### Storage

4 sled named trees:

| Tree | Contents |
|------|----------|
| `trie_nodes` | Node data keyed by hash |
| `trie_values` | Account values keyed by path |
| `trie_roots` | State root per block height |
| `trie_committed_roots` | Reverse index for fast committed root lookup |

LRU cache sits on top of sled. Configurable capacity.

### Committed Root Protection

When a root is committed via `store_root()`, its hash goes into `trie_committed_roots`. During subsequent inserts, `is_committed_root()` is checked before deleting any old node — committed roots never get garbage-collected. Without this, inserting new accounts could accidentally delete nodes that belong to a previous block's state root.

### Merkle Proofs

```rust
let proof = trie.prove(&key)?;
let valid = proof.verify(&key, &value, &root);  // no trie access needed
```

Available via `GET /trie/proof/{address}`.

### Disk Pruning

The trie supports automatic disk pruning to prevent unbounded growth:

```rust
trie.prune(1000)?;  // keep last 1000 versions, GC the rest
```

Pruning steps:
1. Delete old root entries from `trie_roots` and `trie_committed_roots` for versions older than `(current - keep)`
2. Walk all surviving roots to build a live-hash set of reachable nodes
3. Garbage-collect any node/value not in the live set

Default retention: 1000 versions (configurable). Should be called periodically (e.g. every 100 blocks) in the block production loop.

### GC

Orphaned nodes can pile up. Clean them with:

```bash
sentrix chain reset-trie  # rebuilds from current account state
```

Or use the automatic pruning above which handles GC as part of the prune cycle.

### Fork Height

```rust
const STATE_ROOT_FORK_HEIGHT: u64 = 100_000;
```

Below this height, state root isn't in the block hash (backward compat with pre-trie blocks). At and above, it's part of the hash chain.

## Account State

```rust
struct Account {
    balance: u64,  // sentri
    nonce: u64,
}
```

All balance ops use `checked_add`/`checked_sub`. No floats anywhere.

Fee split on transfer: sender pays `amount + fee`. Receiver gets `amount`. Burn gets `ceil(fee/2)`. Validator gets `floor(fee/2)`. Odd sentri goes to burn side.
