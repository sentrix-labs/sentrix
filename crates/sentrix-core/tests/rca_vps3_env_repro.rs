//! VPS3 RCA harness — environment-dependent state_root reproduction.
//!
//! Filed 2026-04-23 after the VPS3 recurring-divergence RCA narrowed to
//! "VPS3's own-block `apply_block_pass2` produces different state_root
//! than VPS1+VPS2 compute on the same block payload." Static audits
//! (HashMap iter + bincode serialisation across consensus crates) turned
//! up no smoking gun, so the remaining hypotheses are environmental:
//! kernel 6.8 vs 5.15, glibc 2.39 vs 2.35, AMD EPYC vs KVM-Common CPU.
//!
//! This file is a scaffold — the tests are `#[ignore]` by default because
//! they require a real chain.db on disk (too large to fixture into the
//! repo). Run manually on two different host environments with the same
//! chain.db snapshot; if the printed state_roots diverge, env is the
//! source.
//!
//! ## Operator workflow
//!
//! 1. Copy a canonical chain.db snapshot to VPS1 (Ubuntu 22.04) and to
//!    VPS4 (Ubuntu 24.04 / AMD EPYC — matches VPS3's env exactly).
//! 2. Run `cargo test -p sentrix-core --test rca_vps3_env_repro -- --ignored --nocapture`
//!    on each host, pointing `TEST_CHAIN_DB` at the snapshot path and
//!    `TEST_HEIGHT_FROM` / `TEST_HEIGHT_TO` at the range of interest.
//! 3. Compare the final line "COMPUTED_ROOT=..." from each host's output.
//!    Equal → env is NOT the divergence source (rule out kernel/glibc/CPU
//!    at the userspace-deterministic layer; look deeper at MDBX/hardware).
//!    Different → env IS a source; narrow by swapping one variable at a
//!    time (e.g. run the 24.04 userspace inside a 22.04 kernel container).
//!
//! ## Prereq: GetBlocks sliding-window fix (BACKLOG #14)
//!
//! A copied-and-running node on VPS4 can't catch up via gossipsub because
//! the `GetBlocks` handler only serves the in-memory `CHAIN_WINDOW_SIZE`
//! (~1000 blocks). This OFFLINE harness sidesteps that by reading blocks
//! directly from the MDBX store, so the sync limitation doesn't block
//! this test. BACKLOG #14 fixes the sync path itself for the LIVE
//! experiment case.

use sentrix_core::blockchain::Blockchain;
use sentrix_core::storage::Storage;
use std::sync::Arc;

/// Load a Blockchain from an existing chain.db path (read-only).
///
/// Caller must set `SENTRIX_ALLOW_UNENCRYPTED_DISK=true` if the snapshot
/// lives on an unencrypted volume (the default for dev/test setups).
fn load_existing_chain(path: &str) -> Blockchain {
    let storage = Storage::open(path).expect("chain.db open failed — check path + encryption env vars");
    let mut bc = storage
        .load_blockchain()
        .expect("load_blockchain failed")
        .expect("chain.db has no persisted state — was `sentrix init` ever run?");
    let mdbx = storage.mdbx_arc();
    bc.init_trie(Arc::clone(&mdbx))
        .expect("init_trie failed — trie storage may be corrupted or rejected by v2.1.5 safeguard");
    bc
}

/// Compute + print the trie_root at a specific height, as already committed
/// by this host's prior block-apply runs. Useful for a read-only cross-host
/// comparison: if `VPS1_host::trie_root_at(H) != VPS4_host::trie_root_at(H)`
/// on the SAME chain.db snapshot, the trie layer itself diverges by env —
/// strong env confirmation.
///
/// NOTE: this only reads persisted trie state; it does NOT re-apply the
/// block. For a true replay test see `replay_and_compare` below.
#[test]
#[ignore = "requires real chain.db — run manually per file header"]
fn read_committed_trie_root() {
    let path = std::env::var("TEST_CHAIN_DB")
        .expect("set TEST_CHAIN_DB=/path/to/chain.db");
    let height: u64 = std::env::var("TEST_HEIGHT")
        .expect("set TEST_HEIGHT=<block index>")
        .parse()
        .expect("TEST_HEIGHT must be an integer");

    let bc = load_existing_chain(&path);
    let root = bc
        .trie_root_at(height)
        .expect("no committed trie root at the requested height");

    println!("CHAIN_DB={}", path);
    println!("HEIGHT={}", height);
    println!("COMPUTED_ROOT={}", hex::encode(root));
}

