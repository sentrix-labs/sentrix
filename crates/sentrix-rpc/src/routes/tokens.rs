// tokens.rs — SRC-20 token REST endpoints. 8 handlers covering deploy /
// transfer / burn and the read side (info / balance / holders / trades).
// All three mutating endpoints (deploy / transfer / burn) accept
// pre-signed `TokenOp` transactions — the server never touches private
// keys. Clients build the `TokenOp` JSON, stuff it into `tx.data`, set
// `tx.to_address = TOKEN_OP_ADDRESS`, sign with their own key, and POST
// the signed envelope here.
//
// Extracted from `routes/mod.rs` as part of backlog #12 phase 2.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use sentrix_primitives::transaction::TokenOp;

use super::{ApiKey, SharedState, SignedTxRequest, api_err};

pub(super) async fn list_tokens(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let tokens = bc.list_tokens();
    Json(serde_json::json!({
        "tokens": tokens,
        "total": tokens.len(),
    }))
}

pub(super) async fn get_token_info(
    State(state): State<SharedState>,
    Path(contract): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let bc = state.read().await;
    match bc.token_info(&contract) {
        Ok(info) => Ok(Json(info)),
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}

pub(super) async fn get_token_balance(
    State(state): State<SharedState>,
    Path((contract, addr)): Path<(String, String)>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let balance = bc.token_balance(&contract, &addr);
    Json(serde_json::json!({
        "contract": contract,
        "address": addr,
        "balance": balance,
    }))
}

pub(super) async fn deploy_token(
    _auth: ApiKey,
    State(state): State<SharedState>,
    Json(req): Json<SignedTxRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let tx = req.transaction;
    let op = TokenOp::decode(&tx.data)
        .ok_or_else(|| api_err("data must contain a valid TokenOp JSON"))?;
    let (name, symbol, total_supply, max_supply) = match &op {
        TokenOp::Deploy {
            name,
            symbol,
            supply,
            max_supply,
            ..
        } => (name.clone(), symbol.clone(), *supply, *max_supply),
        _ => return Err(api_err("expected Deploy operation in tx.data")),
    };
    let deployer = tx.from_address.clone();
    let txid = tx.txid.clone();
    let mut bc = state.write().await;
    bc.add_to_mempool(tx).map_err(|e| api_err(&e.to_string()))?;
    Ok(Json(serde_json::json!({
        "success": true,
        "txid": txid,
        "deployer": deployer,
        "name": name,
        "symbol": symbol,
        "total_supply": total_supply,
        "max_supply": max_supply,
        "status": "pending_in_mempool",
    })))
}

pub(super) async fn token_transfer(
    _auth: ApiKey,
    State(state): State<SharedState>,
    Path(contract): Path<String>,
    Json(req): Json<SignedTxRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let tx = req.transaction;
    let op = TokenOp::decode(&tx.data)
        .ok_or_else(|| api_err("data must contain a valid TokenOp JSON"))?;
    let (to_addr, amount) = match &op {
        TokenOp::Transfer {
            contract: c,
            to,
            amount,
        } => {
            if *c != contract {
                return Err(api_err("contract in data does not match URL"));
            }
            (to.clone(), *amount)
        }
        _ => return Err(api_err("expected Transfer operation in tx.data")),
    };
    let from_addr = tx.from_address.clone();
    let txid = tx.txid.clone();
    let mut bc = state.write().await;
    bc.add_to_mempool(tx).map_err(|e| api_err(&e.to_string()))?;
    Ok(Json(serde_json::json!({
        "success": true,
        "txid": txid,
        "contract": contract,
        "from": from_addr,
        "to": to_addr,
        "amount": amount,
        "status": "pending_in_mempool",
    })))
}

pub(super) async fn token_burn(
    _auth: ApiKey,
    State(state): State<SharedState>,
    Path(contract): Path<String>,
    Json(req): Json<SignedTxRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let tx = req.transaction;
    let op = TokenOp::decode(&tx.data)
        .ok_or_else(|| api_err("data must contain a valid TokenOp JSON"))?;
    let amount = match &op {
        TokenOp::Burn {
            contract: c,
            amount,
        } => {
            if *c != contract {
                return Err(api_err("contract in data does not match URL"));
            }
            *amount
        }
        _ => return Err(api_err("expected Burn operation in tx.data")),
    };
    let burned_by = tx.from_address.clone();
    let txid = tx.txid.clone();
    let mut bc = state.write().await;
    bc.add_to_mempool(tx).map_err(|e| api_err(&e.to_string()))?;
    Ok(Json(serde_json::json!({
        "success": true,
        "txid": txid,
        "contract": contract,
        "burned_by": burned_by,
        "amount": amount,
        "status": "pending_in_mempool",
    })))
}

pub(super) async fn get_token_holders_list(
    State(state): State<SharedState>,
    Path(contract): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let bc = state.read().await;
    match bc.get_token_holders(&contract) {
        Some(holders) => {
            let total = holders.len();
            Ok(Json(serde_json::json!({
                "contract": contract,
                "holders": holders,
                "total": total,
            })))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

pub(super) async fn get_token_trades_list(
    State(state): State<SharedState>,
    Path(contract): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let limit: usize = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(20)
        .min(100);
    let offset: usize = params
        .get("offset")
        .and_then(|o| o.parse().ok())
        .unwrap_or(0);
    let trades = bc.get_token_trades(&contract, limit, offset);
    let count = trades.len();
    Json(serde_json::json!({
        "contract": contract,
        "trades": trades,
        "count": count,
        "pagination": { "limit": limit, "offset": offset },
    }))
}
