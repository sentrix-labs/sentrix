// routes.rs - Sentrix

use axum::{
    Router,
    routing::{get, post},
    extract::{State, Path, FromRequestParts},
    Json,
    http::{StatusCode, request::Parts},
    response::IntoResponse,
};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::collections::HashMap;
// V6-M-03 FIX: tokio::sync::Mutex is async-safe — does not block Tokio worker threads.
// std::sync::Mutex::lock() is a blocking syscall; holding it in async context
// starves other tasks on the same thread under high load.
use tokio::sync::Mutex;
use std::time::Instant;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::cors::{CorsLayer, Any};
use crate::core::blockchain::Blockchain;
use crate::core::transaction::{Transaction, TokenOp};
use crate::core::trie::address::{address_to_key, account_value_decode};
use crate::api::jsonrpc::rpc_dispatcher;
use crate::api::explorer;

// ── API key extractor ─────────────────────────────────────
// Add `_auth: ApiKey` as the first parameter of any handler that needs auth.
// Returns 401 if SENTRIX_API_KEY is set and the request doesn't match.
pub struct ApiKey;

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for ApiKey {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let required = match std::env::var("SENTRIX_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => return Ok(ApiKey), // no key set → always allow
        };
        let provided = parts.headers
            .get("X-API-Key")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if constant_time_eq(provided, &required) {
            Ok(ApiKey)
        } else {
            Err(StatusCode::UNAUTHORIZED)
        }
    }
}

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

// C-01 FIX: Token endpoints now accept pre-signed transactions.
// Private keys MUST be kept client-side — server never receives them.
// Client: build TokenOp JSON → put in tx.data → sign tx → POST here.
#[derive(Deserialize)]
pub struct SignedTxRequest {
    pub transaction: Transaction,
}

// H-05 FIX: Constant-time string comparison — no early return on length mismatch
// (early return leaks key length via timing oracle)
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let max_len = a_bytes.len().max(b_bytes.len());
    // Include length difference in result so mismatched lengths always return false
    let mut result: u8 = (a_bytes.len() ^ b_bytes.len()) as u8;
    for i in 0..max_len {
        let ab = if i < a_bytes.len() { a_bytes[i] } else { 0 };
        let bb = if i < b_bytes.len() { b_bytes[i] } else { 0 };
        result |= ab ^ bb;
    }
    result == 0
}


// ── Per-IP Rate Limiter (V5-06) ──────────────────────────
pub type IpRateLimiter = Arc<Mutex<HashMap<String, (u32, Instant)>>>;
const RATE_LIMIT_WINDOW_SECS: u64 = 60;
const RATE_LIMIT_MAX_REQUESTS: u32 = 60;

async fn ip_rate_limit_middleware(
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    // Extract client IP from proxy headers (set by nginx) or fall back to "unknown"
    let ip = request.headers()
        .get("x-forwarded-for")
        .or_else(|| request.headers().get("x-real-ip"))
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let allowed = if let Some(limiter) = request.extensions().get::<IpRateLimiter>().cloned() {
        let mut map = limiter.lock().await;  // V6-M-03: async lock — yields instead of blocking thread
        let now = Instant::now();
        let entry = map.entry(ip).or_insert((0, now));
        if entry.1.elapsed().as_secs() >= RATE_LIMIT_WINDOW_SECS {
            *entry = (1, now);
            true
        } else {
            entry.0 += 1;
            entry.0 <= RATE_LIMIT_MAX_REQUESTS
        }
    } else {
        true // limiter not configured — allow
    };

    if allowed {
        next.run(request).await
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": "rate limit exceeded",
                "limit": RATE_LIMIT_MAX_REQUESTS,
                "window_secs": RATE_LIMIT_WINDOW_SECS,
            }))
        ).into_response()
    }
}

