#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]
// integration_sentrix_native_rpc.rs — Sprint 1 coverage for the five
// sentrix_* JSON-RPC methods added in feat/sprint1-sentrix-native-rpc.
//
// Tests boot an in-memory Blockchain with 3 validators + 2 delegators,
// wrap it as the SharedState the RPC handler expects, invoke
// jsonrpc_handler with a synthetic request per method, and assert the
// response shape matches the Sprint 1 spec. No network / systemd / VPS.

mod common;

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use sentrix::core::blockchain::Blockchain;
use sentrix::wallet::wallet::Wallet;
use sentrix_rpc::jsonrpc::{JsonRpcRequest, jsonrpc_handler};
use serde_json::{Value, json};
use tokio::sync::RwLock;

fn make_request(method: &str, params: Value) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: method.to_string(),
        params: Some(params),
        id: Some(json!(1)),
    }
}

fn setup_chain_with_dpos() -> (Arc<RwLock<Blockchain>>, String, String, String, String) {
    let (mut bc, admin) = common::setup_single_validator();
    // `common::setup_single_validator` uses a hardcoded admin address for
    // authority.add_validator — replay that through the admin handle.
    let admin_addr = bc.authority.admin_address.clone();

    let val1 = Wallet::generate();
    let val2 = Wallet::generate();
    let val3 = Wallet::generate();
    let del1 = Wallet::generate();
    let del2 = Wallet::generate();

    let min_stake = sentrix_staking::MIN_SELF_STAKE;
    for (i, v) in [&val1, &val2, &val3].iter().enumerate() {
        bc.stake_registry
            .register_validator(
                &v.address,
                min_stake.saturating_add(i as u64 * 1_000_000_000),
                1000,
                0,
            )
            .expect("register");
        bc.authority
            .add_validator(
                &admin_addr,
                v.address.clone(),
                format!("Validator {}", i + 1),
                v.public_key.clone(),
            )
            .expect("add_validator");
    }
    let _ = admin;

    bc.stake_registry
        .delegate(&del1.address, &val1.address, 5_000_000_000, 10)
        .expect("delegate");
    bc.stake_registry
        .delegate(&del2.address, &val2.address, 7_000_000_000, 12)
        .expect("delegate");
    bc.stake_registry
        .update_active_set();

    (
        Arc::new(RwLock::new(bc)),
        val1.address,
        val2.address,
        del1.address,
        del2.address,
    )
}

#[tokio::test]
async fn test_sentrix_get_validator_set_shape() {
    let (state, _, _, _, _) = setup_chain_with_dpos();
    let resp = jsonrpc_handler(State(state), Json(make_request("sentrix_getValidatorSet", json!([])))).await;
    let result = resp.0.result.expect("result");
    assert!(result.get("validators").and_then(|v| v.as_array()).is_some(), "validators array");
    assert!(result.get("active_count").is_some());
    assert!(result.get("total_count").is_some());
    assert!(result.get("total_active_stake").is_some());
    let first = &result["validators"][0];
    for field in [
        "address",
        "name",
        "stake",
        "commission",
        "status",
        "blocks_produced_epoch",
        "uptime",
        "voting_power",
    ] {
        assert!(first.get(field).is_some(), "missing field: {field}");
    }
    assert!(
        first["stake"].as_str().unwrap().starts_with("0x"),
        "stake must be hex"
    );
}

#[tokio::test]
async fn test_sentrix_get_delegations_shape() {
    let (state, val1, _val2, del1, _del2) = setup_chain_with_dpos();
    let resp = jsonrpc_handler(
        State(state),
        Json(make_request("sentrix_getDelegations", json!([del1]))),
    )
    .await;
    let result = resp.0.result.expect("result");
    let list = result["delegations"].as_array().expect("delegations array");
    assert_eq!(list.len(), 1, "del1 has exactly one active delegation");
    let d = &list[0];
    assert_eq!(d["validator"].as_str().unwrap(), val1);
    assert!(d["amount"].as_str().unwrap().starts_with("0x"));
    assert!(d["pending_reward"].as_str().unwrap().starts_with("0x"));
    assert_eq!(d["status"].as_str().unwrap(), "active");
}

#[tokio::test]
async fn test_sentrix_get_delegations_requires_address() {
    let (state, _, _, _, _) = setup_chain_with_dpos();
    let resp = jsonrpc_handler(
        State(state),
        Json(make_request("sentrix_getDelegations", json!([]))),
    )
    .await;
    let err = resp.0.error.expect("error");
    assert_eq!(err.code, -32602, "address required → -32602 invalid params");
}

#[tokio::test]
async fn test_sentrix_get_staking_rewards_default_window() {
    let (state, _, _, del1, _) = setup_chain_with_dpos();
    let resp = jsonrpc_handler(
        State(state),
        Json(make_request("sentrix_getStakingRewards", json!([del1]))),
    )
    .await;
    let result = resp.0.result.expect("result");
    assert!(result["total_lifetime"].as_str().unwrap().starts_with("0x"));
    assert!(result["pending_claimable"].as_str().unwrap().starts_with("0x"));
    assert!(result["by_epoch"].is_array());
    assert!(result["from_epoch"].is_number());
    assert!(result["to_epoch"].is_number());
}

#[tokio::test]
async fn test_sentrix_get_bft_status_poa_mode() {
    // Default chain (no VOYAGER_FORK_HEIGHT env) stays on PoA, so the
    // status payload is the PoA subset with finalized = latest.
    let (state, _, _, _, _) = setup_chain_with_dpos();
    let resp = jsonrpc_handler(
        State(state.clone()),
        Json(make_request("sentrix_getBftStatus", json!([]))),
    )
    .await;
    let result = resp.0.result.expect("result");
    assert_eq!(result["consensus"].as_str().unwrap(), "PoA");
    assert!(result.get("current_leader").is_some());
    assert!(result.get("last_finalized_height").is_some());
    assert!(result.get("last_finalized_hash").is_some());
    // PoA response does NOT include BFT-only fields.
    assert!(result.get("current_round").is_none(), "PoA must omit current_round");
}

#[tokio::test]
async fn test_sentrix_get_finalized_height_poa_equals_latest() {
    let (state, _, _, _, _) = setup_chain_with_dpos();
    let latest = state.read().await.height();
    let resp = jsonrpc_handler(
        State(state),
        Json(make_request("sentrix_getFinalizedHeight", json!([]))),
    )
    .await;
    let result = resp.0.result.expect("result");
    assert_eq!(result["latest_height"].as_u64().unwrap(), latest);
    assert_eq!(
        result["finalized_height"].as_u64().unwrap(),
        latest,
        "PoA instant finality: finalized == latest"
    );
    assert_eq!(
        result["blocks_behind_finality"].as_u64().unwrap(),
        0,
        "PoA lag should be zero"
    );
}
