// jsonrpc.rs - Sentrix — Ethereum-compatible JSON-RPC 2.0

use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::api::routes::{SharedState, ApiKey};
use crate::core::transaction::Transaction;
use crate::wallet::wallet::Wallet;

// ── JSON-RPC types ───────────────────────────────────────
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self { jsonrpc: "2.0".to_string(), result: Some(result), error: None, id }
    }
    pub fn err(id: Option<Value>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(), result: None,
            error: Some(JsonRpcError { code, message: message.to_string() }), id,
        }
    }
}

fn to_hex(n: u64) -> String { format!("0x{:x}", n) }
fn to_hex_u128(n: u128) -> String { format!("0x{:x}", n) }

// ── Main handler ─────────────────────────────────────────
pub async fn jsonrpc_handler(
    State(state): State<SharedState>,
    Json(req): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    let id = req.id.clone();
    let params = req.params.unwrap_or(json!([]));

    let result = match req.method.as_str() {
        "eth_chainId" => {
            Ok(json!(to_hex(7119)))
        }
        "net_version" => {
            Ok(json!("7119"))
        }
        "net_listening" => {
            Ok(json!(true))
        }
        "web3_clientVersion" => {
            Ok(json!("Sentrix/0.1.0/Rust"))
        }
        "eth_blockNumber" => {
            let bc = state.read().await;
            Ok(json!(to_hex(bc.height())))
        }
        "eth_gasPrice" => {
            Ok(json!(to_hex(1_000_000_000)))
        }
        "eth_estimateGas" => {
            Ok(json!(to_hex(21_000)))
        }
        "eth_getBalance" => {
            let address = params[0].as_str().unwrap_or("").to_lowercase();
            let bc = state.read().await;
            let balance = bc.accounts.get_balance(&address);
            let wei = balance as u128 * 10_000_000_000u128;
            Ok(json!(to_hex_u128(wei)))
        }
        "eth_getTransactionCount" => {
            let address = params[0].as_str().unwrap_or("").to_lowercase();
            let bc = state.read().await;
            let nonce = bc.accounts.get_nonce(&address);
            Ok(json!(to_hex(nonce)))
        }
        "eth_getBlockByNumber" => {
            let bc = state.read().await;
            let block_param = params[0].as_str().unwrap_or("latest");
            let index = if block_param == "latest" {
                bc.height()
            } else if block_param == "earliest" {
                0
            } else {
                u64::from_str_radix(block_param.trim_start_matches("0x"), 16).unwrap_or(0)
            };
            match bc.get_block(index) {
                Some(block) => Ok(json!({
                    "number": to_hex(block.index),
                    "hash": format!("0x{}", block.hash),
                    "parentHash": format!("0x{}", block.previous_hash),
                    "timestamp": to_hex(block.timestamp),
                    "miner": block.validator,
                    "transactions": block.transactions.iter().map(|tx| format!("0x{}", tx.txid)).collect::<Vec<_>>(),
                    "transactionsRoot": format!("0x{}", block.merkle_root),
                    "gasLimit": to_hex(30_000_000),
                    "gasUsed": to_hex(0),
                    "difficulty": "0x0",
                    "totalDifficulty": "0x0",
                    "size": to_hex(1000),
                    "extraData": "0x",
                    "nonce": "0x0000000000000000",
                })),
                None => Ok(json!(null)),
            }
        }
        "eth_getBlockByHash" => {
            let hash = params[0].as_str().unwrap_or("")
                .trim_start_matches("0x").to_string();
            let bc = state.read().await;
            match bc.get_block_by_hash(&hash) {
                Some(block) => Ok(json!({
                    "number": to_hex(block.index),
                    "hash": format!("0x{}", block.hash),
                    "parentHash": format!("0x{}", block.previous_hash),
                    "timestamp": to_hex(block.timestamp),
                    "miner": block.validator,
                    "transactions": block.transactions.iter().map(|tx| format!("0x{}", tx.txid)).collect::<Vec<_>>(),
                    "transactionsRoot": format!("0x{}", block.merkle_root),
                    "gasLimit": to_hex(30_000_000),
                    "gasUsed": to_hex(0),
                })),
                None => Ok(json!(null)),
            }
        }
        "eth_getTransactionByHash" => {
            let txid = params[0].as_str().unwrap_or("")
                .trim_start_matches("0x").to_string();
            let bc = state.read().await;
            match bc.get_transaction(&txid) {
                Some(tx_data) => Ok(tx_data),
                None => Ok(json!(null)),
            }
        }
        "eth_getTransactionReceipt" => {
            let txid = params[0].as_str().unwrap_or("")
                .trim_start_matches("0x").to_string();
            let bc = state.read().await;
            match bc.get_transaction(&txid) {
                Some(tx_data) => {
                    let block_index = tx_data["block_index"].as_u64().unwrap_or(0);
                    Ok(json!({
                        "transactionHash": format!("0x{}", txid),
                        "blockNumber": to_hex(block_index),
                        "blockHash": tx_data["block_hash"],
                        "status": "0x1",
                        "gasUsed": to_hex(21_000),
                        "cumulativeGasUsed": to_hex(21_000),
                        "logs": [],
                        "logsBloom": "0x00",
                    }))
                }
                None => Ok(json!(null)),
            }
        }
        "sentrix_sendTransaction" => {
            // params[0] = { "from": "0x...", "to": "0x...", "amount": N, "private_key": "hex" }
            let p = &params[0];
            let to = p["to"].as_str().unwrap_or("").to_lowercase();
            let amount = p["amount"].as_u64().unwrap_or(0);
            let private_key = p["private_key"].as_str().unwrap_or("");
            let fee = p["fee"].as_u64().unwrap_or(0);

            if to.is_empty() || private_key.is_empty() || amount == 0 {
                Err((-32602, "sentrix_sendTransaction requires: to, amount, private_key"))
            } else {
                let wallet = match Wallet::from_private_key(private_key) {
                    Ok(w) => w,
                    Err(_) => return Json(JsonRpcResponse::err(id, -32602, "invalid private_key")),
                };
                let sk = match wallet.get_secret_key() {
                    Ok(k) => k,
                    Err(_) => return Json(JsonRpcResponse::err(id, -32602, "invalid private_key")),
                };
                let pk = match wallet.get_public_key() {
                    Ok(k) => k,
                    Err(_) => return Json(JsonRpcResponse::err(id, -32602, "invalid private_key")),
                };

                let mut bc = state.write().await;
                let nonce = bc.accounts.get_nonce(&wallet.address);
                let chain_id = bc.chain_id;

                let tx = match Transaction::new(
                    wallet.address.clone(), to.clone(),
                    amount, fee, nonce, String::new(), chain_id, &sk, &pk,
                ) {
                    Ok(t) => t,
                    Err(e) => return Json(JsonRpcResponse::err(id, -32603, &e.to_string())),
                };

                let txid = tx.txid.clone();
                match bc.add_to_mempool(tx) {
                    Ok(()) => Ok(json!({ "txid": txid, "status": "pending_in_mempool" })),
                    Err(e) => return Json(JsonRpcResponse::err(id, -32603, &e.to_string())),
                }
            }
        }
        "sentrix_getBalance" => {
            // alias for eth_getBalance — returns SRX as float string
            let address = params[0].as_str().unwrap_or("").to_lowercase();
            let bc = state.read().await;
            let balance = bc.accounts.get_balance(&address);
            let wei = balance as u128 * 10_000_000_000u128;
            Ok(json!(to_hex_u128(wei)))
        }
        "eth_sendRawTransaction" => {
            Err((-32601, "eth_sendRawTransaction not yet supported — use POST /transactions REST API"))
        }
        "eth_call" => {
            Ok(json!("0x"))
        }
        "eth_syncing" => {
            Ok(json!(false))
        }
        "eth_accounts" => {
            Ok(json!([]))
        }
        "eth_getCode" => {
            Ok(json!("0x"))
        }
        "eth_getStorageAt" => {
            Ok(json!("0x0000000000000000000000000000000000000000000000000000000000000000"))
        }
        _ => {
            Err((-32601, "Method not found"))
        }
    };

    Json(match result {
        Ok(val) => JsonRpcResponse::ok(id, val),
        Err((code, msg)) => JsonRpcResponse::err(id, code, msg),
    })
}

