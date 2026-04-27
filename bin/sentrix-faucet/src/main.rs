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
    #[arg(long, env = "SENTRIX_FAUCET_RPC_URL", default_value = "http://127.0.0.1:8545")]
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
    #[arg(long, env = "SENTRIX_FAUCET_ADDR_COOLDOWN_SECS", default_value_t = 86400)]
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
    /// IP → list of drip timestamps within the current window
    ip_history: Arc<DashMap<IpAddr, Vec<Instant>>>,
    /// recipient address → most recent drip timestamp
    addr_last_drip: Arc<DashMap<String, Instant>>,
}

#[derive(Deserialize)]
struct DripRequest {
    /// Recipient address (0x + 40 hex chars). Case-insensitive.
    to: String,
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
    if !without_prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("address must be 0x + 40 hex chars");
    }
    Ok(format!("0x{}", without_prefix))
}

async fn fetch_nonce(http: &reqwest::Client, rpc_url: &str, address: &str) -> Result<u64> {
    let url = format!("{}/accounts/{}/nonce", rpc_url.trim_end_matches('/'), address);
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

/// Rate-limit checks. Returns Ok(()) on pass, Err(reason) on rejection.
fn check_rate_limits(state: &AppState, ip: IpAddr, recipient: &str) -> Result<(), String> {
    let now = Instant::now();
    let window = state.ip_window;

    // Per-IP: prune old entries, then count remaining
    {
        let mut entry = state.ip_history.entry(ip).or_default();
        entry.retain(|t| now.duration_since(*t) < window);
        if entry.len() >= state.ip_max_drips as usize {
            return Err(format!(
                "rate limit: IP {} reached {} drips in {}s window",
                ip,
                state.ip_max_drips,
                window.as_secs()
            ));
        }
        entry.push(now);
    }

    // Per-recipient: cooldown check
    if let Some(last) = state.addr_last_drip.get(recipient)
        && now.duration_since(*last) < state.addr_cooldown
    {
        let remaining = state.addr_cooldown - now.duration_since(*last);
        return Err(format!(
            "address cooldown: try again in {} seconds",
            remaining.as_secs()
        ));
    }
    state.addr_last_drip.insert(recipient.to_string(), now);

    Ok(())
}

async fn handle_drip(
    State(state): State<AppState>,
    ConnectInfo(client): ConnectInfo<SocketAddr>,
    Json(req): Json<DripRequest>,
) -> Result<Json<DripResponse>, (StatusCode, Json<ErrorResponse>)> {
    let recipient = match validate_address(&req.to) {
        Ok(a) => a,
        Err(e) => return Err(err_with(StatusCode::BAD_REQUEST, "bad address", e.to_string())),
    };

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

    let txid = submit_tx(&state.http, &state.rpc_url, &tx).await.map_err(|e| {
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
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    info!(keystore = %cli.keystore, "loading keystore");
    let keystore = Keystore::load(&cli.keystore).context("load keystore")?;
    let wallet: Wallet = keystore.decrypt(&cli.password).context("decrypt keystore")?;

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
        ip_history: Arc::new(DashMap::new()),
        addr_last_drip: Arc::new(DashMap::new()),
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
