//! Microbenches for `SentrixTrie` insert + commit on the hot path.
//!
//! Why this exists: the trie's insert path is the single biggest CPU
//! cost in block_executor::apply. A 10% regression here = a 10% drop
//! in chain TPS at sustained load. Catching it pre-merge is cheaper
//! than catching it post-deploy when fullnode lag starts compounding.
//!
//! Bench groups:
//!  - `insert_single`:  one insert into an empty trie. Measures the
//!                      cold-cache path (every node traversal is a
//!                      fresh MDBX read).
//!  - `insert_batch_100`: 100 sequential inserts into a fresh trie.
//!                      Measures the warm-cache path with the
//!                      TrieCache populated mid-flight — the more
//!                      realistic block-apply shape.
//!  - `commit_100`:     same 100 inserts, then commit() the result.
//!                      Catches regressions in the WriteBatch flush
//!                      + state_root recomputation.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use sentrix_trie::{address::account_value_bytes, tree::SentrixTrie};
use sentrix_storage::MdbxStorage;
use std::sync::Arc;
use tempfile::TempDir;

fn temp_trie() -> (TempDir, SentrixTrie) {
    let dir = TempDir::new().unwrap();
    let mdbx = Arc::new(MdbxStorage::open(dir.path()).unwrap());
    let trie = SentrixTrie::open(mdbx, 0).unwrap();
    (dir, trie)
}

fn random_key(seed: u64) -> [u8; 32] {
    // Deterministic per-seed pseudo-random key. We avoid std::random
    // for criterion bench reproducibility — the seed comes from the
    // iteration index so two runs on the same machine produce the
    // same keys (eliminates noise from input drift).
    let mut k = [0u8; 32];
    let mut s = seed;
    for byte in k.iter_mut() {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *byte = (s >> 33) as u8;
    }
    k
}

fn bench_insert_single(c: &mut Criterion) {
    c.bench_function("insert_single", |b| {
        b.iter_batched(
            || {
                let (_dir, trie) = temp_trie();
                let key = random_key(0);
                let val = account_value_bytes(1_000_000, 0);
                // Hold _dir until iter completes; tempdir cleanup is
                // out of measured region.
                (_dir, trie, key, val)
            },
            |(_dir, mut trie, key, val)| {
                let _ = trie.insert(black_box(&key), black_box(&val));
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_insert_batch_100(c: &mut Criterion) {
    c.bench_function("insert_batch_100", |b| {
        b.iter_batched(
            || {
                let (_dir, trie) = temp_trie();
                let mut pairs = Vec::with_capacity(100);
                for i in 0..100 {
                    pairs.push((random_key(i), account_value_bytes(1_000_000 + i, 0)));
                }
                (_dir, trie, pairs)
            },
            |(_dir, mut trie, pairs)| {
                for (key, val) in &pairs {
                    let _ = trie.insert(black_box(key), black_box(val));
                }
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_commit_100(c: &mut Criterion) {
    c.bench_function("commit_after_100_inserts", |b| {
        b.iter_batched(
            || {
                let (_dir, mut trie) = temp_trie();
                for i in 0..100 {
                    let _ = trie.insert(&random_key(i), &account_value_bytes(1_000_000 + i, 0));
                }
                (_dir, trie)
            },
            |(_dir, mut trie)| {
                let _ = trie.commit(black_box(1));
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, bench_insert_single, bench_insert_batch_100, bench_commit_100);
criterion_main!(benches);
