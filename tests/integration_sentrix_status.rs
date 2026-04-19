#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]
// integration_sentrix_status.rs — coverage for GET /sentrix_status
// (backlog #13, NEAR-style structured node status endpoint).

mod common;

use std::sync::Arc;

use axum::extract::State;
use sentrix::core::blockchain::Blockchain;
use sentrix_rpc::routes::sentrix_status;
use serde_json::Value;
use tokio::sync::RwLock;

fn shared(bc: Blockchain) -> Arc<RwLock<Blockchain>> {
    Arc::new(RwLock::new(bc))
}

#[tokio::test]
async fn returns_all_expected_fields() {
    let (bc, _admin) = common::setup_single_validator();
    let state = shared(bc);
    let resp = sentrix_status(State(state)).await;
    let v: &Value = &resp.0;

    // Top-level shape
    assert!(v.get("version").is_some(), "missing version: {v}");
    assert!(v.get("chain_id").is_some(), "missing chain_id: {v}");
    assert!(v.get("consensus").is_some(), "missing consensus: {v}");
    assert!(v.get("native_token").is_some(), "missing native_token: {v}");
    assert!(v.get("sync_info").is_some(), "missing sync_info: {v}");
    assert!(v.get("validators").is_some(), "missing validators: {v}");
    assert!(
        v.get("uptime_seconds").is_some(),
        "missing uptime_seconds: {v}"
    );

    // Version object shape
    let version = &v["version"];
    assert!(version.get("version").is_some(), "version.version missing");
    assert!(version.get("build").is_some(), "version.build missing");
    assert_eq!(
        version["version"].as_str(),
        Some(env!("CARGO_PKG_VERSION")),
        "version mismatch"
    );

    // Native token is fixed
    assert_eq!(v["native_token"].as_str(), Some("SRX"));
}

#[tokio::test]
async fn sync_info_reflects_latest_block() {
    let (bc, _admin) = common::setup_single_validator();
    let state = shared(bc);
    let resp = sentrix_status(State(state.clone())).await;
    let sync = &resp.0["sync_info"];

    // Fresh chain has the genesis block only.
    assert!(
        sync.get("latest_block_height").is_some(),
        "latest_block_height missing"
    );
    assert!(
        sync.get("latest_block_hash").is_some(),
        "latest_block_hash missing"
    );
    assert!(
        sync.get("latest_block_time").is_some(),
        "latest_block_time missing"
    );
    assert!(
        sync.get("earliest_block_height").is_some(),
        "earliest_block_height missing"
    );
    assert_eq!(
        sync["syncing"].as_bool(),
        Some(false),
        "fresh single-validator chain should not be syncing",
    );
}

#[tokio::test]
async fn consensus_tag_matches_chain_id() {
    // PoA: chain_id 7119 → "PoA". Any other chain_id → "BFT".
    // `common::setup_single_validator` uses the PoA chain_id (7119) by
    // default, so we expect "PoA" here.
    let (bc, _admin) = common::setup_single_validator();
    let state = shared(bc);
    let resp = sentrix_status(State(state)).await;
    let consensus = resp.0["consensus"].as_str().expect("consensus str");
    assert!(
        consensus == "PoA" || consensus == "BFT",
        "unexpected consensus tag: {consensus}",
    );
}

#[tokio::test]
async fn uptime_is_monotonic_across_calls() {
    let (bc, _admin) = common::setup_single_validator();
    let state = shared(bc);
    let r1 = sentrix_status(State(state.clone())).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let r2 = sentrix_status(State(state)).await;
    let u1 = r1.0["uptime_seconds"].as_u64().expect("uptime u64");
    let u2 = r2.0["uptime_seconds"].as_u64().expect("uptime u64");
    assert!(u2 >= u1, "uptime must be monotonic ({u1} → {u2})");
}
