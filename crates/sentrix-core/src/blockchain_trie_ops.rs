//! `Blockchain` state-trie management — init, per-block update,
//! pruning, root accessor. ~550 LOC of impl Blockchain extracted from
//! `blockchain.rs` so the trie seam lives in one focused file.
//!
//! Methods covered:
//! - `trie_root_at` — root hash at a specific version (block height).
//! - `init_trie` — boot-time bind, root-presence check, AccountDB
//!   backfill on first-time-trie nodes (one-time migration).
//! - `update_trie_for_block` — per-block trie mutation: walk every
//!   tx's touched-address set, write fresh AccountDB-derived values
//!   into the trie, commit a new root at `height + 1`. Carries the
//!   2026-05-07 extended-touch-list fork (closes EVM-CREATE / internal
//!   CALL trie-vs-AccountDB divergence) — heavy doc block kept inline.
//! - `maybe_prune_trie` — every 5000-block boundary, dispatch a
//!   background prune of `trie_versions < height - TRIE_KEEP_VERSIONS`.
//!   PRUNE_RUNNING guards overlapping prunes; SENTRIX_DISABLE_TRIE_PRUNE
//!   skips entirely for archive nodes.
//!
//! Rust permits splitting `impl T { … }` across modules within the
//! same crate, so all `bc.init_trie(…)` / `bc.update_trie_for_block()`
//! / `bc.maybe_prune_trie()` call sites keep resolving unchanged.

use hex;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use sentrix_primitives::transaction::{PROTOCOL_TREASURY, TOKEN_OP_ADDRESS};
use sentrix_storage::MdbxStorage;
use sentrix_trie::address::{
    account_value_bytes, address_to_key, epoch_state_key, epoch_state_value_bytes,
    liveness_value_bytes, native_nft_registry_key, native_src20_registry_key,
    pending_rewards_value_bytes, total_minted_key, total_minted_value_bytes,
    validator_liveness_key, validator_pending_rewards_key,
};

/// SIP-6 Phase 1c snapshot row for a single validator's liveness +
/// jail state — captured before the `state_trie` mut-borrow, written
/// in Phase 2c. Tuple: (address, signed_count, missed_count,
/// jail_until, is_jailed).
type LivenessSnapshotRow = (String, u64, u64, u64, bool);

/// SIP-6 Phase 1c snapshot of `EpochManager.current_epoch` — captured
/// before the `state_trie` mut-borrow, written in Phase 2e as a
/// single 80-byte commitment under `epoch_state_key`. Field order
/// mirrors [`sentrix_trie::address::epoch_state_value_bytes`].
struct EpochSnapshot {
    epoch_number: u64,
    start_height: u64,
    end_height: u64,
    total_staked: u64,
    total_rewards: u64,
    total_blocks_produced: u64,
    validator_set: Vec<String>,
}
use sentrix_trie::tree::SentrixTrie;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::address::is_valid_sentrix_address;
use crate::blockchain::Blockchain;

impl Blockchain {
    /// State root committed at `version` (block height), or None if the trie is not initialized
    /// or no root was committed at that version.
    pub fn trie_root_at(&self, version: u64) -> Option<[u8; 32]> {
        self.state_trie
            .as_ref()
            .and_then(|t| t.root_at_version(version).ok().flatten())
    }

