// ratelimit.rs — per-IP rate limiting (global + write-endpoint
// tightened) and its middleware. Pulled out of the monolithic
// `routes.rs` during the backlog #12 refactor.
//
// Two limiters are layered on each request:
// * `GlobalIpLimiter` — cap every endpoint at `SENTRIX_GLOBAL_RATE_LIMIT`
//   (default 300 / min / IP). Raised 2026-04-21 from 60 → 300 so the
//   block-explorer frontend (scan.sentrixchain.com) can poll the
//   ~8 live stats endpoints it uses without tripping the limit on a
//   shared office / NAT IP. Reads are cheap; 300/min is still well
//   below any realistic DoS threshold on this hardware.
// * `WriteIpLimiter` — tighter cap on state-mutating endpoints
//   (`POST /transactions`, `/tokens/*`, `/rpc`) at
//   `SENTRIX_WRITE_RATE_LIMIT` (default 10 / min / IP). Applied ON TOP
//   of the global limit so POSTs are effectively min(10, 300) = 10/min
//   per IP. An attacker hitting POST endpoints burns the write quota
//   first while read traffic from the same IP keeps flowing.

use axum::{Json, http::StatusCode, response::IntoResponse};
use reliakit_primitives::PositiveInt;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

pub type IpRateLimiter = Arc<Mutex<HashMap<String, (u32, Instant)>>>;
const RATE_LIMIT_WINDOW_SECS: u64 = 60;

/// Pure parse/validate of a rate-limit override string into a positive u32.
///
/// Split out from the env read so it can be unit-tested without touching
/// the process environment — `std::env::set_var` is `unsafe` under edition
/// 2024 and is not safe to call from parallel tests (the race is on the
/// global env table, not key names, so unique keys don't help).
///
/// `PositiveInt` rejects a value of `0` — without this guard, setting
/// `SENTRIX_GLOBAL_RATE_LIMIT=0` (or the write equivalent) would cap the
/// limiter at zero requests, silently locking every client out of the
/// endpoint. A `0`, unparseable, or absent value falls back to `default`.
/// The `min(u32::MAX)` clamp keeps the u64 → u32 cast lossless.
fn rate_limit_from_str(value: Option<&str>, default: u32) -> u32 {
    value
        .and_then(|v| v.parse::<u64>().ok())
        .and_then(|n| PositiveInt::new(n).ok())
        .map(|p| p.get().min(u32::MAX as u64) as u32)
        .unwrap_or(default)
}

/// Read a rate-limit override from `var` and parse it via
/// [`rate_limit_from_str`].
fn rate_limit_from_env(var: &str, default: u32) -> u32 {
    rate_limit_from_str(std::env::var(var).ok().as_deref(), default)
}

/// Override via `SENTRIX_GLOBAL_RATE_LIMIT` env var for benchmarking.
/// Default raised from 60 to 300 on 2026-04-21 — block-explorer
/// frontend polls ~8 stats endpoints per tick, single user on shared
/// IP was hitting 60/min within seconds.
pub(super) fn global_rate_limit_max() -> u32 {
    rate_limit_from_env("SENTRIX_GLOBAL_RATE_LIMIT", 300)
}

/// A7: tighter per-IP cap applied only to write / expensive endpoints
/// (`POST /transactions`, `/tokens/deploy|transfer|burn`, `/rpc`).
/// Defends against single-IP spam of state-mutating requests in addition
/// to the global 60 req/min ceiling. Read endpoints stay at the global
/// limit. Override via `SENTRIX_WRITE_RATE_LIMIT` env var for
/// benchmarking (e.g. 10000).
pub(super) fn write_rate_limit_max() -> u32 {
    rate_limit_from_env("SENTRIX_WRITE_RATE_LIMIT", 10)
}

/// Comma-separated list of IPs that bypass both rate limiters. Set via
/// `SENTRIX_RATE_LIMIT_WHITELIST` env. Used so trusted infrastructure
/// (block-explorer indexers, monitoring exporters, internal mesh peers)
/// can drive the RPC at full speed without lifting the per-IP cap for
/// the public internet. Reads the env var per request so operator
/// changes take effect on next call without restart.
fn rate_limit_whitelist_contains(ip: &str) -> bool {
    let raw = match std::env::var("SENTRIX_RATE_LIMIT_WHITELIST") {
        Ok(v) if !v.is_empty() => v,
        _ => return false,
    };
    raw.split(',').any(|entry| entry.trim() == ip)
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
    // wholesale. On Foundation node/2/3 the RPC listener binds a local port and
    // the Caddy LB (build host) is the only upstream — operators who want
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
    if rate_limit_whitelist_contains(&ip) {
        return next.run(request).await;
    }
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
    if rate_limit_whitelist_contains(&ip) {
        return next.run(request).await;
    }
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

#[cfg(test)]
mod tests {
    use super::rate_limit_from_str;

    // Tests target the pure `rate_limit_from_str` helper — no env mutation,
    // so they are sound and parallel-safe (no `unsafe std::env::set_var`).

    #[test]
    fn zero_falls_back_to_default() {
        // PositiveInt rejects 0 → default returned, not 0 (which would lock
        // every client out of the endpoint).
        assert_eq!(rate_limit_from_str(Some("0"), 300), 300);
    }

    #[test]
    fn unparseable_falls_back_to_default() {
        assert_eq!(rate_limit_from_str(Some("not_a_number"), 10), 10);
    }

    #[test]
    fn valid_positive_overrides_default() {
        assert_eq!(rate_limit_from_str(Some("5000"), 10), 5000);
    }

    #[test]
    fn absent_returns_default() {
        assert_eq!(rate_limit_from_str(None, 42), 42);
    }

    #[test]
    fn above_u32_max_clamps_not_truncates() {
        // u64 value beyond u32::MAX clamps to u32::MAX rather than wrapping.
        assert_eq!(rate_limit_from_str(Some("99999999999"), 10), u32::MAX);
    }
}
