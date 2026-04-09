// routes.rs - Sentrix

use axum::{
    Router,
    routing::{get, post},
    extract::{State, Path},
    Json,
    http::StatusCode,
    middleware,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::{CorsLayer, Any};
use crate::core::blockchain::Blockchain;
use crate::core::transaction::{Transaction, TokenOp, TOKEN_OP_ADDRESS};
use crate::wallet::wallet::Wallet;
use crate::api::jsonrpc::rpc_dispatcher;
use crate::api::explorer;

pub type SharedState = Arc<RwLock<Blockchain>>;

// ── Response types ───────────────────────────────────────
#[derive(Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(data: T) -> Json<Self> {
        Json(Self { success: true, data: Some(data), error: None })
    }
    pub fn err(msg: String) -> Json<ApiResponse<()>> {
        Json(ApiResponse { success: false, data: None, error: Some(msg) })
    }
}

// ── Request types ────────────────────────────────────────
#[derive(Deserialize)]
pub struct SendTxRequest {
    pub transaction: Transaction,
}

#[derive(Deserialize)]
pub struct DeployTokenRequest {
    pub from_key: String,  // C-03 FIX: private key hex (proves ownership)
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub total_supply: u64,
    pub deploy_fee: u64,
}

#[derive(Deserialize)]
pub struct TokenTransferRequest {
    pub from_key: String,  // C-03 FIX: private key hex (proves ownership)
    pub to: String,
    pub amount: u64,
    pub gas_fee: u64,
}

#[derive(Deserialize)]
pub struct TokenBurnRequest {
    pub from_key: String,  // C-03 FIX: private key hex (proves ownership)
    pub amount: u64,
    pub gas_fee: u64,
}

// H-05 FIX: Constant-time string comparison to prevent timing attacks
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    if a.len() != b.len() {
        return false;
    }
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

// ── API key middleware ────────────────────────────────────
async fn require_api_key(
    req: axum::http::Request<axum::body::Body>,
    next: middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    // If SENTRIX_API_KEY not set, skip auth (dev mode)
    let required_key = match std::env::var("SENTRIX_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => return Ok(next.run(req).await),
    };

    let provided = req.headers()
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // H-05 FIX: constant-time comparison
    if !constant_time_eq(provided, &required_key) {
        return Err(StatusCode::UNAUTHORIZED);
    }

    Ok(next.run(req).await)
}

// ── Router ───────────────────────────────────────────────
pub fn create_router(state: SharedState) -> Router {
    // I-01 FIX: CORS layer — configurable via SENTRIX_CORS_ORIGIN env var
    let cors = match std::env::var("SENTRIX_CORS_ORIGIN") {
        Ok(origin) if !origin.is_empty() && origin != "*" => {
            CorsLayer::new()
                .allow_origin(
                    origin.parse::<axum::http::HeaderValue>()
                        .unwrap_or(axum::http::HeaderValue::from_static("*"))
                )
                .allow_methods([
                    axum::http::Method::GET,
                    axum::http::Method::POST,
                    axum::http::Method::OPTIONS,
                ])
                .allow_headers([
                    axum::http::header::CONTENT_TYPE,
                    axum::http::header::HeaderName::from_static("x-api-key"),
                ])
        }
        _ => {
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([
                    axum::http::Method::GET,
                    axum::http::Method::POST,
                    axum::http::Method::OPTIONS,
                ])
                .allow_headers([
                    axum::http::header::CONTENT_TYPE,
                    axum::http::header::HeaderName::from_static("x-api-key"),
                ])
        }
    };

    // Public routes (GET — no auth needed)
    let public = Router::new()
        .route("/",                          get(root))
        .route("/health",                    get(health))
        .route("/chain/info",                get(chain_info))
        .route("/chain/blocks",              get(get_blocks))
        .route("/chain/blocks/:index",      get(get_block))
        .route("/chain/validate",            get(validate_chain))
        .route("/accounts/:address/balance", get(get_balance))
        .route("/accounts/:address/nonce",   get(get_nonce))
        .route("/transactions/:txid",       get(get_transaction))
        .route("/mempool",                   get(get_mempool))
        .route("/validators",                get(get_validators))
        .route("/tokens",                    get(list_tokens))
        .route("/tokens/:contract",         get(get_token_info))
        .route("/tokens/:contract/balance/:addr", get(get_token_balance))
        .route("/address/:address/history",  get(get_address_history))
        .route("/address/:address/info",     get(get_address_info));

    // Protected routes (POST — require X-API-Key if SENTRIX_API_KEY is set)
    let protected = Router::new()
        .route("/transactions",              post(send_transaction))
        .route("/tokens/deploy",             post(deploy_token))
        .route("/tokens/:contract/transfer", post(token_transfer))
        .route("/tokens/:contract/burn",     post(token_burn))
        .route("/rpc",                        post(rpc_dispatcher))
        .layer(middleware::from_fn(require_api_key));

    public
        .merge(protected)
        .nest("/explorer", explorer_router(state.clone()))
        .layer(cors)
        .with_state(state)
}

