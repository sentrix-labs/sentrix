// routes.rs - Sentrix Chain

use axum::{
    Router,
    routing::{get, post},
    extract::{State, Path},
    Json,
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::core::blockchain::Blockchain;
use crate::core::transaction::Transaction;
use crate::api::jsonrpc::rpc_dispatcher;

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
    pub deployer: String,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub total_supply: u64,
    pub deploy_fee: u64,
}

#[derive(Deserialize)]
pub struct TokenTransferRequest {
    pub caller: String,
    pub to: String,
    pub amount: u64,
    pub gas_fee: u64,
}

// ── Router ───────────────────────────────────────────────
pub fn create_router(state: SharedState) -> Router {
    Router::new()
        .route("/health",                    get(health))
        .route("/chain/info",                get(chain_info))
        .route("/chain/blocks",              get(get_blocks))
        .route("/chain/blocks/{index}",      get(get_block))
        .route("/chain/validate",            get(validate_chain))
        .route("/accounts/{address}/balance", get(get_balance))
        .route("/accounts/{address}/nonce",   get(get_nonce))
        .route("/transactions",              post(send_transaction))
        .route("/transactions/{txid}",       get(get_transaction))
        .route("/mempool",                   get(get_mempool))
        .route("/validators",                get(get_validators))
        .route("/tokens",                    get(list_tokens))
        .route("/tokens/deploy",             post(deploy_token))
        .route("/tokens/{contract}",         get(get_token_info))
        .route("/tokens/{contract}/balance/{addr}", get(get_token_balance))
        .route("/tokens/{contract}/transfer", post(token_transfer))
        .route("/address/{address}/history",  get(get_address_history))
        .route("/address/{address}/info",     get(get_address_info))
        .route("/rpc",                        post(rpc_dispatcher))
        .with_state(state)
}

// ── Handlers ─────────────────────────────────────────────
async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "node": "sentrix-chain" }))
}

async fn chain_info(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    Json(bc.chain_stats())
}

async fn get_blocks(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let blocks: Vec<serde_json::Value> = bc.chain.iter().map(|b| serde_json::json!({
        "index": b.index,
        "hash": b.hash,
        "previous_hash": b.previous_hash,
        "timestamp": b.timestamp,
        "tx_count": b.tx_count(),
        "validator": b.validator,
        "merkle_root": b.merkle_root,
    })).collect();
    Json(serde_json::json!({ "blocks": blocks, "total": bc.chain.len() }))
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
    let mut bc = state.write().await;
    match bc.deploy_token(
        &req.deployer, req.name.clone(), req.symbol.clone(),
        req.decimals, req.total_supply, req.deploy_fee,
    ) {
        Ok(addr) => Ok(Json(serde_json::json!({
            "success": true,
            "contract_address": addr,
            "name": req.name,
            "symbol": req.symbol,
            "total_supply": req.total_supply,
        }))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "success": false,
            "error": e.to_string(),
        })))),
    }
}

async fn token_transfer(
    State(state): State<SharedState>,
    Path(contract): Path<String>,
    Json(req): Json<TokenTransferRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let mut bc = state.write().await;
    match bc.token_transfer(&contract, &req.caller, &req.to, req.amount, req.gas_fee) {
        Ok(()) => Ok(Json(serde_json::json!({
            "success": true,
            "contract": contract,
            "from": req.caller,
            "to": req.to,
            "amount": req.amount,
        }))),
        Err(e) => Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({
            "success": false,
            "error": e.to_string(),
        })))),
    }
}

// ── Address history handlers ─────────────────────────────

async fn get_address_history(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let history = bc.get_address_history(&address);
    let total = history.len();
    Json(serde_json::json!({
        "address": address,
        "transactions": history,
        "total": total,
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