    /// Initialize the state trie from MDBX storage.
    /// Loads the committed root for the current height, or starts from an empty trie.
    /// Call once at node startup, after loading blockchain state from storage.
    ///
    /// If no trie root exists for the current height but the chain has history,
    /// backfills all non-zero accounts from AccountDB (one-time migration on trie introduction).
    pub fn init_trie(&mut self, mdbx: Arc<MdbxStorage>) -> SentrixResult<()> {
        let height = self.height();
        let mut trie = SentrixTrie::open(mdbx, height)?;

        // First-time trie init on a node whose AccountDB predates SentrixTrie:
        // AccountDB has correct state but the trie is empty — backfill now.
        //
        // Also handles the stale-height case: root hash recorded in trie_roots but
        // the root NODE was removed from trie_nodes during a prior structural cleanup
        // (insert removes replaced internal nodes, including old roots).
        let needs_backfill = if height > 0 {
            match trie.root_at_version(height)? {
                None => {
                    // No trie root entry exists for this height at all.
                    // Expected only on first-time trie init on a chain that predates
                    // SentrixTrie.  After fix/trie-permanent-fix this path should only
                    // be reached once per node lifetime.
                    tracing::warn!(
                        "trie: no root recorded for height {} — first-time backfill from AccountDB",
                        height
                    );
                    true
                }
                Some(root_hash) => {
                    // Root IS recorded in trie_roots but the node is gone from trie_nodes.
                    //
                    // ROOT CAUSE #1 / ROOT CAUSE #3 guard: after fix/trie-permanent-fix,
                    // is_committed_root() prevents insert() from deleting committed roots,
                    // so this branch should NEVER be reached on a healthy node.  If it
                    // is, something has gone seriously wrong (manual data corruption,
                    // storage bug, regression).  Log at ERROR so ops are alerted; the
                    // backfill below may produce a state root that differs from peers.
                    //
                    // ISSUE #268 FALSE-POSITIVE GUARD: empty_hash(0) is the sentinel
                    // for an empty trie level — it's NEVER materialised in trie_nodes
                    // because empty subtrees are short-circuited. So node_exists()
                    // always returns false for it. On a chain where no block has
                    // mutated any account (coinbase-only blocks against an empty
                    // initial state, or genuinely-quiet recovery windows), every
                    // committed root equals empty_hash(0) and the old check fired a
                    // spurious backfill. The backfill from AccountDB then computed
                    // a non-empty root (because AccountDB has the genesis premine
                    // entries), persisted it to MDBX BEFORE the safeguard ran, and
                    // even if the safeguard returned Err the chain.db was already
                    // corrupted. Below STATE_ROOT_FORK_HEIGHT, Storage::load_blockchain
                    // swallows that Err — so the corruption became permanent.
                    // Treat the empty sentinel as "node exists" since the empty
                    // subtree is trivially correct without storage.
                    let node_missing = root_hash != sentrix_trie::node::empty_hash(0)
                        && !trie.node_exists(&root_hash)?;
                    if node_missing {
                        tracing::error!(
                            "trie: CRITICAL — root {} for height {} is recorded in trie_roots \
                             but the node is missing from trie_nodes.  This should not happen \
                             after fix/trie-permanent-fix.  Forcing backfill from AccountDB; \
                             the resulting state root may differ from other peers and cause a fork.",
                            hex::encode(root_hash),
                            height
                        );
                        // CRITICAL: reset working root to empty_hash so backfill inserts
                        // start from a clean slate rather than a stale/deleted root.
                        trie.reset_to_empty();
                    }
                    node_missing
                }
            }
        } else {
            false
        };

        if needs_backfill {
            // CRITICAL: Sort accounts by address for deterministic backfill.
            // HashMap::values() iterates in random order per-process, causing different
            // trie roots on different nodes — the root cause of chain forks after ~17h.
            let mut accounts: Vec<(String, u64, u64)> = self
                .accounts
                .accounts
                .values()
                .filter(|a| a.balance > 0)
                .map(|a| (a.address.clone(), a.balance, a.nonce))
                .collect();
            accounts.sort_by(|a, b| a.0.cmp(&b.0));
            if !accounts.is_empty() {
                tracing::info!(
                    "trie: backfilling {} accounts at height {} (first trie init on existing chain)",
                    accounts.len(),
                    height
                );
                for (addr, balance, nonce) in accounts {
                    let key = address_to_key(&addr);
                    let val = account_value_bytes(balance, nonce);
                    trie.insert(&key, &val)?;
                }
                let backfilled_root = trie.commit(height)?;
                tracing::info!(
                    "trie: backfill complete at height {}, root = {}",
                    height,
                    hex::encode(backfilled_root)
                );

                // Bug #3 safeguard (mainnet freeze 2026-04-21): the incremental
                // path (update_trie_for_block) only inserts accounts touched by
                // blocks, while backfill inserts every account with balance > 0
                // — including premines/genesis accounts that were never touched.
                // For the same logical state, the two paths produce different
                // trie root sets, so a validator recovering via reset_trie +
                // init_trie at height > 0 will compute a state_root that
                // disagrees with peers whose trie was built incrementally from
                // genesis. Without this check, the validator silently forks
                // and every block it produces trips the #1e strict-reject guard.
                //
                // Refuse to start if the backfill root doesn't match the stored
                // header. Operators must recover via rsync chain.db from a
                // healthy peer (whole-trie copy preserves the incremental
                // shape) instead of state_import + reset.
                if let Ok(block) = self.latest_block()
                    && block.index == height
                    && let Some(stored_root) = block.state_root
                    && backfilled_root != stored_root
                {
                    return Err(SentrixError::Internal(format!(
                        "trie backfill at height {} produced root {} but the \
                         block header at that height records state_root {}. \
                         The rebuilt trie disagrees with the canonical chain \
                         (bug #3). Refusing to start to prevent a silent \
                         state fork. Recovery: rsync /opt/sentrix/data/chain.db \
                         from a healthy peer with all validators stopped, \
                         instead of `sentrix state import` + reset_trie.",
                        height,
                        hex::encode(backfilled_root),
                        hex::encode(stored_root)
                    )));
                }
            }
        }

        // Boot-time integrity check — added post-2026-04-21 3-way fork.
        // The existing checks above catch: (a) missing root entry in
        // trie_roots, (b) missing root node in trie_nodes, (c) backfill
        // root ≠ header state_root (bug #3 guard). What they DON'T catch
        // is an orphan reference BELOW the root — e.g. the root exists
        // and references a middle-layer node that was deleted by a
        // pre-v2.1.5 state_import. A validator booting on that broken
        // DB would produce blocks with `state_root=None` and get rejected
        // by strict peers — the exact #1e CRITICAL pattern observed in
        // the 2026-04-21 fork.
        //
        // Walk the current root once and refuse to boot past
        // STATE_ROOT_FORK_HEIGHT if any orphan is found. Below the fork
        // height the old hash format ignores state_root entirely, so a
        // broken trie can't cause consensus divergence — warn-only there.
        if trie_integrity_check_skipped() {
            tracing::warn!(
                "SENTRIX_SKIP_TRIE_INTEGRITY=1 set; skipping boot-time trie integrity check"
            );
        } else if let Err(e) = trie.verify_integrity() {
            if height >= sentrix_primitives::block::STATE_ROOT_FORK_HEIGHT {
                return Err(SentrixError::Internal(format!(
                    "trie integrity check failed at height {height}: {e}"
                )));
            }
            tracing::warn!(
                "trie integrity warning at height {} (below fork height — allowed): {}",
                height,
                e
            );
        }

        self.state_trie = Some(trie);
        Ok(())
    }

