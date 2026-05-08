// Operational scaffold — register macros + .unwrap() are startup-only,
// failure means the binary can't initialise so panic is the right behaviour.
#![allow(clippy::unwrap_used)]

//! sentrix-prom-exporter — Prometheus metrics exporter for Sentrix chain.
//!
//! Polls `/sentrix_status` per validator endpoint every N seconds, exposes
//! per-validator labelled gauges that the chain binary itself doesn't yet
//! emit. Designed to fill the Grafana dashboard gap until chain code grows
//! native `/metrics` endpoint.
//!
//! Metrics:
//!   sentrix_chain_height{validator,network}             — per-validator tip height
//!   sentrix_block_age_seconds{validator,network}        — now() - latest_block_time
//!   sentrix_chain_tip_hash_short{validator,network}     — first 4 hex of latest_block_hash as u32 (lets PromQL count distinct values for divergence alert)
//!   sentrix_active_validators{network}                  — chain-wide count
//!   sentrix_mempool_size{validator,network}             — mempool entries
//!   sentrix_uptime_seconds{validator,network}
//!   sentrix_probe_failed_total{validator,network}       — counter, +1 per scrape miss
//!
//! The tip-hash-as-u32 metric is the key one. Cluster-healthy = all
//! validators report the same value at the same time. Divergence alert
//! is `count(count by (sentrix_chain_tip_hash_short) (sentrix_chain_tip_hash_short)) > 1`.

use prometheus::{
    register_gauge_vec, register_int_counter_vec, register_int_gauge_vec, Encoder,
    GaugeVec, IntCounterVec, IntGaugeVec, TextEncoder,
};
use serde::Deserialize;
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{info, warn};

static HEIGHT: OnceLock<IntGaugeVec> = OnceLock::new();
static BLOCK_AGE: OnceLock<GaugeVec> = OnceLock::new();
static TIP_HASH: OnceLock<IntGaugeVec> = OnceLock::new();
static ACTIVE_VALIDATORS: OnceLock<IntGaugeVec> = OnceLock::new();
static MEMPOOL: OnceLock<IntGaugeVec> = OnceLock::new();
static UPTIME: OnceLock<IntGaugeVec> = OnceLock::new();
static PROBE_FAILED: OnceLock<IntCounterVec> = OnceLock::new();

#[derive(Deserialize)]
struct SentrixStatus {
    sync_info: SyncInfo,
    validators: Option<Validators>,
    mempool: Option<Mempool>,
    uptime_seconds: Option<u64>,
}

#[derive(Deserialize)]
struct SyncInfo {
    latest_block_height: u64,
    latest_block_time: u64,
    latest_block_hash: String,
}

#[derive(Deserialize)]
struct Validators {
    active_count: u64,
}

#[derive(Deserialize)]
struct Mempool {
    size: u64,
}

#[derive(Clone, Debug)]
struct Target {
    name: String,
    network: String,
    url: String,
}

fn init_metrics() {
    let height = register_int_gauge_vec!(
        "sentrix_chain_height",
        "Latest block height per validator",
        &["validator", "network"]
    )
    .unwrap();
    let block_age = register_gauge_vec!(
        "sentrix_block_age_seconds",
        "Age of latest finalized block",
        &["validator", "network"]
    )
    .unwrap();
    let tip_hash = register_int_gauge_vec!(
        "sentrix_chain_tip_hash_short",
        "First 8 hex of latest_block_hash decoded to u32 (for cluster-divergence alerts)",
        &["validator", "network"]
    )
    .unwrap();
    let active = register_int_gauge_vec!(
        "sentrix_active_validators",
        "Active validator set size",
        &["network"]
    )
    .unwrap();
    let mempool = register_int_gauge_vec!(
        "sentrix_mempool_size",
        "Mempool entries",
        &["validator", "network"]
    )
    .unwrap();
    let uptime = register_int_gauge_vec!(
        "sentrix_uptime_seconds",
        "Validator process uptime",
        &["validator", "network"]
    )
    .unwrap();
    let failed = register_int_counter_vec!(
        "sentrix_probe_failed_total",
        "Probe failures",
        &["validator", "network"]
    )
    .unwrap();

    HEIGHT.set(height).unwrap();
    BLOCK_AGE.set(block_age).unwrap();
    TIP_HASH.set(tip_hash).unwrap();
    ACTIVE_VALIDATORS.set(active).unwrap();
    MEMPOOL.set(mempool).unwrap();
    UPTIME.set(uptime).unwrap();
    PROBE_FAILED.set(failed).unwrap();
}

