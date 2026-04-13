// tests/integration_trie.rs - Sentrix — SentrixTrie integration tests

use secp256k1::{Secp256k1, rand::rngs::OsRng};
use sentrix::core::blockchain::Blockchain;
use sentrix::core::trie::{
    SentrixTrie, address_to_key, account_value_bytes, account_value_decode, empty_hash,
};
use sentrix::wallet::wallet::Wallet;

// ── Helpers ───────────────────────────────────────────────────

fn temp_db() -> (tempfile::TempDir, sled::Db) {
    let dir = tempfile::TempDir::new().unwrap();
    let db  = sled::open(dir.path()).unwrap();
    (dir, db)
}

/// Generate a valid secp256k1 keypair and derive a Sentrix address.
fn make_validator() -> (secp256k1::SecretKey, String, String) {
    let secp = Secp256k1::new();
    let (sk, pk) = secp.generate_keypair(&mut OsRng);
    let addr    = Wallet::derive_address(&pk);
    let pk_hex  = hex::encode(pk.serialize());
    (sk, addr, pk_hex)
}

/// Build a Blockchain with one real validator + trie initialized.
/// Returns (bc, validator_address).
fn setup(db: &sled::Db) -> (Blockchain, String) {
    let (_sk, vaddr, vk_hex) = make_validator();
    let mut bc = Blockchain::new("admin".to_string());
    bc.authority
        .add_validator("admin", vaddr.clone(), "V1".to_string(), vk_hex)
        .unwrap();
    bc.init_trie(db).unwrap();
    (bc, vaddr)
}

// ── Tests ─────────────────────────────────────────────────────

/// A freshly opened trie must equal the canonical empty-tree root.
#[test]
fn test_empty_trie_root() {
    let (_dir, db) = temp_db();
    let trie = SentrixTrie::open(&db, 0).unwrap();
    assert_eq!(trie.root_hash(), empty_hash(0));
}

/// Insert a key, get it back — full roundtrip through sled + LRU.
#[test]
fn test_insert_get() {
    let (_dir, db) = temp_db();
    let mut trie = SentrixTrie::open(&db, 0).unwrap();
    let key = address_to_key("0x1111111111111111111111111111111111111111");
    let val = account_value_bytes(9_999_999, 3);
    trie.insert(&key, &val).unwrap();
    let got = trie.get(&key).unwrap();
    assert_eq!(got.as_deref(), Some(val.as_slice()));
}

/// Membership proof must verify against the current root.
#[test]
fn test_proof_membership() {
    let (_dir, db) = temp_db();
    let mut trie = SentrixTrie::open(&db, 0).unwrap();
    let key = address_to_key("0xaaaa");
    trie.insert(&key, &account_value_bytes(1_000, 0)).unwrap();
    let root  = trie.root_hash();
    let proof = trie.prove(&key).unwrap();
    assert!(proof.found, "key must be found");
    assert!(proof.verify_membership(&root), "membership proof must verify");
}

/// Non-membership proof must verify for a key that was never inserted.
#[test]
fn test_proof_nonmembership() {
    let (_dir, db) = temp_db();
    let mut trie = SentrixTrie::open(&db, 0).unwrap();
    let present = address_to_key("0xaaaa");
    let absent  = address_to_key("0xbbbb");
    trie.insert(&present, &account_value_bytes(1, 0)).unwrap();
    let root  = trie.root_hash();
    let proof = trie.prove(&absent).unwrap();
    assert!(!proof.found, "absent key must not be found");
    assert!(proof.verify_nonmembership(&root), "non-membership proof must verify");
}

/// Committed root at version v must survive further inserts + commits.
#[test]
fn test_versioned_checkout() {
    let (_dir, db) = temp_db();
    let mut trie = SentrixTrie::open(&db, 0).unwrap();
    let k1 = address_to_key("0xaaaa");
    let k2 = address_to_key("0xbbbb");
    trie.insert(&k1, &account_value_bytes(100, 0)).unwrap();
    let root_v1 = trie.commit(1).unwrap();

    trie.insert(&k2, &account_value_bytes(200, 0)).unwrap();
    trie.commit(2).unwrap();

    assert_eq!(trie.root_at_version(1).unwrap(), Some(root_v1));
    assert_ne!(
        trie.root_at_version(1).unwrap(),
        trie.root_at_version(2).unwrap()
    );
}

