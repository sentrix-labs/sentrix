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

// ── Native-module state-root activation preflight (read-only) ──────────
//
// `sentrix state preflight` reports whether THIS node looks ready for a
// native-module state_root activation (see
// `audits/native-state-in-trie-activation-playbook.md`). It is strictly
// read-only: it opens the existing DB, loads the persisted Blockchain, reads
// fields, and prints. It never writes, never inits/resets the trie, never sets
// env, never enables a fork, and makes no network calls.
//
// A single node cannot prove cross-validator agreement — the operator must
// compare the reported `src20_canonical_hash` / `nft_canonical_hash` /
// `state_root` across ALL validators (per the playbook). This tool surfaces the
// per-node values + a local verdict to make that comparison possible.

/// One fork gate's status.
struct GateStatus {
    enabled: bool,
    /// Activation height when enabled; `None` when disabled (`u64::MAX`).
    activation_height: Option<u64>,
}

impl GateStatus {
    fn read(height: u64) -> Self {
        if height == u64::MAX {
            GateStatus {
                enabled: false,
                activation_height: None,
            }
        } else {
            GateStatus {
                enabled: true,
                activation_height: Some(height),
            }
        }
    }
}

/// Read-only preflight snapshot for a single node.
struct PreflightReport {
    version: String,
    height: u64,
    tip_hash: String,
    state_root: Option<String>,
    src20_canonical_hash: String,
    nft_canonical_hash: String,
    native_state_in_trie_gate: GateStatus,
    nft_tokenop_gate: GateStatus,
    verdict: &'static str,
    notes: Vec<String>,
}

/// Build the preflight report from an already-loaded chain. Pure (no I/O), so
/// it is unit-testable without a DB. `native_gate_height` / `nft_gate_height`
/// are the configured fork heights (passed in so the caller owns the env read).
fn build_preflight_report(
    bc: &Blockchain,
    native_gate_height: u64,
    nft_gate_height: u64,
) -> PreflightReport {
    let height = bc.height();
    let tip = bc.latest_block().ok();
    let tip_hash = tip.map(|b| b.hash.clone()).unwrap_or_default();
    let state_root = tip.and_then(|b| b.state_root).map(hex::encode);

    let src20_canonical_hash = hex::encode(bc.contracts.canonical_hash());
    let nft_canonical_hash = hex::encode(bc.nft_registry.canonical_hash());

    let native = GateStatus::read(native_gate_height);
    let nft = GateStatus::read(nft_gate_height);

    // Verdict: NOT_READY for hard problems (unreadable tip), WARNING for things
    // the operator must notice, READY for a clean pre-activation baseline.
    let mut notes: Vec<String> = Vec::new();
    let mut warn = false;
    let mut not_ready = false;

    if tip.is_none() {
        notes.push("chain not initialized / tip block unreadable".into());
        not_ready = true;
    }
    if height == 0 {
        notes.push("chain at genesis height 0 — nothing committed yet".into());
        warn = true;
    }
    if state_root.is_none() && tip.is_some() {
        notes.push(
            "tip block has no state_root (pre-trie block) — STATE_IN_TRIE may not be active on this node".into(),
        );
        warn = true;
    }
    if native.enabled {
        notes.push(format!(
            "native state-trie gate ALREADY pinned at height {} — activation already scheduled/done; do not re-pin",
            native.activation_height.unwrap_or_default()
        ));
        warn = true;
    }
    if nft.enabled {
        notes.push(
            "NFT TokenOp gate is ENABLED — per the playbook it must NOT be activated together with native state-trie".into(),
        );
        warn = true;
    }
    if !not_ready && !warn {
        notes.push(
            "clean pre-activation baseline — compare src20_canonical_hash, nft_canonical_hash and state_root across ALL validators before pinning a height (see activation playbook)".into(),
        );
    }

    let verdict = if not_ready {
        "NOT_READY"
    } else if warn {
        "WARNING"
    } else {
        "READY"
    };

    PreflightReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        height,
        tip_hash,
        state_root,
        src20_canonical_hash,
        nft_canonical_hash,
        native_state_in_trie_gate: native,
        nft_tokenop_gate: nft,
        verdict,
        notes,
    }
}

fn gate_str(g: &GateStatus) -> String {
    match g.activation_height {
        Some(h) => format!("ENABLED (activation height {h})"),
        None => "DISABLED (u64::MAX)".to_string(),
    }
}

fn render_text(r: &PreflightReport) -> String {
    let mut out = String::new();
    out.push_str("Native-module state-root activation preflight (read-only)\n");
    out.push_str("=========================================================\n");
    out.push_str(&format!("binary version         : {}\n", r.version));
    out.push_str(&format!("chain height           : {}\n", r.height));
    out.push_str(&format!("tip hash               : {}\n", r.tip_hash));
    out.push_str(&format!(
        "state_root (tip)       : {}\n",
        r.state_root.as_deref().unwrap_or("<none>")
    ));
    out.push_str(&format!(
        "SRC-20 canonical hash  : {}\n",
        r.src20_canonical_hash
    ));
    out.push_str(&format!(
        "NFT canonical hash     : {}\n",
        r.nft_canonical_hash
    ));
    out.push_str(&format!(
        "NATIVE_STATE_IN_TRIE   : {}\n",
        gate_str(&r.native_state_in_trie_gate)
    ));
    out.push_str(&format!(
        "NFT_TOKENOP            : {}\n",
        gate_str(&r.nft_tokenop_gate)
    ));
    out.push_str(&format!("\nVERDICT: {}\n", r.verdict));
    for n in &r.notes {
        out.push_str(&format!("  - {n}\n"));
    }
    out.push_str(
        "\nNote: this reports THIS node only. Activation requires the SAME hashes\n\
         and state_root across ALL validators — compare before pinning a height.\n",
    );
    out
}

