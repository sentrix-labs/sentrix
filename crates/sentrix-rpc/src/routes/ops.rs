// ops.rs — operator + discovery endpoints. Five handlers:
// `/`, `/health`, `/sentrix_status`, `/metrics`, `/admin/log`.
//
// Extracted from `routes/mod.rs` as part of backlog #12 phase 2c. Shared
// `START_TIME` lives here — it's consumed by both `sentrix_status` and
// `metrics` to report process uptime, and eagerly pinned by
// `create_router` so the first /sentrix_status call after boot sees a
// non-zero value.

use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};

use super::{ApiKey, SharedState};

pub(super) static START_TIME: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

/// BACKLOG #16 counter: incremented by main.rs whenever a P2P-received
/// block fails to persist to MDBX. Exposed as `sentrix_peer_block_save_fails_total`
/// on the /metrics endpoint so Prometheus can alert on `rate(... > 0)`.
/// Gap-creating events are otherwise silent (block advances in memory,
/// disk persistence fails, CHAIN_WINDOW_SIZE rolls → permanent gap).
pub static PEER_BLOCK_SAVE_FAILS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Set by main.rs when a `NodeEvent::SyncForkDetected` is received —
/// meaning a block import failed because our local chain head has a
/// different hash than the canonical network expects. Cleared when a
/// new block is successfully applied (sync recovered).
///
/// While true, `/health` returns HTTP 503 so the Docker healthcheck
/// fails and operators are alerted. Automatic recovery is not possible
/// without a storage rollback API; the operator must copy a canonical
/// `chain.db` from a healthy validator.
pub static FORK_DETECTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Block height at which the fork was first detected.
pub static FORK_DETECTED_HEIGHT: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Unix timestamp (seconds) when fork was first detected.
pub static FORK_DETECTED_AT_UNIX: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Our local head hash at the time fork was detected, stored as a
/// newline-terminated string in an atomic-compatible slot via a Mutex.
pub static FORK_LOCAL_HEAD: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

