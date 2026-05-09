//! v2.1.90 Patch B3 — operator-driven dry-run against a real chain.db.
//!
//! Loads the chain.db at the path in env var `SENTRIX_B3_DRYRUN_PATH`
//! through `Storage::load_blockchain` (which now includes the B3
//! trie-canonical reconcile pass) and prints what the reconcile saw
//! and repaired. Intended for one-shot operator probes against a
//! sandbox copy of a torn chain.db; never run in CI.
//!
//! Run with:
//!   SENTRIX_B3_DRYRUN_PATH=/path/to/sandbox \
//!     cargo test -p sentrix-core --test v2_1_90_b3_dry_run \
//!     -- --ignored --nocapture
//!
//! Reads the underlying `Blockchain.accounts.PROTOCOL_TREASURY` value
//! before and after the reconcile and prints both, plus the trie's
//! committed value, so the operator can verify whether B3 fixed the
//! divergence on this specific chain.db.

use sentrix_core::storage::Storage;
use sentrix_primitives::transaction::PROTOCOL_TREASURY;
use sentrix_storage::tables::TABLE_STATE;
use std::sync::Arc;

#[test]
#[ignore = "operator-driven; needs SENTRIX_B3_DRYRUN_PATH"]
fn b3_dry_run_against_real_chain_db() {
    let path = match std::env::var("SENTRIX_B3_DRYRUN_PATH") {
        Ok(p) => p,
        Err(_) => panic!("SENTRIX_B3_DRYRUN_PATH not set"),
    };

    println!();
    println!("═══ B3 dry-run on chain.db at {} ═══", path);

    // SAFETY: env vars are process-wide. This test is operator-run,
    // single-threaded by design (cargo test --test ... runs one binary).
    unsafe {
        std::env::set_var("SENTRIX_ALLOW_UNENCRYPTED_DISK", "true");
    }

    // Step 1: read the current blob's PROTOCOL_TREASURY balance + nonce
    // BEFORE letting load_blockchain run, by opening MDBX directly.
    let mdbx_pre = Arc::new(
        sentrix_storage::MdbxStorage::open(std::path::Path::new(&path)).expect("open mdbx (pre)"),
    );
    let raw_blob: Vec<u8> = mdbx_pre
        .get(TABLE_STATE, b"state")
        .expect("read state")
        .expect("state present in chain.db");
    let bc_blob: serde_json::Value = serde_json::from_slice(&raw_blob).expect("parse state");

    let pre_treasury_balance = bc_blob
        .pointer(&format!("/accounts/accounts/{}/balance", PROTOCOL_TREASURY))
        .and_then(|v| v.as_u64())
        .unwrap_or(u64::MAX);
    let pre_treasury_nonce = bc_blob
        .pointer(&format!("/accounts/accounts/{}/nonce", PROTOCOL_TREASURY))
        .and_then(|v| v.as_u64())
        .unwrap_or(u64::MAX);

    let blob_height_before: Option<u64> = match mdbx_pre
        .get(TABLE_STATE, b"blob_height")
        .expect("read blob_height")
    {
        Some(b) => serde_json::from_slice(&b).ok(),
        None => None,
    };

    let height_key_before: Option<u64> = match mdbx_pre
        .get(sentrix_storage::tables::TABLE_META, b"height")
        .expect("read height")
    {
        Some(b) => serde_json::from_slice(&b).ok(),
        None => None,
    };

    println!("PRE-RECONCILE:");
    println!("  height key       = {:?}", height_key_before);
    println!("  blob_height key  = {:?}", blob_height_before);
    println!(
        "  blob treasury    = balance={} nonce={}",
        pre_treasury_balance, pre_treasury_nonce
    );

    // Drop our direct MDBX handle so Storage::open can claim the lock.
    drop(mdbx_pre);

    // Step 2: run Storage::load_blockchain (B3 reconciles inside).
    let storage = Storage::open(&path).expect("Storage::open");
    let bc = storage
        .load_blockchain()
        .expect("load_blockchain ok")
        .expect("blockchain state present");

    let post_treasury_balance = bc.accounts.get_balance(PROTOCOL_TREASURY);
    let post_treasury_nonce = bc.accounts.get_nonce(PROTOCOL_TREASURY);
    let height_after = bc.height();

    // Re-open MDBX to read the post-reconcile blob_height.
    drop(bc);
    drop(storage);
    let mdbx_post =
        sentrix_storage::MdbxStorage::open(std::path::Path::new(&path)).expect("open mdbx (post)");
    let blob_height_after: Option<u64> = match mdbx_post
        .get(TABLE_STATE, b"blob_height")
        .expect("read blob_height post")
    {
        Some(b) => serde_json::from_slice(&b).ok(),
        None => None,
    };

    let raw_blob_post: Vec<u8> = mdbx_post
        .get(TABLE_STATE, b"state")
        .expect("read state post")
        .expect("state present");
    let bc_blob_post: serde_json::Value =
        serde_json::from_slice(&raw_blob_post).expect("parse state post");
    let blob_treasury_post = bc_blob_post
        .pointer(&format!("/accounts/accounts/{}/balance", PROTOCOL_TREASURY))
        .and_then(|v| v.as_u64())
        .unwrap_or(u64::MAX);

    println!();
    println!("POST-RECONCILE:");
    println!("  bc.height()          = {}", height_after);
    println!("  blob_height key      = {:?}", blob_height_after);
    println!(
        "  in-memory treasury   = balance={} nonce={}",
        post_treasury_balance, post_treasury_nonce
    );
    println!("  on-disk blob treasury = balance={}", blob_treasury_post);

    let delta_balance = post_treasury_balance.abs_diff(pre_treasury_balance);
    let direction = if post_treasury_balance >= pre_treasury_balance {
        "+"
    } else {
        "-"
    };

    println!();
    println!("DELTA:");
    println!(
        "  balance change       = {}{} sentri ({}{} block-rewards × 100M)",
        direction,
        delta_balance,
        direction,
        delta_balance / 100_000_000
    );
    println!(
        "  blob_height transition: {:?} -> {:?}",
        blob_height_before, blob_height_after
    );
    println!();
    println!("VERDICT:");
    if pre_treasury_balance == post_treasury_balance && pre_treasury_nonce == post_treasury_nonce {
        println!("  No reconcile fired for PROTOCOL_TREASURY — blob already matched trie.");
        println!("  This chain.db is internally consistent (blob == trie). B3 is a no-op for it.");
        println!("  Cross-validator divergence (if any) is at the trie level itself and");
        println!("  requires chain.db rsync from a healthy peer.");
    } else {
        println!("  B3 RECONCILED PROTOCOL_TREASURY:");
        println!("    blob (before) = {}", pre_treasury_balance);
        println!("    trie / blob (after) = {}", post_treasury_balance);
        println!("    delta = {}{}", direction, delta_balance);
        println!("  This chain.db can be unblocked by a B3-aware restart without rsync.");
    }
    println!();
}