/// After init_trie + add_block, the block must carry a non-None state_root.
#[test]
fn test_state_root_in_block() {
    let (_dir, db) = temp_db();
    let (mut bc, vaddr) = setup(&db);

    let block = bc.create_block(&vaddr).unwrap();
    bc.add_block(block).unwrap();

    let last = bc.latest_block().unwrap();
    assert!(
        last.state_root.is_some(),
        "block must carry state_root when trie is active"
    );
}

/// Two consecutive blocks must produce different state_roots (validator earns rewards).
#[test]
fn test_state_root_changes_after_tx() {
    let (_dir, db) = temp_db();
    let (mut bc, vaddr) = setup(&db);

    let b1 = bc.create_block(&vaddr).unwrap();
    bc.add_block(b1).unwrap();
    let root1 = bc.latest_block().unwrap().state_root;

    let b2 = bc.create_block(&vaddr).unwrap();
    bc.add_block(b2).unwrap();
    let root2 = bc.latest_block().unwrap().state_root;

    assert!(root1.is_some());
    assert!(root2.is_some());
    assert_ne!(root1, root2, "state root must change as validator earns block rewards");
}

/// Total supply must increment by exactly BLOCK_REWARD per block with trie active.
#[test]
fn test_supply_invariant_with_trie() {
    let (_dir, db) = temp_db();
    let (mut bc, vaddr) = setup(&db);

    let before = bc.total_minted();
    for _ in 0..3 {
        let block = bc.create_block(&vaddr).unwrap();
        bc.add_block(block).unwrap();
    }
    let expected = before + 3 * 100_000_000; // 3 × 1 SRX
    assert_eq!(bc.total_minted(), expected);
}

/// Root committed at each block height must be recoverable via trie_root_at().
#[test]
fn test_historical_state_query() {
    let (_dir, db) = temp_db();
    let (mut bc, vaddr) = setup(&db);

    let mut roots: Vec<[u8; 32]> = Vec::new();
    for _ in 0..3 {
        let block = bc.create_block(&vaddr).unwrap();
        bc.add_block(block).unwrap();
        roots.push(bc.latest_block().unwrap().state_root.unwrap());
    }

    // Each committed root must be retrievable
    for (i, &expected) in roots.iter().enumerate() {
        let version = (i + 1) as u64;
        let stored = bc.trie_root_at(version);
        assert_eq!(stored, Some(expected), "root at version {} must match", version);
    }

    // All roots are distinct (validator balance changes every block)
    assert_ne!(roots[0], roots[1]);
    assert_ne!(roots[1], roots[2]);
}

/// Account state in the trie must match the AccountDB state after block execution.
#[test]
fn test_account_state_in_trie_matches_blockchain() {
    let (_dir, db) = temp_db();
    let (mut bc, vaddr) = setup(&db);

    let block = bc.create_block(&vaddr).unwrap();
    bc.add_block(block).unwrap();

    // Check validator's trie balance equals the AccountDB balance
    let expected_balance = bc.accounts.get_balance(&vaddr);
    let key   = address_to_key(&vaddr);
    let root  = bc.latest_block().unwrap().state_root.unwrap();

    // Re-open a fresh trie view on the same DB to test persistence
    let mut trie = SentrixTrie::open(&db, bc.height()).unwrap();
    let proof = trie.prove(&key).unwrap();

    assert!(proof.found, "validator must be in trie");
    assert!(proof.verify_membership(&root), "proof must verify against block state_root");
    let (bal, _nonce) = account_value_decode(&proof.value).unwrap();
    assert_eq!(bal, expected_balance, "trie balance must match AccountDB");
}

