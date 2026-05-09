// db.rs - Sentrix — Per-block persistent storage (MDBX backend)

use crate::blockchain::{Blockchain, CHAIN_WINDOW_SIZE};
use sentrix_primitives::block::Block;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use sentrix_primitives::transaction::PROTOCOL_TREASURY;
use sentrix_storage::{ChainStorage, MdbxStorage};
use sentrix_trie::{account_value_decode, address_to_key};
use std::collections::BTreeSet;
use std::sync::Arc;

pub struct Storage {
    chain: ChainStorage,
}

impl Storage {
    pub fn open(path: &str) -> SentrixResult<Self> {
        let chain =
            ChainStorage::open(path).map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(Self { chain })
    }

    pub fn ensure_hash_index(&self) -> SentrixResult<()> {
        self.chain
            .ensure_hash_index()
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    /// Get a cloneable Arc handle to the underlying MdbxStorage.
    /// Used for trie init and txid index binding.
    pub fn mdbx_arc(&self) -> Arc<MdbxStorage> {
        self.chain.mdbx_arc()
    }

    // ── Blockchain state (everything except blocks) ──────

    pub fn save_blockchain(&self, blockchain: &Blockchain) -> SentrixResult<()> {
        self.chain
            .save_blockchain(blockchain, &blockchain.chain)
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        Ok(())
    }

    pub fn load_blockchain(&self) -> SentrixResult<Option<Blockchain>> {
        tracing::info!("Loading blockchain state from MDBX...");
        // Try loading state
        let mut bc: Blockchain = match self
            .chain
            .load_state()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?
        {
            Some(bc) => bc,
            None => {
                // Fallback: old "blockchain" key in meta table
                // (not expected with fresh MDBX DBs, but handles migration edge cases)
                return Ok(None);
            }
        };

        // Load only the sliding window (last CHAIN_WINDOW_SIZE blocks) into RAM.
        let height = self
            .chain
            .load_height()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        let window_start = height.saturating_sub(CHAIN_WINDOW_SIZE as u64 - 1);
        let mut blocks = Vec::with_capacity((height - window_start + 1) as usize);
        for i in window_start..=height {
            match self.load_block(i)? {
                Some(block) => blocks.push(block),
                None => {
                    let effective = if let Some(last) = blocks.last() {
                        last.index
                    } else {
                        let mut h = window_start.saturating_sub(1);
                        while h > 0 && self.load_block(h)?.is_none() {
                            h = h.saturating_sub(1);
                        }
                        let new_start = h.saturating_sub(CHAIN_WINDOW_SIZE as u64 - 1);
                        for j in new_start..=h {
                            if let Some(b) = self.load_block(j)? {
                                blocks.push(b);
                            }
                        }
                        h
                    };
                    tracing::warn!(
                        "Block {} missing (stored height = {}). \
                         Adjusting height to {}. Node will re-sync from peers.",
                        i,
                        height,
                        effective
                    );
                    self.chain
                        .save_height(effective)
                        .map_err(|e| SentrixError::StorageError(e.to_string()))?;
                    break;
                }
            }
        }
        bc.chain = blocks;
        tracing::info!(
            "Blockchain state loaded: height {} ({} blocks in window)",
            bc.height(),
            bc.chain.len()
        );

        // M-05: validate the in-memory chain window on load so a
        // corrupted DB is surfaced instead of silently serving stale
        // data via eth_getBlock* / get_transaction until a peer gossip
        // later discovers the divergence. Only warn (do not fail
        // start-up) because live nodes can recover via P2P sync; a
        // hard failure would keep an otherwise functional validator
        // offline when its peers could re-populate the chain window.
        if !bc.is_valid_chain_window() {
            tracing::error!(
                "M-05: loaded chain window failed integrity check (height={}, window_len={}). \
                 Node will continue but is likely to re-sync from peers; investigate \
                 the underlying DB corruption before this recurs.",
                height,
                bc.chain.len()
            );
        }

        // Restore state trie from MDBX.
        //
        // HARD-FAIL on trie init failure above STATE_ROOT_FORK_HEIGHT: past the
        // fork height, `state_root` is part of the block hash, so a node that
        // silently fails trie init would produce blocks with `state_root = None`
        // while peers with working tries produce `state_root = Some(...)`. The
        // hashes diverge → chain fork. Mainnet stall on 2026-04-20 was caused
        // by exactly this silent-warn path. Refusing to start surfaces broken
        // trie state to operators immediately; the chain tolerates one
        // validator offline better than a silently-diverging validator.
        //
        // Below fork height the old hash format ignores state_root entirely, so
        // a failed trie init cannot cause consensus divergence — warn-only is
        // still safe there.
        let mdbx = self.mdbx_arc();
        if let Err(e) = bc.init_trie(mdbx.clone()) {
            let h = bc.height();
            if h >= sentrix_primitives::block::STATE_ROOT_FORK_HEIGHT {
                return Err(SentrixError::Internal(format!(
                    "trie init failed at height {h}: {e}. Past STATE_ROOT_FORK_HEIGHT the trie \
                     must be functional — state_root is part of the block hash and silent trie \
                     failure causes chain forks. Resync from peers or wipe data dir to recover."
                )));
            }
            tracing::warn!(
                "trie init failed at height {} (below fork height — allowed): {}",
                h,
                e
            );
        }

        // Bind storage handles for txid_index lookups
        if let Err(e) = bc.init_storage_handle(mdbx.clone()) {
            tracing::warn!("txid_index init failed: {}", e);
        } else {
            match bc.backfill_txid_index(&mdbx) {
                Ok(0) => {}
                Ok(n) => {
                    tracing::info!("txid_index: backfilled {} tx entries from stored blocks", n)
                }
                Err(e) => tracing::warn!("txid_index backfill failed: {}", e),
            }
        }

        // Audit M6 (2026-05-06): chain.db doesn't persist the derived
        // mempool sidecars (they're `#[serde(skip)]`), so after
        // deserialise rebuild them from the loaded mempool. Skipping
        // this would have `mempool_txids` empty while `mempool` holds
        // entries — duplicate-tx detection would silently let
        // duplicates back in until the next post-block retain
        // rebuilt the index.
        bc.rebuild_mempool_sidecars();

        // Patch B3 — trie-canonical reconciliation on load.
        //
        // The bincode `state` blob carries a snapshot of `accounts`,
        // but under the pre-B1 non-atomic save flow (or any chain.db
        // repaired with `state` and trie out of sync) the in-memory
        // balances can drift from what the trie commits. The trie root
        // is consensus-verified, so the trie is the source of truth;
        // the blob is treated as a cache.
        //
        // For every protocol-system, validator, stake-related, and
        // any-non-zero account, read the trie leaf at the loaded
        // height, decode (balance, nonce), and overwrite the blob's
        // value if they differ. Accounts with no trie leaf are left
        // alone (covers the genesis/premine path that was never
        // touched by `update_trie_for_block`).
        //
        // Errors propagate: a trie-lookup failure during reconcile
        // means the trie is unreadable, not just that an account is
        // absent — refuse to start so an operator surfaces it instead
        // of silently running on inconsistent state.
        let (checked, repaired) = Self::reconcile_accounts_from_trie(&mut bc)?;
        if repaired > 0 {
            tracing::warn!(
                "load_blockchain B3: reconciled {}/{} accounts from trie at height {} \
                 (blob was stale; trie is canonical)",
                repaired,
                checked,
                bc.height()
            );
            // Persist the repaired state via the atomic B1 path so the
            // next load is a no-op + the blob_height checkpoint advances.
            self.chain
                .save_blockchain(&bc, &bc.chain)
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
        } else {
            tracing::debug!(
                "load_blockchain B3: {}/{} accounts checked, none required reconcile at height {}",
                repaired,
                checked,
                bc.height()
            );
        }

        // Patch B2 (v2.1.90 state-root determinism RCA): if persisted
        // chain height is ahead of the bincode blob's checkpoint
        // height, the on-disk view is inconsistent — `accounts` is
        // older than `chain` + the trie. Replay the missing blocks
        // through the peer-apply path so `accounts` catches up before
        // we hand the loaded chain back to the caller.
        //
        // `blob_height` is `None` for chain.dbs written before B1 (the
        // atomic-persist change). For those we trust the legacy invariant
        // (height == bc.height()) and skip the replay; if they were
        // already torn, operator recovery via chain.db rsync from a
        // healthy peer is the documented path.
        if let Some(blob_height) = self
            .chain
            .load_blob_height()
            .map_err(|e| SentrixError::StorageError(e.to_string()))?
            && height > blob_height
        {
            let missing = height - blob_height;
            tracing::warn!(
                "load_blockchain: disk height {} is {} blocks ahead of blob_height {} \
                 — replaying {} block(s) to align accounts state (Patch B2)",
                height,
                missing,
                blob_height,
                missing
            );
            // Truncate the in-memory chain window back to blob_height.
            // The disk-window load above filled bc.chain up to disk_height,
            // but bc.accounts is still at blob_height — replaying through
            // add_block_from_peer requires the chain head to match the
            // accounts state so the new block's `expected index` lines up.
            bc.chain.retain(|b| b.index <= blob_height);
            for h in (blob_height + 1)..=height {
                let block = self.load_block(h)?.ok_or_else(|| {
                    SentrixError::Internal(format!(
                        "load_blockchain replay: block {h} missing on disk \
                         (blob_height={blob_height} disk_height={height}); \
                         cannot reconstruct accounts state. Recovery: rsync chain.db \
                         from a healthy peer."
                    ))
                })?;
                bc.add_block_from_peer(block).map_err(|e| {
                    SentrixError::Internal(format!("load_blockchain replay failed at h={h}: {e}"))
                })?;
            }
            // Persist the now-consistent state so the next load is
            // a no-op (and the blob_height checkpoint advances).
            self.chain
                .save_blockchain(&bc, &bc.chain)
                .map_err(|e| SentrixError::StorageError(e.to_string()))?;
            tracing::info!(
                "load_blockchain: replay complete; blob_height now {}",
                height
            );
        }

        Ok(Some(bc))
    }

    // ── Per-block operations ─────────────────────────────

    pub fn save_block(&self, block: &Block) -> SentrixResult<()> {
        self.chain
            .save_block(block)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn load_block(&self, index: u64) -> SentrixResult<Option<Block>> {
        self.chain
            .load_block(index)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn load_block_by_hash(&self, hash: &str) -> SentrixResult<Option<Block>> {
        self.chain
            .load_block_by_hash(hash)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn load_blocks_range(&self, from: u64, to: u64) -> SentrixResult<Vec<Block>> {
        self.chain
            .load_blocks_range(from, to)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    // ── Height ───────────────────────────────────────────

    pub fn save_height(&self, height: u64) -> SentrixResult<()> {
        self.chain
            .save_height(height)
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn load_height(&self) -> SentrixResult<u64> {
        self.chain
            .load_height()
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    /// Test-only access to the underlying ChainStorage. Public because
    /// integration tests need `load_blob_height()` to verify the Patch
    /// B1 atomicity invariant; production code should not need this.
    #[doc(hidden)]
    pub fn chain_for_test(&self) -> &sentrix_storage::ChainStorage {
        &self.chain
    }

    /// Patch B3 — reconcile in-memory `accounts` against the trie
    /// leaves at the loaded height. Returns (checked, repaired).
    ///
    /// The trie at `bc.height()` is the consensus-verified source of
    /// truth; the blob's `accounts` is a cache that can drift if a
    /// previous save was non-atomic. For every account we expect the
    /// trie to know about — protocol-system addresses, validators in
    /// `stake_registry`, and any address with non-zero balance/nonce
    /// in the blob — read the trie leaf and overwrite the blob's
    /// (balance, nonce) if they differ.
    ///
    /// Addresses with no trie leaf (`Ok(None)`) are skipped: the trie
    /// only stores accounts that have been touched by
    /// `update_trie_for_block`, while `bc.accounts` may also carry
    /// premine / genesis accounts that pre-date the first trie touch.
    /// We must not zero those out.
    ///
    /// Trie-lookup errors (`Err(_)`) propagate: a corrupted trie is a
    /// hard-fail, not silent fallback.
    fn reconcile_accounts_from_trie(bc: &mut Blockchain) -> SentrixResult<(usize, usize)> {
        // Build the candidate address set first — sorted + deduped so
        // the reconcile order is deterministic across runs (helps debug
        // logs and makes tests stable).
        let mut addrs: BTreeSet<String> = BTreeSet::new();

        // 1. Protocol-system sentinel.
        addrs.insert(PROTOCOL_TREASURY.to_string());

        // 2. Validators in stake_registry — both the active set and
        //    the full validator map (so jailed/unbonding entries get
        //    reconciled too if they have a trie leaf).
        for addr in bc.stake_registry.active_set.iter() {
            addrs.insert(addr.clone());
        }
        for addr in bc.stake_registry.validators.keys() {
            addrs.insert(addr.clone());
        }

        // 3. Any account in the blob with non-zero balance or nonce.
        //    Catches user accounts that have been touched at some point
        //    in chain history; misses purely-zero accounts but those
        //    have no state to drift on.
        for (addr, acct) in bc.accounts.accounts.iter() {
            if acct.balance > 0 || acct.nonce > 0 {
                addrs.insert(addr.clone());
            }
        }

        // Split-borrow: we need `&mut bc.state_trie` for `trie.get`
        // (which mutates the cache) and `&mut bc.accounts` to apply
        // any repairs. The struct fields are independent, so this
        // satisfies the borrow checker.
        let trie = bc.state_trie.as_mut().ok_or_else(|| {
            SentrixError::Internal(
                "B3 reconcile: state_trie is None — init_trie must run before reconcile"
                    .to_string(),
            )
        })?;

        // Phase 1: read all trie leaves into a buffer. This avoids
        // holding the trie borrow while we mutate accounts in phase 2.
        let mut trie_values: Vec<(String, Option<(u64, u64)>)> = Vec::with_capacity(addrs.len());
        for addr in &addrs {
            let key = address_to_key(addr);
            let leaf = trie.get(&key).map_err(|e| {
                SentrixError::Internal(format!(
                    "B3 reconcile: trie lookup for {addr} failed at h={}: {e}",
                    bc.chain.last().map(|b| b.index).unwrap_or(0)
                ))
            })?;
            let decoded = leaf.and_then(|bytes| account_value_decode(&bytes));
            trie_values.push((addr.clone(), decoded));
        }

        // Phase 2: apply repairs.
        let height = bc.chain.last().map(|b| b.index).unwrap_or(0);
        let mut checked = 0usize;
        let mut repaired = 0usize;
        for (addr, trie_view) in trie_values {
            checked += 1;
            let Some((trie_balance, trie_nonce)) = trie_view else {
                // No trie leaf for this address — skip. Either it's a
                // premine account that has never been touched, or it
                // has zero state at this height. Either way, the blob
                // is the only source of truth for it, so we leave it.
                continue;
            };
            let (blob_balance, blob_nonce) = bc
                .accounts
                .accounts
                .get(&addr)
                .map(|a| (a.balance, a.nonce))
                .unwrap_or((0, 0));
            if trie_balance != blob_balance || trie_nonce != blob_nonce {
                tracing::warn!(
                    "B3 reconcile: account {} blob=(bal={},nonce={}) != trie=(bal={},nonce={}) \
                     at h={} — overwriting blob from trie (canonical)",
                    addr,
                    blob_balance,
                    blob_nonce,
                    trie_balance,
                    trie_nonce,
                    height
                );
                let acct = bc.accounts.get_or_create(&addr);
                acct.balance = trie_balance;
                acct.nonce = trie_nonce;
                repaired += 1;
            }
        }

        // The reconcile path mutates `accounts.touched_in_block` as a
        // side effect of `get_or_create`. Clear it so the next
        // `apply_block_pass2` doesn't pick up a stale touch list.
        bc.accounts.clear_touched_in_block();

        Ok((checked, repaired))
    }

    // ── Utility ──────────────────────────────────────────

    pub fn has_blockchain(&self) -> bool {
        self.chain.has_blockchain()
    }

    pub fn clear(&self) -> SentrixResult<()> {
        self.chain
            .clear()
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn reset_trie(&self) -> SentrixResult<()> {
        self.chain
            .reset_trie()
            .map_err(|e| SentrixError::StorageError(e.to_string()))
    }

    pub fn db_size_bytes(&self) -> u64 {
        self.chain.db_size_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockchain::Blockchain;

    fn temp_db_path() -> String {
        let dir = std::env::temp_dir().join(format!("sentrix_test_{}", uuid::Uuid::new_v4()));
        dir.to_str().unwrap().to_string()
    }

    #[test]
    fn test_open_storage() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();
        assert!(!storage.has_blockchain());
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_save_and_load_blockchain() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let mut bc = Blockchain::new("admin".to_string());
        bc.authority.add_validator_unchecked(
            "val1".to_string(),
            "Validator 1".to_string(),
            "pk1".to_string(),
        );

        storage.save_blockchain(&bc).unwrap();
        assert!(storage.has_blockchain());

        let loaded = storage.load_blockchain().unwrap().unwrap();
        assert_eq!(loaded.height(), bc.height());
        assert_eq!(loaded.total_minted, bc.total_minted);
        assert_eq!(loaded.chain_id, bc.chain_id);
        assert_eq!(loaded.chain.len(), bc.chain.len());

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_per_block_storage() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let mut bc = Blockchain::new("admin".to_string());
        bc.authority.add_validator_unchecked(
            "val1".to_string(),
            "V1".to_string(),
            "pk1".to_string(),
        );

        let block = bc.create_block("val1").unwrap();
        bc.add_block(block).unwrap();

        storage.save_blockchain(&bc).unwrap();

        let b0 = storage.load_block(0).unwrap().unwrap();
        assert_eq!(b0.index, 0);

        let b1 = storage.load_block(1).unwrap().unwrap();
        assert_eq!(b1.index, 1);
        assert_eq!(b1.validator, "val1");

        assert!(storage.load_block(99).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_load_block_by_hash() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let bc = Blockchain::new("admin".to_string());
        storage.save_blockchain(&bc).unwrap();

        let genesis_hash = bc.chain[0].hash.clone();
        let found = storage.load_block_by_hash(&genesis_hash).unwrap().unwrap();
        assert_eq!(found.index, 0);

        assert!(storage.load_block_by_hash("nonexistent").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_load_blocks_range() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let mut bc = Blockchain::new("admin".to_string());
        bc.authority.add_validator_unchecked(
            "val1".to_string(),
            "V1".to_string(),
            "pk1".to_string(),
        );

        for _ in 0..3 {
            let block = bc.create_block("val1").unwrap();
            bc.add_block(block).unwrap();
        }
        storage.save_blockchain(&bc).unwrap();

        let range = storage.load_blocks_range(1, 3).unwrap();
        assert_eq!(range.len(), 3);
        assert_eq!(range[0].index, 1);
        assert_eq!(range[2].index, 3);

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_save_and_load_height() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        storage.save_height(42).unwrap();
        let height = storage.load_height().unwrap();
        assert_eq!(height, 42);

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_persistence_across_reopen() {
        let path = temp_db_path();

        {
            let storage = Storage::open(&path).unwrap();
            let bc = Blockchain::new("admin".to_string());
            storage.save_blockchain(&bc).unwrap();
        }

        {
            let storage = Storage::open(&path).unwrap();
            assert!(storage.has_blockchain());
            let loaded = storage.load_blockchain().unwrap().unwrap();
            assert_eq!(loaded.height(), 0);
            let b0 = storage.load_block(0).unwrap().unwrap();
            assert_eq!(b0.index, 0);
        }

        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_db_size() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();
        let bc = Blockchain::new("admin".to_string());
        storage.save_blockchain(&bc).unwrap();
        assert!(storage.db_size_bytes() > 0);
        let _ = std::fs::remove_dir_all(&path);
    }

    #[test]
    fn test_save_blockchain_overwrites_stale_block() {
        let path = temp_db_path();
        let storage = Storage::open(&path).unwrap();

        let bc = Blockchain::new("admin".to_string());
        let canonical_hash = bc.chain[0].hash.clone();

        let mut stale = bc.chain[0].clone();
        stale.hash = "stale_h1_hash".to_string();
        storage.save_block(&stale).unwrap();

        storage.save_blockchain(&bc).unwrap();

        let stored = storage.load_block(0).unwrap().unwrap();
        assert_eq!(
            stored.hash, canonical_hash,
            "save_blockchain must overwrite stale H1 block with canonical H2 block"
        );

        let _ = std::fs::remove_dir_all(&path);
    }
}
