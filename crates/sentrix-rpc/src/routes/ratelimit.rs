// ratelimit.rs — per-IP rate limiting (global + write-endpoint
// tightened) and its middleware. Pulled out of the monolithic
// `routes.rs` during the backlog #12 refactor.
//
// Two limiters are layered on each request:
// * `GlobalIpLimiter` — cap every endpoint at `SENTRIX_GLOBAL_RATE_LIMIT`
//   (default 60 / min / IP).
// * `WriteIpLimiter` — tighter cap on state-mutating endpoints
//   (`POST /transactions`, `/tokens/*`, `/rpc`) at
//   `SENTRIX_WRITE_RATE_LIMIT` (default 10 / min / IP). An attacker
//   hitting POST endpoints burns the write quota first while read
//   traffic from the same IP keeps flowing.

use axum::{Json, http::StatusCode, response::IntoResponse};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

pub type IpRateLimiter = Arc<Mutex<HashMap<String, (u32, Instant)>>>;
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

/// Override via `SENTRIX_GLOBAL_RATE_LIMIT` env var for benchmarking.
pub(super) fn global_rate_limit_max() -> u32 {
    std::env::var("SENTRIX_GLOBAL_RATE_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60)
}

/// A7: tighter per-IP cap applied only to write / expensive endpoints
/// (`POST /transactions`, `/tokens/deploy|transfer|burn`, `/rpc`).
/// Defends against single-IP spam of state-mutating requests in addition
/// to the global 60 req/min ceiling. Read endpoints stay at the global
/// limit. Override via `SENTRIX_WRITE_RATE_LIMIT` env var for
/// benchmarking (e.g. 10000).
pub(super) fn write_rate_limit_max() -> u32 {
    std::env::var("SENTRIX_WRITE_RATE_LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10)
}

/// A7: distinct limiter newtypes so write + read counters do not alias
/// each other. Both are registered as separate `Extension<T>` entries on
/// requests.
#[derive(Clone)]
pub struct GlobalIpLimiter(pub IpRateLimiter);

#[derive(Clone)]
pub struct WriteIpLimiter(pub IpRateLimiter);

fn extract_client_ip(request: &axum::http::Request<axum::body::Body>) -> String {
    // P1: trust X-Forwarded-For / X-Real-IP only when
    // `SENTRIX_TRUST_PROXY=1`. Previously these headers were always
    // consulted first, so any client could spoof their source IP by
    // sending a fake X-Forwarded-For and bypass the per-IP rate limit
    // wholesale. On VPS1/2/3 the RPC listener binds a local port and
    // the Caddy LB (VPS4) is the only upstream — operators who want
    // the LB-set IP to be authoritative opt in via the env var; all
    // other deployments fall back to the TCP socket peer address.
    let trust_proxy = matches!(
        std::env::var("SENTRIX_TRUST_PROXY").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    );
    if trust_proxy
        && let Some(ip) = request
            .headers()
            .get("x-forwarded-for")
            .or_else(|| request.headers().get("x-real-ip"))
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    {
        return ip;
    }
    request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| ci.0.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

async fn check_rate_limit(
    limiter: IpRateLimiter,
    ip: String,
    max_requests: u32,
    window_secs: u64,
) -> bool {
    let mut map = limiter.lock().await;
    if map.len() > 10_000 {
        map.retain(|_, (_, ts)| ts.elapsed().as_secs() < window_secs);
    }
    let now = Instant::now();
    let entry = map.entry(ip).or_insert((0, now));
    if entry.1.elapsed().as_secs() >= window_secs {
        *entry = (1, now);
        true
    } else {
        entry.0 += 1;
        entry.0 <= max_requests
    }
}

fn rate_limit_response(max: u32, window: u64) -> axum::response::Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({
            "error": "rate limit exceeded",
            "limit": max,
            "window_secs": window,
        })),
    )
        .into_response()
}

pub(super) async fn ip_rate_limit_middleware(
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let ip = extract_client_ip(&request);
    let allowed = if let Some(limiter) = request.extensions().get::<GlobalIpLimiter>().cloned() {
        check_rate_limit(
            limiter.0,
            ip,
            global_rate_limit_max(),
            RATE_LIMIT_WINDOW_SECS,
        )
        .await
    } else {
        true
    };
    if allowed {
        next.run(request).await
    } else {
        rate_limit_response(global_rate_limit_max(), RATE_LIMIT_WINDOW_SECS)
    }
}

/// A7: stricter write-endpoint rate limit (10 req/min per IP). Layered
/// on top of the global 60/min limit, so an attacker hitting POST
/// endpoints burns the write quota first while read traffic from the
/// same IP keeps flowing. Returns 429 with the same response shape as
/// the global limit.
pub(super) async fn write_rate_limit_middleware(
    request: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let ip = extract_client_ip(&request);
    let allowed = if let Some(limiter) = request.extensions().get::<WriteIpLimiter>().cloned() {
        check_rate_limit(
            limiter.0,
            ip,
            write_rate_limit_max(),
            RATE_LIMIT_WINDOW_SECS,
        )
        .await
    } else {
        true
    };
    if allowed {
        next.run(request).await
    } else {
        rate_limit_response(write_rate_limit_max(), RATE_LIMIT_WINDOW_SECS)
    }
}
