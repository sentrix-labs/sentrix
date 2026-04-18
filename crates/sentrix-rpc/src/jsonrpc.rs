// jsonrpc.rs - Sentrix — Ethereum-compatible JSON-RPC 2.0

use crate::routes::{ApiKey, SharedState};
use sentrix_primitives::transaction::Transaction;
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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
        Self {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }
    pub fn err(id: Option<Value>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
            }),
            id,
        }
    }
}

fn to_hex(n: u64) -> String {
    format!("0x{:x}", n)
}
fn to_hex_u128(n: u128) -> String {
    format!("0x{:x}", n)
}

// ── Main handler ─────────────────────────────────────────
pub async fn jsonrpc_handler(
    State(state): State<SharedState>,
    Json(req): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    let id = req.id.clone();
    let params = req.params.unwrap_or(json!([]));

    let result = match req.method.as_str() {
        "eth_chainId" => {
            let bc = state.read().await;
            Ok(json!(to_hex(bc.chain_id)))
        }
        "net_version" => {
            let bc = state.read().await;
            Ok(json!(bc.chain_id.to_string()))
        }
        "net_listening" => Ok(json!(true)),
        "web3_clientVersion" => Ok(json!(format!("Sentrix/{}/Rust", env!("CARGO_PKG_VERSION")))),
        "eth_blockNumber" => {
            let bc = state.read().await;
            Ok(json!(to_hex(bc.height())))
        }
        "eth_gasPrice" => Ok(json!(to_hex(1_000_000_000))),
        "eth_estimateGas" => {
            // Gas estimation — 21000 for transfers, higher for contract calls
            let call_obj = &params[0];
            let data_hex = call_obj["data"].as_str().unwrap_or("0x");
            if data_hex.len() > 2 {
                Ok(json!(to_hex(100_000)))
            } else {
                Ok(json!(to_hex(21_000)))
            }
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
            let hash = params[0]
                .as_str()
                .unwrap_or("")
                .trim_start_matches("0x")
                .to_string();
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
            let txid = params[0]
                .as_str()
                .unwrap_or("")
                .trim_start_matches("0x")
                .to_string();
            let bc = state.read().await;
            match bc.get_transaction(&txid) {
                Some(tx_data) => Ok(tx_data),
                None => Ok(json!(null)),
            }
        }
        "eth_getTransactionReceipt" => {
            let txid = params[0]
                .as_str()
                .unwrap_or("")
                .trim_start_matches("0x")
                .to_string();
            let bc = state.read().await;
            match bc.get_transaction(&txid) {
                Some(tx_data) => {
                    let block_index = tx_data["block_index"].as_u64().unwrap_or(0);
                    // A2: failed EVM tx → status=0x0 (reverted), success → 0x1.
                    // Native (non-EVM) txs that reach a block always succeeded — they are
                    // validated atomically in Pass 1 and only committed if Pass 2 succeeds,
                    // so they are never recorded as failed.
                    let status = if bc.accounts.is_evm_tx_failed(&txid) {
                        "0x0"
                    } else {
                        "0x1"
                    };
                    Ok(json!({
                        "transactionHash": format!("0x{}", txid),
                        "blockNumber": to_hex(block_index),
                        "blockHash": tx_data["block_hash"],
                        "status": status,
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
            // JSON-RPC token operations accept signed transactions only — no private_key in params.
            // params[0] must be a pre-signed Transaction object (same fields as POST /transactions).
            // Client is responsible for signing the transaction locally before sending.
            let tx: Transaction = match serde_json::from_value(params[0].clone()) {
                Ok(t) => t,
                Err(e) => {
                    return Json(JsonRpcResponse::err(
                        id,
                        -32602,
                        &format!("invalid transaction object: {}", e),
                    ));
                }
            };
            let txid = tx.txid.clone();
            let mut bc = state.write().await;
            match bc.add_to_mempool(tx) {
                Ok(()) => Ok(json!({ "txid": txid, "status": "pending_in_mempool" })),
                Err(e) => return Json(JsonRpcResponse::err(id, -32603, &e.to_string())),
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
            // Decode RLP-encoded signed Ethereum transaction (legacy or EIP-1559/2930/4844).
            // Recover sender, convert to Sentrix Transaction format, add to mempool.
            if !state.read().await.is_evm_active() {
                return Json(JsonRpcResponse::err(id, -32000, "EVM not active yet"));
            }
            let raw_hex = params[0].as_str().unwrap_or("").trim_start_matches("0x");
            let raw_bytes = match hex::decode(raw_hex) {
                Ok(b) => b,
                Err(_) => return Json(JsonRpcResponse::err(id, -32602, "invalid hex")),
            };

            use alloy_consensus::TxEnvelope;
            use alloy_eips::eip2718::Decodable2718;

            let envelope: TxEnvelope = match TxEnvelope::decode_2718(&mut raw_bytes.as_slice()) {
                Ok(env) => env,
                Err(e) => {
                    return Json(JsonRpcResponse::err(
                        id,
                        -32602,
                        &format!("RLP decode failed: {}", e),
                    ));
                }
            };

            // Recover sender address from signature
            use alloy_consensus::Transaction as AlloyTx;
            use alloy_consensus::transaction::SignerRecoverable;
            let sender: alloy_primitives::Address = match envelope.recover_signer() {
                Ok(addr) => addr,
                Err(e) => {
                    return Json(JsonRpcResponse::err(
                        id,
                        -32602,
                        &format!("signer recovery failed: {}", e),
                    ));
                }
            };
            let sender_str = format!("0x{}", hex::encode(sender.as_slice()));

            // Extract tx fields
            let nonce = envelope.nonce();
            let gas_limit = envelope.gas_limit();
            let value_u256: alloy_primitives::U256 = envelope.value();
            let data_bytes = envelope.input().to_vec();
            let to_kind = envelope.kind();
            let chain_id = envelope.chain_id().unwrap_or(0);

            // Convert Ethereum value (wei) to Sentrix sentri (1 SRX = 1e18 wei = 1e8 sentri)
            // 1 sentri = 1e10 wei
            let value_wei: u128 = value_u256.try_into().unwrap_or(u128::MAX);
            let amount_sentri = (value_wei / 10_000_000_000u128) as u64;

            // Build Sentrix Transaction. txid = keccak256 of raw bytes (Ethereum tx hash)
            use sha3::{Digest as _, Keccak256};
            let tx_hash = Keccak256::digest(&raw_bytes);
            let txid = hex::encode(tx_hash);

            let to_str = match to_kind {
                alloy_primitives::TxKind::Call(addr) => {
                    format!("0x{}", hex::encode(addr.as_slice()))
                }
                alloy_primitives::TxKind::Create => {
                    sentrix_primitives::transaction::TOKEN_OP_ADDRESS.to_string()
                }
            };

            // Encode EVM call data as hex in the data field (will be decoded by block_executor)
            let evm_data = format!("EVM:{}:{}", gas_limit, hex::encode(&data_bytes));

            let sentrix_tx = Transaction {
                txid: txid.clone(),
                from_address: sender_str,
                to_address: to_str,
                amount: amount_sentri,
                fee: sentrix_primitives::transaction::MIN_TX_FEE,
                nonce,
                data: evm_data,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                chain_id,
                signature: hex::encode(&raw_bytes), // store full raw tx for re-execution
                public_key: String::new(),          // not needed — sender derived from signature
            };

            let mut bc = state.write().await;
            match bc.add_to_mempool(sentrix_tx) {
                Ok(()) => Ok(json!(format!("0x{}", txid))),
                Err(e) => {
                    return Json(JsonRpcResponse::err(id, -32603, &e.to_string()));
                }
            }
        }
        "eth_call" => {
            // Execute a read-only EVM call without state mutation.
            // params[0] = {from, to, data, value, gas}
            if !state.read().await.is_evm_active() {
                return Json(JsonRpcResponse::err(id, -32000, "EVM not active yet"));
            }
            let call_obj = &params[0];
            let from_str = call_obj["from"]
                .as_str()
                .unwrap_or("0x0000000000000000000000000000000000000000");
            let to_str = call_obj["to"].as_str().unwrap_or("");
            let data_hex = call_obj["data"]
                .as_str()
                .unwrap_or("0x")
                .trim_start_matches("0x");
            let data_bytes = hex::decode(data_hex).unwrap_or_default();
            let gas_limit = call_obj["gas"]
                .as_str()
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                .unwrap_or(30_000_000);

            let bc = state.read().await;
            use sentrix_evm::database::parse_sentrix_address;

            // Snapshot chain_id before bc is dropped — execute_call below needs it.
            let chain_id = bc.chain_id;

            let from_addr =
                parse_sentrix_address(from_str).unwrap_or(alloy_primitives::Address::ZERO);
            let to_addr = parse_sentrix_address(to_str);

            let tx_kind = match to_addr {
                Some(addr) => revm::primitives::TxKind::Call(addr),
                None => revm::primitives::TxKind::Create,
            };

            let tx = revm::context::TxEnv::builder()
                .caller(from_addr)
                .kind(tx_kind)
                .data(alloy_primitives::Bytes::from(data_bytes))
                .gas_limit(gas_limit)
                .gas_price(0)
                .nonce(bc.accounts.get_nonce(from_str))
                .chain_id(Some(chain_id))
                .build()
                .unwrap_or_default();

            let base_fee = sentrix_evm::gas::INITIAL_BASE_FEE;

            // Populate InMemoryDB with sender account (so gas payment works).
            // Also load target contract if it has code.
            let mut in_mem_db = revm::database::InMemoryDB::default();
            let sender_balance = bc.accounts.get_balance(from_str);
            let sender_nonce = bc.accounts.get_nonce(from_str);
            in_mem_db.insert_account_info(
                from_addr,
                revm::state::AccountInfo {
                    balance: alloy_primitives::U256::from(sender_balance)
                        .saturating_mul(alloy_primitives::U256::from(10_000_000_000u64)),
                    nonce: sender_nonce,
                    code_hash: revm::primitives::KECCAK_EMPTY,
                    account_id: None,
                    code: None,
                },
            );
            // Load target contract if present
            if let Some(target) = to_addr
                && let Some(target_account) = bc.accounts.accounts.get(to_str)
                && target_account.is_contract()
            {
                let code_hash_hex = hex::encode(target_account.code_hash);
                if let Some(code_bytes) = bc.accounts.get_contract_code(&code_hash_hex) {
                    let bytecode = revm::state::Bytecode::new_raw(alloy_primitives::Bytes::from(
                        code_bytes.clone(),
                    ));
                    let code_hash = alloy_primitives::B256::from(target_account.code_hash);
                    in_mem_db.insert_account_info(
                        target,
                        revm::state::AccountInfo {
                            balance: alloy_primitives::U256::from(target_account.balance),
                            nonce: target_account.nonce,
                            code_hash,
                            account_id: None,
                            code: Some(bytecode),
                        },
                    );
                }
            }
            drop(bc);

            match sentrix_evm::executor::execute_call(in_mem_db, tx, base_fee, chain_id) {
                Ok(receipt) => {
                    let output_hex = format!("0x{}", hex::encode(&receipt.output));
                    Ok(json!(output_hex))
                }
                Err(e) => {
                    tracing::warn!("eth_call EVM execution failed: {}", e);
                    // Return empty result instead of error for compatibility
                    Ok(json!("0x"))
                }
            }
        }
        "eth_syncing" => Ok(json!(false)),
        "eth_accounts" => Ok(json!([])),
        "eth_getCode" => {
            // Return contract bytecode for an address
            let address = params[0].as_str().unwrap_or("").to_lowercase();
            let bc = state.read().await;
            if let Some(account) = bc.accounts.accounts.get(&address) {
                if account.is_contract() {
                    let code_hash_hex = hex::encode(account.code_hash);
                    if let Some(code) = bc.accounts.get_contract_code(&code_hash_hex) {
                        Ok(json!(format!("0x{}", hex::encode(code))))
                    } else {
                        Ok(json!("0x"))
                    }
                } else {
                    Ok(json!("0x"))
                }
            } else {
                Ok(json!("0x"))
            }
        }
        "eth_getStorageAt" => {
            // Return contract storage at slot
            let address = params[0].as_str().unwrap_or("").to_lowercase();
            let slot = params[1].as_str().unwrap_or("0x0");
            let slot_hex = slot.trim_start_matches("0x");
            let bc = state.read().await;
            if let Some(value) = bc.accounts.get_contract_storage(&address, slot_hex) {
                Ok(json!(format!("0x{}", hex::encode(value))))
            } else {
                Ok(json!(
                    "0x0000000000000000000000000000000000000000000000000000000000000000"
                ))
            }
        }
        _ => Err((-32601, "Method not found")),
    };

    Json(match result {
        Ok(val) => JsonRpcResponse::ok(id, val),
        Err((code, msg)) => JsonRpcResponse::err(id, code, msg),
    })
}

// Hard cap on batch size to prevent CPU saturation from oversized batch requests
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
            Err(_) => {
                return Json(JsonRpcResponse::err(None, -32600, "Invalid Request")).into_response();
            }
        };

        // Reject oversized batches before deserializing individual requests
        if requests.len() > MAX_BATCH_SIZE {
            return Json(JsonRpcResponse::err(
                None,
                -32600,
                &format!(
                    "batch too large: max {} requests, got {}",
                    MAX_BATCH_SIZE,
                    requests.len()
                ),
            ))
            .into_response();
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
            Err(_) => {
                return Json(JsonRpcResponse::err(None, -32600, "Invalid Request")).into_response();
            }
        };
        jsonrpc_handler(State(state), Json(req))
            .await
            .into_response()
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
        use sentrix_core::blockchain::Blockchain;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let bc = Blockchain::new("admin".to_string());
        let state: crate::routes::SharedState = Arc::new(RwLock::new(bc));

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

        let response = rpc_dispatcher(ApiKey, axum::extract::State(state), body).await;

        // Response should be an error about batch too large
        let body_bytes = axum::body::to_bytes(response.into_body(), 10_000)
            .await
            .unwrap();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(
            body_str.contains("batch too large"),
            "Expected batch too large error, got: {}",
            body_str
        );
    }
}
