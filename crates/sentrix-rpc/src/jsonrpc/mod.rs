// jsonrpc/mod.rs - Sentrix — Ethereum-compatible JSON-RPC 2.0 dispatcher.
//
// Namespace-specific handlers (eth_*, net_*, web3_*, sentrix_*) live in
// sibling modules under `crate::jsonrpc::` — this file wires them up
// behind the batch/single dispatcher and keeps the JSON-RPC 2.0
// envelope types. Shared helpers are in `helpers.rs`. (backlog #11)

mod eth;
mod helpers;
mod net;
mod sentrix;
mod web3;

use crate::routes::{ApiKey, SharedState};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Per-namespace dispatch result: `Ok(Value)` for the RPC result payload,
/// `Err((code, message))` for a JSON-RPC error the caller will wrap into
/// a `JsonRpcResponse::err`. Kept `pub(crate)` so namespace modules can
/// return it without leaking the type outside the crate.
pub(crate) type DispatchResult = Result<Value, (i32, String)>;

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

// ── Main handler ─────────────────────────────────────────
pub async fn jsonrpc_handler(
    State(state): State<SharedState>,
    Json(req): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    Json(dispatch_request(&state, req).await)
}

/// Dispatch a parsed JSON-RPC request to the right namespace and
/// return a `JsonRpcResponse`. Extracted from `jsonrpc_handler` so the
/// WebSocket transport (`crates/sentrix-rpc/src/ws/mod.rs`) can reuse
/// the exact same dispatch logic without going through axum's `State`
/// + `Json` extractors.
///
/// HTTP and WS share this single dispatch path → 100% method parity
/// across transports without duplicate code or drift risk.
pub(crate) async fn dispatch_request(
    state: &SharedState,
    req: JsonRpcRequest,
) -> JsonRpcResponse {
    let id = req.id.clone();
    let params = req.params.unwrap_or(json!([]));
    let method = req.method.as_str();

    // Route by namespace prefix. Per-namespace modules own the full
    // match over their method names and return a `DispatchResult` that
    // we wrap into the JSON-RPC envelope.
    let result = if method.starts_with("eth_") {
        eth::dispatch(method, &params, state).await
    } else if method.starts_with("net_") {
        net::dispatch(method, &params, state).await
    } else if method.starts_with("web3_") {
        web3::dispatch(method, &params, state).await
    } else if method.starts_with("sentrix_") {
        sentrix::dispatch(method, &params, state).await
    } else {
        Err((-32601, format!("method not found: {}", method)))
    };

    match result {
        Ok(val) => JsonRpcResponse::ok(id, val),
        Err((code, msg)) => JsonRpcResponse::err(id, code, &msg),
    }
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

    if let Some(arr) = value.as_array() {
        // M-03: reject oversize batches BEFORE the per-element typed
        // deserialisation allocates a second copy. The raw `Value` parse
        // above is bounded by axum's body-size limit; this guard closes
        // the second amplification where 100 MB of arbitrary JSON would
        // round-trip through `Vec<JsonRpcRequest>` before being rejected.
        if arr.len() > MAX_BATCH_SIZE {
            return Json(JsonRpcResponse::err(
                None,
                -32600,
                &format!(
                    "batch too large: max {} requests, got {}",
                    MAX_BATCH_SIZE,
                    arr.len()
                ),
            ))
            .into_response();
        }

        let requests: Vec<JsonRpcRequest> = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(_) => {
                return Json(JsonRpcResponse::err(None, -32600, "Invalid Request")).into_response();
            }
        };

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