// ── Router ───────────────────────────────────────────────
pub fn create_router(state: SharedState) -> Router {
    // M-06 FIX: CORS — fail-safe restrictive default.
    // If SENTRIX_CORS_ORIGIN is not set → no cross-origin allowed (safest default).
    // Use SENTRIX_CORS_ORIGIN=* only for local development; set specific origins in production.
    let cors = match std::env::var("SENTRIX_CORS_ORIGIN").ok().as_deref() {
        Some("*") => {
            // Explicit wildcard — allow all origins (dev only)
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
        Some(origin) if !origin.is_empty() => {
            // Specific origin (production)
            CorsLayer::new()
                .allow_origin(
                    origin.parse::<axum::http::HeaderValue>()
                        .unwrap_or(axum::http::HeaderValue::from_static("null"))
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
            // M-06 FIX: Not set → restrictive default, no cross-origin requests allowed.
            // Set SENTRIX_CORS_ORIGIN in .env to enable cross-origin access.
            CorsLayer::new()
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

    // V5-06: Per-IP rate limiter shared across all requests
    let rate_limiter: IpRateLimiter = Arc::new(Mutex::new(HashMap::new()));

    // Single router — auth is enforced via the ApiKey extractor embedded
    // in each protected handler's parameter list, not via route layers.
    Router::new()
        // ── Public GET routes ────────────────────────────────────
        .route("/",                               get(root))
        .route("/health",                         get(health))
        .route("/chain/info",                     get(chain_info))
        .route("/chain/blocks",                   get(get_blocks))
        .route("/chain/blocks/:index",            get(get_block))
        .route("/chain/validate",                 get(validate_chain))
        .route("/accounts/:address/balance",      get(get_balance))
        .route("/accounts/:address/nonce",        get(get_nonce))
        .route("/mempool",                        get(get_mempool))
        .route("/validators",                     get(get_validators))
        // ── Short-form aliases (CoinBlast / Faucet) ──────────────
        .route("/blocks",                         get(get_blocks))
        .route("/blocks/:height",                 get(get_block))
        .route("/wallets/:address",               get(get_wallet_info))
        .route("/transactions",                   get(list_transactions).post(send_transaction))
        .route("/transactions/:txid",             get(get_transaction))
        // ── Token endpoints ──────────────────────────────────────
        .route("/tokens",                         get(list_tokens))
        .route("/tokens/:contract",               get(get_token_info))
        .route("/tokens/:contract/balance/:addr", get(get_token_balance))
        .route("/tokens/:contract/holders",       get(get_token_holders_list))
        .route("/tokens/:contract/trades",        get(get_token_trades_list))
        .route("/tokens/deploy",                  post(deploy_token))
        .route("/tokens/:contract/transfer",      post(token_transfer))
        .route("/tokens/:contract/burn",          post(token_burn))
        // ── Rich list ────────────────────────────────────────────
        .route("/richlist",                       get(get_richlist))
        // ── Address history ──────────────────────────────────────
        .route("/address/:address/history",       get(get_address_history))
        .route("/address/:address/info",          get(get_address_info))
        // ── State trie ───────────────────────────────────────────
        .route("/address/:address/proof",         get(get_address_proof))
        .route("/chain/state-root/:height",       get(get_state_root))
        // ── RPC ──────────────────────────────────────────────────
        .route("/rpc",                            post(rpc_dispatcher))
        // ── Admin ────────────────────────────────────────────────
        .route("/admin/log",                      get(get_admin_log))
        // ── Stats ────────────────────────────────────────────────
        .route("/stats/daily",                    get(explorer::stats_daily))
        // ── Explorer ─────────────────────────────────────────────
        .nest("/explorer", explorer_router(state.clone()))
        .layer(cors)
        // C-02 FIX: Global HTTP concurrency limit — prevent CPU saturation from concurrent heavy
        // requests (e.g. /chain/validate).
        .layer(ConcurrencyLimitLayer::new(500))
        // V5-06: Per-IP rate limit (60 req/min, defense-in-depth behind nginx)
        // Layer order: Extension (outer) → rate_limit middleware → concurrency → cors → handler
        .layer(axum::middleware::from_fn(ip_rate_limit_middleware))
        .layer(axum::Extension(rate_limiter))
        .with_state(state)
}

fn explorer_router(_state: SharedState) -> Router<SharedState> {
    Router::new()
        .route("/",                 get(explorer::explorer_home))
        .route("/blocks",           get(explorer::explorer_blocks))
        .route("/transactions",     get(explorer::explorer_transactions))
        .route("/validators",       get(explorer::explorer_validators))
        .route("/tokens",           get(explorer::explorer_tokens))
        .route("/richlist",           get(explorer::explorer_richlist))
        .route("/mempool",            get(explorer::explorer_mempool))
        .route("/validator/:address", get(explorer::explorer_validator))
        .route("/token/:contract",    get(explorer::explorer_token))
        .route("/block/:index",       get(explorer::explorer_block))
        .route("/address/:address",   get(explorer::explorer_address))
        .route("/tx/:txid",           get(explorer::explorer_tx))
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

    let total = bc.height() + 1; // I-01 FIX: use true height, not window size
    let start_skip = (page * limit) as usize;

    let blocks: Vec<serde_json::Value> = bc.chain.iter()
        .rev() // newest first (window only — last CHAIN_WINDOW_SIZE blocks)
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
        Some(block) => serde_json::to_value(block)
            .map(Json)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR), // L-05 FIX: no unwrap
        None => Err(StatusCode::NOT_FOUND),
    }
}

// M-07 FIX: Cache last validation result per block height to avoid O(n) recompute on every call.
static VALIDATE_CACHE_HEIGHT: AtomicU64 = AtomicU64::new(u64::MAX);
static VALIDATE_CACHE_RESULT: AtomicBool = AtomicBool::new(false);

// M-07 FIX: validate_chain now requires X-API-Key authentication.
// An O(n) full chain scan on a 92,000+ block chain per unauthenticated request = DoS vector.
async fn validate_chain(
    _auth: ApiKey,
    State(state): State<SharedState>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let height = bc.height();

    // Return cached result if chain height hasn't changed since last run
    if VALIDATE_CACHE_HEIGHT.load(Ordering::Relaxed) == height {
        let cached_valid = VALIDATE_CACHE_RESULT.load(Ordering::Relaxed);
        return Json(serde_json::json!({
            "valid": cached_valid,
            "height": height,
            "total_blocks": bc.height() + 1, // I-01 FIX: true total, not window size
            "cached": true,
        }));
    }

    // Full O(n) chain scan — only runs when height has changed
    let valid = bc.is_valid_chain();
    VALIDATE_CACHE_HEIGHT.store(height, Ordering::Relaxed);
    VALIDATE_CACHE_RESULT.store(valid, Ordering::Relaxed);

    Json(serde_json::json!({
        "valid": valid,
        "height": height,
        "total_blocks": bc.chain.len(),
        "cached": false,
    }))
}

async fn get_balance(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    // H-05 FIX: Normalize address to lowercase (REST vs RPC consistency)
    let address = address.to_lowercase();
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
    // H-05 FIX: Normalize address to lowercase
    let address = address.to_lowercase();
    let bc = state.read().await;
    let nonce = bc.accounts.get_nonce(&address);
    Json(serde_json::json!({ "address": address, "nonce": nonce }))
}

async fn send_transaction(
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

// C-01 FIX: Token endpoints no longer accept private keys.
// Client must sign the transaction locally:
//   1. Build TokenOp JSON → put in tx.data
//   2. Set tx.to_address = TOKEN_OP_ADDRESS ("0x0000000000000000000000000000000000000000")
//   3. Sign with local private key
//   4. POST { "transaction": <signed_tx> } to this endpoint

async fn deploy_token(
    _auth: ApiKey,
    State(state): State<SharedState>,
    Json(req): Json<SignedTxRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let tx = req.transaction;
    // Validate data contains a Deploy token op
    let op = TokenOp::decode(&tx.data)
        .ok_or_else(|| api_err("data must contain a valid TokenOp JSON"))?;
    let (name, symbol, total_supply, max_supply) = match &op {
        TokenOp::Deploy { name, symbol, supply, max_supply, .. } => {
            (name.clone(), symbol.clone(), *supply, *max_supply)
        }
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

async fn token_transfer(
    _auth: ApiKey,
    State(state): State<SharedState>,
    Path(contract): Path<String>,
    Json(req): Json<SignedTxRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let tx = req.transaction;
    let op = TokenOp::decode(&tx.data)
        .ok_or_else(|| api_err("data must contain a valid TokenOp JSON"))?;
    let (to_addr, amount) = match &op {
        TokenOp::Transfer { contract: c, to, amount } => {
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

async fn token_burn(
    _auth: ApiKey,
    State(state): State<SharedState>,
    Path(contract): Path<String>,
    Json(req): Json<SignedTxRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)> {
    let tx = req.transaction;
    let op = TokenOp::decode(&tx.data)
        .ok_or_else(|| api_err("data must contain a valid TokenOp JSON"))?;
    let amount = match &op {
        TokenOp::Burn { contract: c, amount } => {
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

// ── Short-form alias handlers ────────────────────────────

async fn get_wallet_info(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let balance = bc.accounts.get_balance(&address);
    let nonce = bc.accounts.get_nonce(&address);
    // V6-M-04: get_address_tx_count returns window-aware metadata (see chain_queries.rs)
    let tx_count_info = bc.get_address_tx_count(&address);
    Json(serde_json::json!({
        "address": address,
        "balance_sentri": balance,
        "balance_srx": balance as f64 / 100_000_000.0,
        "nonce": nonce,
        "tx_count": tx_count_info,
    }))
}

async fn list_transactions(
    State(state): State<SharedState>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let limit: usize = params.get("limit").and_then(|l| l.parse().ok()).unwrap_or(20).min(100);
    let offset: usize = params.get("offset").and_then(|o| o.parse().ok()).unwrap_or(0);
    let txs = bc.get_latest_transactions(limit, offset);
    let count = txs.len();
    Json(serde_json::json!({
        "transactions": txs,
        "count": count,
        "pagination": { "limit": limit, "offset": offset },
    }))
}

async fn get_token_holders_list(
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

async fn get_token_trades_list(
    State(state): State<SharedState>,
    Path(contract): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let limit: usize = params.get("limit").and_then(|l| l.parse().ok()).unwrap_or(20).min(100);
    let offset: usize = params.get("offset").and_then(|o| o.parse().ok()).unwrap_or(0);
    let trades = bc.get_token_trades(&contract, limit, offset);
    let count = trades.len();
    Json(serde_json::json!({
        "contract": contract,
        "trades": trades,
        "count": count,
        "pagination": { "limit": limit, "offset": offset },
    }))
}

async fn get_richlist(State(state): State<SharedState>) -> Json<serde_json::Value> {
    const TOTAL_SUPPLY_SENTRI: u64 = 210_000_000 * 100_000_000;
    let bc = state.read().await;
    let mut holders: Vec<serde_json::Value> = bc.accounts.accounts
        .iter()
        .filter(|(_, a)| a.balance > 0)
        .map(|(addr, a)| {
            let pct = a.balance as f64 / TOTAL_SUPPLY_SENTRI as f64 * 100.0;
            serde_json::json!({
                "address": addr,
                "balance_sentri": a.balance,
                "balance_srx": a.balance as f64 / 100_000_000.0,
                "percent_of_supply": pct,
            })
        })
        .collect();
    holders.sort_by(|a, b| {
        let ba = a["balance_sentri"].as_u64().unwrap_or(0);
        let bb = b["balance_sentri"].as_u64().unwrap_or(0);
        bb.cmp(&ba)
    });
    holders.truncate(50);
    let total = holders.len();
    Json(serde_json::json!({ "holders": holders, "total": total }))
}

// I-03: Admin audit log — requires X-API-Key authentication
async fn get_admin_log(
    _auth: ApiKey,
    State(state): State<SharedState>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    Json(serde_json::json!({
        "log": bc.authority.admin_log,
        "count": bc.authority.admin_log.len(),
    }))
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
    // V6-M-04: get_address_tx_count returns window-aware metadata (see chain_queries.rs)
    let tx_count_info = bc.get_address_tx_count(&address);
    Json(serde_json::json!({
        "address": address,
        "balance_sentri": balance,
        "balance_srx": balance as f64 / 100_000_000.0,
        "nonce": nonce,
        "tx_count": tx_count_info,
    }))
}

// ── State-trie endpoints ──────────────────────────────────────

/// GET /address/:address/proof
/// Returns a Merkle membership/non-membership proof for the address in the current state trie.
/// Requires the trie to be initialized (init_trie called at node startup).
async fn get_address_proof(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    // V7-L-02: validate address format BEFORE acquiring any lock.
    // Rejects obviously-invalid inputs early (no lock overhead, no trie traversal).
    if !crate::core::blockchain::is_valid_sentrix_address(&address) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid address format: expected 0x + 40 hex chars",
                "received": address,
            })),
        )
            .into_response();
    }

    // V7-M-03: downgrade from WRITE to READ lock.
    // prove() previously took &mut self due to LRU mutation; TrieCache now uses an
    // internal Mutex<LruCache>, so prove() takes &self — a read lock is sufficient.
    // This prevents proof requests from blocking block production (which needs a write lock).
    let bc = state.read().await;
    let key = address_to_key(&address);
    match bc.state_trie.as_ref() {
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "state trie not initialized",
                "hint": "node must be started with --trie flag"
            })),
        )
            .into_response(),
        Some(trie) => match trie.prove(&key) {
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response(),
            Ok(proof) => {
                let (balance, nonce) = if proof.found {
                    account_value_decode(&proof.value)
                        .map(|(b, n)| (Some(b), Some(n)))
                        .unwrap_or((None, None))
                } else {
                    (None, None)
                };
                Json(serde_json::json!({
                    "address": address,
                    "found": proof.found,
                    "balance_sentri": balance,
                    "nonce": nonce,
                    "key_hex": hex::encode(proof.key),
                    "depth": proof.depth,
                    "terminal_hash_hex": hex::encode(proof.terminal_hash),
                    "siblings_hex": proof.siblings.iter().map(hex::encode).collect::<Vec<_>>(),
                    "root_hex": hex::encode(trie.root_hash()),
                    // V7-I-01: document proof scope — only native SRX is committed.
                    "scope": "native_srx_only",
                    "scope_note": "Proof covers native SRX balance and nonce only. SRX-20 token balances are not committed to the state root.",
                }))
                .into_response()
            }
        },
    }
}

/// GET /chain/state-root/:height
/// Returns the committed state root hash for the given block height.
async fn get_state_root(
    State(state): State<SharedState>,
    Path(height): Path<u64>,
) -> impl IntoResponse {
    let bc = state.read().await;
    match bc.state_trie.as_ref() {
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "state trie not initialized"
            })),
        )
            .into_response(),
        Some(trie) => match trie.root_at_version(height) {
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response(),
            Ok(opt) => Json(serde_json::json!({
                "height": height,
                "state_root_hex": opt.map(hex::encode),
            }))
            .into_response(),
        },
    }
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

    // ── M-06: constant_time_eq length-independence ────────

    #[test]
    fn test_m06_constant_time_eq_no_early_exit_on_length_mismatch() {
        // Both comparisons must traverse the full max-length loop, not short-circuit.
        // We verify correctness: different lengths always return false regardless of content.
        assert!(!constant_time_eq("a", "aa"));
        assert!(!constant_time_eq("aa", "a"));
        assert!(!constant_time_eq("key_32_chars_long_abcdefghijklmn", "key_32_chars_long_abcdefghijklm"));
        // Prefix match but different length — must still fail
        assert!(!constant_time_eq("sentrix", "sentrix_extra"));
    }

    #[test]
    fn test_m06_constant_time_eq_same_length_wrong_content() {
        // Same length, different content — must be false
        assert!(!constant_time_eq("AAAAAAAAAAAAAAAA", "AAAAAAAAAAAAAAAB"));
        assert!(!constant_time_eq("0000000000000000", "0000000000000001"));
        // Verify it's really comparing all bytes (last byte differs)
        assert!(!constant_time_eq("abcdefghijklmnop", "abcdefghijklmnoq"));
    }

    #[test]
    fn test_m06_constant_time_eq_empty_cases() {
        // Edge cases
        assert!(constant_time_eq("", ""));
        assert!(!constant_time_eq("", "a"));
        assert!(!constant_time_eq("a", ""));
    }

    // ── M-07: validate_chain cache logic ──────────────────

    #[test]
    fn test_m07_validate_cache_statics_initialized() {
        // VALIDATE_CACHE_HEIGHT starts at u64::MAX (sentinel = never cached)
        // VALIDATE_CACHE_RESULT starts at false
        // After a fresh start, loading the statics should not panic.
        let h = VALIDATE_CACHE_HEIGHT.load(std::sync::atomic::Ordering::Relaxed);
        let r = VALIDATE_CACHE_RESULT.load(std::sync::atomic::Ordering::Relaxed);
        // Height is either u64::MAX (never run) or a valid height from a previous test.
        // Either way, the atomics must be readable without panic.
        let _ = h;
        let _ = r;
    }

    #[test]
    fn test_m07_validate_cache_update() {
        // Simulate the cache update logic used by validate_chain handler.
        let test_height: u64 = 12_345;
        let test_valid = true;

        VALIDATE_CACHE_HEIGHT.store(test_height, std::sync::atomic::Ordering::Relaxed);
        VALIDATE_CACHE_RESULT.store(test_valid, std::sync::atomic::Ordering::Relaxed);

        // Reading back should match
        assert_eq!(VALIDATE_CACHE_HEIGHT.load(std::sync::atomic::Ordering::Relaxed), test_height);
        assert_eq!(VALIDATE_CACHE_RESULT.load(std::sync::atomic::Ordering::Relaxed), test_valid);

        // Different height means cache miss (simulate)
        let different_height = test_height + 1;
        let is_cache_hit = VALIDATE_CACHE_HEIGHT.load(std::sync::atomic::Ordering::Relaxed) == different_height;
        assert!(!is_cache_hit);

        // Same height is a cache hit
        let is_same_hit = VALIDATE_CACHE_HEIGHT.load(std::sync::atomic::Ordering::Relaxed) == test_height;
        assert!(is_same_hit);
    }

    // ── L-05: serde_json error propagation tests ──────────

    #[test]
    fn test_l05_block_serializes_to_json_value() {
        // Verify Block can be serialized without panic — confirming map_err path is safe.
        use crate::core::blockchain::Blockchain;
        let bc = Blockchain::new("admin".to_string());
        let block = &bc.chain[0];
        let result = serde_json::to_value(block);
        assert!(result.is_ok(), "genesis block must serialize cleanly");
        let val = result.unwrap();
        assert!(val.get("index").is_some());
        assert!(val.get("hash").is_some());
    }

    #[test]
    fn test_l05_no_unwrap_in_get_block_response_path() {
        // Ensure the fix compiles: serde_json::to_value(...).map(Json).map_err(...)
        // This test validates the fix by exercising serde serialization of a Block.
        use crate::core::blockchain::Blockchain;
        let bc = Blockchain::new("admin".to_string());
        let block = bc.chain[0].clone();

        // Replicate what the handler does (without the StatusCode wrapper)
        let serialized = serde_json::to_value(&block);
        assert!(serialized.is_ok());
        let json_val = serialized.unwrap();
        assert_eq!(json_val["index"], 0);
    }

    // ── I-03: admin log serialization tests ──────────────

    #[test]
    fn test_i03_admin_log_serializes_to_json() {
        // AdminEvent must serialize cleanly for the /admin/log endpoint
        use crate::core::authority::AdminEvent;
        let event = AdminEvent {
            operation: "add_validator".to_string(),
            caller: "admin".to_string(),
            target_address: "0xabc123".to_string(),
            target_name: "Validator 1".to_string(),
            timestamp: 1_700_000_000,
        };
        let val = serde_json::to_value(&event).unwrap();
        assert_eq!(val["operation"], "add_validator");
        assert_eq!(val["caller"], "admin");
        assert_eq!(val["target_address"], "0xabc123");
        assert_eq!(val["target_name"], "Validator 1");
        assert_eq!(val["timestamp"], 1_700_000_000_u64);
    }

    #[test]
    fn test_i03_admin_log_in_blockchain_context() {
        // Verify admin_log is accessible on the blockchain state (used by /admin/log handler)
        use crate::core::blockchain::Blockchain;
        let bc = Blockchain::new("admin".to_string());
        // Fresh blockchain has an empty admin log
        assert_eq!(bc.authority.admin_log.len(), 0);
        // The log serializes correctly
        let log_json = serde_json::to_value(&bc.authority.admin_log).unwrap();
        assert!(log_json.is_array());
    }
}