async fn poll_one(client: &reqwest::Client, t: &Target) -> Option<()> {
    let resp = client
        .get(format!("{}/sentrix_status", t.url))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .ok()?;
    let status: SentrixStatus = resp.json().await.ok()?;

    HEIGHT.get()?
        .with_label_values(&[&t.name, &t.network])
        .set(status.sync_info.latest_block_height as i64);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as f64;
    BLOCK_AGE.get()?
        .with_label_values(&[&t.name, &t.network])
        .set(now - status.sync_info.latest_block_time as f64);

    // Tip hash as u32 — first 8 hex chars after 0x prefix
    let h = status.sync_info.latest_block_hash.trim_start_matches("0x");
    if h.len() >= 8 {
        let v = u32::from_str_radix(&h[..8], 16).unwrap_or(0);
        TIP_HASH.get()?
            .with_label_values(&[&t.name, &t.network])
            .set(v as i64);
    }

    if let Some(v) = status.validators {
        ACTIVE_VALIDATORS.get()?
            .with_label_values(&[&t.network])
            .set(v.active_count as i64);
    }
    if let Some(m) = status.mempool {
        MEMPOOL.get()?
            .with_label_values(&[&t.name, &t.network])
            .set(m.size as i64);
    }
    if let Some(u) = status.uptime_seconds {
        UPTIME.get()?
            .with_label_values(&[&t.name, &t.network])
            .set(u as i64);
    }

    Some(())
}

async fn poll_loop(targets: Vec<Target>, interval: Duration) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let mut tick = tokio::time::interval(interval);
    loop {
        tick.tick().await;
        for t in &targets {
            if poll_one(&client, t).await.is_none() {
                PROBE_FAILED.get().unwrap()
                    .with_label_values(&[&t.name, &t.network])
                    .inc();
                warn!(target = %t.name, network = %t.network, "probe failed");
            }
        }
    }
}

async fn metrics_handler(
    _req: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<http_body_util::Full<hyper::body::Bytes>>, hyper::Error> {
    let metric_families = prometheus::gather();
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    encoder.encode(&metric_families, &mut buffer).unwrap();
    Ok(hyper::Response::builder()
        .header("Content-Type", encoder.format_type())
        .body(http_body_util::Full::new(buffer.into()))
        .unwrap())
}

fn parse_targets() -> Vec<Target> {
    // Targets via env or hardcoded defaults for current Pattern B topology.
    // Format: SENTRIX_TARGETS="name:network:url,name:network:url,..."
    if let Ok(s) = std::env::var("SENTRIX_TARGETS") {
        return s
            .split(',')
            .filter_map(|item| {
                let parts: Vec<&str> = item.splitn(3, ':').collect();
                if parts.len() == 3 {
                    Some(Target {
                        name: parts[0].into(),
                        network: parts[1].into(),
                        url: parts[2].into(),
                    })
                } else {
                    None
                }
            })
            .collect();
    }
    // Defaults: 4 mainnet validators + 4 testnet validators
    vec![
        Target { name: "core".into(),       network: "mainnet".into(), url: "http://10.20.0.4:8545".into() },
        Target { name: "foundation".into(), network: "mainnet".into(), url: "http://10.20.0.6:8545".into() },
        Target { name: "treasury".into(),   network: "mainnet".into(), url: "http://10.20.0.6:8549".into() },
        Target { name: "beacon".into(),     network: "mainnet".into(), url: "http://10.20.0.6:8553".into() },
        Target { name: "fullnode-1".into(), network: "mainnet".into(), url: "http://10.20.0.6:8557".into() },
        Target { name: "fullnode-2".into(), network: "mainnet".into(), url: "http://10.20.0.6:8561".into() },
        Target { name: "val1".into(),       network: "testnet".into(), url: "http://127.0.0.1:9545".into() },
        Target { name: "val2".into(),       network: "testnet".into(), url: "http://127.0.0.1:9546".into() },
        Target { name: "val3".into(),       network: "testnet".into(), url: "http://127.0.0.1:9547".into() },
        Target { name: "val4".into(),       network: "testnet".into(), url: "http://127.0.0.1:9548".into() },
    ]
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    init_metrics();

    let targets = parse_targets();
    info!(count = targets.len(), "starting prom-exporter");

    let interval = std::env::var("SENTRIX_PROBE_INTERVAL_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(15u64);

    tokio::spawn(poll_loop(targets, Duration::from_secs(interval)));

    let bind = std::env::var("SENTRIX_EXPORTER_BIND")
        .unwrap_or_else(|_| "0.0.0.0:9101".into());
    let addr: std::net::SocketAddr = bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(addr = %addr, "metrics endpoint listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let io = hyper_util::rt::TokioIo::new(stream);
        tokio::spawn(async move {
            if let Err(err) = hyper::server::conn::http1::Builder::new()
                .serve_connection(io, hyper::service::service_fn(metrics_handler))
                .await
            {
                warn!(?err, "conn error");
            }
        });
    }
}
