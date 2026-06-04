//! Sentrix testnet faucet — small HTTP service that signs and submits
//! drip transactions from a pre-loaded keystore.
//!
//! Lifecycle:
//!   1. Load keystore at startup, decrypt with `SENTRIX_FAUCET_PASSWORD`
//!      env var. Hold private key in memory only; never log.
//!   2. Bind HTTP listener on `--listen`. Expose `POST /faucet/drip`.
//!   3. On request: rate-limit by source IP and recipient address,
//!      fetch current nonce from RPC, build + sign tx, POST to
//!      `RPC_URL/transactions`, return txid.
//!
//! Hardening (in-process):
//!   - Per-IP rate limit (token bucket via DashMap)
//!   - Per-recipient cooldown (one drip per address per `cooldown_secs`)
//!   - Address regex sanity (no other formats accepted)
//!   - Nonce fetched fresh each request (no caching that could double-spend)
//!
//! Hardening (deployment, NOT in this binary):
//!   - Cloudflare or Caddy CAPTCHA in front (e.g. Turnstile) for bot deterrence
//!   - HTTPS via reverse proxy
//!   - Bind to localhost only; expose via reverse proxy

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Router,
    extract::{ConnectInfo, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
};
use clap::Parser;
use dashmap::DashMap;
use reliakit_primitives::{HexString, NonEmptyStr, PositiveInt};
use reliakit_ratelimit::RateLimiter;
use reliakit_validate::{Valid, Validate, ValidationError};
use secp256k1::{PublicKey, SecretKey};
use sentrix_primitives::transaction::{MIN_TX_FEE, Transaction};
use sentrix_wallet::{Keystore, Wallet};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(version, about = "Sentrix testnet faucet HTTP service")]
struct Cli {
    /// Path to the encrypted keystore JSON file
    #[arg(long, env = "SENTRIX_FAUCET_KEYSTORE")]
    keystore: String,

    /// Keystore password — prefer the env var over the CLI flag (CLI flag
    /// leaves password in shell history)
    #[arg(long, env = "SENTRIX_FAUCET_PASSWORD", hide_env_values = true)]
    password: String,

    /// RPC base URL (POST /transactions, GET /accounts/{addr}/nonce)
    #[arg(
        long,
        env = "SENTRIX_FAUCET_RPC_URL",
        default_value = "http://127.0.0.1:8545"
    )]
    rpc_url: String,

    /// Bind address for the HTTP server
    #[arg(long, env = "SENTRIX_FAUCET_LISTEN", default_value = "127.0.0.1:8546")]
    listen: SocketAddr,

    /// Drip amount in sentri (1 SRX = 100_000_000 sentri). Default 100 SRX.
    #[arg(long, env = "SENTRIX_FAUCET_DRIP_AMOUNT", default_value_t = 100 * 100_000_000)]
    drip_amount: u64,

    /// Chain ID (must match testnet)
    #[arg(long, env = "SENTRIX_FAUCET_CHAIN_ID", default_value_t = 7120)]
    chain_id: u64,

    /// Per-IP rate-limit window (seconds)
    #[arg(long, env = "SENTRIX_FAUCET_IP_WINDOW_SECS", default_value_t = 3600)]
    ip_window_secs: u64,

    /// Max drips per IP per window
    #[arg(long, env = "SENTRIX_FAUCET_IP_MAX_DRIPS", default_value_t = 3)]
    ip_max_drips: u32,

    /// Per-recipient cooldown seconds. Same address can drip again only
    /// after this elapses.
    #[arg(
        long,
        env = "SENTRIX_FAUCET_ADDR_COOLDOWN_SECS",
        default_value_t = 86400
    )]
    addr_cooldown_secs: u64,

    /// Tx fee paid by the faucet (sentri). MIN_TX_FEE by default.
    #[arg(long, env = "SENTRIX_FAUCET_TX_FEE", default_value_t = MIN_TX_FEE)]
    tx_fee: u64,
}

