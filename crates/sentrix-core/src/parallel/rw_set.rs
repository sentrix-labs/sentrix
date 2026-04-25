//! Read/write set derivation for parallel transaction execution.
//!
//! Scaffold per `internal design doc` §Phase 1.
//! Pure functions, no I/O, deterministic by construction. The actual
//! body of `derive_access` is `unimplemented!`-free but returns a
//! conservative `Pessimistic` access set so any caller that wires this
//! up before the impl lands gets a no-parallelism-but-correct fallback
//! rather than incorrect parallelism.
//!
//! **Determinism contract (must hold on every implementation):**
//! - `derive_access(tx, validator)` returns the same `TxAccess` for
//!   the same input bytes across all Rust toolchains, all platforms,
//!   and all runs.
//! - All sets use `BTreeSet` — never `HashSet` — so iteration order
//!   is `Ord`-stable across libstd versions.
//! - No floating-point, no `Instant::now()`, no env-var reads, no
//!   process-id, no thread-local state.
//!
//! **Anti-goals for this scaffold:**
//! - Do NOT call this from production block-apply path yet
//! - Do NOT add EVM speculative-execution helpers (Frontier-v2 work)

use std::collections::BTreeSet;

/// Global counters that participate in state_root and may be read or
/// written by transaction application. Each is a single shared slot
/// — any tx that touches one creates a write-write conflict with every
/// other tx that also touches it, forcing them into separate batches.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlobalKey {
    /// Total SRX minted (incremented on coinbase). Every block touches
    /// this in the coinbase application, but coinbase runs sequentially
    /// before any parallel batch, so it doesn't affect batching.
    TotalMinted,
    /// Total SRX burned (incremented on every fee-burn split). Every
    /// non-coinbase tx touches this. Forces a global write conflict
    /// — meaning v1 batching cannot parallelise *fee-paying* transfers
    /// at all. Mitigation in v2: aggregate fee-burns per batch into a
    /// single write at batch-merge time.
    TotalBurned,
}

/// Discriminated union of state keys a transaction can read or write.
/// Used as the comparison primitive for conflict-graph edge detection.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AccountKey {
    /// SRX balance + nonce keyed by Sentrix address (0x-prefixed
    /// 40-hex). Equality on the full string preserves case-insensitive
    /// determinism IF and ONLY IF the protocol normalises addresses
    /// to lowercase at tx-submission time. Frontier impl MUST verify
    /// this normalisation invariant via test before activating.
    Account(String),
    /// EVM contract storage slot (contract_address, slot_index_be32).
    /// EVM txs in v1 do NOT use this fine-grained key — they use
    /// `Pessimistic`. This variant is reserved for the Frontier-v2
    /// speculative-execution path.
    ContractStorage(String, [u8; 32]),
    /// Global counter slots that conflict with every other access to
    /// the same counter.
    GlobalCounter(GlobalKey),
    /// Sentinel "this tx might touch any state" key. Conflicts with
    /// every other key including itself. Used by EVM txs in v1 since
    /// we don't statically know what slots they read/write. Forces
    /// EVM txs to single-tx batches → effectively sequential for EVM.
    Pessimistic,
}

/// Per-transaction read + write access sets. Both are `BTreeSet` so
/// iteration order is deterministic across all platforms and toolchains.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TxAccess {
    pub reads: BTreeSet<AccountKey>,
    pub writes: BTreeSet<AccountKey>,
}

impl TxAccess {
    /// Construct the maximally-conservative access set. Every variant
    /// of `AccountKey::Pessimistic` conflicts with every key (including
    /// other `Pessimistic` instances), so a tx with this access set
    /// always ends up in a singleton batch — equivalent to sequential
    /// execution. Used as the v1 default for EVM txs.
    pub fn pessimistic() -> Self {
        let mut reads = BTreeSet::new();
        reads.insert(AccountKey::Pessimistic);
        let mut writes = BTreeSet::new();
        writes.insert(AccountKey::Pessimistic);
        Self { reads, writes }
    }

    /// Two access sets conflict iff either's write set intersects the
    /// other's read or write set. This is the conflict-graph edge
    /// predicate.
    ///
    /// `Pessimistic` is in every set on both sides for an EVM tx, so
    /// any EVM tx conflicts with every other tx — forcing it into its
    /// own batch.
    pub fn conflicts_with(&self, other: &TxAccess) -> bool {
        self.writes.iter().any(|w| other.reads.contains(w) || other.writes.contains(w))
            || other.writes.iter().any(|w| self.reads.contains(w) || self.writes.contains(w))
    }
}

