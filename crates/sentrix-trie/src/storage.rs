// trie/storage.rs - Sentrix — Persistent MDBX-backed trie storage

use crate::node::{NodeHash, TrieNode};
use sentrix_primitives::{SentrixError, SentrixResult};
use sentrix_storage::{MdbxStorage, tables};
use std::sync::Arc;

/// Low-level persistent storage for trie nodes, values, and version→root mappings.
///
/// Four MDBX tables (same logical layout as the old sled trees):
/// - `trie_nodes`           : NodeHash → bincode(TrieNode)
/// - `trie_values`          : NodeHash → raw account-state bytes
/// - `trie_roots`           : version u64 BE → NodeHash
/// - `trie_committed_roots` : NodeHash → version u64 BE (reverse index for O(1) is_committed_root)
///
/// `Clone` is cheap — `Arc<MdbxStorage>` is reference-counted.
#[derive(Clone)]
pub struct TrieStorage {
    mdbx: Arc<MdbxStorage>,
}

impl TrieStorage {
    /// Open trie storage backed by the given MdbxStorage.
    /// On first open (migration), backfills `trie_committed_roots` from `trie_roots`.
    pub fn new(mdbx: Arc<MdbxStorage>) -> SentrixResult<Self> {
        let storage = Self { mdbx };
        storage.ensure_committed_roots_index()?;
        Ok(storage)
    }

    /// Backfill `trie_committed_roots` from `trie_roots` if the reverse index is absent.
    /// O(n_blocks) one-time cost on migration; O(1) fast-path on all subsequent opens.
    fn ensure_committed_roots_index(&self) -> SentrixResult<()> {
        // Fast path: sentinel present means the index is already complete.
        if self
            .mdbx
            .has(tables::TABLE_TRIE_COMMITTED, b"__ready__")
            .map_err(|e| SentrixError::StorageError(e.to_string()))?
        {
            return Ok(());
        }

        // Slow path: scan trie_roots and populate the reverse index.
        let entries = self
            .mdbx
            .iter(tables::TABLE_TRIE_ROOTS)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        let mut any = false;
        for (k, v) in &entries {
            if v.len() == 32 {
                self.mdbx
                    .put(tables::TABLE_TRIE_COMMITTED, v, k)
                    .map_err(|e| SentrixError::StorageError(e.to_string()))?;
                any = true;
            }
        }

        if any {
            self.mdbx
                .put(tables::TABLE_TRIE_COMMITTED, b"__ready__", b"1")
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        }
        Ok(())
    }

    // ── Nodes ─────────────────────────────────────────────

