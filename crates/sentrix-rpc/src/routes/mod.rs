// routes/mod.rs - Sentrix REST API. Shared bits (auth, rate limiting,
// request/response DTOs) live in sibling submodules; this file wires
// them up via `create_router` and carries the per-resource handlers
// (chain / accounts / staking / tokens / epoch / ops). Phase 2 of the
// backlog #12 refactor will further split those handler groups into
// dedicated modules. (backlog #12 phase 1)

mod accounts;
mod auth;
mod cache;
mod chain;
mod epoch;
mod ops;
mod ratelimit;
mod staking;
mod tokens;
mod transactions;
mod types;

pub use auth::{ApiKey, constant_time_eq};
pub use ops::sentrix_status;
pub use ratelimit::{GlobalIpLimiter, IpRateLimiter, WriteIpLimiter};
pub use types::{ApiResponse, SendTxRequest, SignedTxRequest};

use accounts::{
    get_address_history, get_address_info, get_address_proof, get_balance, get_nonce, get_richlist,
    get_state_root, get_wallet_info, list_transactions,
};
use chain::{chain_info, get_block, get_blocks, validate_chain};
use epoch::{epoch_current, epoch_history};
use ops::{START_TIME, get_admin_log, health, metrics, root};
use cache::cache_control_middleware;
use ratelimit::{ip_rate_limit_middleware, write_rate_limit_middleware};
use staking::{get_validators, staking_delegations, staking_unbonding, staking_validators};
use tokens::{
    deploy_token, get_token_balance, get_token_holders_list, get_token_info, get_token_trades_list,
    list_tokens, token_burn, token_transfer,
};
use transactions::{get_mempool, get_transaction, send_transaction};

use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    http::StatusCode,
    routing::{get, post},
};
use std::collections::HashMap;

// tokio::sync::Mutex is async-safe — does not block Tokio worker threads.
// std::sync::Mutex::lock() is a blocking syscall; holding it in async context
// starves other tasks on the same thread under high load.
use crate::explorer;
use crate::jsonrpc::rpc_dispatcher;
use sentrix_core::blockchain::Blockchain;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::cors::{Any, CorsLayer};

pub type SharedState = Arc<RwLock<Blockchain>>;