    /// Update the trie with current account state for every address touched in the last block,
    /// commit at that block's height, and return the new state root.
    /// Returns Ok(None) if the trie has not been initialized.
    ///
    /// Trie errors are propagated — callers must handle state root failures explicitly.
    ///
    /// Split into two phases to satisfy the borrow checker:
    ///   Phase 1 — immutable borrows of `chain` and `accounts` → collect owned data.
    ///   Phase 2 — mutable borrow of `state_trie` → insert + commit.
    pub fn update_trie_for_block(&mut self) -> SentrixResult<Option<[u8; 32]>> {
        // STATE_ROOT_V2 canonical-treasury rebase — runs BEFORE the trie-init
        // check so it fires independent of trie readiness. Targets the
        // exact activation block; force-sets in-memory PROTOCOL_TREASURY
        // to the operator-set canonical so all validators agree on the
        // value the trie is about to commit. Without either env var,
        // this is a no-op. See touch-list section below for the full
        // post-mortem on the original 2026-05-06 first-activation fork.
        let state_root_v2_height_for_rebase = std::env::var("STATE_ROOT_V2_HEIGHT")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(u64::MAX);
        let activation_block_index = self.chain.last().map(|b| b.index);
        if activation_block_index == Some(state_root_v2_height_for_rebase) {
            if let Some(canonical) = std::env::var("STATE_ROOT_V2_TREASURY_BALANCE")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
            {
                let prior = self.accounts.get_balance(PROTOCOL_TREASURY);
                if prior != canonical {
                    let delta = canonical as i128 - prior as i128;
                    tracing::warn!(
                        "STATE_ROOT_V2 activation rebase at h={}: PROTOCOL_TREASURY \
                         {} → {} (delta {} sentri). Operator-set canonical override.",
                        state_root_v2_height_for_rebase,
                        prior,
                        canonical,
                        delta
                    );
                } else {
                    tracing::info!(
                        "STATE_ROOT_V2 activation at h={}: PROTOCOL_TREASURY \
                         balance already matches canonical {} sentri (no rebase)",
                        state_root_v2_height_for_rebase,
                        canonical
                    );
                }
                self.accounts.set_balance(PROTOCOL_TREASURY, canonical);
            } else {
                tracing::warn!(
                    "STATE_ROOT_V2 activation at h={} WITHOUT \
                     STATE_ROOT_V2_TREASURY_BALANCE override — fork risk if \
                     in-memory PROTOCOL_TREASURY balance differs across validators. \
                     See blockchain.rs::update_trie_for_block runbook.",
                    state_root_v2_height_for_rebase
                );
            }
        }

        if self.state_trie.is_none() {
            // Pre-STATE_ROOT_FORK_HEIGHT, missing trie is acceptable —
            // state_root isn't part of the block hash. Past the fork
            // height, a None state_root would diverge silently from
            // peers who computed a real one, so refuse to participate.
            // load_blockchain warned on the init failure that got us
            // here; this guard turns the warn into a hard refusal at
            // the consensus boundary so the validator stops producing
            // ghost blocks rather than forking the network.
            let next_height = self.height().saturating_add(1);
            if next_height >= sentrix_primitives::block::STATE_ROOT_FORK_HEIGHT {
                return Err(SentrixError::Internal(format!(
                    "trie unavailable but next block height {next_height} requires \
                     state_root (>= STATE_ROOT_FORK_HEIGHT). Recovery: wipe data dir \
                     and resync from a healthy peer. Validator should stop producing \
                     blocks until trie is rebuilt — running here would silently fork \
                     the chain."
                )));
            }
            return Ok(None);
        }
        let trace = std::env::var("SENTRIX_TRIE_TRACE").is_ok();

        // Phase 1: extract addresses + block index from the last block
        let (touched_addrs, block_index) = {
            let block = match self.chain.last() {
                Some(b) => b,
                None => return Ok(None),
            };
            let mut addrs: Vec<String> = Vec::new();
            for tx in &block.transactions {
                if is_valid_sentrix_address(&tx.from_address) {
                    addrs.push(tx.from_address.clone());
                }
                // Skip TOKEN_OP_ADDRESS — its SRX balance is always 0 and would trigger
                // a no-op delete() traversal on every token-op block.
                if is_valid_sentrix_address(&tx.to_address) && tx.to_address != TOKEN_OP_ADDRESS {
                    addrs.push(tx.to_address.clone());
                }
            }
            if is_valid_sentrix_address(&block.validator) {
                addrs.push(block.validator.clone());
            }
            // STATE_ROOT_V2 fix (gated by STATE_ROOT_V2_HEIGHT env var):
            //
            // Pre-fix `update_trie_for_block` derived `touched_addrs`
            // strictly from each tx's `from`/`to` plus the proposer's
            // address. Coinbase txs render `to_address = <validator>`
            // for human display — but per V4 reward v2 fork (active
            // since h=590,100) the actual state mutation routes the
            // mint to PROTOCOL_TREASURY (`0x...0002`). PROTOCOL_TREASURY
            // never appeared in `touched_addrs`, so the trie never saw
            // its balance change, and the state_root froze the moment
            // coinbase-only blocks became the steady state (~h=1.25M).
            //
            // 2026-05-05 audit confirmed: state_root identical across
            // h=1.5M / 1.6M / 1.62M while PROTOCOL_TREASURY's actual
            // balance grew by ~370K SRX over the same window. State
            // commitment honestly represented the trie's view; the
            // trie just wasn't tracking the system account.
            //
            // !!! ACTIVATION POST-MORTEM 2026-05-06 !!!
            //
            // First activation attempt FORKED the cluster within 30s.
            // Root cause: PROTOCOL_TREASURY in-memory balance had
            // silently drifted across the 4 validators for ~700K
            // blocks, because the very bug this fix targets was the
            // mechanism that prevented consensus from detecting drift.
            // Snapshot at activation:
            //   validator A: 1036005.66 SRX
            //   validator B: 1035994.66 SRX (~11 SRX less)
            //   validator C: 1035916.66 SRX (~89 SRX less)
            //   validator D: 1036005.66 SRX (matches A)
            //
            // Activation makes each node insert its OWN local balance
            // into the trie, so the 2-of-4 split (A+D vs B+C) produced
            // two competing state_roots. BFT couldn't reach 3-of-4
            // majority. Recovery: chain.db rsync from canonical (A) to
            // the drifted nodes, restart with v2 disabled.
            //
            // Lesson: the simple touch-list addition is NOT a self-
            // sufficient fix. Reactivation requires ONE of these
            // companion mechanisms FIRST:
            //
            //   Option A (preferred): canonical balance reconciliation
            //     pre-activation. Sample PROTOCOL_TREASURY across all
            //     validators, pick canonical, sync via chain.db rsync,
            //     THEN activate. Requires operator pre-flight pass.
            //
            //   Option B (cleanest): rewrite this block to recompute
            //     PROTOCOL_TREASURY balance from chain history (sum of
            //     coinbase mints since h=590,100) rather than reading
            //     in-memory state. Makes activation deterministic
            //     regardless of local drift. Bigger change.
            //
            //   Option C (current default): leave dormant indefinitely.
            //     state_root commitment cosmetically broken but chain
            //     functional. Acceptable until a light-client / SPV
            //     consumer actually needs to verify treasury state.
            //
            // Operator runbook for activation (only after Option A or B):
            //   1. Run canonical balance reconciliation pass (Option A)
            //      OR confirm history-recompute logic deployed (Option B)
            //   2. Pick activation_height = current_tip + 600 (~10min lead)
            //   3. Halt all 4 mainnet validators in parallel
            //   4. Append `STATE_ROOT_V2_HEIGHT=<h>` to each /etc/<svc>/<svc>.env
            //   5. Simul-start; verify post-flip state_root agreement
            //      across all 4 within 60s of activation block
            //   6. If divergence detected, halt-all immediately and
            //      revert to default (set env to far-future height)
            //
            // The default of u64::MAX leaves the fix dormant on hosts
            // without the env var so it ships safely.
            let state_root_v2_height = std::env::var("STATE_ROOT_V2_HEIGHT")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(u64::MAX);
            if block.index >= state_root_v2_height {
                addrs.push(PROTOCOL_TREASURY.to_string());
            }
            (addrs, block.index)
        };

        // EXTENDED_TOUCH_LIST fork (2026-05-07, drift-halt RCA): post-fork
        // augment the legacy list with every address mutated during apply.
        // Picks up EVM-CREATE'd contracts + internal-CALL recipients +
        // contract-storage SSTOREs that the legacy `tx.from`/`tx.to`
        // derivation misses. Drained from AccountDB's per-block
        // accumulator. Pre-fork: no-op, legacy behaviour identity.
        let touched_addrs: Vec<String> = if Self::is_extended_touch_list_height(block_index) {
            let mut combined = touched_addrs;
            let extra = self.accounts.drain_touched_in_block();
            for addr in extra {
                combined.push(addr);
            }
            combined
        } else {
            touched_addrs
        };
        // All borrows on `self.chain` released here.
        // (Canonical-treasury rebase already fired at function entry —
        // see top of update_trie_for_block.)

        // Phase 1b: snapshot current balances + nonces (immutable borrow of `accounts`)
        // CRITICAL: Use BTreeSet (sorted, deterministic) — NOT HashSet (random per-process).
        // HashSet iteration order differs across nodes, causing different trie insert order.
        // Even though the Binary SMT root should be order-independent in theory, using
        // deterministic order eliminates any possibility of implementation-level divergence.
        let unique: std::collections::BTreeSet<String> = touched_addrs.into_iter().collect();
        let updates: Vec<(String, u64, u64)> = unique
            .iter()
            .map(|a| {
                (
                    a.clone(),
                    self.accounts.get_balance(a),
                    self.accounts.get_nonce(a),
                )
            })
            .collect();
        // Borrow of `accounts` ends after collect().

        // Phase 1c — SIP-6 Bug A (post-fork only): snapshot every
        // validator's off-trie consensus state into trie inputs BEFORE
        // we re-borrow `self.state_trie` mutably. Pre-fork these are
        // empty `Vec`s / `None`, so the legacy trie write path is
        // bit-identical.
        //
        // Sorted by address for cross-validator determinism (the
        // HashMap iteration order is per-process, but a sorted Vec is
        // identical everywhere — same property the balance write path
        // gets from its BTreeSet above).
        let pending_rewards_updates: Vec<(String, u64)>;
        let liveness_updates: Vec<LivenessSnapshotRow>;
        let total_minted_snapshot: Option<u64>;
        let epoch_snapshot: Option<EpochSnapshot>;
        if Self::is_state_in_trie_height(block_index) {
            let mut rewards: Vec<(String, u64)> = self
                .stake_registry
                .validators
                .iter()
                .map(|(addr, val)| (addr.clone(), val.pending_rewards))
                .collect();
            rewards.sort_by(|a, b| a.0.cmp(&b.0));

            let mut liveness: Vec<LivenessSnapshotRow> = self
                .stake_registry
                .validators
                .iter()
                .map(|(addr, val)| {
                    let (signed, missed) = self.slashing.liveness.get_stats(addr);
                    (addr.clone(), signed, missed, val.jail_until, val.is_jailed)
                })
                .collect();
            liveness.sort_by(|a, b| a.0.cmp(&b.0));

            let epoch = &self.epoch_manager.current_epoch;
            pending_rewards_updates = rewards;
            liveness_updates = liveness;
            total_minted_snapshot = Some(self.total_minted);
            epoch_snapshot = Some(EpochSnapshot {
                epoch_number: epoch.epoch_number,
                start_height: epoch.start_height,
                end_height: epoch.end_height,
                total_staked: epoch.total_staked,
                total_rewards: epoch.total_rewards,
                total_blocks_produced: epoch.total_blocks_produced,
                validator_set: epoch.validator_set.clone(),
            });
        } else {
            pending_rewards_updates = Vec::new();
            liveness_updates = Vec::new();
            total_minted_snapshot = None;
            epoch_snapshot = None;
        }
        // Borrow of `stake_registry` / `slashing` / `total_minted` ends here.

        // Native-module state commitment. Independent of the SIP-6
        // STATE_IN_TRIE gate above (which is already active on testnet —
        // reusing it would retroactively fork testnet's state_root). Capture
        // the SRC-20 + NFT registry canonical hashes here, before the trie
        // mut-borrow, and commit them in Phase 2f. Pre-fork both are None and
        // the state_root is unchanged from today.
        let native_src20_hash: Option<[u8; 32]>;
        let native_nft_hash: Option<[u8; 32]>;
        if Self::is_native_state_in_trie_height(block_index) {
            native_src20_hash = Some(self.contracts.canonical_hash());
            native_nft_hash = Some(self.nft_registry.canonical_hash());
        } else {
            native_src20_hash = None;
            native_nft_hash = None;
        }

        if trace {
            eprintln!("[trie-trace] update_trie_for_block at h={block_index}");
            eprintln!("[trie-trace] touched (sorted): {} addresses", updates.len());
            for (addr, balance, nonce) in &updates {
                let key = address_to_key(addr);
                let value = account_value_bytes(*balance, *nonce);
                eprintln!(
                    "[trie-trace]   addr={addr} balance={balance} nonce={nonce} key={} value={}",
                    hex::encode(key),
                    hex::encode(&value)
                );
            }
        }

        // Phase 2: mutable borrow of `state_trie`
        let trie = match self.state_trie.as_mut() {
            Some(t) => t,
            None => return Ok(None),
        };
        if trace {
            eprintln!(
                "[trie-trace] root pre-update: {}",
                hex::encode(trie.root_hash())
            );
        }
        for (addr, balance, nonce) in updates {
            let key = address_to_key(&addr);
            // Trace the existing leaf BEFORE we mutate
            if trace {
                let existing = trie.get(&key)?;
                eprintln!(
                    "[trie-trace]   existing leaf for {addr}: {}",
                    existing
                        .as_ref()
                        .map(hex::encode)
                        .unwrap_or_else(|| "<none>".into())
                );
            }
            if balance == 0 {
                trie.delete(&key)?;
            } else {
                let value = account_value_bytes(balance, nonce);
                trie.insert(&key, &value)?;
            }
            if trace {
                eprintln!(
                    "[trie-trace]   root after {addr}: {}",
                    hex::encode(trie.root_hash())
                );
            }
        }

        // Phase 2b — SIP-6 Bug A (post-fork only): commit per-validator
        // pending_rewards into the trie under a domain-separated key
        // (`validator_pending_rewards_key`) so any drift across
        // validators surfaces immediately as a state_root mismatch
        // instead of silently diverging on the off-trie HashMap until
        // a ClaimRewards / Unjail / AddSelfStake tx consumes the
        // drifted value.
        //
        // Empty `pending_rewards == 0` is written as `delete` to keep
        // the trie footprint minimal and the key absent for vals that
        // have never accrued (or have just claimed) — matches the
        // balance loop's zero handling above.
        for (addr, rewards) in &pending_rewards_updates {
            let key = validator_pending_rewards_key(addr);
            if *rewards == 0 {
                trie.delete(&key)?;
            } else {
                let value = pending_rewards_value_bytes(*rewards);
                trie.insert(&key, &value)?;
            }
            if trace {
                eprintln!(
                    "[trie-trace]   pending_rewards {addr}={rewards} → root={}",
                    hex::encode(trie.root_hash())
                );
            }
        }

        // Phase 2c — SIP-6 Bug A (post-fork only): commit per-validator
        // liveness snapshot (signed_count, missed_count, jail_until,
        // is_jailed) under `validator_liveness_key`. Drift on any of
        // these fields silently changed active_set / jail evaluation
        // pre-fork (mainnet halt #9 class). Post-fork divergence
        // surfaces in state_root at the block where it first appears.
        //
        // Unlike pending_rewards we always insert (never delete) —
        // a value of (0,0,0,false) is a legitimate "validator
        // registered, no signing yet, not jailed" state distinct from
        // "validator not in registry"; using delete would conflate them.
        for (addr, signed, missed, jail_until, is_jailed) in &liveness_updates {
            let key = validator_liveness_key(addr);
            let value = liveness_value_bytes(*signed, *missed, *jail_until, *is_jailed);
            trie.insert(&key, &value)?;
            if trace {
                eprintln!(
                    "[trie-trace]   liveness {addr} signed={signed} missed={missed} \
                     jail_until={jail_until} is_jailed={is_jailed} → root={}",
                    hex::encode(trie.root_hash())
                );
            }
        }

        // Phase 2d — SIP-6 Bug A (post-fork only): commit the global
        // `Blockchain.total_minted` counter under a fixed key
        // (`total_minted_key`). Drift here meant validators disagreed
        // on circulating supply for tokenomics gating (halving math,
        // ClaimRewards budget) — surfaced on apply paths that read it
        // back later. Same insert-always semantics as Phase 2c.
        if let Some(total) = total_minted_snapshot {
            let key = total_minted_key();
            let value = total_minted_value_bytes(total);
            trie.insert(&key, &value)?;
            if trace {
                eprintln!(
                    "[trie-trace]   total_minted={total} → root={}",
                    hex::encode(trie.root_hash())
                );
            }
        }

        // Phase 2e — SIP-6 Bug A (post-fork only): commit
        // `EpochManager.current_epoch` as a single 80-byte snapshot
        // (epoch_number + start/end_height + total_staked +
        // total_rewards + total_blocks_produced + validator_set_hash).
        // Closes the last off-trie consensus state class — drift on
        // any epoch field (active-set rotation, accumulators) surfaces
        // as state_root mismatch at the block where it first appears.
        if let Some(snap) = epoch_snapshot {
            let key = epoch_state_key();
            let value = epoch_state_value_bytes(
                snap.epoch_number,
                snap.start_height,
                snap.end_height,
                snap.total_staked,
                snap.total_rewards,
                snap.total_blocks_produced,
                &snap.validator_set,
            );
            trie.insert(&key, &value)?;
            if trace {
                eprintln!(
                    "[trie-trace]   epoch_state epoch={} blocks={} rewards={} → root={}",
                    snap.epoch_number,
                    snap.total_blocks_produced,
                    snap.total_rewards,
                    hex::encode(trie.root_hash())
                );
            }
        }

        // Phase 2f — native-module state commitment (post
        // NATIVE_STATE_IN_TRIE fork only). Each registry's canonical hash is
        // written under a single fixed key, overwritten every block, so the
        // state_root reflects all SRC-20 + NFT state. Always-insert (even when
        // empty) — an empty registry has a stable canonical hash distinct from
        // any populated one, same insert-always semantics as total_minted.
        if let Some(h) = native_src20_hash {
            trie.insert(&native_src20_registry_key(), &h)?;
            if trace {
                eprintln!(
                    "[trie-trace]   native_src20 hash={} → root={}",
                    hex::encode(&h[..8]),
                    hex::encode(trie.root_hash())
                );
            }
        }
        if let Some(h) = native_nft_hash {
            trie.insert(&native_nft_registry_key(), &h)?;
            if trace {
                eprintln!(
                    "[trie-trace]   native_nft hash={} → root={}",
                    hex::encode(&h[..8]),
                    hex::encode(trie.root_hash())
                );
            }
        }

        let root = trie.commit(block_index)?;
        if trace {
            eprintln!(
                "[trie-trace] commit at h={block_index} → root={}",
                hex::encode(root)
            );
        }
        Ok(Some(root))
    }