// ── Delete tests ──────────────────────────────────────────────

/// Deleting an existing key must make it unreachable and change the root.
#[test]
fn test_delete_existing_key() {
    let (_dir, db) = temp_db();
    let mut trie = SentrixTrie::open(&db, 0).unwrap();
    let key = address_to_key("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
    let val = account_value_bytes(1_000_000, 5);

    trie.insert(&key, &val).unwrap();
    let root_after_insert = trie.root_hash();

    let root_after_delete = trie.delete(&key).unwrap();

    // Key must be gone
    assert!(trie.get(&key).unwrap().is_none(), "key must be absent after delete");
    // Root must differ from inserted root
    assert_ne!(root_after_delete, root_after_insert);
    // Root must equal empty trie (single item deleted)
    assert_eq!(root_after_delete, empty_hash(0), "single-item delete must restore empty root");
}

/// Deleting a key that was never inserted must be a no-op (same root, no error).
#[test]
fn test_delete_nonexistent_noop() {
    let (_dir, db) = temp_db();
    let mut trie = SentrixTrie::open(&db, 0).unwrap();
    let present = address_to_key("0xaaaa");
    let absent  = address_to_key("0xbbbb");
    trie.insert(&present, &account_value_bytes(100, 0)).unwrap();
    let root_before = trie.root_hash();

    let root_after = trie.delete(&absent).unwrap();

    assert_eq!(root_before, root_after, "delete of absent key must not change root");
    // Present key must still be there
    assert!(trie.get(&present).unwrap().is_some());
}

/// Deleting one of two keys must leave the other intact.
#[test]
fn test_delete_one_of_two_keys() {
    let (_dir, db) = temp_db();
    let mut trie = SentrixTrie::open(&db, 0).unwrap();
    let k1 = address_to_key("0x1111111111111111111111111111111111111111");
    let k2 = address_to_key("0x2222222222222222222222222222222222222222");
    trie.insert(&k1, &account_value_bytes(111, 0)).unwrap();
    trie.insert(&k2, &account_value_bytes(222, 0)).unwrap();

    trie.delete(&k1).unwrap();

    assert!(trie.get(&k1).unwrap().is_none(), "k1 must be deleted");
    let v2 = trie.get(&k2).unwrap().unwrap();
    assert_eq!(account_value_decode(&v2).unwrap().0, 222, "k2 must survive");
}

/// Re-inserting a deleted key must work and produce a fresh valid root.
#[test]
fn test_reinsert_after_delete() {
    let (_dir, db) = temp_db();
    let mut trie = SentrixTrie::open(&db, 0).unwrap();
    let key = address_to_key("0xaaaa");

    trie.insert(&key, &account_value_bytes(100, 0)).unwrap();
    trie.delete(&key).unwrap();
    trie.insert(&key, &account_value_bytes(200, 1)).unwrap();

    let val = trie.get(&key).unwrap().unwrap();
    let (bal, nonce) = account_value_decode(&val).unwrap();
    assert_eq!(bal, 200);
    assert_eq!(nonce, 1);
}

/// Inserting data, committing, reopening the DB, and reading must work (persistence test).
#[test]
fn test_trie_persists_after_restart() {
    let dir = tempfile::TempDir::new().unwrap();

    let key = address_to_key("0xcafecafecafecafecafecafecafecafecafecafe");
    let val = account_value_bytes(999_999, 42);

    // Session 1: insert + commit
    {
        let db = sled::open(dir.path()).unwrap();
        let mut trie = SentrixTrie::open(&db, 0).unwrap();
        trie.insert(&key, &val).unwrap();
        trie.commit(1).unwrap();
        // db drops here — sled flushes on Drop
    }

    // Session 2: reopen and read
    {
        let db = sled::open(dir.path()).unwrap();
        let mut trie = SentrixTrie::open(&db, 1).unwrap();
        let got = trie.get(&key).unwrap();
        assert_eq!(got.as_deref(), Some(val.as_slice()), "data must survive DB reopen");
    }
}