fn explorer_router(_state: SharedState) -> Router<SharedState> {
    Router::new()
        .route("/",             get(explorer::explorer_home))
        .route("/validators",   get(explorer::explorer_validators))
        .route("/tokens",       get(explorer::explorer_tokens))
        .route("/block/:index",     get(explorer::explorer_block))
        .route("/address/:address", get(explorer::explorer_address))
        .route("/tx/:txid",         get(explorer::explorer_tx))
}

// ── Handlers ─────────────────────────────────────────────
async fn root() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "name": "Sentrix",
        "chain_id": 7119,
        "version": "0.1.0",
        "docs": {
            "chain_info": "/chain/info",
            "blocks": "/chain/blocks",
            "tokens": "/tokens",
            "validators": "/validators",
            "explorer": "/explorer",
            "health": "/health",
            "rpc": "POST /rpc"
        }
    }))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "node": "sentrix-chain" }))
}

async fn chain_info(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    Json(bc.chain_stats())
}

// H-07 FIX: Paginated block listing (default 20, max 100, newest first)
async fn get_blocks(
    State(state): State<SharedState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let page: u64 = params.get("page").and_then(|p| p.parse().ok()).unwrap_or(0);
    let limit: u64 = params.get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(20)
        .min(100); // hard cap at 100

    let total = bc.chain.len() as u64;
    let start_skip = (page * limit) as usize;

    let blocks: Vec<serde_json::Value> = bc.chain.iter()
        .rev() // newest first
        .skip(start_skip)
        .take(limit as usize)
        .map(|b| serde_json::json!({
            "index": b.index,
            "hash": b.hash,
            "previous_hash": b.previous_hash,
            "timestamp": b.timestamp,
            "tx_count": b.tx_count(),
            "validator": b.validator,
            "merkle_root": b.merkle_root,
        }))
        .collect();

    let has_more = (start_skip + blocks.len()) < total as usize;

    Json(serde_json::json!({
        "blocks": blocks,
        "pagination": {
            "page": page,
            "limit": limit,
            "total": total,
            "has_more": has_more
        }
    }))
}

async fn get_block(
    State(state): State<SharedState>,
    Path(index): Path<u64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let bc = state.read().await;
    match bc.get_block(index) {
        Some(block) => Ok(Json(serde_json::to_value(block).unwrap())),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn validate_chain(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let valid = bc.is_valid_chain();
    Json(serde_json::json!({
        "valid": valid,
        "height": bc.height(),
        "total_blocks": bc.chain.len(),
    }))
}

async fn get_balance(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let balance = bc.accounts.get_balance(&address);
    Json(serde_json::json!({
        "address": address,
        "balance_sentri": balance,
        "balance_srx": balance as f64 / 100_000_000.0,
    }))
}

async fn get_nonce(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let nonce = bc.accounts.get_nonce(&address);
    Json(serde_json::json!({ "address": address, "nonce": nonce }))
}

async fn send_transaction(
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
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "success": false,
            "error": e.to_string(),
        })))),
    }
}

