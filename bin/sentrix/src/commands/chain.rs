//! `sentrix chain …` subcommands — read-only chain queries + chain.db
//! reconciliation helpers (`reset-trie`, `verify-deep`).
//!
//! Extracted from `main.rs`. Pure CLI handlers; the underlying logic
//! lives in `sentrix-core::Blockchain` and `sentrix-trie`.
//!
//! `verify-deep`'s heavier imports (`std::sync::Arc`,
//! `sentrix::core::trie::*`) live inside the function body — they're
//! one-shot use, and keeping them local lets the module header stay
//! focused on what every command in here shares.

use sentrix::storage::db::Storage;

use crate::get_db_path;

pub fn cmd_chain_info() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let stats = bc.chain_stats();
    println!("{}", serde_json::to_string_pretty(&stats)?);
    Ok(())
}

pub fn cmd_chain_validate() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let valid = bc.is_valid_chain_window();
    println!("Chain valid: {}", valid);
    println!("Height: {}", bc.height());
    println!("Total blocks: {}", bc.height() + 1);
    Ok(())
}

pub fn cmd_chain_block(index: u64) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    match bc.get_block_any(index) {
        Some(block) => println!("{}", serde_json::to_string_pretty(&block)?),
        None => println!("Block {} not found", index),
    }
    Ok(())
}

pub fn cmd_chain_reset_trie(i_understand_divergence_risk: bool) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    if !storage.has_blockchain() {
        anyhow::bail!("Chain not initialized.");
    }

    // 2026-04-21 mainnet 3-way fork root cause: pre-v2.1.5 `state_import` on
    // production validators reset the trie to empty and re-populated it from
    // the imported account set. The backfilled trie produced a state_root
    // that disagreed with peers whose trie was built incrementally from
    // genesis — silent fork. v2.1.5 added a boot-time backfill-vs-header
    // guard, and PR #206 added a full trie-reachability check, but the
    // cleanest protection is to refuse reset-trie on a production chain
    // unless the operator explicitly acknowledges the divergence risk.
    let height = storage
        .load_height()
        .map_err(|e| anyhow::anyhow!("reading chain height: {e}"))?;
    if height > 0 {
        // Two ways to authorize: explicit CLI flag (preferred) or the
        // legacy env-var override (kept for back-compat with existing
        // ops scripts). Either alone is sufficient.
        let env_override = std::env::var("SENTRIX_ALLOW_RESET_TRIE_ON_NONZERO_HEIGHT")
            .map(|v| v == "1")
            .unwrap_or(false);
        if !i_understand_divergence_risk && !env_override {
            anyhow::bail!(
                "Refusing reset-trie on a chain at height {height} > 0.\n\
                 \n\
                 This command wipes trie_nodes/trie_values/trie_roots and \
                 rebuilds from AccountDB on next boot. The incremental \
                 path (update_trie_for_block during apply_block) only \
                 inserts accounts touched by blocks; backfill inserts \
                 every account with balance > 0. For the same logical \
                 state, the two paths produce different trie node shapes, \
                 so a SINGLE validator that runs reset-trie while peers \
                 keep their incrementally-built tries will silently fork \
                 — see the 2026-04-21 3-way fork incident for what that \
                 looks like in prod.\n\
                 \n\
                 Use cases:\n\
                 \n\
                 (1) Single damaged peer, fleet healthy: prefer rsync \
                 from a confirmed-halted canonical peer. The whole-trie \
                 copy preserves the incremental shape and there's no \
                 fork risk.\n\
                 \n\
                 (2) Cluster-wide recovery (e.g. after a chain.db edit \
                 like force-unjail that mutates AccountDB without a \
                 corresponding trie commit): halt ALL peers, run \
                 reset-trie on the canonical peer, tar-pipe its \
                 (trie-empty) chain.db to every other peer, simultaneous \
                 start. Each peer's init_trie then backfills from the \
                 (identical) post-edit AccountDB → all peers converge \
                 on the same backfill-shape trie. Re-run with \
                 `--i-understand-divergence-risk` to acknowledge and \
                 proceed.\n\
                 \n\
                 The legacy env-var override \
                 `SENTRIX_ALLOW_RESET_TRIE_ON_NONZERO_HEIGHT=1` is also \
                 honored for back-compat with existing ops scripts."
            );
        }
        tracing::warn!(
            "reset-trie proceeding on non-zero height (h={height}) — \
             cluster-wide procedure required: this peer's rebuilt trie \
             will only agree with peers if they also reset+restart from \
             the SAME post-edit AccountDB. A single-peer reset will fork."
        );
    }

    storage.reset_trie()?;
    println!(
        "Trie state cleared. Start the node normally — it will rebuild the trie from AccountDB."
    );
    if height > 0 {
        println!();
        println!(
            "REMINDER: cluster-wide procedure required on a non-genesis chain. \
             tar-pipe this chain.db to every peer (with all peers halted) before \
             starting any of them, otherwise this peer will fork."
        );
    }
    Ok(())
}

