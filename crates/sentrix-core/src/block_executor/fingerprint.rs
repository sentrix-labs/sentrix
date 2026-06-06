//! State fingerprinting + apply profiling — cross-node drift diagnosis.
//!
//! Pure code motion out of `block_executor.rs` (no logic change). These are
//! debug/observability helpers, gated behind `SENTRIX_STATE_FINGERPRINT` /
//! `SENTRIX_APPLY_PROFILE`; they never feed consensus. Kept together so the
//! apply path in `block_executor.rs` stays focused on state transition.

use crate::blockchain::Blockchain;

/// Per-block apply-phase profile emitter. Single exit point for the
/// `apply-profile h=... txs=... tx_apply=...ms trie=...ms post=...ms total=...ms`
/// log line, so every return path through `apply_block_pass2` records
/// timing when SENTRIX_APPLY_PROFILE=1. Pre-helper, the legacy-tolerated
/// branch returned without emitting — replay of pre-cutoff blocks left a
/// gap in the profile log.
///
/// No-op when any of the three timestamps is None (profiling disabled).
pub(crate) fn emit_apply_profile(
    t0: Option<std::time::Instant>,
    t1: Option<std::time::Instant>,
    t2: Option<std::time::Instant>,
    height: u64,
    txs: usize,
) {
    if let (Some(t0), Some(t1), Some(t2)) = (t0, t1, t2) {
        let t3 = std::time::Instant::now();
        tracing::info!(
            target: "apply_profile",
            "apply-profile h={} txs={} tx_apply={}ms trie={}ms post={}ms total={}ms",
            height,
            txs,
            t1.duration_since(t0).as_millis(),
            t2.duration_since(t1).as_millis(),
            t3.duration_since(t2).as_millis(),
            t3.duration_since(t0).as_millis(),
        );
    }
}

/// State-drift fingerprint emitter (debug-only). When
/// `SENTRIX_STATE_FINGERPRINT=1` the apply path calls this at the end
/// of every block; cross-host log diff at next halt pinpoints the
/// first diverging block.
///
/// What we hash:
///   1. AccountDB.accounts — sorted by address, each (balance, nonce,
///      code_hash, storage_root)
///   2. AccountDB.total_burned
///   3. AccountDB.contract_code — sorted by code_hash, each
///      sha256(bytecode)
///   4. AccountDB.contract_storage — sorted by composite key, raw
///      value bytes
///   5. Blockchain.total_minted
///
/// We deliberately skip stake_registry — bincode HashMap serialisation
/// is non-deterministic, and the drift we're chasing manifests in
/// AccountDB (via trie roots that read from AccountDB).
///
/// Output line shape:
///   [STATE-FP] h=<height> acc=<8-byte-hex> fp=<8-byte-hex>
///
/// Cost when enabled: ~O(N) sha256 over AccountDB content per block,
/// where N = total accounts + contract storage entries. At Sentrix
/// mainnet scale (~tens of thousands of accounts, sparse contract
/// storage) this adds ~1-2 ms per block. Acceptable for debug runs.
pub(crate) fn emit_state_fingerprint(bc: &Blockchain, height: u64) {
    if std::env::var_os("SENTRIX_STATE_FINGERPRINT").is_none() {
        return;
    }
    let (acc_fp, fp) = compute_state_fingerprint(bc);
    // eprintln! matches the existing `[V2-DBG]` trace pattern in
    // `update_trie_for_block` — guaranteed journalctl visibility
    // regardless of RUST_LOG / tracing subscriber filter config.
    eprintln!(
        "[STATE-FP] h={} acc={} fp={}",
        height,
        hex::encode(&acc_fp[..8]),
        hex::encode(&fp[..8]),
    );
}