#[derive(Clone)]
struct AppState {
    secret_key: SecretKey,
    public_key: PublicKey,
    address: String,
    rpc_url: String,
    chain_id: u64,
    drip_amount: u64,
    tx_fee: u64,
    ip_window: Duration,
    ip_max_drips: u32,
    addr_cooldown: Duration,
    http: reqwest::Client,
    /// Monotonic base — rate-limit "now" is `start.elapsed()` in ms, the unit
    /// reliakit-ratelimit buckets are clocked in (the crate never reads the
    /// clock itself).
    start: Instant,
    /// IP → token bucket (`ip_max_drips` per `ip_window`). O(1) per IP, vs the
    /// old per-IP `Vec<Instant>` that grew with traffic.
    ip_buckets: Arc<DashMap<IpAddr, RateLimiter>>,
    /// recipient address → 1-token bucket refilling every `addr_cooldown`
    /// (one drip per address per cooldown).
    addr_buckets: Arc<DashMap<String, RateLimiter>>,
}

#[derive(Deserialize)]
struct DripRequest {
    /// Recipient address (0x + 40 hex chars). Case-insensitive.
    to: String,
}

impl Validate for DripRequest {
    type Error = ValidationError;

    fn validate(&self) -> Result<(), Self::Error> {
        let lower = self.to.to_lowercase();
        let without_prefix = lower.strip_prefix("0x").unwrap_or(&lower);
        if without_prefix.len() != 40 {
            return Err(ValidationError::new("address must be 0x + 40 hex chars"));
        }
        HexString::new(without_prefix)
            .map_err(|_| ValidationError::new("address contains invalid hex characters"))?;
        Ok(())
    }
}

#[derive(Serialize)]
struct DripResponse {
    txid: String,
    from: String,
    to: String,
    amount: u64,
    nonce: u64,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
    detail: Option<String>,
}

fn err(status: StatusCode, msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: msg.into(),
            detail: None,
        }),
    )
}

fn err_with(status: StatusCode, msg: &str, detail: String) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: msg.into(),
            detail: Some(detail),
        }),
    )
}

fn validate_address(addr: &str) -> Result<String> {
    let lower = addr.to_lowercase();
    let without_prefix = lower.strip_prefix("0x").unwrap_or(&lower);
    if without_prefix.len() != 40 {
        bail!("address must be 0x + 40 hex chars");
    }
    // HexString validates that all chars are valid hex digits (0-9, a-f, A-F).
    HexString::new(without_prefix).map_err(|_| anyhow!("address must be 0x + 40 hex chars"))?;
    Ok(format!("0x{}", without_prefix))
}

async fn fetch_nonce(http: &reqwest::Client, rpc_url: &str, address: &str) -> Result<u64> {
    let url = format!(
        "{}/accounts/{}/nonce",
        rpc_url.trim_end_matches('/'),
        address
    );
    let resp = http.get(&url).send().await.context("nonce request")?;
    if !resp.status().is_success() {
        bail!("nonce HTTP {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await.context("nonce parse")?;
    body.get("nonce")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("nonce missing in response"))
}

async fn submit_tx(http: &reqwest::Client, rpc_url: &str, tx: &Transaction) -> Result<String> {
    let url = format!("{}/transactions", rpc_url.trim_end_matches('/'));
    let resp = http.post(&url).json(tx).send().await.context("tx submit")?;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.context("submit response parse")?;
    if !status.is_success() {
        bail!("tx submit HTTP {}: {}", status, body);
    }
    body.get("txid")
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| anyhow!("txid missing in response: {}", body))
}

/// Per-IP bucket: `max_drips` capacity, refilling one token every
/// `window_ms / max_drips` — steady rate of `max_drips` per window with a
/// full-burst allowance. RateLimiter::new clamps every arg to ≥1, so a zero
/// interval can't divide.
fn ip_bucket(max_drips: u32, window_ms: u64) -> RateLimiter {
    let cap = max_drips.max(1) as u64;
    RateLimiter::new(cap, 1, window_ms / cap)
}

/// Per-recipient cooldown bucket: a single token refilling every
/// `cooldown_ms` — exactly "one drip per address per cooldown".
fn addr_bucket(cooldown_ms: u64) -> RateLimiter {
    RateLimiter::new(1, 1, cooldown_ms)
}