pub(super) async fn root(State(state): State<SharedState>) -> Json<serde_json::Value> {
    // Read the runtime consensus state from chain.db's persistent
    // voyager_activated flag rather than the chain_id heuristic. The
    // old `chain_id == 7119 ? PoA : BFT` mapping was wrong post-2026-04-25
    // when mainnet (7119) activated Voyager. RPC consumers (block
    // explorers, wallets) need accurate consensus mode.
    let bc = state.read().await;
    let chain_id = bc.chain_id;
    // Read the loaded genesis name so testnet binaries return
    // "Sentrix Testnet" instead of pretending to be mainnet. Pre-fix
    // the literal "Sentrix" string here was the same on every network
    // — wallets that probe `/` for self-describe couldn't tell which
    // rail they were on without checking chain_id.
    let chain_name = bc.chain_name.clone();
    let consensus = if bc.voyager_activated {
        "DPoS+BFT"
    } else {
        "PoA"
    };
    drop(bc);
    Json(serde_json::json!({
        "name": chain_name,
        "version": env!("CARGO_PKG_VERSION"),
        "chain_id": chain_id,
        "consensus": consensus,
        "native_token": "SRX",
        "docs": {
            "rpc_jsonrpc": "POST /rpc",
            "rest": {
                "chain_info": "/chain/info",
                "blocks": "/chain/blocks",
                "block": "/chain/blocks/{height}",
                "transactions": "/transactions",
                "transaction": "/transactions/{txid}",
                "address": "/address/{address}",
                "address_history": "/address/{address}/history",
                "accounts": "/accounts/{address}",
                "account_balance": "/accounts/{address}/balance",
                "account_nonce": "/accounts/{address}/nonce",
                "account_code": "/accounts/{address}/code",
                "tokens": "/tokens",
                "token_info": "/tokens/{contract}",
                "validators": "/validators",
                "staking": "/staking/validators",
                "epoch": "/epoch/current",
                "mempool": "/mempool"
            },
            "ops": {
                "health": "/health",
                "status": "/sentrix_status",
                "status_extended": "/sentrix_status_extended",
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

/// Node health check. Returns HTTP 200 when healthy, HTTP 503 when the
/// node is forked or stale. The Docker healthcheck hits this endpoint;
/// a 503 causes the container to be marked unhealthy, surfacing the
/// condition to operators via `docker ps` and alerting pipelines.
///
/// Unhealthy conditions:
/// - `FORK_DETECTED`: local chain head doesn't match canonical network.
///   Requires operator intervention (copy canonical chain.db).
/// - Chain stale: latest block timestamp > `STALE_THRESHOLD_SECS` ago.
///   Indicates the node has stopped receiving/finalising blocks.
pub(super) async fn health(State(state): State<SharedState>) -> impl IntoResponse {
    use std::sync::atomic::Ordering;

    const STALE_THRESHOLD_SECS: u64 = 120;

    // Acquire loads: pairs with the Release swap in main.rs so that when we
    // observe FORK_DETECTED=true we are guaranteed to see the up-to-date
    // HEIGHT/AT_UNIX values written before the swap.
    let fork = FORK_DETECTED.load(Ordering::Acquire);
    let fork_height = FORK_DETECTED_HEIGHT.load(Ordering::Acquire);
    let fork_at_unix = FORK_DETECTED_AT_UNIX.load(Ordering::Acquire);
    // Mutex::lock() provides its own Acquire fence for the string content.
    // Clone inside the closure so the MutexGuard is dropped before any await.
    // On poisoning: recover the inner value so diagnostic info is not lost.
    let fork_local_head = FORK_LOCAL_HEAD
        .lock()
        .map(|g| g.clone())
        .unwrap_or_else(|e| {
            tracing::warn!("FORK_LOCAL_HEAD mutex poisoned — recovering inner value");
            e.into_inner().clone()
        });

    let bc = state.read().await;
    let (height, head_hash, last_block_ts) = bc
        .latest_block()
        .ok()
        .map(|b| (b.index, b.hash.clone(), b.timestamp))
        .unwrap_or((0, String::new(), 0));
    drop(bc);

    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Only flag stale if the node has been running longer than the threshold.
    // A fresh start (or a node catching up from a cold chain.db) shouldn't
    // appear stale during the initial boot window — the old chain.db timestamp
    // predates the current run.
    let uptime_secs = START_TIME.get().map(|t| t.elapsed().as_secs()).unwrap_or(0);
    let stale = uptime_secs > STALE_THRESHOLD_SECS
        && last_block_ts > 0
        && now_unix.saturating_sub(last_block_ts) > STALE_THRESHOLD_SECS;

    if fork {
        let body = serde_json::json!({
            "status": "fork_detected",
            "node": "sentrix-chain",
            "height": height,
            "head_hash": head_hash,
            "fork_detected": true,
            "fork_at_height": fork_height,
            "fork_detected_at_unix": fork_at_unix,
            "fork_local_head_at_detection": fork_local_head,
            "stale": stale,
            "recovery": "Copy canonical chain.db from a healthy validator and restart."
        });
        return (StatusCode::SERVICE_UNAVAILABLE, Json(body));
    }

    if stale {
        let body = serde_json::json!({
            "status": "stale",
            "node": "sentrix-chain",
            "height": height,
            "head_hash": head_hash,
            "fork_detected": false,
            "stale": true,
            "last_block_unix": last_block_ts,
            "stale_for_secs": now_unix.saturating_sub(last_block_ts),
        });
        return (StatusCode::SERVICE_UNAVAILABLE, Json(body));
    }

    let body = serde_json::json!({
        "status": "ok",
        "node": "sentrix-chain",
        "height": height,
        "head_hash": head_hash,
        "fork_detected": false,
        "stale": false,
    });
    (StatusCode::OK, Json(body))
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
    // Same fix as root() — runtime flag, not chain_id heuristic.
    let consensus = if bc.voyager_activated {
        "DPoS+BFT"
    } else {
        "PoA"
    };
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

/// `/sentrix_status_extended` — superset of `/sentrix_status` for ops dashboards.
///
/// Adds the fields a phone-screen "is the chain healthy?" check needs:
/// chain age (seconds since latest block timestamp; >5 s = degraded, >60 s = halted),
/// rolling block-time avg over the last 10 blocks, mempool depth, supply
/// snapshot, top-stake validators, plus a single-word `health` field that
/// rolls all of the above into a green/yellow/red verdict an alerting
/// script can `jq -r .health` against.
///
/// All fields read from a single `state.read().await` to avoid producing
/// inconsistent snapshots under load.
pub async fn sentrix_status_extended(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let uptime = START_TIME
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_secs();
    let bc = state.read().await;
    let chain_id = bc.chain_id;
    let consensus = if bc.voyager_activated {
        "DPoS+BFT"
    } else {
        "PoA"
    };
    let latest = bc.latest_block().ok().cloned();
    let (latest_height, latest_hash, latest_timestamp) = latest
        .as_ref()
        .map(|b| (b.index, b.hash.clone(), b.timestamp))
        .unwrap_or((0, String::new(), 0));
    let earliest_height = bc.chain.first().map(|b| b.index).unwrap_or(0);
    let active_validators = if consensus == "PoA" {
        bc.authority.active_count()
    } else {
        bc.stake_registry.active_count()
    };
    let mempool_size = bc.mempool_size();
    let total_minted = bc.total_minted;
    let total_burned = bc.accounts.total_burned;
    let circulating = total_minted.saturating_sub(total_burned);
    let token_count = bc.list_tokens().len();

    // Rolling block-time avg over the last 10 blocks. Same window the
    // /metrics path uses; reproduced here so dashboards don't have to
    // parse Prometheus text.
    let chain = &bc.chain;
    let block_time_avg_recent = if chain.len() >= 2 {
        let tail = if chain.len() > 11 {
            &chain[chain.len() - 11..]
        } else {
            &chain[..]
        };
        let mut sum = 0i64;
        let mut n = 0i64;
        for w in tail.windows(2) {
            sum += w[1].timestamp.saturating_sub(w[0].timestamp) as i64;
            n += 1;
        }
        if n > 0 {
            Some(sum as f64 / n as f64)
        } else {
            None
        }
    } else {
        None
    };

    // Top 4 by stake (matches mainnet N=4 today; Foundation, Treasury val5,
    // Core, Beacon usually). Ops-friendly summary so a glance at the JSON
    // tells you which keys are voting.
    let mut validators_by_stake: Vec<(String, u64)> = bc
        .stake_registry
        .validators
        .iter()
        .map(|(addr, v)| (addr.clone(), v.total_stake()))
        .collect();
    validators_by_stake.sort_by_key(|v| std::cmp::Reverse(v.1));
    let top_validators: Vec<serde_json::Value> = validators_by_stake
        .iter()
        .take(7)
        .map(|(addr, stake)| {
            let active = bc.stake_registry.active_set.contains(addr);
            serde_json::json!({
                "address": addr,
                "stake_sentri": stake,
                "active": active,
            })
        })
        .collect();
    let total_active_stake: u64 = bc
        .stake_registry
        .active_set
        .iter()
        .filter_map(|a| bc.stake_registry.get_validator(a))
        .map(|v| v.total_stake())
        .sum();

    drop(bc);

    // chain_age_seconds — "now - latest block timestamp", clamped to 0
    // for nodes with clock-skew. Anything >5 s under steady 1 s blocks is
    // a sign the chain is degraded; >60 s is the watchdog-fire threshold.
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let chain_age_seconds = (now_secs - latest_timestamp as i64).max(0);

    // Single-word health verdict. Phone-dashboard / Telegram alert
    // scripts pull this with `jq -r .health` and route on green/yellow/red.
    let health = if chain_age_seconds > 60 {
        "red"
    } else if chain_age_seconds > 5 || active_validators < 3 {
        "yellow"
    } else {
        "green"
    };

    Json(serde_json::json!({
        "version": {
            "version": env!("CARGO_PKG_VERSION"),
            "build": option_env!("SENTRIX_BUILD_SHA").unwrap_or("unknown"),
        },
        "chain_id": chain_id,
        "consensus": consensus,
        "native_token": "SRX",
        "health": health,
        "sync_info": {
            "latest_block_height": latest_height,
            "latest_block_hash": latest_hash,
            "latest_block_time": latest_timestamp,
            "earliest_block_height": earliest_height,
            "chain_age_seconds": chain_age_seconds,
            "block_time_avg_recent_seconds": block_time_avg_recent,
        },
        "validators": {
            "active_count": active_validators,
            "total_active_stake_sentri": total_active_stake,
            "top": top_validators,
        },
        "mempool": {
            "size": mempool_size,
        },
        "supply": {
            "minted_sentri": total_minted,
            "burned_sentri": total_burned,
            "circulating_sentri": circulating,
        },
        "ecosystem": {
            "deployed_tokens": token_count,
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

    // v2.2.21 follow-up: append default-registry metrics (bft_*, plus
    // anything else other modules register against the global registry).
    // TextEncoder produces the standard Prometheus exposition format, so
    // appending after the hand-rolled sentrix_* block is wire-compatible.
    let mut registry_buf: Vec<u8> = Vec::new();
    let encoder = prometheus::TextEncoder::new();
    let metric_families = prometheus::default_registry().gather();
    if let Err(e) = prometheus::Encoder::encode(&encoder, &metric_families, &mut registry_buf) {
        tracing::warn!(
            "metrics endpoint: default-registry encode failed: {} \
             — sentrix_* metrics still served, bft_* omitted",
            e
        );
        registry_buf.clear();
    }
    let registry_metrics = String::from_utf8(registry_buf).unwrap_or_default();

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
         sentrix_peer_block_save_fails_total {peer_save_fails}\n\
         {registry_metrics}",
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, http::StatusCode};
    use sentrix_core::blockchain::Blockchain;
    use std::sync::{Arc, atomic::Ordering};
    use tokio::sync::RwLock;

    // Tests that touch process-level atomics must run sequentially to avoid
    // races between parallel test threads. Using tokio::sync::Mutex so the
    // guard can be held across .await points without triggering the
    // `clippy::await_holding_lock` lint.
    static TEST_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

    fn test_lock() -> &'static tokio::sync::Mutex<()> {
        TEST_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
    }

    fn make_state() -> SharedState {
        Arc::new(RwLock::new(Blockchain::new(
            "0x0000000000000000000000000000000000000001".into(),
        )))
    }

    fn reset_fork_state() {
        FORK_DETECTED.store(false, Ordering::SeqCst);
        FORK_DETECTED_HEIGHT.store(0, Ordering::SeqCst);
        FORK_DETECTED_AT_UNIX.store(0, Ordering::SeqCst);
        if let Ok(mut g) = FORK_LOCAL_HEAD.lock() {
            g.clear();
        }
    }

    /// Health returns 200 + `status: ok` when no fork is detected and chain
    /// is fresh. (The genesis block has no blocks so last_block_ts=0 and
    /// the stale guard `last_block_ts > 0` prevents a false stale alarm.)
    #[tokio::test]
    async fn test_health_ok_when_no_fork() {
        let _guard = test_lock().lock().await;
        reset_fork_state();

        let state = make_state();
        let resp = health(State(state)).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["fork_detected"], false);
    }

    /// Health returns 503 + `status: fork_detected` when FORK_DETECTED is set.
    /// This is what the Docker healthcheck sees when the node is on a
    /// divergent branch.
    #[tokio::test]
    async fn test_health_503_when_fork_detected() {
        let _guard = test_lock().lock().await;
        reset_fork_state();

        FORK_DETECTED.store(true, Ordering::SeqCst);
        FORK_DETECTED_HEIGHT.store(6_132_038, Ordering::SeqCst);
        FORK_DETECTED_AT_UNIX.store(1_748_000_000, Ordering::SeqCst);
        if let Ok(mut g) = FORK_LOCAL_HEAD.lock() {
            *g = "deadbeef01234567".to_string();
        }

        let state = make_state();
        let resp = health(State(state)).await.into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "fork_detected");
        assert_eq!(json["fork_detected"], true);
        assert_eq!(json["fork_at_height"], 6_132_038u64);
        assert_eq!(json["fork_local_head_at_detection"], "deadbeef01234567");

        reset_fork_state();
    }

    /// Clearing FORK_DETECTED (as the NewBlock handler does) switches health
    /// back to 200. This simulates a transient fork that resolved after sync.
    #[tokio::test]
    async fn test_health_recovers_when_fork_cleared() {
        let _guard = test_lock().lock().await;
        reset_fork_state();

        // Simulate: fork detected, then NewBlock clears the flag.
        FORK_DETECTED.store(true, Ordering::SeqCst);
        FORK_DETECTED.store(false, Ordering::SeqCst);

        let state = make_state();
        let resp = health(State(state)).await.into_response();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
