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

#[test]
fn canonical_delete_to_empty_matches_fresh_empty() {
    // Deleting the last key must return the canonical empty-tree root.
    let a = [7u8; 32];
    let (_d0, empty) = fresh_trie(true);
    let empty_root = hex::encode(empty.root_hash());

    let (_d1, mut t) = fresh_trie(true);
    t.insert(&a, b"v").unwrap();
    t.delete(&a).unwrap();
    assert_eq!(hex::encode(t.root_hash()), empty_root);
    assert_eq!(t.get(&a).unwrap(), None);
}

#[test]
fn canonical_delete_keeps_multikey_sibling_subtree() {
    // A,B share a 255-bit prefix (a deep two-key subtree); C diverges from both
    // at bit 0. Deleting C — whose sibling is the A/B internal subtree, not a
    // lone leaf — must leave exactly {A,B}, matching a direct {A,B} insert.
    let a = [0u8; 32];
    let mut b = [0u8; 32];
    b[31] = 1;
    let mut c = [0u8; 32];
    c[0] = 0x80;

    let (_d1, mut t_ab) = fresh_trie(true);
    t_ab.insert(&a, b"va").unwrap();
    t_ab.insert(&b, b"vb").unwrap();
    let root_ab = hex::encode(t_ab.root_hash());

    let (_d2, mut t_abc) = fresh_trie(true);
    t_abc.insert(&a, b"va").unwrap();
    t_abc.insert(&b, b"vb").unwrap();
    t_abc.insert(&c, b"vc").unwrap();
    t_abc.delete(&c).unwrap();
    let root_abc_minus_c = hex::encode(t_abc.root_hash());

    assert_eq!(
        root_ab, root_abc_minus_c,
        "deleting C must leave the A/B subtree exactly as a direct {{A,B}} insert"
    );
    // A and B still present and intact, C gone.
    assert_eq!(t_abc.get(&a).unwrap().as_deref(), Some(&b"va"[..]));
    assert_eq!(t_abc.get(&b).unwrap().as_deref(), Some(&b"vb"[..]));
    assert_eq!(t_abc.get(&c).unwrap(), None);
}
