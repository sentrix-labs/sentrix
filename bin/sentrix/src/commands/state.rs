//! `sentrix state …` subcommands — export / import / verify the full
//! account + validator + contract snapshot.
//!
//! Extracted from `main.rs`. Same pattern as the other `commands/`
//! modules: pure CLI handlers, the real work lives in
//! `sentrix-core::blockchain` (export_state / import_state /
//! verify_snapshot).
//!
//! `state import` carries one of the heaviest comment blocks in the
//! binary because the v2.1.5 / 2026-04-21 mainnet 3-way fork was
//! caused by this exact path. The bail-on-non-zero-height guard +
//! env-override + trie-reset-on-import are intentional safety rails;
//! the comments stay with the code so any future edit sees why the
//! shape is the way it is.

use sentrix::core::blockchain::Blockchain;
use sentrix::storage::db::Storage;

use crate::get_db_path;

pub fn cmd_state_export(output: Option<String>) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    let snapshot = bc.export_state()?;
    let out_path = output.unwrap_or_else(|| format!("state_{}.json", snapshot.metadata.height));
    let json = serde_json::to_string_pretty(&snapshot)?;
    std::fs::write(&out_path, &json)?;
    println!(
        "State exported: {} ({} accounts, {} validators, {:.4} SRX total)",
        out_path,
        snapshot.accounts.len(),
        snapshot.validators.len(),
        snapshot.accounts.iter().map(|a| a.balance).sum::<u64>() as f64 / 100_000_000.0
    );
    println!("Height: {}", snapshot.metadata.height);
    println!("Chain ID: {}", snapshot.metadata.chain_id);
    Ok(())
}

pub fn cmd_state_import(input: &str, force: bool) -> anyhow::Result<()> {
    if !force {
        anyhow::bail!(
            "State import replaces ALL current accounts, validators, and contracts.\n\
             This is destructive. Pass --force to confirm."
        );
    }

    let json = std::fs::read_to_string(input)?;
    let snapshot: sentrix::core::state_export::StateSnapshot = serde_json::from_str(&json)?;

    // Verify first
    Blockchain::verify_snapshot(&snapshot)?;

    let storage = Storage::open(&get_db_path())?;

    // 2026-04-21 mainnet 3-way fork root cause: pre-v2.1.5 `state_import` on
    // production validators re-populated the account set without rebuilding
    // the trie identically to peers'. The v2.1.5 trie-reset-on-import fix
    // + v2.1.6 strict state_root enforcement + PR #206 boot-time integrity
    // check now catch the damage, but the safest contract is: never allow
    // state_import on a non-genesis chain at all. On mainnet / an existing
    // network, the right recovery is rsync-from-peer (preserves incremental
    // trie shape, matches peers bit-for-bit). state_import is only correct
    // for fresh genesis bootstrapping or isolated devnet testing.
    let current_height = storage
        .load_height()
        .map_err(|e| anyhow::anyhow!("reading chain height: {e}"))?;
    if current_height > 0 {
        // Env override check lives INSIDE this branch. A prior draft ordered
        // `bail!` first and the override after, making the override dead code.
        let override_set = std::env::var("SENTRIX_ALLOW_STATE_IMPORT_ON_NONZERO_HEIGHT")
            .map(|v| v == "1")
            .unwrap_or(false);
        if !override_set {
            anyhow::bail!(
                "Refusing state import on a chain at height {current_height} > 0.\n\
                 This command wipes and rebuilds AccountDB from the snapshot, \
                 then resets the trie so init_trie rebuilds it on next boot. On \
                 a chain past genesis that rebuild CAN produce a state_root that \
                 disagrees with peers who built their trie incrementally block by \
                 block (see the 2026-04-21 3-way fork incident for what that \
                 looks like — took ~30h to recover).\n\
                 \n\
                 Correct recoveries on a non-genesis chain:\n\
                 1. Stop this node.\n\
                 2. rsync /opt/sentrix/data/chain.db from a healthy peer (all validators stopped).\n\
                 3. Restart. Boot-time integrity check confirms the copy is intact.\n\
                 \n\
                 If you really need state_import on a non-genesis chain (isolated \
                 devnet / one-off testing only), set `SENTRIX_ALLOW_STATE_IMPORT_ON_NONZERO_HEIGHT=1` \
                 in your environment. There is no supported use of this flag on a shared chain."
            );
        }
        tracing::warn!(
            "state import proceeding on non-zero height (h={current_height}) via env override — \
             fork is very likely on a shared chain"
        );
    }

    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    let count = bc.import_state(&snapshot)?;

    // ROOT CAUSE fix (2026-04-21 deploy rollback post-mortem): import only
    // rewrites `accounts` and counters. The trie storage (trie_nodes +
    // trie_values + trie_roots MDBX tables) is untouched. On the next
    // `sentrix start`, `init_trie` finds the existing committed root for
    // the current height + its nodes still present → uses the stale trie
    // that reflects the PRE-import accounts. Every block applied after
    // restart then computes a state_root from the stale trie, diverging
    // from peers whose trie matches their (non-imported) accounts. The
    // `#1e strict reject` guard fires and the chain halts.
    //
    // Resetting the trie here forces `init_trie` to backfill from the
    // freshly imported accounts on next startup. The backfill produces
    // the SAME root any validator would compute from the same account
    // set, restoring cross-validator determinism.
    //
    // RESET BEFORE SAVE (CR #648): the previous order was
    // save_blockchain → reset_trie. If reset_trie failed mid-flight,
    // the imported accounts were already persisted while the stale
    // trie tables remained — exactly the unsafe mixed state this
    // command exists to prevent. The non-zero-height guard above then
    // blocks retry. Wiping the trie tables first means a failure here
    // leaves a torn-but-detectable state (reset succeeded, no save)
    // and the operator's retry path is clean.
    storage.reset_trie()?;
    storage.save_blockchain(&bc)?;

    println!(
        "State imported: {} accounts from snapshot at height {}",
        count, snapshot.metadata.height
    );
    println!("Trie storage reset — next startup will rebuild it from the imported accounts.");
    println!("Restart the node to rebuild the state trie.");
    Ok(())
}

pub fn cmd_state_verify(input: &str) -> anyhow::Result<()> {
    let json = std::fs::read_to_string(input)?;
    let snapshot: sentrix::core::state_export::StateSnapshot = serde_json::from_str(&json)?;
    let summary = Blockchain::verify_snapshot(&snapshot)?;
    println!("{}", summary);
    Ok(())
}
