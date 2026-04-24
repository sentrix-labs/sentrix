// ops.rs — operator + discovery endpoints. Five handlers:
// `/`, `/health`, `/sentrix_status`, `/metrics`, `/admin/log`.
//
// Extracted from `routes/mod.rs` as part of backlog #12 phase 2c. Shared
// `START_TIME` lives here — it's consumed by both `sentrix_status` and
// `metrics` to report process uptime, and eagerly pinned by
// `create_router` so the first /sentrix_status call after boot sees a
// non-zero value.

use axum::{Json, extract::State, response::IntoResponse};

use super::{ApiKey, SharedState};

pub(super) static START_TIME: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// BACKLOG #16 counter: incremented by main.rs whenever a P2P-received
/// block fails to persist to MDBX. Exposed as `sentrix_peer_block_save_fails_total`
/// on the /metrics endpoint so Prometheus can alert on `rate(... > 0)`.
/// Gap-creating events are otherwise silent (block advances in memory,
/// disk persistence fails, CHAIN_WINDOW_SIZE rolls → permanent gap).
pub static PEER_BLOCK_SAVE_FAILS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub(super) async fn root() -> Json<serde_json::Value> {
    let chain_id = sentrix_core::blockchain::get_chain_id();
    let consensus = if chain_id == 7119 { "PoA" } else { "BFT" };
    Json(serde_json::json!({
        "name": "Sentrix",
        "version": env!("CARGO_PKG_VERSION"),
        "chain_id": chain_id,
        "consensus": consensus,
        "native_token": "SRX",
        "docs": {
            "rpc_jsonrpc": "POST /rpc",
            "rest": {
                "chain_info": "/chain/info",
                "blocks": "/chain/blocks",
                "transactions": "/transactions",
                "accounts": "/accounts/{address}",
                "tokens": "/tokens",
                "validators": "/validators",
                "staking": "/staking",
                "epoch": "/epoch/current",
                "mempool": "/mempool"
            },
            "ops": {
                "health": "/health",
                "status": "/sentrix_status",
                "metrics": "/metrics",
                "explorer_builtin": "/explorer"
            }
        },
        "jsonrpc_namespaces": {
            "eth_": "Ethereum-compatible (MetaMask, ethers.js, Hardhat)",
            "net_": "Network info",
            "web3_": "Client version",
            "sentrix_": "Native Sentrix (validators, BFT, staking, delegations, finality)"
        }
    }))
}

pub(super) async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "ok", "node": "sentrix-chain" }))
}

/// Structured node status (NEAR-style).
///
/// Distinct from `/` (which advertises the API surface) and `/chain/info`
/// (which describes the chain itself): this is the operator-facing
/// "is my node healthy and on the right fork" snapshot.
///
/// Returns version/build, consensus mode, sync info (head block,
/// earliest-retained block, syncing flag), active validator count, and
/// process uptime in seconds.
pub async fn sentrix_status(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let uptime = START_TIME
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs();
    let bc = state.read().await;
    let chain_id = bc.chain_id;
    let consensus = if chain_id == 7119 { "PoA" } else { "BFT" };
    let latest = bc.latest_block().ok().cloned();
    let (latest_height, latest_hash, latest_timestamp) = latest
        .as_ref()
        .map(|b| (b.index, b.hash.clone(), b.timestamp))
        .unwrap_or((0, String::new(), 0));
    // Window start = earliest block we can answer from RAM. Useful for
    // clients deciding whether to use this node as a history source.
    let earliest_height = bc.chain.first().map(|b| b.index).unwrap_or(0);
    // PoA reads from the authority set; Voyager/BFT reads from the DPoS
    // stake registry. Picking the wrong source returns 0 (the other set
    // is empty on that chain).
    let active_validators = if consensus == "PoA" {
        bc.authority.active_count()
    } else {
        bc.stake_registry.active_count()
    };
    // "Syncing" here means we are behind any known peer. Without a peer
    // view here, we approximate `syncing = false` (operators watching this
    // should cross-check with /chain/info window_is_partial).
    let syncing = false;

    Json(serde_json::json!({
        "version": {
            "version": env!("CARGO_PKG_VERSION"),
            "build": option_env!("SENTRIX_BUILD_SHA").unwrap_or("unknown"),
        },
        "chain_id": chain_id,
        "consensus": consensus,
        "native_token": "SRX",
        "sync_info": {
            "latest_block_height": latest_height,
            "latest_block_hash": latest_hash,
            "latest_block_time": latest_timestamp,
            "earliest_block_height": earliest_height,
            "syncing": syncing,
        },
        "validators": {
            "active_count": active_validators,
        },
        "uptime_seconds": uptime,
    }))
}

