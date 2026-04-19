#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]
// integration_eth_block_receipts.rs — coverage for `eth_getBlockReceipts`
// (backlog #8). Input shape supports: string block tag / hex number /
// block hash / { blockHash | blockNumber } object. Output is an array of
// receipt objects in transaction order with per-tx cumulativeGasUsed.

mod common;

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use sentrix::core::blockchain::Blockchain;
use sentrix_rpc::jsonrpc::{JsonRpcRequest, jsonrpc_handler};
use serde_json::{Value, json};
use tokio::sync::RwLock;

fn make_request(params: Value) -> JsonRpcRequest {
    JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        method: "eth_getBlockReceipts".to_string(),
        params: Some(params),
        id: Some(json!(1)),
    }
}

async fn call(state: Arc<RwLock<Blockchain>>, params: Value) -> Value {
    let resp = jsonrpc_handler(State(state), Json(make_request(params))).await;
    serde_json::to_value(&resp.0).expect("response → Value")
}

#[tokio::test]
async fn latest_tag_returns_array() {
    let (bc, _admin) = common::setup_single_validator();
    let state = Arc::new(RwLock::new(bc));
    let resp = call(state, json!(["latest"])).await;
    assert!(
        resp["error"].is_null(),
        "latest tag must not error: {resp:?}"
    );
    assert!(
        resp["result"].is_array(),
        "result must be array, got {:?}",
        resp["result"],
    );
}

#[tokio::test]
async fn earliest_tag_returns_genesis_receipts() {
    let (bc, _admin) = common::setup_single_validator();
    let state = Arc::new(RwLock::new(bc));
    let resp = call(state, json!(["earliest"])).await;
    let arr = resp["result"].as_array().expect("array");
    // Genesis typically carries premine + coinbase. At minimum we expect
    // the coinbase tx to produce a receipt.
    assert!(
        !arr.is_empty(),
        "earliest block should have at least one receipt",
    );
    // Receipt shape — every entry must carry these keys.
    for r in arr {
        for key in [
            "transactionHash",
            "transactionIndex",
            "blockNumber",
            "blockHash",
            "status",
            "gasUsed",
            "cumulativeGasUsed",
            "logs",
            "logsBloom",
        ] {
            assert!(
                r.get(key).is_some(),
                "missing field `{key}` in receipt: {r:?}",
            );
        }
    }
}

#[tokio::test]
async fn cumulative_gas_used_is_monotonic() {
    let (bc, _admin) = common::setup_single_validator();
    let state = Arc::new(RwLock::new(bc));
    let resp = call(state, json!(["earliest"])).await;
    let arr = resp["result"].as_array().expect("array");
    let mut prev: u64 = 0;
    for r in arr {
        let cur_hex = r["cumulativeGasUsed"]
            .as_str()
            .expect("cumulativeGasUsed hex");
        let cur = u64::from_str_radix(cur_hex.trim_start_matches("0x"), 16)
            .expect("u64 from hex");
        assert!(
            cur >= prev,
            "cumulativeGasUsed must be non-decreasing ({prev} → {cur})",
        );
        prev = cur;
    }
}

#[tokio::test]
async fn hex_block_number_resolves() {
    let (bc, _admin) = common::setup_single_validator();
    let state = Arc::new(RwLock::new(bc));
    let resp = call(state, json!(["0x0"])).await;
    assert!(resp["error"].is_null(), "hex 0x0 must resolve: {resp:?}");
    assert!(resp["result"].is_array());
}

#[tokio::test]
async fn block_number_object_form() {
    let (bc, _admin) = common::setup_single_validator();
    let state = Arc::new(RwLock::new(bc));
    let resp = call(state, json!([{"blockNumber": "latest"}])).await;
    assert!(
        resp["error"].is_null(),
        "object form must resolve: {resp:?}"
    );
    assert!(resp["result"].is_array());
}

#[tokio::test]
async fn missing_block_returns_null() {
    let (bc, _admin) = common::setup_single_validator();
    let state = Arc::new(RwLock::new(bc));
    // Height 0xffffffff won't exist on a fresh chain.
    let resp = call(state, json!(["0xffffffff"])).await;
    assert!(
        resp["error"].is_null(),
        "non-existent block should return null, not error: {resp:?}",
    );
    assert!(
        resp["result"].is_null(),
        "non-existent block must return null result, got {:?}",
        resp["result"],
    );
}

#[tokio::test]
async fn malformed_params_return_error() {
    let (bc, _admin) = common::setup_single_validator();
    let state = Arc::new(RwLock::new(bc));
    let resp = call(state, json!([42])).await; // raw number — not supported
    assert!(
        !resp["error"].is_null(),
        "malformed param should error: {resp:?}",
    );
}

#[tokio::test]
async fn block_hash_path_resolves() {
    let (bc, _admin) = common::setup_single_validator();
    // Grab genesis block hash.
    let genesis_hash = bc.chain.first().map(|b| b.hash.clone()).expect("genesis");
    let state = Arc::new(RwLock::new(bc));
    let resp = call(state, json!([format!("0x{genesis_hash}")])).await;
    assert!(
        resp["error"].is_null(),
        "block-hash lookup must succeed: {resp:?}",
    );
    assert!(resp["result"].is_array());
}