/// Rate-limit checks. Returns Ok(()) on pass, Err(reason) on rejection.
fn check_rate_limits(state: &AppState, ip: IpAddr, recipient: &str) -> Result<(), String> {
    let now = state.start.elapsed().as_millis() as u64;
    let ip_window_ms = state.ip_window.as_millis() as u64;
    let cooldown_ms = state.addr_cooldown.as_millis() as u64;

    // Per-IP token bucket. DashMap's entry guard holds the per-key shard lock
    // across try_acquire_one, so refill+consume is atomic under concurrent
    // requests from the same IP.
    {
        let mut bucket = state
            .ip_buckets
            .entry(ip)
            .or_insert_with(|| ip_bucket(state.ip_max_drips, ip_window_ms));
        if !bucket.try_acquire_one(now) {
            let retry_s = bucket
                .retry_after(now, 1)
                .unwrap_or(ip_window_ms)
                .div_ceil(1000);
            return Err(format!(
                "rate limit: IP {} reached {} drips per {}s — retry in {}s",
                ip,
                state.ip_max_drips,
                ip_window_ms / 1000,
                retry_s,
            ));
        }
    }

    // Per-recipient cooldown as a 1-token bucket. The same entry-guard
    // atomicity closes the TOCTOU the old get+check+insert pattern had:
    // two parallel requests for one address serialize on the per-key lock,
    // so only one acquires the single token per cooldown.
    {
        let mut bucket = state
            .addr_buckets
            .entry(recipient.to_string())
            .or_insert_with(|| addr_bucket(cooldown_ms));
        if !bucket.try_acquire_one(now) {
            let retry_s = bucket
                .retry_after(now, 1)
                .unwrap_or(cooldown_ms)
                .div_ceil(1000);
            return Err(format!("address cooldown: try again in {} seconds", retry_s));
        }
    }

    Ok(())
}

async fn handle_drip(
    State(state): State<AppState>,
    ConnectInfo(client): ConnectInfo<SocketAddr>,
    Json(req): Json<DripRequest>,
) -> Result<Json<DripResponse>, (StatusCode, Json<ErrorResponse>)> {
    // Valid::new runs DripRequest::validate() — rejects bad addresses before
    // any state mutation or RPC call.
    let req = Valid::new(req)
        .map_err(|e| err_with(StatusCode::BAD_REQUEST, "bad address", e.to_string()))?;
    // validate_address already ran inside Validate; safe to unwrap the Ok.
    let recipient = validate_address(&req.to).expect("already validated");

    if recipient == state.address {
        return Err(err(StatusCode::BAD_REQUEST, "cannot drip to self"));
    }

    if let Err(reason) = check_rate_limits(&state, client.ip(), &recipient) {
        warn!(ip = %client.ip(), recipient = %recipient, reason = %reason, "drip rejected");
        return Err(err_with(
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded",
            reason,
        ));
    }

    let nonce = fetch_nonce(&state.http, &state.rpc_url, &state.address)
        .await
        .map_err(|e| {
            error!(?e, "nonce fetch failed");
            err_with(
                StatusCode::SERVICE_UNAVAILABLE,
                "rpc nonce fetch failed",
                e.to_string(),
            )
        })?;

    let tx = Transaction::new(
        state.address.clone(),
        recipient.clone(),
        state.drip_amount,
        state.tx_fee,
        nonce,
        String::new(),
        state.chain_id,
        &state.secret_key,
        &state.public_key,
    )
    .map_err(|e| {
        error!(?e, "tx build failed");
        err_with(
            StatusCode::INTERNAL_SERVER_ERROR,
            "tx build failed",
            format!("{:?}", e),
        )
    })?;

    let txid = submit_tx(&state.http, &state.rpc_url, &tx)
        .await
        .map_err(|e| {
            error!(?e, "tx submit failed");
            err_with(
                StatusCode::SERVICE_UNAVAILABLE,
                "rpc submit failed",
                e.to_string(),
            )
        })?;

    info!(
        ip = %client.ip(),
        recipient = %recipient,
        amount_sentri = state.drip_amount,
        txid = %txid,
        "drip dispensed"
    );

    Ok(Json(DripResponse {
        txid,
        from: state.address.clone(),
        to: recipient,
        amount: state.drip_amount,
        nonce,
    }))
}