/// Replay a block range against a fresh trie: start from the committed
/// state at `FROM-1`, re-apply blocks `FROM..=TO`, compare each recomputed
/// state_root against the one stamped on the block header.
///
/// If any block's recomputed root differs from its stamped root, print
/// the mismatch — that's the exact block + height where this host's
/// apply path diverges from whoever originally produced the block.
///
/// Execution cost: proportional to `TO - FROM + 1` blocks × per-block
/// apply latency (~1-5ms on EPYC with warm MDBX cache). 10K blocks =~
/// 10-50s. Keep ranges narrow for iterative debugging.
///
/// STATUS: skeleton — needs a Blockchain API that takes a starting
/// AccountDB + trie snapshot and accepts replayed blocks without
/// gossip/validation side-effects. Wire-up is the first action next
/// session. See TODOs inline.
#[test]
#[ignore = "scaffold — wiring incomplete, see TODOs in body"]
fn replay_and_compare() {
    let path = std::env::var("TEST_CHAIN_DB")
        .expect("set TEST_CHAIN_DB=/path/to/chain.db");
    let from: u64 = std::env::var("TEST_HEIGHT_FROM")
        .expect("set TEST_HEIGHT_FROM=<start block index>")
        .parse()
        .expect("TEST_HEIGHT_FROM must be an integer");
    let to: u64 = std::env::var("TEST_HEIGHT_TO")
        .expect("set TEST_HEIGHT_TO=<end block index, inclusive>")
        .parse()
        .expect("TEST_HEIGHT_TO must be an integer");

    // TODO(next-session): build a read-only view of the state at height FROM-1.
    // Options explored:
    //   (a) Clone the Blockchain struct, then truncate self.chain + rebuild
    //       accounts from TABLE_STATE at that height. Requires a snapshot
    //       table per height or a walk-back-from-tip primitive (neither
    //       exists today — add via a new blockchain.rs::snapshot_at(h)).
    //   (b) Seed a fresh Blockchain + replay blocks 1..=FROM-1 from the
    //       chain.db's TABLE_BLOCKS, reaching the "canonical" state at
    //       FROM-1. Expensive for large FROM but requires no new API.
    //
    // (b) is the pragmatic MVP.
    let _bc = load_existing_chain(&path);

    println!("CHAIN_DB={}", path);
    println!("RANGE={}..={}", from, to);

    for h in from..=to {
        // TODO(next-session):
        //   1. Read block h from storage via Storage::load_block(h).
        //   2. Apply it to the reconstructed state from step (b) above:
        //      `state.add_block(block.clone())?` — the same path validators
        //      run in production.
        //   3. Pull the recomputed root via `state.trie_root_at(h)`.
        //   4. Compare against `block.state_root` (what the original
        //      producer stamped).
        //   5. On mismatch, print `MISMATCH height={} stamped={} computed={}`
        //      and return Ok — the caller diffs the two hosts' outputs.
        //
        // Once the above is wired, add a second TODO: also serialise the
        // AccountDB post-apply (or a hash of its sorted entries) so a diff
        // of VPS1 vs VPS4 output can pinpoint the account whose balance
        // computes differently, not just that the aggregate root differs.
        let _ = h;
    }

    panic!(
        "replay_and_compare: wiring incomplete — see TODOs in test body. \
         This scaffold compiles and documents the workflow but does not \
         yet execute the replay loop. Next session: implement option (b) \
         above, then run on VPS1 + VPS4 with the same chain.db snapshot."
    );
}