    pub fn store_node(&self, hash: &NodeHash, node: &TrieNode) -> SentrixResult<()> {
        let bytes = bincode::serialize(node)
            .map_err(|e| SentrixError::SerializationError(e.to_string()))?;
        self.mdbx
            .put(tables::TABLE_TRIE_NODES, hash, &bytes)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    /// Flush a buffer of (node, value) writes plus the new root + reverse-index
    /// entry in a single MDBX transaction. Replaces the per-node + per-value
    /// individual `put()` calls that opened a fresh MDBX transaction each —
    /// ~2560 writes per typical mainnet block × ~150 µs per transaction
    /// = the bulk of the trie phase in `apply_profile`. Batched, the same
    /// 2560 puts share one transaction and one fsync.
    ///
    /// Atomicity is critical: the root advance must land in the same MDBX
    /// commit as the node/value writes it references, otherwise a crash
    /// between flushing nodes and storing the root would leave us with a
    /// fresh root pointing into the previous block's node set on next boot.
    /// Single tx fixes this by construction.
    pub fn flush_trie_batch(
        &self,
        pending_nodes: &[(NodeHash, TrieNode)],
        pending_values: &[(NodeHash, Vec<u8>)],
        version: u64,
        root: &NodeHash,
    ) -> SentrixResult<()> {
        let batch = self
            .mdbx
            .begin_write()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        for (hash, node) in pending_nodes {
            let bytes = bincode::serialize(node)
                .map_err(|e| SentrixError::SerializationError(e.to_string()))?;
            batch
                .put(tables::TABLE_TRIE_NODES, hash, &bytes)
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        }
        for (hash, value) in pending_values {
            batch
                .put(tables::TABLE_TRIE_VALUES, hash, value)
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        }
        batch
            .put(
                tables::TABLE_TRIE_ROOTS,
                &version.to_be_bytes(),
                root.as_slice(),
            )
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        batch
            .put(
                tables::TABLE_TRIE_COMMITTED,
                root.as_slice(),
                &version.to_be_bytes(),
            )
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        batch
            .commit()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    pub fn load_node(&self, hash: &NodeHash) -> SentrixResult<Option<TrieNode>> {
        match self
            .mdbx
            .get(tables::TABLE_TRIE_NODES, hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?
        {
            Some(bytes) => {
                let node = bincode::deserialize::<TrieNode>(&bytes)
                    .map_err(|e| SentrixError::SerializationError(e.to_string()))?;
                Ok(Some(node))
            }
            None => Ok(None),
        }
    }

    /// Remove a node entry from persistent storage (called when a leaf is replaced).
    pub fn delete_node(&self, hash: &NodeHash) -> SentrixResult<()> {
        self.mdbx
            .delete(tables::TABLE_TRIE_NODES, hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    // ── Values ────────────────────────────────────────────

    pub fn store_value(&self, hash: &NodeHash, value: &[u8]) -> SentrixResult<()> {
        self.mdbx
            .put(tables::TABLE_TRIE_VALUES, hash, value)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    pub fn load_value(&self, hash: &NodeHash) -> SentrixResult<Option<Vec<u8>>> {
        self.mdbx
            .get(tables::TABLE_TRIE_VALUES, hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    /// Remove a value blob from persistent storage (called when a leaf is replaced).
    pub fn delete_value(&self, hash: &NodeHash) -> SentrixResult<()> {
        self.mdbx
            .delete(tables::TABLE_TRIE_VALUES, hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    // ── Roots ─────────────────────────────────────────────

    pub fn store_root(&self, version: u64, root: &NodeHash) -> SentrixResult<()> {
        self.mdbx
            .put(
                tables::TABLE_TRIE_ROOTS,
                &version.to_be_bytes(),
                root.as_slice(),
            )
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        // Maintain reverse index: NodeHash → version (O(1) is_committed_root lookups).
        self.mdbx
            .put(
                tables::TABLE_TRIE_COMMITTED,
                root.as_slice(),
                &version.to_be_bytes(),
            )
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        if !self
            .mdbx
            .has(tables::TABLE_TRIE_COMMITTED, b"__ready__")
            .unwrap_or(false)
        {
            let _ = self
                .mdbx
                .put(tables::TABLE_TRIE_COMMITTED, b"__ready__", b"1");
        }
        Ok(())
    }

    pub fn load_root(&self, version: u64) -> SentrixResult<Option<NodeHash>> {
        match self
            .mdbx
            .get(tables::TABLE_TRIE_ROOTS, &version.to_be_bytes())
            .map_err(|e| SentrixError::StorageError(e.to_string()))?
        {
            Some(bytes) if bytes.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&bytes);
                Ok(Some(arr))
            }
            Some(_) => Err(SentrixError::StorageError(
                "corrupt trie root: wrong byte length".to_string(),
            )),
            None => Ok(None),
        }
    }

    /// Check whether `hash` is currently recorded as a committed root for any version.
    /// O(1) via `trie_committed_roots` reverse index.
    pub fn is_committed_root(&self, hash: &NodeHash) -> SentrixResult<bool> {
        self.mdbx
            .has(tables::TABLE_TRIE_COMMITTED, hash.as_slice())
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    /// Highest committed version currently in `TABLE_TRIE_ROOTS`.
    /// Returns `None` if the table is empty.
    ///
    /// Used by `SentrixTrie::prune` to augment the live-set walk with
    /// roots that were committed AFTER the cloned trie snapshot was
    /// taken — a critical race-window closer when prune runs in a
    /// background thread (per `maybe_prune_trie` at
    /// `crates/sentrix-core/src/blockchain_trie_ops.rs:555`).
    ///
    /// Cost: one cursor walk over `TABLE_TRIE_ROOTS`. The table holds at
    /// most `TRIE_KEEP_VERSIONS` entries (default ~1000), so this is
    /// O(1000) in practice — milliseconds. We could use `last()` on the
    /// cursor if the storage layer exposed it, but a forward scan is
    /// good enough and matches the existing `prune_old_roots` access
    /// pattern below.
    pub fn latest_version(&self) -> SentrixResult<Option<u64>> {
        let mut max_version: Option<u64> = None;
        self.mdbx
            .iter_from(tables::TABLE_TRIE_ROOTS, &[], |k, _v| {
                if k.len() == 8 {
                    let mut buf = [0u8; 8];
                    buf.copy_from_slice(k);
                    let v = u64::from_be_bytes(buf);
                    max_version = Some(max_version.map_or(v, |old| old.max(v)));
                }
                true
            })
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(max_version)
    }

    /// Prune old trie roots, keeping only the last `keep` versions.
    ///
    /// Walks `TABLE_TRIE_ROOTS` via a streaming cursor (`iter_from`) so the
    /// full table never lands in a `Vec<(Vec<u8>, Vec<u8>)>` — see the
    /// `iter_from` callsite rationale; same class of fix as PR #575 for
    /// the logs/receipts paths.
    pub fn prune_old_roots(&self, latest_version: u64, keep: u64) -> SentrixResult<usize> {
        if latest_version <= keep {
            return Ok(0);
        }
        let cutoff = latest_version - keep;

        let mut to_delete: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        self.mdbx
            .iter_from(tables::TABLE_TRIE_ROOTS, &[], |k, v| {
                if k.len() == 8 {
                    let mut buf = [0u8; 8];
                    buf.copy_from_slice(k);
                    let version = u64::from_be_bytes(buf);
                    if version <= cutoff {
                        to_delete.push((k.to_vec(), v.to_vec()));
                    }
                }
                true
            })
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        let mut removed = 0usize;
        for (key, root_hash) in &to_delete {
            self.mdbx
                .delete(tables::TABLE_TRIE_ROOTS, key.as_slice())
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
            if root_hash.len() == 32 {
                self.mdbx
                    .delete(tables::TABLE_TRIE_COMMITTED, root_hash.as_slice())
                    .map_err(|e| SentrixError::StorageError(e.to_string()))?;
            }
            removed += 1;
        }

        Ok(removed)
    }

    /// Garbage-collect node and value entries not present in `live_hashes`.
    ///
    /// Prefer [`Self::gc_nodes`] + [`Self::gc_values`] for the periodic prune:
    /// the nodes pass alone can run 10–20 min on a big chain.db, during which
    /// the apply loop commits new leaf VALUES. Driving both passes from one
    /// `live` snapshot lets the values pass delete those freshly-committed
    /// (still-live) values — the 2026-06-04 testnet trie corruption. The split
    /// methods let the caller refresh `live` between passes. This combined
    /// method stays for non-racy callers (one-shot recovery / tests).
    pub fn gc_orphaned_nodes(
        &self,
        live_hashes: &std::collections::HashSet<NodeHash>,
    ) -> SentrixResult<usize> {
        let node_count = self.gc_nodes(live_hashes)?;
        let value_count = self.gc_values(live_hashes)?;
        Ok(node_count + value_count)
    }

    /// GC only the trie_nodes table against `live_hashes`. See
    /// [`Self::gc_orphaned_nodes`] for why nodes and values are split.
    pub fn gc_nodes(
        &self,
        live_hashes: &std::collections::HashSet<NodeHash>,
    ) -> SentrixResult<usize> {
        self.gc_table(tables::TABLE_TRIE_NODES, live_hashes)
    }

    /// GC only the trie_values table against `live_hashes`. The caller MUST
    /// refresh `live_hashes` with any roots committed since the nodes pass
    /// before calling this, or it will delete leaf values written during the
    /// (long) nodes pass.
    pub fn gc_values(
        &self,
        live_hashes: &std::collections::HashSet<NodeHash>,
    ) -> SentrixResult<usize> {
        self.gc_table(tables::TABLE_TRIE_VALUES, live_hashes)
    }

    /// Generational GC for the trie_nodes table — see [`Self::gc_table_generational`].
    pub fn gc_nodes_generational(
        &self,
        live_hashes: &std::collections::HashSet<NodeHash>,
        version: u64,
    ) -> SentrixResult<usize> {
        self.gc_table_generational(tables::TABLE_TRIE_NODES, b'n', live_hashes, version)
    }

    /// Generational GC for the trie_values table — see [`Self::gc_table_generational`].
    pub fn gc_values_generational(
        &self,
        live_hashes: &std::collections::HashSet<NodeHash>,
        version: u64,
    ) -> SentrixResult<usize> {
        self.gc_table_generational(tables::TABLE_TRIE_VALUES, b'v', live_hashes, version)
    }

    /// Race-free generational GC: defer deletes by one prune cycle via
    /// `TABLE_TRIE_TOMBSTONES` (keyed `disc || hash` → tombstone version u64 BE).
    ///
    /// The old `gc_table` deleted any hash not in the live-set snapshot. But the
    /// snapshot is frozen when the background prune is spawned, and blocks keep
    /// committing new (live) nodes/values during the multi-minute walk — those
    /// aren't in the snapshot, so they were deleted as "orphans" (the recurring
    /// "missing node" stalls; #791 only narrowed the window).
    ///
    /// Generational fix:
    /// - **Phase A (reap):** for each existing tombstone of this `disc` —
    ///   live again → drop tombstone (false orphan, keep entry); still orphan
    ///   AND tombstoned in an earlier cycle (`tv < version`) → delete entry +
    ///   tombstone; tombstoned this cycle → leave.
    /// - **Phase B (mark):** tombstone any orphan not already tombstoned, at
    ///   `version`.
    ///
    /// A hash committed DURING this prune is orphan vs the snapshot, so Phase B
    /// tombstones it — but next prune it's a recent live node, so Phase A drops
    /// the tombstone instead of deleting it. Worst-case failure mode is benign:
    /// under-deletion (storage grows), never deletion of a live entry. Returns
    /// the count actually deleted this cycle.
    ///
    /// CALLER CONTRACT: `live_hashes` must be augmented to the latest committed
    /// root IMMEDIATELY before this call (`tree.rs::prune` re-walks the on-disk
    /// roots into `live` just before each gc pass). That collapses the reap-vs-
    /// commit window to this method's own scan+delete duration (ms — the
    /// tombstone table is small), versus the old immediate-delete that raced the
    /// whole multi-minute walk. RESIDUAL (CodeRabbit, 2026-06-06): a
    /// content-addressed hash re-committed inside that ms window can still be
    /// reaped here; fully eliminating it needs the reap coupled to writer
    /// synchronization (walk+delete in one RW txn) — the tracked complete fix
    /// this PR deliberately defers to avoid a 10–20 min apply-blocking write
    /// lock. This change strictly narrows the race; it does not pretend to close
    /// it. Boot-time verify_integrity remains the backstop.
    fn gc_table_generational(
        &self,
        data_table: &str,
        disc: u8,
        live_hashes: &std::collections::HashSet<NodeHash>,
        version: u64,
    ) -> SentrixResult<usize> {
        use std::collections::HashSet;

        // Phase A — scan existing tombstones for this discriminator.
        let mut reap: Vec<NodeHash> = Vec::new(); // still-orphan, prior cycle → delete
        let mut clear: Vec<NodeHash> = Vec::new(); // resurrected → drop tombstone only
        let mut tombstoned: HashSet<NodeHash> = HashSet::new();
        self.mdbx
            .iter_from(tables::TABLE_TRIE_TOMBSTONES, &[], |k, v| {
                if k.len() == 33 && k[0] == disc {
                    let mut h = [0u8; 32];
                    h.copy_from_slice(&k[1..]);
                    tombstoned.insert(h);
                    if live_hashes.contains(&h) {
                        clear.push(h);
                    } else {
                        // Fail closed: a malformed tombstone payload must NOT
                        // authorise deleting trie data. Defaulting to tv=0 would
                        // make a corrupt entry instantly reapable; instead keep
                        // the entry (skip reaping) and surface the corruption.
                        match <[u8; 8]>::try_from(v) {
                            Ok(b) => {
                                if u64::from_be_bytes(b) < version {
                                    reap.push(h);
                                }
                            }
                            Err(_) => {
                                tracing::warn!(
                                    "trie GC: malformed tombstone payload ({} bytes) — \
                                     keeping entry (fail-closed)",
                                    v.len()
                                );
                            }
                        }
                    }
                }
                true
            })
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        let mut tomb_key = [0u8; 33];
        tomb_key[0] = disc;
        for h in clear.iter().chain(reap.iter()) {
            tomb_key[1..].copy_from_slice(h);
            self.mdbx
                .delete(tables::TABLE_TRIE_TOMBSTONES, &tomb_key)
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        }
        let deleted = reap.len();
        for h in &reap {
            self.mdbx
                .delete(data_table, h)
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        }

        // Phase B — tombstone new orphans not already tracked.
        let mut new_tomb: Vec<NodeHash> = Vec::new();
        self.mdbx
            .iter_from(data_table, &[], |k, _v| {
                if k.len() == 32 {
                    let mut h = [0u8; 32];
                    h.copy_from_slice(k);
                    if !live_hashes.contains(&h) && !tombstoned.contains(&h) {
                        new_tomb.push(h);
                    }
                }
                true
            })
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        let vbytes = version.to_be_bytes();
        for h in &new_tomb {
            tomb_key[1..].copy_from_slice(h);
            self.mdbx
                .put(tables::TABLE_TRIE_TOMBSTONES, &tomb_key, &vbytes)
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        }
        Ok(deleted)
    }

    /// Shared helper: scan an MDBX table for hashes not in `live_hashes` and remove them.
    ///
    /// Streams the table via `iter_from` so only the orphan-hash subset
    /// (32 bytes each) lands in memory, not every `(key, value)` pair.
    /// Earned via the 2026-05-12 fullnode wedge: `iter()` materialised a
    /// `Vec<(Vec<u8>, Vec<u8>)>` of the entire `TABLE_TRIE_NODES`, which
    /// on a 4.8 GB chain.db inside a 4 GiB container froze the chain-apply
    /// loop for ~16+ min at every 1000-block prune boundary. The cursor
    /// walk keeps a single MDBX RO txn open for the same duration but
    /// avoids the per-row Vec allocation amortisation that was the
    /// actual stall cause.
    fn gc_table(
        &self,
        table: &str,
        live_hashes: &std::collections::HashSet<NodeHash>,
    ) -> SentrixResult<usize> {
        let mut to_delete: Vec<NodeHash> = Vec::new();
        self.mdbx
            .iter_from(table, &[], |k, _v| {
                if k.len() == 32 {
                    let mut arr = [0u8; 32];
                    arr.copy_from_slice(k);
                    if !live_hashes.contains(&arr) {
                        to_delete.push(arr);
                    }
                }
                true
            })
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;

        let count = to_delete.len();
        for hash in &to_delete {
            self.mdbx
                .delete(table, hash)
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        }
        Ok(count)
    }

    /// Count entries in a trie table. Used by tests.
    pub fn count(&self, table: &str) -> SentrixResult<usize> {
        self.mdbx
            .count(table)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{TrieNode, empty_hash};
    use std::collections::HashSet;

    fn temp_storage() -> (tempfile::TempDir, TrieStorage) {
        let dir = tempfile::TempDir::new().unwrap();
        let mdbx = Arc::new(MdbxStorage::open(dir.path()).unwrap());
        let storage = TrieStorage::new(mdbx).unwrap();
        (dir, storage)
    }

    fn dummy_hash(byte: u8) -> NodeHash {
        let mut h = [0u8; 32];
        h[0] = byte;
        h
    }

    #[test]
    fn test_is_committed_root_true_for_stored() {
        let (_dir, storage) = temp_storage();
        let root = dummy_hash(0x10);
        storage.store_root(1, &root).unwrap();
        assert!(
            storage.is_committed_root(&root).unwrap(),
            "is_committed_root must return true for a stored root"
        );
    }

    #[test]
    fn test_is_committed_root_false_for_unknown() {
        let (_dir, storage) = temp_storage();
        let committed = dummy_hash(0x10);
        let other = dummy_hash(0x20);
        storage.store_root(1, &committed).unwrap();
        assert!(
            !storage.is_committed_root(&other).unwrap(),
            "is_committed_root must return false for a hash not in trie_roots"
        );
    }

    #[test]
    fn test_store_root_no_blocking_flush() {
        let (_dir, storage) = temp_storage();
        let root = dummy_hash(0xFF);
        for v in 0u64..50 {
            storage.store_root(v, &root).unwrap();
        }
        assert_eq!(storage.load_root(0).unwrap(), Some(root));
        assert_eq!(storage.load_root(49).unwrap(), Some(root));
    }

    #[test]
    fn test_delete_node_removes_entry() {
        let (_dir, storage) = temp_storage();
        let hash = dummy_hash(0xAB);
        let node = TrieNode::Leaf {
            key: [1u8; 32],
            value_hash: [2u8; 32],
        };

        storage.store_node(&hash, &node).unwrap();
        assert!(
            storage.load_node(&hash).unwrap().is_some(),
            "node must exist after store"
        );

        storage.delete_node(&hash).unwrap();
        assert!(
            storage.load_node(&hash).unwrap().is_none(),
            "node must be absent after delete"
        );
    }

    #[test]
    fn test_delete_value_removes_entry() {
        let (_dir, storage) = temp_storage();
        let hash = dummy_hash(0xCD);
        let val = b"balance_data";

        storage.store_value(&hash, val).unwrap();
        assert!(
            storage.load_value(&hash).unwrap().is_some(),
            "value must exist after store"
        );

        storage.delete_value(&hash).unwrap();
        assert!(
            storage.load_value(&hash).unwrap().is_none(),
            "value must be absent after delete"
        );
    }

    #[test]
    fn test_gc_orphaned_nodes_removes_unlisted() {
        let (_dir, storage) = temp_storage();
        let live_hash = dummy_hash(0x01);
        let orphan_hash = dummy_hash(0x02);

        let node = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: empty_hash(0),
        };
        storage.store_node(&live_hash, &node).unwrap();
        storage.store_node(&orphan_hash, &node).unwrap();

        let mut live: HashSet<NodeHash> = HashSet::new();
        live.insert(live_hash);

        let removed = storage.gc_orphaned_nodes(&live).unwrap();
        assert_eq!(removed, 1, "exactly one orphan must be removed");
        assert!(
            storage.load_node(&live_hash).unwrap().is_some(),
            "live node must survive GC"
        );
        assert!(
            storage.load_node(&orphan_hash).unwrap().is_none(),
            "orphan must be removed by GC"
        );
    }

    #[test]
    fn test_generational_gc_defers_then_reaps() {
        let (_dir, storage) = temp_storage();
        let live_hash = dummy_hash(0x01);
        let orphan_hash = dummy_hash(0x02);
        let node = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: empty_hash(0),
        };
        storage.store_node(&live_hash, &node).unwrap();
        storage.store_node(&orphan_hash, &node).unwrap();

        let mut live: HashSet<NodeHash> = HashSet::new();
        live.insert(live_hash);

        // Cycle 1: orphan is tombstoned, NOT deleted (deferred).
        let d1 = storage.gc_nodes_generational(&live, 100).unwrap();
        assert_eq!(d1, 0, "first cycle defers all deletes (tombstone only)");
        assert!(
            storage.load_node(&orphan_hash).unwrap().is_some(),
            "orphan survives the cycle it was first seen"
        );

        // Cycle 2 (higher version): still orphan → now reaped.
        let d2 = storage.gc_nodes_generational(&live, 200).unwrap();
        assert_eq!(d2, 1, "second cycle reaps the still-orphan tombstoned node");
        assert!(
            storage.load_node(&orphan_hash).unwrap().is_none(),
            "stale orphan deleted after a full cycle"
        );
        assert!(
            storage.load_node(&live_hash).unwrap().is_some(),
            "live node always survives"
        );
    }

    #[test]
    fn test_generational_gc_spares_node_committed_during_prune() {
        // The #791 race: a node orphan vs THIS cycle's snapshot (it was
        // committed mid-prune) but live NEXT cycle must never be deleted.
        // The old gc_table deleted it in cycle 1 → "missing node" stall.
        let (_dir, storage) = temp_storage();
        let raced = dummy_hash(0x03);
        let node = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: empty_hash(0),
        };
        storage.store_node(&raced, &node).unwrap();

        // Cycle 1: snapshot live-set misses it → tombstoned, not deleted.
        let empty: HashSet<NodeHash> = HashSet::new();
        let d1 = storage.gc_nodes_generational(&empty, 100).unwrap();
        assert_eq!(d1, 0);
        assert!(
            storage.load_node(&raced).unwrap().is_some(),
            "raced node survives cycle 1 (tombstoned, not deleted)"
        );

        // Cycle 2: now it IS live → tombstone dropped, node spared.
        let mut live: HashSet<NodeHash> = HashSet::new();
        live.insert(raced);
        let d2 = storage.gc_nodes_generational(&live, 200).unwrap();
        assert_eq!(d2, 0, "resurrected node must not be deleted");
        assert!(
            storage.load_node(&raced).unwrap().is_some(),
            "raced/live node survives — #791 race fixed"
        );
    }

    #[test]
    fn test_generational_gc_keeps_data_on_malformed_tombstone() {
        // Fail closed: a corrupt (non-8-byte) tombstone payload must NOT
        // authorise deleting trie data.
        let (_dir, storage) = temp_storage();
        let orphan = dummy_hash(0x05);
        let node = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: empty_hash(0),
        };
        storage.store_node(&orphan, &node).unwrap();

        // Plant a malformed (4-byte) tombstone for the orphan as if a prior cycle.
        let mut tk = [0u8; 33];
        tk[0] = b'n';
        tk[1..].copy_from_slice(&orphan);
        storage
            .mdbx
            .put(
                sentrix_storage::tables::TABLE_TRIE_TOMBSTONES,
                &tk,
                &[0u8; 4],
            )
            .unwrap();

        let empty: HashSet<NodeHash> = HashSet::new();
        let deleted = storage.gc_nodes_generational(&empty, 999).unwrap();
        assert_eq!(deleted, 0, "malformed tombstone must not authorise deletion");
        assert!(
            storage.load_node(&orphan).unwrap().is_some(),
            "trie data must survive a malformed tombstone (fail-closed)"
        );
    }

    #[test]
    fn test_gc_empty_live_set_removes_all() {
        let (_dir, storage) = temp_storage();
        let node = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: empty_hash(0),
        };
        for i in 0u8..5 {
            storage.store_node(&dummy_hash(i), &node).unwrap();
        }
        let removed = storage.gc_orphaned_nodes(&HashSet::new()).unwrap();
        assert_eq!(
            removed, 5,
            "all 5 nodes must be removed when live set is empty"
        );
    }

    #[test]
    fn test_gc_also_removes_orphan_values() {
        let (_dir, storage) = temp_storage();
        let live_hash = dummy_hash(0x01);
        let orphan_hash = dummy_hash(0x02);

        let node = TrieNode::Leaf {
            key: [0u8; 32],
            value_hash: empty_hash(0),
        };
        storage.store_node(&live_hash, &node).unwrap();
        storage.store_node(&orphan_hash, &node).unwrap();
        storage.store_value(&live_hash, b"live_data").unwrap();
        storage.store_value(&orphan_hash, b"orphan_data").unwrap();

        let mut live: std::collections::HashSet<NodeHash> = std::collections::HashSet::new();
        live.insert(live_hash);

        let removed = storage.gc_orphaned_nodes(&live).unwrap();
        assert_eq!(
            removed, 2,
            "GC must remove both orphan node and orphan value"
        );
        assert!(
            storage.load_value(&live_hash).unwrap().is_some(),
            "live value must survive GC"
        );
        assert!(
            storage.load_value(&orphan_hash).unwrap().is_none(),
            "orphan value must be removed"
        );
    }

    #[test]
    fn test_v10_c02_committed_root_reverse_index_populated_by_store_root() {
        let (_dir, storage) = temp_storage();
        let root = dummy_hash(0x42);
        storage.store_root(7, &root).unwrap();
        assert!(
            storage
                .mdbx
                .has(tables::TABLE_TRIE_COMMITTED, root.as_slice())
                .unwrap(),
            "trie_committed_roots must contain the hash after store_root()"
        );
    }

    #[test]
    fn test_v10_c02_is_committed_root_o1_lookup() {
        let (_dir, storage) = temp_storage();
        let r1 = dummy_hash(0x11);
        let r2 = dummy_hash(0x22);
        let r3 = dummy_hash(0x33);
        storage.store_root(1, &r1).unwrap();
        storage.store_root(2, &r2).unwrap();
        assert!(storage.is_committed_root(&r1).unwrap(), "r1 must be found");
        assert!(storage.is_committed_root(&r2).unwrap(), "r2 must be found");
        assert!(
            !storage.is_committed_root(&r3).unwrap(),
            "r3 was never stored"
        );
    }

    #[test]
    fn test_v10_c02_migration_backfills_existing_roots() {
        // Simulate pre-migration DB: write directly to trie_roots, bypassing store_root().
        let dir = tempfile::TempDir::new().unwrap();
        let mdbx = Arc::new(MdbxStorage::open(dir.path()).unwrap());

        let root = dummy_hash(0xAA);
        mdbx.put(tables::TABLE_TRIE_ROOTS, &1u64.to_be_bytes(), &root[..])
            .unwrap();

        // Re-open via TrieStorage::new() — triggers ensure_committed_roots_index()
        let storage = TrieStorage::new(mdbx).unwrap();

        assert!(
            storage.is_committed_root(&root).unwrap(),
            "ensure_committed_roots_index() must backfill pre-migration roots"
        );
    }

    #[test]
    fn test_prune_old_roots_removes_stale() {
        let (_dir, storage) = temp_storage();
        for v in 1u64..=10 {
            storage.store_root(v, &dummy_hash(v as u8)).unwrap();
        }
        let removed = storage.prune_old_roots(10, 3).unwrap();
        assert_eq!(removed, 7, "should remove versions 1-7");
        assert!(
            storage.load_root(8).unwrap().is_some(),
            "version 8 must survive"
        );
        assert!(
            storage.load_root(10).unwrap().is_some(),
            "version 10 must survive"
        );
        assert!(
            storage.load_root(1).unwrap().is_none(),
            "version 1 must be pruned"
        );
        assert!(
            storage.load_root(7).unwrap().is_none(),
            "version 7 must be pruned"
        );
    }

    #[test]
    fn test_prune_old_roots_noop_when_few_versions() {
        let (_dir, storage) = temp_storage();
        for v in 1u64..=5 {
            storage.store_root(v, &dummy_hash(v as u8)).unwrap();
        }
        let removed = storage.prune_old_roots(5, 10).unwrap();
        assert_eq!(removed, 0, "should not prune when versions < keep");
    }

    #[test]
    fn test_prune_removes_reverse_index() {
        let (_dir, storage) = temp_storage();
        let root = dummy_hash(0x42);
        storage.store_root(1, &root).unwrap();
        storage.store_root(10, &dummy_hash(0xFF)).unwrap();
        assert!(storage.is_committed_root(&root).unwrap());

        storage.prune_old_roots(10, 1).unwrap();
        assert!(
            !storage.is_committed_root(&root).unwrap(),
            "pruned root must be removed from reverse index"
        );
    }
}