async fn handle_health(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    // Best-effort: report the faucet's current nonce + a hint balance from
    // /accounts/{addr}. If RPC is down, we still return the static info.
    let mut info = serde_json::json!({
        "address": state.address,
        "chain_id": state.chain_id,
        "drip_amount_sentri": state.drip_amount,
        "drip_amount_srx": state.drip_amount as f64 / 100_000_000.0,
        "rpc_url": state.rpc_url,
    });

    if let Ok(nonce) = fetch_nonce(&state.http, &state.rpc_url, &state.address).await {
        info["nonce"] = serde_json::json!(nonce);
    }

    Ok(Json(info))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    // Reject invalid config values early — before any I/O — using reliakit-primitives.
    PositiveInt::new(cli.drip_amount)
        .map_err(|_| anyhow!("SENTRIX_FAUCET_DRIP_AMOUNT must be > 0"))?;
    NonEmptyStr::new(&cli.rpc_url)
        .map_err(|_| anyhow!("SENTRIX_FAUCET_RPC_URL must not be empty"))?;

    info!(keystore = %cli.keystore, "loading keystore");
    let keystore = Keystore::load(&cli.keystore).context("load keystore")?;
    let wallet: Wallet = keystore
        .decrypt(&cli.password)
        .context("decrypt keystore")?;

    let secret_key = wallet.get_secret_key().context("extract secret key")?;
    let public_key = wallet.get_public_key().context("extract public key")?;
    let address = wallet.address.clone();

    info!(
        address = %address,
        chain_id = cli.chain_id,
        drip_srx = cli.drip_amount as f64 / 100_000_000.0,
        listen = %cli.listen,
        rpc = %cli.rpc_url,
        "faucet ready"
    );

    let state = AppState {
        secret_key,
        public_key,
        address,
        rpc_url: cli.rpc_url,
        chain_id: cli.chain_id,
        drip_amount: cli.drip_amount,
        tx_fee: cli.tx_fee,
        ip_window: Duration::from_secs(cli.ip_window_secs),
        ip_max_drips: cli.ip_max_drips,
        addr_cooldown: Duration::from_secs(cli.addr_cooldown_secs),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .context("build reqwest client")?,
        start: Instant::now(),
        ip_buckets: Arc::new(DashMap::new()),
        addr_buckets: Arc::new(DashMap::new()),
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/faucet/drip", post(handle_drip))
        .route("/faucet/health", get(handle_health))
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("bind {}", cli.listen))?;

    info!("serving on {}", cli.listen);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        tokio::signal::ctrl_c().await.ok();
        info!("shutdown signal received");
    })
    .await
    .context("http server")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_bucket_allows_burst_then_blocks() {
        // 5 drips per 5000ms window → cap 5, +1 every 1000ms.
        let mut b = ip_bucket(5, 5000);
        for _ in 0..5 {
            assert!(b.try_acquire_one(0), "burst of capacity must pass");
        }
        assert!(!b.try_acquire_one(0), "6th in the same instant is blocked");
        // retry_after points at the next refill (~1 interval).
        let wait = b.retry_after(0, 1).expect("1 <= capacity");
        assert!(wait > 0 && wait <= 1000, "retry within one interval, got {wait}");
        // After one refill interval, exactly one token is back.
        assert!(b.try_acquire_one(1000));
        assert!(!b.try_acquire_one(1000));
    }

    #[test]
    fn ip_bucket_zero_max_does_not_panic() {
        // RateLimiter::new clamps to ≥1, so a misconfigured 0 still works.
        let mut b = ip_bucket(0, 1000);
        assert!(b.try_acquire_one(0));
    }

    #[test]
    fn addr_bucket_one_per_cooldown() {
        // One drip, then blocked until a full cooldown elapses.
        let mut b = addr_bucket(60_000);
        assert!(b.try_acquire_one(0), "first drip allowed");
        assert!(!b.try_acquire_one(0), "second within cooldown blocked");
        assert!(!b.try_acquire_one(59_999), "still blocked just before cooldown");
        assert!(b.try_acquire_one(60_000), "allowed once cooldown elapses");
    }

    #[test]
    fn addr_bucket_retry_after_reports_remaining() {
        let mut b = addr_bucket(60_000);
        assert!(b.try_acquire_one(0));
        // At t=10s into a 60s cooldown, ~50s remain.
        let wait = b.retry_after(10_000, 1).expect("1 <= capacity");
        assert_eq!(wait, 50_000);
    }
}