/// Prometheus-format metrics endpoint. Returns plain text `text/plain;
/// version=0.0.4` so Prometheus, Grafana Agent, and Datadog can scrape
/// directly.
///
/// No authentication — these are public chain metrics that any dashboard
/// or monitoring system can consume.
pub(super) async fn metrics(State(state): State<SharedState>) -> axum::response::Response {
    let uptime = START_TIME
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs();
    let bc = state.read().await;
    let height = bc.height();
    let validators = bc.authority.active_count();
    let mempool = bc.mempool_size();
    let chain_id = bc.chain_id;
    let deployed_tokens = bc.list_tokens().len();
    let total_minted_sentri = bc.total_minted;
    let total_burned_sentri = bc.accounts.total_burned;
    // Circulating = minted − burned. Cheap to compute here so Prometheus/Grafana
    // can chart it directly without users learning the burn semantics.
    let circulating_sentri = total_minted_sentri.saturating_sub(total_burned_sentri);

    // Compute avg block time from last 10 blocks in the window.
    let mut block_times: Vec<u64> = Vec::new();
    let chain = &bc.chain;
    if chain.len() >= 2 {
        let tail = if chain.len() > 11 {
            &chain[chain.len() - 11..]
        } else {
            chain.as_slice()
        };
        for w in tail.windows(2) {
            let dt = w[1].timestamp.saturating_sub(w[0].timestamp);
            if dt > 0 && dt < 60 {
                block_times.push(dt);
            }
        }
    }
    let avg_block_time = if block_times.is_empty() {
        3.0
    } else {
        block_times.iter().sum::<u64>() as f64 / block_times.len() as f64
    };

    // Avg tx per block (last 10).
    let tx_per_block: f64 = if chain.len() >= 2 {
        let tail = if chain.len() > 10 {
            &chain[chain.len() - 10..]
        } else {
            chain.as_slice()
        };
        tail.iter().map(|b| b.tx_count() as f64).sum::<f64>() / tail.len() as f64
    } else {
        0.0
    };

    let body = format!(
        "# HELP sentrix_block_height Current chain height.\n\
         # TYPE sentrix_block_height gauge\n\
         sentrix_block_height{{chain_id=\"{chain_id}\"}} {height}\n\
         # HELP sentrix_active_validators Number of active PoA/DPoS validators.\n\
         # TYPE sentrix_active_validators gauge\n\
         sentrix_active_validators {validators}\n\
         # HELP sentrix_tx_pool_size Number of pending transactions in mempool.\n\
         # TYPE sentrix_tx_pool_size gauge\n\
         sentrix_tx_pool_size {mempool}\n\
         # HELP sentrix_tx_per_block Average transactions per block (last 10 blocks).\n\
         # TYPE sentrix_tx_per_block gauge\n\
         sentrix_tx_per_block {tx_per_block:.2}\n\
         # HELP sentrix_block_time_seconds Average block time in seconds (last 10 blocks).\n\
         # TYPE sentrix_block_time_seconds gauge\n\
         sentrix_block_time_seconds {avg_block_time:.2}\n\
         # HELP sentrix_deployed_tokens Number of deployed SRC-20/SRC-20 token contracts.\n\
         # TYPE sentrix_deployed_tokens gauge\n\
         sentrix_deployed_tokens {deployed_tokens}\n\
         # HELP sentrix_uptime_seconds Seconds since node process started.\n\
         # TYPE sentrix_uptime_seconds counter\n\
         sentrix_uptime_seconds {uptime}\n\
         # HELP sentrix_chain_id Chain identifier.\n\
         # TYPE sentrix_chain_id gauge\n\
         sentrix_chain_id {chain_id}\n\
         # HELP sentrix_total_minted_sentri Total SRX ever minted by the chain (coinbase rewards + genesis premine). 1 SRX = 100_000_000 sentri.\n\
         # TYPE sentrix_total_minted_sentri counter\n\
         sentrix_total_minted_sentri {total_minted_sentri}\n\
         # HELP sentrix_total_burned_sentri Total SRX burned (50% of each tx fee + explicit burns). Monotonically increasing counter.\n\
         # TYPE sentrix_total_burned_sentri counter\n\
         sentrix_total_burned_sentri {total_burned_sentri}\n\
         # HELP sentrix_circulating_supply_sentri Currently circulating SRX = total_minted − total_burned.\n\
         # TYPE sentrix_circulating_supply_sentri gauge\n\
         sentrix_circulating_supply_sentri {circulating_sentri}\n\
         # HELP sentrix_peer_block_save_fails_total Count of P2P-received blocks whose MDBX save failed (BACKLOG #16). Rate>0 means chain history is developing TABLE_META gaps — investigate MDBX disk / lock / permissions immediately.\n\
         # TYPE sentrix_peer_block_save_fails_total counter\n\
         sentrix_peer_block_save_fails_total {peer_save_fails}\n",
        peer_save_fails = PEER_BLOCK_SAVE_FAILS.load(std::sync::atomic::Ordering::Relaxed)
    );

    axum::response::Response::builder()
        .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
        .body(axum::body::Body::from(body))
        .unwrap_or_default()
        .into_response()
}

/// Admin audit log — requires `X-API-Key` authentication.
pub(super) async fn get_admin_log(
    _auth: ApiKey,
    State(state): State<SharedState>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    Json(serde_json::json!({
        "log": bc.authority.admin_log,
        "count": bc.authority.admin_log.len(),
    }))
}