// ── Router ───────────────────────────────────────────────
pub fn create_router(state: SharedState) -> Router {
    // Eagerly pin the process start time so /sentrix_status and /metrics
    // report uptime relative to boot, not to the first handler call that
    // happened to trigger the OnceLock. Without this, uptime_seconds was
    // 0 on the first /sentrix_status request and undercounted thereafter.
    let _ = START_TIME.get_or_init(Instant::now);

    // CORS uses a fail-safe restrictive default — no cross-origin allowed unless SENTRIX_CORS_ORIGIN is set.
    // Use SENTRIX_CORS_ORIGIN=* for local development only; set specific origins in production.
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
            // Specific origin (production). Silently falling back to the
            // literal "null" header on parse failure meant a typo in
            // SENTRIX_CORS_ORIGIN produced a router that rejected every
            // browser request without surfacing the misconfig. Panic at
            // startup instead so operators see the problem immediately.
            let header = origin.parse::<axum::http::HeaderValue>().unwrap_or_else(|e| {
                panic!(
                    "SENTRIX_CORS_ORIGIN={origin:?} is not a valid HTTP header value: {e}"
                )
            });
            CorsLayer::new()
                .allow_origin(header)
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
            // No SENTRIX_CORS_ORIGIN set → restrictive default, no cross-origin requests allowed.
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

    // A6/A7: separate counters for global vs write paths so an attacker
    // hitting writes burns the write quota first without starving read
    // traffic from the same IP.
    let global_limiter = GlobalIpLimiter(Arc::new(Mutex::new(HashMap::new())));
    let write_limiter = WriteIpLimiter(Arc::new(Mutex::new(HashMap::new())));

    // A7: Write endpoints carry a stricter rate limit (10 req/min per IP).
    // Built as a sub-router so the write middleware applies only to these
    // routes; the merged outer router still enforces the global 60/min cap.
    let write_router: Router<SharedState> = Router::new()
        .route("/transactions", post(send_transaction))
        .route("/tokens/deploy", post(deploy_token))
        .route("/tokens/{contract}/transfer", post(token_transfer))
        .route("/tokens/{contract}/burn", post(token_burn))
        .route("/rpc", post(rpc_dispatcher))
        .layer(axum::middleware::from_fn(write_rate_limit_middleware))
        .layer(axum::Extension(write_limiter));

    // Single router — auth is enforced via the ApiKey extractor embedded
    // in each protected handler's parameter list, not via route layers.
    Router::new()
        // ── Public GET routes ────────────────────────────────────
        .route("/", get(root))
        .route("/health", get(health))
        .route("/sentrix_status", get(sentrix_status))
        .route("/metrics", get(metrics))
        .route("/chain/info", get(chain_info))
        .route("/chain/blocks", get(get_blocks))
        .route("/chain/blocks/{index}", get(get_block))
        .route("/chain/validate", get(validate_chain))
        .route("/accounts/{address}/balance", get(get_balance))
        .route("/accounts/{address}/nonce", get(get_nonce))
        .route("/mempool", get(get_mempool))
        .route("/validators", get(get_validators))
        // ── Short-form aliases (CoinBlast / Faucet) ──────────────
        .route("/blocks", get(get_blocks))
        .route("/blocks/{height}", get(get_block))
        .route("/wallets/{address}", get(get_wallet_info))
        // GET /transactions stays here; POST /transactions is on write_router.
        .route("/transactions", get(list_transactions))
        .route("/transactions/{txid}", get(get_transaction))
        // ── Token endpoints ──────────────────────────────────────
        .route("/tokens", get(list_tokens))
        .route("/tokens/{contract}", get(get_token_info))
        .route("/tokens/{contract}/balance/{addr}", get(get_token_balance))
        .route("/tokens/{contract}/holders", get(get_token_holders_list))
        .route("/tokens/{contract}/trades", get(get_token_trades_list))
        // ── Rich list ────────────────────────────────────────────
        .route("/richlist", get(get_richlist))
        // ── Address history ──────────────────────────────────────
        .route("/address/{address}/history", get(get_address_history))
        .route("/address/{address}/info", get(get_address_info))
        // ── State trie ───────────────────────────────────────────
        .route("/address/{address}/proof", get(get_address_proof))
        .route("/chain/state-root/{height}", get(get_state_root))
        // ── Staking (Voyager DPoS) ───────────────────────────────
        .route("/staking/validators", get(staking_validators))
        .route("/staking/delegations/{address}", get(staking_delegations))
        .route("/staking/unbonding/{address}", get(staking_unbonding))
        .route("/epoch/current", get(epoch_current))
        .route("/epoch/history", get(epoch_history))
        // ── Explorer/Wallet new REST surface (explorer_api.rs) ───
        // Matches the `/accounts/...` paths expected by the
        // sentrix-scan and sentrix-wallet-web frontends. The older
        // `/address/...` routes above are kept for back-compat.
        .route(
            "/accounts/{address}/history",
            get(crate::explorer_api::accounts_history),
        )
        .route("/accounts/top", get(crate::explorer_api::accounts_top))
        .route(
            "/accounts/{address}/tokens",
            get(crate::explorer_api::accounts_tokens),
        )
        .route(
            "/accounts/{address}/code",
            get(crate::explorer_api::accounts_code),
        )
        .route(
            "/tokens/{contract}/transfers",
            get(crate::explorer_api::tokens_transfers),
        )
        // Replace holders list response shape with the frontend-expected
        // `{ holders, total }` layout computing `percentage` per holder.
        // The older `/tokens/{contract}/holders` → get_token_holders_list
        // is NOT rewired here because axum's first-match wins on route
        // registration order; we keep back-compat by adding a v2 route
        // under the same path in explorer_api — older callers hitting
        // the original route keep their payload.
        .route(
            "/tokens/{contract}/holders-v2",
            get(crate::explorer_api::tokens_holders),
        )
        .route(
            "/chain/performance",
            get(crate::explorer_api::chain_performance),
        )
        .route(
            "/validators/{address}/delegators",
            get(crate::explorer_api::validator_delegators),
        )
        .route(
            "/validators/{address}/rewards",
            get(crate::explorer_api::validator_rewards),
        )
        .route(
            "/validators/{address}/blocks-over-time",
            get(crate::explorer_api::validator_blocks_over_time),
        )
        // ── Admin ────────────────────────────────────────────────
        .route("/admin/log", get(get_admin_log))
        // ── Stats ────────────────────────────────────────────────
        .route("/stats/daily", get(explorer::stats_daily))
        // ── Explorer ─────────────────────────────────────────────
        .nest("/explorer", explorer_router(state.clone()))
        // ── Write endpoints (stricter rate limit) ────────────────
        .merge(write_router)
        // Axum layer order: LAST `.layer()` call is OUTERMOST
        // (sees request first, response last). We want:
        //   cors           (outermost — 429/4xx/5xx responses MUST carry
        //                   access-control-allow-origin or the browser
        //                   reports CORS blocked instead of rate limit)
        //   DefaultBodyLimit
        //   Extension(global_limiter)
        //   ip_rate_limit_middleware  (rejects 429 before entering the body)
        //   ConcurrencyLimitLayer
        //   cache_control_middleware  (inner — sets Cache-Control on 2xx
        //                              GET responses from the handlers)
        //   handler
        .layer(axum::middleware::from_fn(cache_control_middleware))
        .layer(ConcurrencyLimitLayer::new(500))
        .layer(axum::middleware::from_fn(ip_rate_limit_middleware))
        .layer(axum::Extension(global_limiter))
        // Reject request bodies larger than 1 MiB — prevents memory exhaustion from unbounded payloads.
        // Single transactions and JSON-RPC batches are well under this limit; legitimate clients
        // are never affected. Without this, an attacker can stream arbitrary bytes until the node OOMs.
        .layer(DefaultBodyLimit::max(1_048_576))
        .layer(cors)
        .with_state(state)
}