    /// Periodically reclaim trie storage. Called after every successful block
    /// commit; only does work when the height is a multiple of TRIE_PRUNE_EVERY.
    ///
    /// `keep_versions` historical roots remain walkable; older ones and any
    /// nodes/values exclusively referenced by them get GC'd.
    ///
    /// Pruning failure is logged but never propagated — a failed prune leaves
    /// extra storage on disk but does not break consensus.
    ///
    /// 2026-05-12 (v2.2.4): prune now runs on its own OS thread instead of
    /// inline. apply_block_pass2 calls this while holding the Blockchain
    /// write lock from the gossip-block apply task in libp2p_node; the
    /// cursor walk in gc_orphaned_nodes is O(N) over the trie node table
    /// (millions of entries on mainnet) and was holding chain.write() for
    /// tens of seconds at every 1000-block boundary, freezing the apply
    /// loop and producing the recurring "silent fullnode wedge". By
    /// dispatching to std::thread we release the write lock immediately;
    /// the prune work proceeds against MDBX in parallel with the next
    /// block applies. PRUNE_RUNNING gates overlap so a slow prune doesn't
    /// queue behind itself when the next boundary fires.
    pub fn maybe_prune_trie(&self) {
        // 2026-05-12: bumped 1000 → 5000 because the per-1000-block
        // prune walk takes 10–20 min on mainnet's 4.8 GB chain.db, and
        // during the delete-batch phase MDBX write contention pushes
        // bt from 2 s/blk to 5–10 s/blk. Stretching the interval 5×
        // means fewer-but-longer contention windows (per hour) and
        // more uninterrupted steady-state. TRIE_KEEP_VERSIONS unchanged
        // — we still retain the last 1000 historical roots for any
        // archive-node use; the deletion batch each prune just covers
        // a larger range of older versions (~4000 instead of ~0–1000).
        const TRIE_PRUNE_EVERY: u64 = 5000;
        const TRIE_KEEP_VERSIONS: u64 = 1000;

        // Archive-mode opt-in: when SENTRIX_DISABLE_TRIE_PRUNE=1 is set
        // in the environment, the periodic prune skips entirely. The
        // node accumulates every historical trie version, enabling
        // state-at-past-block queries (eth_call at historic h, bridge
        // proofs, explorer historical analytics). Off by default — only
        // the dedicated archive fullnode sets this; validators stay
        // lean. Predicate matches SENTRIX_APPLY_PROFILE's "1"-only
        // semantics (block_executor.rs:635) for consistency.
        if trie_prune_disabled() {
            return;
        }

        let height = self.height();
        if height == 0 || !height.is_multiple_of(TRIE_PRUNE_EVERY) {
            return;
        }

        if PRUNE_RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            tracing::info!(
                "trie prune at height {} skipped: previous prune still running",
                height
            );
            return;
        }