async fn get_transaction(
    State(state): State<SharedState>,
    Path(txid): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let bc = state.read().await;
    for block in bc.chain.iter() {
        for tx in block.transactions.iter() {
            if tx.txid == txid {
                return Ok(Json(serde_json::json!({
                    "transaction": tx,
                    "block_index": block.index,
                    "block_hash": block.hash,
                })));
            }
        }
    }
    Err(StatusCode::NOT_FOUND)
}

async fn get_mempool(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let txs: Vec<&Transaction> = bc.mempool.iter().collect();
    Json(serde_json::json!({
        "size": txs.len(),
        "transactions": txs,
    }))
}

async fn get_validators(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let validators: Vec<serde_json::Value> = bc.authority.validators.values().map(|v| serde_json::json!({
        "address": v.address,
        "name": v.name,
        "is_active": v.is_active,
        "blocks_produced": v.blocks_produced,
        "registered_at": v.registered_at,
    })).collect();
    Json(serde_json::json!({
        "validators": validators,
        "active": bc.authority.active_count(),
        "total": bc.authority.validator_count(),
    }))
}

// ── Token handlers ───────────────────────────────────────

async fn list_tokens(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let tokens = bc.list_tokens();
    Json(serde_json::json!({
        "tokens": tokens,
        "total": tokens.len(),
    }))
}

async fn get_token_info(
    State(state): State<SharedState>,
    Path(contract): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let bc = state.read().await;
    match bc.token_info(&contract) {
        Ok(info) => Ok(Json(info)),
        Err(_) => Err(StatusCode::NOT_FOUND),
    }
}

async fn get_token_balance(
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

async fn deploy_token(
    State(state): State<SharedState>,
    Json(req): Json<DeployTokenRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let wallet = Wallet::from_private_key(&req.from_key)
        .map_err(|_| api_err("invalid private key"))?;
    let sk = wallet.get_secret_key().map_err(|_| api_err("invalid key"))?;
    let pk = wallet.get_public_key().map_err(|_| api_err("invalid key"))?;

    let token_op = TokenOp::Deploy {
        name: req.name.clone(),
        symbol: req.symbol.clone(),
        decimals: req.decimals,
        supply: req.total_supply,
    };

    let mut bc = state.write().await;
    let nonce = bc.accounts.get_nonce(&wallet.address);
    let chain_id = bc.chain_id;
    let data = token_op.encode().map_err(|e| api_err(&e.to_string()))?;

    let tx = Transaction::new(
        wallet.address.clone(), TOKEN_OP_ADDRESS.to_string(),
        0, req.deploy_fee, nonce, data, chain_id, &sk, &pk,
    ).map_err(|e| api_err(&e.to_string()))?;

    let txid = tx.txid.clone();
    bc.add_to_mempool(tx).map_err(|e| api_err(&e.to_string()))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "txid": txid,
        "deployer": wallet.address,
        "name": req.name,
        "symbol": req.symbol,
        "total_supply": req.total_supply,
        "status": "pending_in_mempool",
    })))
}