// M-03 FIX: hard cap on batch size
const MAX_BATCH_SIZE: usize = 100;

// ── Smart dispatcher (single + batch) ────────────────────
pub async fn rpc_dispatcher(
    _auth: ApiKey,
    State(state): State<SharedState>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(JsonRpcResponse::err(None, -32700, "Parse error")).into_response(),
    };

    if value.is_array() {
        let requests: Vec<JsonRpcRequest> = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(_) => return Json(JsonRpcResponse::err(None, -32600, "Invalid Request")).into_response(),
        };

        // M-03 FIX: reject oversized batches
        if requests.len() > MAX_BATCH_SIZE {
            return Json(JsonRpcResponse::err(
                None, -32600,
                &format!("batch too large: max {} requests, got {}", MAX_BATCH_SIZE, requests.len()),
            )).into_response();
        }

        let mut responses = Vec::new();
        for req in requests {
            let resp = jsonrpc_handler(State(state.clone()), Json(req)).await;
            responses.push(resp.0);
        }
        Json(responses).into_response()
    } else {
        let req: JsonRpcRequest = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(_) => return Json(JsonRpcResponse::err(None, -32600, "Invalid Request")).into_response(),
        };
        jsonrpc_handler(State(state), Json(req)).await.into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_m03_max_batch_size_constant() {
        // Verify the constant is set and reasonable
        assert_eq!(MAX_BATCH_SIZE, 100);
    }

    #[tokio::test]
    async fn test_m03_batch_too_large_rejected() {
        use std::sync::Arc;
        use tokio::sync::RwLock;
        use crate::core::blockchain::Blockchain;

        let bc = Blockchain::new("admin".to_string());
        let state: crate::api::routes::SharedState = Arc::new(RwLock::new(bc));

        // Build a batch of 101 requests (exceeds MAX_BATCH_SIZE)
        let mut requests = Vec::new();
        for i in 0..101 {
            requests.push(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_chainId",
                "params": [],
                "id": i
            }));
        }
        let body = axum::body::Bytes::from(serde_json::to_vec(&requests).unwrap());

        let response = rpc_dispatcher(
            ApiKey,
            axum::extract::State(state),
            body,
        ).await;

        // Response should be an error about batch too large
        let body_bytes = axum::body::to_bytes(response.into_body(), 10_000).await.unwrap();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(body_str.contains("batch too large"), "Expected batch too large error, got: {}", body_str);
    }
}