/// Compute `(account_fingerprint, combined_fingerprint)` for the current
/// state. Split out of `emit_state_fingerprint` so it's testable without the
/// `SENTRIX_STATE_FINGERPRINT` env gate or stderr capture.
///
/// The combined fingerprint folds, in fixed order:
///   account state · total_minted · SRC-20 ContractRegistry · NFT NftRegistry
///
/// Native-module state commitment (2026-06-03): before this, the fingerprint
/// hashed only accounts + total_minted + EVM code/storage, so two validators
/// could diverge purely in SRC-20 or NFT state and still print identical
/// `[STATE-FP]` lines — the incident tool was blind on those paths. Both
/// registries now contribute via their `canonical_hash()` (sorted, HashMap-
/// order-independent). Consensus-enforced trie/state_root commitment of the
/// same state is the fork-gated follow-up; this closes the debug-visibility
/// gap without a consensus change.
pub(crate) fn compute_state_fingerprint(bc: &Blockchain) -> ([u8; 32], [u8; 32]) {
    use sha2::{Digest, Sha256};

    let mut acc_hasher = Sha256::new();
    let mut accounts: Vec<(&String, &sentrix_primitives::account::Account)> =
        bc.accounts.accounts.iter().collect();
    accounts.sort_by(|a, b| a.0.cmp(b.0));
    for (addr, account) in accounts {
        acc_hasher.update(addr.as_bytes());
        acc_hasher.update(account.balance.to_be_bytes());
        acc_hasher.update(account.nonce.to_be_bytes());
        acc_hasher.update(account.code_hash);
        acc_hasher.update(account.storage_root);
    }
    acc_hasher.update(bc.accounts.total_burned.to_be_bytes());

    let mut codes: Vec<(&String, &Vec<u8>)> = bc.accounts.contract_code.iter().collect();
    codes.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in codes {
        acc_hasher.update(k.as_bytes());
        let h: [u8; 32] = Sha256::digest(v).into();
        acc_hasher.update(h);
    }

    let mut storage: Vec<(&String, &Vec<u8>)> = bc.accounts.contract_storage.iter().collect();
    storage.sort_by(|a, b| a.0.cmp(b.0));
    for (k, v) in storage {
        acc_hasher.update(k.as_bytes());
        acc_hasher.update(v);
    }

    let acc_fp: [u8; 32] = acc_hasher.finalize().into();
    let mut combined = Sha256::new();
    combined.update(acc_fp);
    combined.update(bc.total_minted.to_be_bytes());
    // Native-module state: SRC-20 then NFT, each canonical (sorted) so
    // HashMap iteration order can't move the fingerprint.
    combined.update(bc.contracts.canonical_hash());
    combined.update(bc.nft_registry.canonical_hash());
    let fp: [u8; 32] = combined.finalize().into();
    (acc_fp, fp)
}

/// Per-component state fingerprints for cross-node drift diagnosis.
///
/// `compute_state_fingerprint` folds everything into one root, which tells
/// you THAT two nodes diverged but not WHERE. This splits each component out
/// (accounts / EVM code / EVM storage / mint / burn / SRC-20 / NFT) so a diff
/// across nodes at the same height points straight at the divergent subsystem.
/// Added 2026-06-04 chasing the post-#782 state-determinism drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateComponents {
    /// balance + nonce + code_hash + storage_root over sorted accounts.
    pub accounts_fp: [u8; 32],
    /// hash over sorted EVM contract bytecode.
    pub contract_code_fp: [u8; 32],
    /// hash over sorted EVM contract storage slots.
    pub contract_storage_fp: [u8; 32],
    pub total_minted: u64,
    pub total_burned: u64,
    /// SRC-20 native registry canonical hash.
    pub src20_fp: [u8; 32],
    /// NFT native registry canonical hash.
    pub nft_fp: [u8; 32],
    /// account count (cheap sanity signal alongside the hashes).
    pub account_count: usize,
}

/// Compute the per-component fingerprints — see [`StateComponents`].
pub fn state_components(bc: &Blockchain) -> StateComponents {
    use sha2::{Digest, Sha256};

    let mut accounts: Vec<(&String, &sentrix_primitives::account::Account)> =
        bc.accounts.accounts.iter().collect();
    accounts.sort_by(|a, b| a.0.cmp(b.0));
    let account_count = accounts.len();
    let mut acc_h = Sha256::new();
    for (addr, account) in accounts {
        acc_h.update(addr.as_bytes());
        acc_h.update(account.balance.to_be_bytes());
        acc_h.update(account.nonce.to_be_bytes());
        acc_h.update(account.code_hash);
        acc_h.update(account.storage_root);
    }

    let mut codes: Vec<(&String, &Vec<u8>)> = bc.accounts.contract_code.iter().collect();
    codes.sort_by(|a, b| a.0.cmp(b.0));
    let mut code_h = Sha256::new();
    for (k, v) in codes {
        code_h.update(k.as_bytes());
        let h: [u8; 32] = Sha256::digest(v).into();
        code_h.update(h);
    }

    let mut storage: Vec<(&String, &Vec<u8>)> = bc.accounts.contract_storage.iter().collect();
    storage.sort_by(|a, b| a.0.cmp(b.0));
    let mut stor_h = Sha256::new();
    for (k, v) in storage {
        stor_h.update(k.as_bytes());
        stor_h.update(v);
    }

    StateComponents {
        accounts_fp: acc_h.finalize().into(),
        contract_code_fp: code_h.finalize().into(),
        contract_storage_fp: stor_h.finalize().into(),
        total_minted: bc.total_minted,
        total_burned: bc.accounts.total_burned,
        src20_fp: bc.contracts.canonical_hash(),
        nft_fp: bc.nft_registry.canonical_hash(),
        account_count,
    }
}