fn render_json(r: &PreflightReport) -> String {
    let v = serde_json::json!({
        "version": r.version,
        "height": r.height,
        "tip_hash": r.tip_hash,
        "state_root": r.state_root,
        "src20_canonical_hash": r.src20_canonical_hash,
        "nft_canonical_hash": r.nft_canonical_hash,
        "native_state_in_trie_gate": {
            "enabled": r.native_state_in_trie_gate.enabled,
            "activation_height": r.native_state_in_trie_gate.activation_height,
        },
        "nft_tokenop_gate": {
            "enabled": r.nft_tokenop_gate.enabled,
            "activation_height": r.nft_tokenop_gate.activation_height,
        },
        "verdict": r.verdict,
        "notes": r.notes,
    });
    // serde_json::Value serializes object keys in sorted order (BTreeMap), so
    // this is deterministic across runs.
    serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".to_string())
}

/// `sentrix state preflight [--json]` — read-only activation readiness report.
pub fn cmd_state_preflight(json: bool) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    let native = sentrix::core::fork_heights::get_native_state_in_trie_height();
    let nft = sentrix::core::fork_heights::get_nft_tokenop_height();
    let report = build_preflight_report(&bc, native, nft);

    if json {
        println!("{}", render_json(&report));
    } else {
        print!("{}", render_text(&report));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISABLED: u64 = u64::MAX;

    fn fresh() -> Blockchain {
        Blockchain::new("admin".to_string())
    }

    // 1. Empty registries → stable report (same twice, hashes present).
    #[test]
    fn empty_registry_preflight_stable() {
        let bc = fresh();
        let a = build_preflight_report(&bc, DISABLED, DISABLED);
        let b = build_preflight_report(&bc, DISABLED, DISABLED);
        assert_eq!(a.src20_canonical_hash, b.src20_canonical_hash);
        assert_eq!(a.nft_canonical_hash, b.nft_canonical_hash);
        assert!(!a.src20_canonical_hash.is_empty());
        assert!(!a.nft_canonical_hash.is_empty());
        assert_eq!(render_json(&a), render_json(&b));
    }

    // 2. Populated SRC-20 registry changes the reported hash.
    #[test]
    fn populated_src20_changes_hash() {
        let empty = build_preflight_report(&fresh(), DISABLED, DISABLED);
        let mut bc = fresh();
        bc.contracts
            .deploy("0xdeployer", "Tok", "TOK", 8, 1_000, 0, "seed")
            .unwrap();
        let populated = build_preflight_report(&bc, DISABLED, DISABLED);
        assert_ne!(
            empty.src20_canonical_hash, populated.src20_canonical_hash,
            "SRC-20 deploy must change the reported canonical hash"
        );
    }

    // 3. Populated NFT registry changes the reported hash.
    #[test]
    fn populated_nft_changes_hash() {
        let empty = build_preflight_report(&fresh(), DISABLED, DISABLED);
        let mut bc = fresh();
        bc.nft_registry
            .deploy_collection("0xcreator", "C", "C", "u", None, true, true, "seed")
            .unwrap();
        let populated = build_preflight_report(&bc, DISABLED, DISABLED);
        assert_ne!(
            empty.nft_canonical_hash, populated.nft_canonical_hash,
            "NFT deploy must change the reported canonical hash"
        );
    }

    // 4. Disabled gates are reported clearly.
    #[test]
    fn disabled_gates_reported() {
        let r = build_preflight_report(&fresh(), DISABLED, DISABLED);
        assert!(!r.native_state_in_trie_gate.enabled);
        assert!(r.native_state_in_trie_gate.activation_height.is_none());
        assert!(!r.nft_tokenop_gate.enabled);
        assert!(r.nft_tokenop_gate.activation_height.is_none());
        assert!(render_text(&r).contains("DISABLED (u64::MAX)"));
    }

    // Enabled native gate surfaces a WARNING + the pinned height.
    #[test]
    fn enabled_native_gate_warns() {
        let r = build_preflight_report(&fresh(), 1_000, DISABLED);
        assert!(r.native_state_in_trie_gate.enabled);
        assert_eq!(r.native_state_in_trie_gate.activation_height, Some(1_000));
        assert_eq!(r.verdict, "WARNING");
        assert!(r.notes.iter().any(|n| n.contains("pinned at height")));
    }

    // 5. JSON output is deterministic for identical state.
    #[test]
    fn json_output_deterministic() {
        let mut bc = fresh();
        bc.contracts
            .deploy("0xdeployer", "Tok", "TOK", 8, 1_000, 0, "seed")
            .unwrap();
        let r1 = build_preflight_report(&bc, DISABLED, DISABLED);
        let r2 = build_preflight_report(&bc, DISABLED, DISABLED);
        assert_eq!(render_json(&r1), render_json(&r2));
        let parsed: serde_json::Value = serde_json::from_str(&render_json(&r1)).unwrap();
        assert!(parsed.get("verdict").is_some());
        assert!(parsed.get("src20_canonical_hash").is_some());
    }
}