        let Some(trie) = self.state_trie.as_ref().cloned() else {
            PRUNE_RUNNING.store(false, Ordering::Release);
            return;
        };

        std::thread::Builder::new()
            .name(format!("trie-prune-h{}", height))
            .spawn(move || {
                let outcome = trie.prune(TRIE_KEEP_VERSIONS);
                PRUNE_RUNNING.store(false, Ordering::Release);
                match outcome {
                    Ok((roots, nodes)) if roots > 0 || nodes > 0 => {
                        tracing::info!(
                            "trie maintenance at height {}: retired {} old roots, GC'd {} nodes/values",
                            height,
                            roots,
                            nodes
                        );
                    }
                    Ok(_) => {} // nothing to do
                    Err(e) => {
                        tracing::warn!(
                            "trie prune at height {} failed: {} (storage will continue to grow until next successful prune)",
                            height,
                            e
                        );
                    }
                }
            })
            .map_or_else(
                |e| {
                    PRUNE_RUNNING.store(false, Ordering::Release);
                    tracing::warn!(
                        "trie prune at height {} could not spawn background thread: {}",
                        height,
                        e
                    );
                },
                |_handle| {},
            );
    }
}

/// Guard: only one trie prune in flight at a time. Set true at the start
/// of `maybe_prune_trie` via compare_exchange; the spawned thread clears
/// it on completion (success or error). If a second 1000-block boundary
/// fires while a prior prune is still walking, we skip the second cycle
/// — storage will continue to grow until the next successful prune, same
/// as the existing "failed prune" semantics documented above.
static PRUNE_RUNNING: AtomicBool = AtomicBool::new(false);