async fn token_transfer(
    State(state): State<SharedState>,
    Path(contract): Path<String>,
    Json(req): Json<TokenTransferRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let wallet = Wallet::from_private_key(&req.from_key)
        .map_err(|_| api_err("invalid private key"))?;
    let sk = wallet.get_secret_key().map_err(|_| api_err("invalid key"))?;
    let pk = wallet.get_public_key().map_err(|_| api_err("invalid key"))?;

    let token_op = TokenOp::Transfer {
        contract: contract.clone(),
        to: req.to.clone(),
        amount: req.amount,
    };

    let mut bc = state.write().await;
    let nonce = bc.accounts.get_nonce(&wallet.address);
    let chain_id = bc.chain_id;
    let data = token_op.encode().map_err(|e| api_err(&e.to_string()))?;

    let tx = Transaction::new(
        wallet.address.clone(), TOKEN_OP_ADDRESS.to_string(),
        0, req.gas_fee, nonce, data, chain_id, &sk, &pk,
    ).map_err(|e| api_err(&e.to_string()))?;

    let txid = tx.txid.clone();
    bc.add_to_mempool(tx).map_err(|e| api_err(&e.to_string()))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "txid": txid,
        "contract": contract,
        "from": wallet.address,
        "to": req.to,
        "amount": req.amount,
        "status": "pending_in_mempool",
    })))
}

async fn token_burn(
    State(state): State<SharedState>,
    Path(contract): Path<String>,
    Json(req): Json<TokenBurnRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let wallet = Wallet::from_private_key(&req.from_key)
        .map_err(|_| api_err("invalid private key"))?;
    let sk = wallet.get_secret_key().map_err(|_| api_err("invalid key"))?;
    let pk = wallet.get_public_key().map_err(|_| api_err("invalid key"))?;

    let token_op = TokenOp::Burn {
        contract: contract.clone(),
        amount: req.amount,
    };

    let mut bc = state.write().await;
    let nonce = bc.accounts.get_nonce(&wallet.address);
    let chain_id = bc.chain_id;
    let data = token_op.encode().map_err(|e| api_err(&e.to_string()))?;

    let tx = Transaction::new(
        wallet.address.clone(), TOKEN_OP_ADDRESS.to_string(),
        0, req.gas_fee, nonce, data, chain_id, &sk, &pk,
    ).map_err(|e| api_err(&e.to_string()))?;

    let txid = tx.txid.clone();
    bc.add_to_mempool(tx).map_err(|e| api_err(&e.to_string()))?;

    Ok(Json(serde_json::json!({
        "success": true,
        "txid": txid,
        "contract": contract,
        "burned_by": wallet.address,
        "amount": req.amount,
        "status": "pending_in_mempool",
    })))
}

// Helper for API error responses
fn api_err(msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (StatusCode::BAD_REQUEST, Json(serde_json::json!({"success": false, "error": msg})))
}

// ── Address history handlers ─────────────────────────────

// L-03 FIX: paginated address history
async fn get_address_history(
    State(state): State<SharedState>,
    Path(address): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let limit: usize = params.get("limit").and_then(|l| l.parse().ok()).unwrap_or(20).min(100);
    let offset: usize = params.get("offset").and_then(|o| o.parse().ok()).unwrap_or(0);
    let history = bc.get_address_history(&address, limit, offset);
    let count = history.len();
    Json(serde_json::json!({
        "address": address,
        "transactions": history,
        "count": count,
        "pagination": { "limit": limit, "offset": offset, "has_more": count == limit }
    }))
}

async fn get_address_info(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let balance = bc.accounts.get_balance(&address);
    let nonce = bc.accounts.get_nonce(&address);
    let tx_count = bc.get_address_tx_count(&address);
    Json(serde_json::json!({
        "address": address,
        "balance_sentri": balance,
        "balance_srx": balance as f64 / 100_000_000.0,
        "nonce": nonce,
        "tx_count": tx_count,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_h05_constant_time_eq() {
        // Equal strings
        assert!(constant_time_eq("abc123", "abc123"));
        assert!(constant_time_eq("", ""));
        assert!(constant_time_eq("sentrix-api-key-xyz", "sentrix-api-key-xyz"));

        // Unequal strings (same length)
        assert!(!constant_time_eq("abc123", "abc124"));
        assert!(!constant_time_eq("aaaaaa", "bbbbbb"));

        // Different lengths
        assert!(!constant_time_eq("short", "longer_string"));
        assert!(!constant_time_eq("abc", "ab"));
        assert!(!constant_time_eq("", "x"));
    }
}