/// Deep cross-table consistency check (issue #268 2026-04-25 RCA).
///
/// Walks every AccountDB entry with balance > 0, computes the expected trie
/// value via `account_value_bytes(balance, nonce)`, and compares to the
/// actual leaf the trie returns for `address_to_key(address)`. Catches
/// mixed-timestamp chain.db produced by rsync-while-live: trie tables and
/// AccountDB at different MDBX commit snapshots, internally inconsistent,
/// boots silently, diverges on first block apply.
///
/// Run with node STOPPED (MDBX is single-writer). Returns exit code 0 on
/// match, 1 on mismatch with a per-address summary on stdout.
pub fn cmd_chain_verify_deep() -> anyhow::Result<()> {
    use sentrix::core::trie::{account_value_bytes, address_to_key};
    use std::sync::Arc;

    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    let mdbx = storage.mdbx_arc();
    bc.init_trie(Arc::clone(&mdbx))?;

    let height = bc.height();
    let stored_root = bc.trie_root_at(height).map(hex::encode);
    println!("chain height: {height}");
    println!("stored trie root @ height: {:?}", stored_root);

    // First gate: cryptographic relationship within the trie itself.
    // Catches rsync-while-live MDBX corruption where nodes load cleanly but
    // parent-hash relationships are broken — the actual #268 v2.1.21 canary
    // failure mode that the simpler AccountDB ↔ trie consistency check
    // (below) cannot detect.
    if let Some(trie) = bc.state_trie.as_ref() {
        match trie.verify_integrity_strict() {
            Ok(()) => println!("trie strict-integrity: OK (all node hashes match content)"),
            Err(e) => {
                println!("trie strict-integrity: FAIL");
                println!("  {}", e);
                println!();
                println!(
                    "Recovery: this chain.db is unsafe to start. Halt all peer \
                     validators (verify with `pgrep sentrix` returning empty), then \
                     rsync chain.db from a confirmed-halted canonical peer. Re-run \
                     `sentrix chain verify-deep` to confirm clean."
                );
                anyhow::bail!("trie strict-integrity check failed");
            }
        }
    }

    let trie = bc
        .state_trie
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("trie not initialised"))?;

    let total_accounts = bc.accounts.accounts.len();
    let mut checked = 0usize;
    let mut zero_balance_skipped = 0usize;
    let mut mismatches: Vec<(String, u64, u64, Option<Vec<u8>>)> = Vec::new();

    // Sort for deterministic output.
    let mut entries: Vec<&sentrix::core::account::Account> =
        bc.accounts.accounts.values().collect();
    entries.sort_by(|a, b| a.address.cmp(&b.address));

    for account in entries {
        if account.balance == 0 {
            zero_balance_skipped += 1;
            continue;
        }
        let key = address_to_key(&account.address);
        let expected = account_value_bytes(account.balance, account.nonce);
        let actual = trie.get(&key)?;
        match &actual {
            Some(bytes) if *bytes == expected => {}
            _ => {
                mismatches.push((
                    account.address.clone(),
                    account.balance,
                    account.nonce,
                    actual.clone(),
                ));
            }
        }
        checked += 1;
    }

    println!(
        "scanned {} accounts ({} checked with balance > 0, {} skipped with balance = 0)",
        total_accounts, checked, zero_balance_skipped
    );

    if mismatches.is_empty() {
        println!("VERDICT: trie ↔ AccountDB CONSISTENT");
        Ok(())
    } else {
        println!(
            "VERDICT: {} MISMATCHES — chain.db is internally inconsistent (likely rsync-while-live origin)",
            mismatches.len()
        );
        for (addr, balance, nonce, trie_leaf) in mismatches.iter().take(20) {
            println!(
                "  {} accountdb=(balance={}, nonce={}) trie_leaf={}",
                addr,
                balance,
                nonce,
                trie_leaf
                    .as_ref()
                    .map(hex::encode)
                    .unwrap_or_else(|| "<missing>".to_string())
            );
        }
        if mismatches.len() > 20 {
            println!("  ... and {} more", mismatches.len() - 20);
        }
        println!();
        println!("Recovery: this chain.db is unsafe to start. Halt all peer validators,");
        println!("rsync from a confirmed-halted canonical peer (NOT a live one), then re-run");
        println!("`sentrix chain verify-deep` to confirm clean before starting the validator.");
        anyhow::bail!("trie ↔ AccountDB inconsistency detected");
    }
}

/// Reclaim trie storage by deleting nodes/values unreachable from the last
/// `keep` committed roots — the RACE-FREE counterpart to the background prune.
///
/// MUST run with the node STOPPED. MDBX is single-writer, and concurrent block
/// commits are exactly what make the background prune delete still-live nodes
/// (the recurring "missing node" stalls — which is why the background prune is
/// now off by default). With the node quiesced, the live-set walk reads a
/// consistent chain.db and only genuine orphans are removed.
///
/// Operator runbook: halt the validator (verify `pgrep sentrix` is empty),
/// run `sentrix chain prune`, restart. No fork risk — deleting unreachable
/// trie nodes does not change the state_root, which only commits reachable
/// nodes. Safe to run on a single peer (unlike reset-trie).
pub fn cmd_chain_prune(keep: u64) -> anyhow::Result<()> {
    use std::sync::Arc;

    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let mdbx = storage.mdbx_arc();
    bc.init_trie(Arc::clone(&mdbx))?;

    let height = bc.height();
    let trie = bc
        .state_trie
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("trie not initialised"))?;

    println!("Offline trie prune at height {height}, keeping the last {keep} roots.");
    println!("(Run ONLY with the node STOPPED — MDBX is single-writer.)");
    let (roots, gc) = trie.prune_offline(keep)?;
    if roots == 0 {
        println!("Nothing to prune (fewer than {keep} retained roots, or already lean).");
    } else {
        println!("Pruned: retired {roots} old roots, GC'd {gc} nodes/values.");
    }
    Ok(())
}
