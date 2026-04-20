// transactions.rs — tx submit / lookup / mempool peek. `send_transaction`
// is the authed write path that feeds `Blockchain::add_to_mempool`;
// `get_transaction` falls through to the sled txid index for blocks
// that have aged out of the in-memory window; `get_mempool` is a
// read-only snapshot.
//
// Extracted from `routes/mod.rs` as part of backlog #12 phase 2f.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use sentrix_primitives::transaction::Transaction;

use super::{ApiKey, SendTxRequest, SharedState};

pub(super) async fn send_transaction(
    _auth: ApiKey,
    State(state): State<SharedState>,
    Json(req): Json<SendTxRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let mut bc = state.write().await;
    match bc.add_to_mempool(req.transaction.clone()) {
        Ok(()) => Ok(Json(serde_json::json!({
            "success": true,
            "txid": req.transaction.txid,
            "message": "transaction added to mempool",
        }))),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": e.to_string(),
            })),
        )),
    }
}

pub(super) async fn get_transaction(
    State(state): State<SharedState>,
    Path(txid): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let bc = state.read().await;
    // A5: delegate to Blockchain::get_transaction so lookups fall through to
    // the sled txid_index for blocks evicted from the in-memory window.
    match bc.get_transaction(&txid) {
        Some(value) => Ok(Json(value)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

pub(super) async fn get_mempool(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let txs: Vec<&Transaction> = bc.mempool.iter().collect();
    Json(serde_json::json!({
        "size": txs.len(),
        "transactions": txs,
    }))
}