/// Derive the read/write access set for a transaction. SCAFFOLD
/// VERSION returns `Pessimistic` for everything, which means any
/// caller that wires this up before the real implementation lands
/// gets sequential-equivalent behaviour (every tx in its own batch).
///
/// The real implementation (Phase F-3 of the impl plan) discriminates
/// on tx kind:
/// - Native transfer → reads {from, to}, writes {from, to, validator, TotalBurned}
/// - Token op → reads {from, TOKEN_OP_ADDRESS}, writes {from, TOKEN_OP_ADDRESS, validator}
/// - EVM → `Pessimistic` (v1) or fine-grained slot tracking (v2)
///
/// `_validator` is the block proposer's address — they receive fees,
/// so every fee-paying tx writes to their account. Captured as a
/// parameter (not a global) for testability and determinism.
pub fn derive_access(_tx_payload: &[u8], _validator: &str) -> TxAccess {
    // SCAFFOLD: pessimistic default. Real impl ships in Phase F-3.
    TxAccess::pessimistic()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pessimistic access conflicts with itself — sanity-check that
    /// the worst-case path forces serial execution.
    #[test]
    fn pessimistic_conflicts_with_pessimistic() {
        let a = TxAccess::pessimistic();
        let b = TxAccess::pessimistic();
        assert!(a.conflicts_with(&b));
    }

    /// Disjoint native-transfer-shaped access sets do not conflict.
    /// This is the property the real impl will preserve and the
    /// scheduler relies on for parallelism.
    #[test]
    fn disjoint_native_accesses_do_not_conflict() {
        let mut a = TxAccess::default();
        a.reads.insert(AccountKey::Account("0xa".into()));
        a.reads.insert(AccountKey::Account("0xb".into()));
        a.writes.insert(AccountKey::Account("0xa".into()));
        a.writes.insert(AccountKey::Account("0xb".into()));

        let mut b = TxAccess::default();
        b.reads.insert(AccountKey::Account("0xc".into()));
        b.reads.insert(AccountKey::Account("0xd".into()));
        b.writes.insert(AccountKey::Account("0xc".into()));
        b.writes.insert(AccountKey::Account("0xd".into()));

        assert!(!a.conflicts_with(&b));
    }

    /// Shared-receiver write-write conflict.
    #[test]
    fn shared_write_target_creates_conflict() {
        let mut a = TxAccess::default();
        a.writes.insert(AccountKey::Account("0xshared".into()));

        let mut b = TxAccess::default();
        b.writes.insert(AccountKey::Account("0xshared".into()));

        assert!(a.conflicts_with(&b));
    }

    /// Read-write conflict (one tx reads what another writes).
    #[test]
    fn read_write_overlap_creates_conflict() {
        let mut reader = TxAccess::default();
        reader.reads.insert(AccountKey::Account("0xshared".into()));

        let mut writer = TxAccess::default();
        writer.writes.insert(AccountKey::Account("0xshared".into()));

        assert!(reader.conflicts_with(&writer));
        assert!(writer.conflicts_with(&reader));
    }

    /// `derive_access` scaffold returns pessimistic — this pins the
    /// SCAFFOLD contract. When the real impl lands and starts
    /// returning fine-grained access sets, this test must be updated
    /// (intentionally, signalling that the scaffold is being replaced).
    #[test]
    fn scaffold_derive_access_is_pessimistic() {
        let access = derive_access(b"any tx bytes", "0xvalidator");
        assert_eq!(access, TxAccess::pessimistic());
    }

    /// Determinism: same input → same output, every time.
    #[test]
    fn derive_access_is_deterministic() {
        let payload = b"some tx payload bytes";
        let a1 = derive_access(payload, "0xvalidator");
        let a2 = derive_access(payload, "0xvalidator");
        let a3 = derive_access(payload, "0xvalidator");
        assert_eq!(a1, a2);
        assert_eq!(a2, a3);
    }

    /// BTreeSet iteration order is deterministic — verify by
    /// constructing the same access set two different ways and
    /// asserting they iterate identically.
    #[test]
    fn access_set_iteration_is_deterministic() {
        let keys = vec![
            AccountKey::Account("0xc".into()),
            AccountKey::Account("0xa".into()),
            AccountKey::Account("0xb".into()),
        ];

        let mut a = TxAccess::default();
        for k in &keys {
            a.reads.insert(k.clone());
        }

        let mut b = TxAccess::default();
        for k in keys.iter().rev() {
            b.reads.insert(k.clone());
        }

        let a_iter: Vec<_> = a.reads.iter().collect();
        let b_iter: Vec<_> = b.reads.iter().collect();
        assert_eq!(a_iter, b_iter, "BTreeSet iteration must be order-independent of insertion");
    }
}