/// Archive-mode opt-in. When `SENTRIX_DISABLE_TRIE_PRUNE=1` is set in
/// the environment, [`Blockchain::maybe_prune_trie`] returns immediately
/// without scheduling a prune. The node accumulates every historical
/// trie version forever — enabling state-at-past-block queries
/// (`eth_call` at historic h, bridge proofs, explorer historical
/// analytics) at the cost of unbounded disk growth.
///
/// Default off. Production validators leave this unset and keep the
/// rolling `TRIE_KEEP_VERSIONS = 1000` window. Dedicated archive
/// fullnodes set this flag.
///
/// Match SENTRIX_APPLY_PROFILE's strict "1" semantics (any other value
/// is treated as off) so accidental `=true` / `=yes` / empty-value
/// settings don't silently activate the archive path.
pub(crate) fn trie_prune_disabled() -> bool {
    std::env::var_os("SENTRIX_DISABLE_TRIE_PRUNE").is_some_and(|v| v == "1")
}

const TESTNET_CHAIN_ID: u64 = 7120;

/// Testnet recovery opt-out for boot-time trie reachability checks.
///
/// Existing testnet runtimes carry this flag while their historical trie
/// tables are known to have orphan references. Keep the predicate strict:
/// only the explicit value `1` disables the check, and only on testnet.
pub(crate) fn trie_integrity_check_skipped() -> bool {
    crate::chain_params::get_chain_id() == TESTNET_CHAIN_ID
        && std::env::var_os("SENTRIX_SKIP_TRIE_INTEGRITY").is_some_and(|v| v == "1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::env_test_lock;

    #[test]
    fn trie_integrity_skip_is_testnet_only_and_strictly_opt_in() {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("SENTRIX_CHAIN_ID");
            std::env::remove_var("SENTRIX_SKIP_TRIE_INTEGRITY");
            assert!(!trie_integrity_check_skipped());

            std::env::set_var("SENTRIX_CHAIN_ID", TESTNET_CHAIN_ID.to_string());
            std::env::set_var("SENTRIX_SKIP_TRIE_INTEGRITY", "true");
            assert!(!trie_integrity_check_skipped());

            std::env::set_var("SENTRIX_SKIP_TRIE_INTEGRITY", "1");
            assert!(trie_integrity_check_skipped());

            std::env::set_var("SENTRIX_CHAIN_ID", "7119");
            assert!(!trie_integrity_check_skipped());

            std::env::set_var("SENTRIX_CHAIN_ID", "999999");
            assert!(!trie_integrity_check_skipped());

            std::env::remove_var("SENTRIX_CHAIN_ID");
            std::env::remove_var("SENTRIX_SKIP_TRIE_INTEGRITY");
        }
    }
}
