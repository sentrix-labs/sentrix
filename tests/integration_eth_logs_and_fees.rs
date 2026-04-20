#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]
// integration_eth_logs_and_fees.rs — Sprint 2 coverage for eth_getLogs,
// eth_feeHistory, eth_maxPriorityFeePerGas. These tests exercise the RPC
// handlers against an in-memory Blockchain — no MDBX persistence — so
// log queries return empty sets but the response shapes must still match
// the Ethereum JSON-RPC spec so wallets don't error.

mod common;

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use sentrix::core::blockchain::Blockchain;
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

fn fresh_state() -> Arc<RwLock<Blockchain>> {
    let (bc, _admin) = common::setup_single_validator();
    Arc::new(RwLock::new(bc))
}

#[tokio::test]
async fn test_eth_get_logs_empty_chain_returns_empty_array() {
    let state = fresh_state();
    let resp = jsonrpc_handler(
        State(state),
        Json(make_request(
            "eth_getLogs",
            json!([{"fromBlock": "0x0", "toBlock": "latest"}]),
        )),
    )
    .await;
    let result = resp.0.result.expect("result");
    assert!(result.is_array(), "eth_getLogs must return an array");
    assert_eq!(result.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_eth_get_logs_rejects_range_over_10k() {
    let state = fresh_state();
    let resp = jsonrpc_handler(
        State(state),
        Json(make_request(
            "eth_getLogs",
            json!([{"fromBlock": "0x0", "toBlock": "0x2711"}]),
        )),
    )
    .await;
    let err = resp.0.error.expect("should error");
    assert_eq!(err.code, -32005, "must be -32005 limit exceeded");
}

#[tokio::test]
async fn test_eth_get_logs_rejects_inverted_range() {
    let state = fresh_state();
    let resp = jsonrpc_handler(
        State(state),
        Json(make_request(
            "eth_getLogs",
            json!([{"fromBlock": "0x10", "toBlock": "0x5"}]),
        )),
    )
    .await;
    let err = resp.0.error.expect("should error");
    assert_eq!(err.code, -32602);
}

#[tokio::test]
async fn test_eth_get_logs_requires_filter_object() {
    let state = fresh_state();
    let resp = jsonrpc_handler(State(state), Json(make_request("eth_getLogs", json!([])))).await;
    let err = resp.0.error.expect("should error");
    assert_eq!(err.code, -32602);
}

#[tokio::test]
async fn test_eth_fee_history_shape() {
    let state = fresh_state();
    let resp = jsonrpc_handler(
        State(state),
        Json(make_request(
            "eth_feeHistory",
            json!(["0x4", "latest", [25.0, 50.0, 75.0]]),
        )),
    )
    .await;
    let result = resp.0.result.expect("result");
    assert!(result["oldestBlock"].as_str().unwrap().starts_with("0x"));
    let base_fees = result["baseFeePerGas"]
        .as_array()
        .expect("baseFeePerGas array");
    let ratios = result["gasUsedRatio"]
        .as_array()
        .expect("gasUsedRatio array");
    let rewards = result["reward"].as_array().expect("reward array");
    assert_eq!(base_fees.len(), 5, "baseFeePerGas always blockCount+1");
    assert_eq!(ratios.len(), rewards.len(), "ratios + rewards must align");
    if let Some(first_reward) = rewards.first() {
        assert_eq!(
            first_reward.as_array().unwrap().len(),
            3,
            "reward[i] len == percentiles len"
        );
    }
}

#[tokio::test]
async fn test_eth_max_priority_fee_per_gas_returns_hex() {
    let state = fresh_state();
    let resp = jsonrpc_handler(
        State(state),
        Json(make_request("eth_maxPriorityFeePerGas", json!([]))),
    )
    .await;
    let result = resp.0.result.expect("result");
    let s = result.as_str().expect("string");
    assert!(s.starts_with("0x"));
    u64::from_str_radix(s.trim_start_matches("0x"), 16).expect("valid hex");
}