fn explorer_router(_state: SharedState) -> Router<SharedState> {
    Router::new()
        .route("/", get(explorer::explorer_home))
        .route("/blocks", get(explorer::explorer_blocks))
        .route("/transactions", get(explorer::explorer_transactions))
        .route("/validators", get(explorer::explorer_validators))
        .route("/tokens", get(explorer::explorer_tokens))
        .route("/richlist", get(explorer::explorer_richlist))
        .route("/mempool", get(explorer::explorer_mempool))
        .route("/validator/{address}", get(explorer::explorer_validator))
        .route("/token/{contract}", get(explorer::explorer_token))
        .route("/block/{index}", get(explorer::explorer_block))
        .route("/address/{address}", get(explorer::explorer_address))
        .route("/tx/{txid}", get(explorer::explorer_tx))
}

// ── Handlers ─────────────────────────────────────────────

// ── Token handlers ───────────────────────────────────────

// ── Short-form alias handlers ────────────────────────────

// Helper for API error responses
pub(super) fn api_err(msg: &str) -> (StatusCode, Json<serde_json::Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({"success": false, "error": msg})),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_h05_constant_time_eq() {
        // Equal strings
        assert!(constant_time_eq("abc123", "abc123"));
        assert!(constant_time_eq("", ""));
        assert!(constant_time_eq(
            "sentrix-api-key-xyz",
            "sentrix-api-key-xyz"
        ));

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
        assert!(!constant_time_eq(
            "key_32_chars_long_abcdefghijklmn",
            "key_32_chars_long_abcdefghijklm"
        ));
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

    // ── L-05: serde_json error propagation tests ──────────

    #[test]
    fn test_l05_block_serializes_to_json_value() {
        // Verify Block can be serialized without panic — confirming map_err path is safe.
        use sentrix_core::blockchain::Blockchain;
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
        use sentrix_core::blockchain::Blockchain;
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
        use sentrix_core::authority::AdminEvent;
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
        use sentrix_core::blockchain::Blockchain;
        let bc = Blockchain::new("admin".to_string());
        // Fresh blockchain has an empty admin log
        assert_eq!(bc.authority.admin_log.len(), 0);
        // The log serializes correctly
        let log_json = serde_json::to_value(&bc.authority.admin_log).unwrap();
        assert!(log_json.is_array());
    }
}
