//! Conflict-graph scheduler for parallel transaction batching.
//!
//! Scaffold per `internal design doc` §Phase 2.
//! `build_batches` is currently a sequential-equivalent stub: it returns
//! one tx per batch, preserving original block order. This is correct
//! (every batch has zero internal conflicts) but extracts zero
//! parallelism. The real implementation does greedy graph colouring.
//!
//! **Determinism contract:**
//! - Same input txs + same validator → byte-identical Vec<Batch> output
//!   on every run, every machine, every Rust toolchain
//! - Within a batch, txs are ordered by their original block index
//! - Across batches, batches are ordered by their lowest tx index
//! - Algorithm uses `BTreeSet` / `BTreeMap` only, never `HashSet` / `HashMap`,
//!   so iteration order is `Ord`-stable across libstd versions

use crate::parallel::rw_set::{TxAccess, derive_access};

/// A batch of transactions that can execute in parallel because no two
/// txs in the batch have conflicting r/w access sets. The `tx_indices`
/// reference positions in the original block transaction list — they
/// are NOT a copy of the transaction data, just indices.
///
/// Indices within a batch are sorted ascending. The implementer
/// applying the batch reads txs from the block in this order — the
/// per-tx order within a batch is not consensus-relevant (they don't
/// touch shared state by construction) but pinning it eliminates
/// non-determinism risk from batch-internal ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub tx_indices: Vec<usize>,
}

impl Batch {
    pub fn singleton(idx: usize) -> Self {
        Self { tx_indices: vec![idx] }
    }
}

/// Build the parallel-execution batches for `txs` produced by `validator`.
///
/// SCAFFOLD: returns one tx per batch (sequential-equivalent). Every
/// batch is internally conflict-free (trivially — only one tx per
/// batch), so the parallel apply path is safe even with the stub.
/// The real impl (Phase F-4 of the impl plan) does greedy graph
/// colouring to pack independent txs into shared batches.
///
/// `txs` is `&[T]` over an opaque byte representation so this module
/// stays decoupled from the `Transaction` struct's concrete shape.
/// The real impl will accept `&[Transaction]` once it lands; the
/// stub takes anything that can produce a payload-byte view via
/// `AsRef<[u8]>`.
///
/// Coinbase txs (block.transactions[0]) are NEVER batched here —
/// the apply path runs coinbase sequentially before any parallel
/// batch executes. `build_batches` operates on `block.transactions[1..]`.
pub fn build_batches<T: AsRef<[u8]>>(txs: &[T], validator: &str) -> Vec<Batch> {
    // SCAFFOLD: one batch per tx, in original order. Real impl ships
    // in Phase F-4 — greedy colouring over conflict graph.
    let _accesses: Vec<TxAccess> = txs
        .iter()
        .map(|tx| derive_access(tx.as_ref(), validator))
        .collect();

    txs.iter().enumerate().map(|(i, _)| Batch::singleton(i)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stub: every tx ends up in its own singleton batch.
    #[test]
    fn scaffold_emits_singleton_batches() {
        let txs: Vec<&[u8]> = vec![b"a", b"b", b"c"];
        let batches = build_batches(&txs, "0xvalidator");
        assert_eq!(batches.len(), 3);
        for (i, batch) in batches.iter().enumerate() {
            assert_eq!(batch.tx_indices, vec![i], "batch {} should be singleton", i);
        }
    }

    /// Empty input → empty output, not panic.
    #[test]
    fn empty_block_yields_empty_batches() {
        let txs: Vec<&[u8]> = vec![];
        let batches = build_batches(&txs, "0xvalidator");
        assert!(batches.is_empty());
    }

    /// Determinism: same input → same output, every time. Property
    /// the real impl must preserve. Pinning it now prevents future
    /// regressions in the scaffold contract.
    #[test]
    fn build_batches_is_deterministic() {
        let txs: Vec<&[u8]> = vec![b"alpha", b"bravo", b"charlie", b"delta"];
        let b1 = build_batches(&txs, "0xv1");
        let b2 = build_batches(&txs, "0xv1");
        let b3 = build_batches(&txs, "0xv1");
        assert_eq!(b1, b2);
        assert_eq!(b2, b3);
    }

    /// Different validator → still same batching (validator only
    /// affects access sets, not which txs are present). Real impl
    /// MAY produce different batches if the validator address shows
    /// up as a write target — pinning the scaffold's behaviour here
    /// will signal the test needs updating when the impl lands.
    #[test]
    fn scaffold_validator_independent() {
        let txs: Vec<&[u8]> = vec![b"x", b"y"];
        let v1 = build_batches(&txs, "0xv1");
        let v2 = build_batches(&txs, "0xv2");
        assert_eq!(v1, v2);
    }

    /// Batches preserve original tx order. Required for the apply
    /// path's commit ordering — txs within a batch are conflict-free
    /// so any internal order is correct, but pinning ascending order
    /// removes ambiguity for log readability + debug.
    #[test]
    fn batches_preserve_ascending_order() {
        let txs: Vec<&[u8]> = vec![b"0", b"1", b"2", b"3", b"4"];
        let batches = build_batches(&txs, "0xv");
        let flat: Vec<usize> = batches.iter().flat_map(|b| b.tx_indices.iter().copied()).collect();
        assert_eq!(flat, vec![0, 1, 2, 3, 4]);
    }
}
