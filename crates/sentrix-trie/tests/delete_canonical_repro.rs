//! Canonical-delete fork: `delete` must restore the SMT to the exact shape a
//! direct insert produces, so the root is a pure function of the current
//! key->value set (insertion/deletion-history independent).
//!
//! Pre-fork (`set_canonical_delete(false)`, the default) `delete` leaves a
//! surviving sibling leaf at its pushed-down depth, so the root of {A} depends
//! on whether a sibling was ever inserted+deleted — the non-pure state_root bug
//! that let validators with different block-apply subsets diverge.
//!
//! Post-fork (`set_canonical_delete(true)`) the lone sibling leaf is pulled
//! back up and the two roots match.

use sentrix_storage::MdbxStorage;
use sentrix_trie::SentrixTrie;
use std::sync::Arc;

fn fresh_trie(canonical: bool) -> (tempfile::TempDir, SentrixTrie) {
    let dir = tempfile::tempdir().unwrap();
    let mdbx = Arc::new(MdbxStorage::open(dir.path()).unwrap());
    let mut trie = SentrixTrie::open(mdbx, 0).unwrap();
    trie.set_canonical_delete(canonical);
    (dir, trie)
}

/// root of {A} reached directly vs reached via "insert A, insert B, delete B".
/// A and B share a 255-bit prefix so inserting B pushes A down to depth 255.
fn roots(canonical: bool) -> (String, String) {
    let a = [0u8; 32];
    let mut b = [0u8; 32];
    b[31] = 1;

    let (_d1, mut t1) = fresh_trie(canonical);
    t1.insert(&a, b"valueA").unwrap();
    let direct = hex::encode(t1.root_hash());

    let (_d2, mut t2) = fresh_trie(canonical);
    t2.insert(&a, b"valueA").unwrap();
    t2.insert(&b, b"valueB").unwrap();
    t2.delete(&b).unwrap();
    let after_delete = hex::encode(t2.root_hash());

    (direct, after_delete)
}

#[test]
fn canonical_delete_root_is_history_independent() {
    let (direct, after_delete) = roots(true);
    assert_eq!(
        direct, after_delete,
        "post-fork: root of {{A}} must be identical regardless of delete history"
    );
}

#[test]
fn legacy_delete_root_still_history_dependent() {
    // Documents the pre-fork behaviour the gate preserves: without the fix the
    // surviving leaf is left deep, so the two roots differ.
    let (direct, after_delete) = roots(false);
    assert_ne!(
        direct, after_delete,
        "pre-fork (legacy) root is expected to depend on delete history"
    );
}
