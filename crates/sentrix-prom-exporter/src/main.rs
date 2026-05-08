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

    if let Some(v) = decode_tip_hash_short(&status.sync_info.latest_block_hash) {
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

/// Decode the first 8 hex chars of `latest_block_hash` (after stripping
/// any `0x` prefix) into a u32 short-form fingerprint. Returns None if
/// the string is too short or contains non-hex chars.
///
/// Used as a divergence indicator: when all validators agree on the same
/// block, every host emits the same value; when one diverges, PromQL's
/// `count(count by (sentrix_chain_tip_hash_short))` jumps to ≥ 2.
fn decode_tip_hash_short(s: &str) -> Option<u32> {
    let h = s.trim_start_matches("0x");
    if h.len() < 8 {
        return None;
    }
    u32::from_str_radix(&h[..8], 16).ok()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_tip_hash_short_strips_0x_prefix() {
        let v = decode_tip_hash_short("0xdeadbeefcafebabe").unwrap();
        assert_eq!(v, 0xdeadbeef);
    }

    #[test]
    fn decode_tip_hash_short_without_prefix() {
        let v = decode_tip_hash_short("12345678abcdef").unwrap();
        assert_eq!(v, 0x12345678);
    }

    #[test]
    fn decode_tip_hash_short_too_short_returns_none() {
        assert!(decode_tip_hash_short("0xabc").is_none());
        assert!(decode_tip_hash_short("").is_none());
        assert!(decode_tip_hash_short("0x").is_none());
        // 7 hex chars — under threshold.
        assert!(decode_tip_hash_short("0xabcdefa").is_none());
    }

    #[test]
    fn decode_tip_hash_short_non_hex_returns_none() {
        assert!(decode_tip_hash_short("0xZZZZZZZZ").is_none());
        assert!(decode_tip_hash_short("not-a-hash-at-all").is_none());
    }

    #[test]
    fn decode_tip_hash_short_zero() {
        assert_eq!(decode_tip_hash_short("0x00000000ffff").unwrap(), 0);
    }

    // All four parse_targets scenarios live in one test because std::env is
    // process-global; parallel tests touching SENTRIX_TARGETS would race.
    #[test]
    fn parse_targets_all_scenarios() {
        // SAFETY: single-test scope, no concurrent env access in this body.
        unsafe {
            // ── 1. Default topology (env unset) ──
            std::env::remove_var("SENTRIX_TARGETS");
            let t = parse_targets();
            // 6 mainnet (4 vals + 2 fullnodes) + 4 testnet = 10
            assert_eq!(t.len(), 10);
            let mainnet = t.iter().filter(|x| x.network == "mainnet").count();
            let testnet = t.iter().filter(|x| x.network == "testnet").count();
            assert_eq!(mainnet, 6);
            assert_eq!(testnet, 4);
            assert!(t.iter().all(|x| x.url.starts_with("http://")));

            // ── 2. Env override with 3 well-formed targets ──
            std::env::set_var(
                "SENTRIX_TARGETS",
                "alpha:mainnet:http://10.0.0.1:8545,bravo:testnet:http://10.0.0.2:9545,charlie:mainnet:http://example.com:8545",
            );
            let t = parse_targets();
            assert_eq!(t.len(), 3);
            assert_eq!(t[0].name, "alpha");
            assert_eq!(t[0].network, "mainnet");
            assert_eq!(t[0].url, "http://10.0.0.1:8545");
            assert_eq!(t[1].name, "bravo");
            assert_eq!(t[2].name, "charlie");

            // ── 3. Malformed entries silently skipped ──
            std::env::set_var(
                "SENTRIX_TARGETS",
                "good:mainnet:http://1.1.1.1:8545,bad-no-colons,also:bad,fine:testnet:http://2.2.2.2:9545",
            );
            let t = parse_targets();
            // Only the two 3-part entries survive.
            assert_eq!(t.len(), 2);
            assert_eq!(t[0].name, "good");
            assert_eq!(t[1].name, "fine");

            // ── 4. URL with colons (splitn(3) keeps tail intact) ──
            std::env::set_var("SENTRIX_TARGETS", "x:net:http://host.example:8545/path");
            let t = parse_targets();
            assert_eq!(t.len(), 1);
            assert_eq!(t[0].url, "http://host.example:8545/path");

            std::env::remove_var("SENTRIX_TARGETS");
        }
    }

    #[test]
    fn sentrix_status_deserialises_minimal_shape() {
        let body = r#"{
            "sync_info": {
                "latest_block_height": 1234,
                "latest_block_time": 1715000000,
                "latest_block_hash": "0xabcdef0123456789"
            },
            "validators": null,
            "mempool": null,
            "uptime_seconds": null
        }"#;
        let s: SentrixStatus = serde_json::from_str(body).unwrap();
        assert_eq!(s.sync_info.latest_block_height, 1234);
        assert_eq!(s.sync_info.latest_block_time, 1715000000);
        assert_eq!(s.sync_info.latest_block_hash, "0xabcdef0123456789");
        assert!(s.validators.is_none());
        assert!(s.mempool.is_none());
        assert!(s.uptime_seconds.is_none());
    }

    #[test]
    fn sentrix_status_deserialises_full_shape() {
        let body = r#"{
            "sync_info": {
                "latest_block_height": 1,
                "latest_block_time": 2,
                "latest_block_hash": "0x00000001"
            },
            "validators": { "active_count": 4 },
            "mempool": { "size": 7 },
            "uptime_seconds": 999
        }"#;
        let s: SentrixStatus = serde_json::from_str(body).unwrap();
        assert_eq!(s.validators.unwrap().active_count, 4);
        assert_eq!(s.mempool.unwrap().size, 7);
        assert_eq!(s.uptime_seconds.unwrap(), 999);
    }

    #[test]
    fn target_clone_and_debug_present() {
        let t = Target {
            name: "n".into(),
            network: "mainnet".into(),
            url: "http://x".into(),
        };
        let t2 = t.clone();
        assert_eq!(t2.name, "n");
        // Debug formatting (derived) renders the struct.
        let s = format!("{:?}", t);
        assert!(s.contains("Target"));
    }

    // Async coverage: init_metrics + poll_one + the prometheus encode path
    // metrics_handler uses. All in one #[tokio::test] because the
    // OnceLock-backed metrics statics are process-global and prometheus's
    // default registry is a singleton — sequencing avoids cross-test races.
    #[tokio::test]
    async fn init_metrics_then_poll_against_local_server() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // ── 1. init_metrics walks every register macro + OnceLock.set ──
        init_metrics();
        assert!(HEIGHT.get().is_some());
        assert!(BLOCK_AGE.get().is_some());
        assert!(TIP_HASH.get().is_some());
        assert!(ACTIVE_VALIDATORS.get().is_some());
        assert!(MEMPOOL.get().is_some());
        assert!(UPTIME.get().is_some());
        assert!(PROBE_FAILED.get().is_some());

        // ── 2. Prometheus gather + encode (mirrors metrics_handler body) ──
        // Pre-populate one metric so the output is non-trivial.
        HEIGHT.get().unwrap()
            .with_label_values(&["test-val", "test-net"])
            .set(42);
        let metric_families = prometheus::gather();
        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        let body = String::from_utf8(buffer).unwrap();
        assert!(body.contains("sentrix_chain_height"));
        assert!(body.contains("test-val"));
        assert!(body.contains("test-net"));
        assert!(body.contains("42"));

        // ── 3. poll_one against a local HTTP fixture ──
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let canned = r#"{
            "sync_info": {
                "latest_block_height": 12345,
                "latest_block_time": 1000000000,
                "latest_block_hash": "0xcafebabedeadbeef0011223344556677"
            },
            "validators": { "active_count": 4 },
            "mempool": { "size": 2 },
            "uptime_seconds": 60
        }"#;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                canned.len(),
                canned,
            );
            stream.write_all(resp.as_bytes()).await.unwrap();
            stream.shutdown().await.ok();
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let target = Target {
            name: "fixture".into(),
            network: "fxnet".into(),
            url: format!("http://{}", addr),
        };
        let res = poll_one(&client, &target).await;
        assert!(res.is_some(), "poll_one returned None — fixture handshake failed");

        let h = HEIGHT.get().unwrap()
            .get_metric_with_label_values(&["fixture", "fxnet"])
            .unwrap()
            .get();
        assert_eq!(h, 12345);
        let m = MEMPOOL.get().unwrap()
            .get_metric_with_label_values(&["fixture", "fxnet"])
            .unwrap()
            .get();
        assert_eq!(m, 2);
        let av = ACTIVE_VALIDATORS.get().unwrap()
            .get_metric_with_label_values(&["fxnet"])
            .unwrap()
            .get();
        assert_eq!(av, 4);
        let up = UPTIME.get().unwrap()
            .get_metric_with_label_values(&["fixture", "fxnet"])
            .unwrap()
            .get();
        assert_eq!(up, 60);
        let th = TIP_HASH.get().unwrap()
            .get_metric_with_label_values(&["fixture", "fxnet"])
            .unwrap()
            .get();
        assert_eq!(th, 0xcafebabe_u32 as i64);

        let _ = server.await;

        // ── 4. poll_one against an unreachable address returns None ──
        let dead = Target {
            name: "dead".into(),
            network: "fxnet".into(),
            url: "http://127.0.0.1:1".into(),
        };
        let res = poll_one(&client, &dead).await;
        assert!(res.is_none(), "poll_one against closed port should return None");
    }
}
