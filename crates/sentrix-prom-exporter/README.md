# sentrix-prom-exporter

[![crates.io](https://img.shields.io/crates/v/sentrix-prom-exporter.svg)](https://crates.io/crates/sentrix-prom-exporter)
[![docs.rs](https://docs.rs/sentrix-prom-exporter/badge.svg)](https://docs.rs/sentrix-prom-exporter)

Standalone Prometheus exporter that polls per-validator `/sentrix_status` endpoints and exposes labelled gauges for Grafana.

## Why this crate exists

The chain binary doesn't emit a native `/metrics` endpoint yet, so the Grafana
dashboards need a side-car that scrapes the existing JSON `/sentrix_status` endpoint
on each validator and re-publishes the numbers as Prometheus metrics with
`{validator, network}` labels. The key metric is `sentrix_chain_tip_hash_short` — the
first 8 hex chars of the tip hash decoded to u32 — which makes cluster-divergence a
one-line PromQL query:
`count(count by (sentrix_chain_tip_hash_short) (sentrix_chain_tip_hash_short)) > 1`.

Runs as its own binary, no chain crate deps. Reads `SENTRIX_TARGETS` env (format
`name:network:url,...`), `SENTRIX_PROBE_INTERVAL_SEC` (default 15), and
`SENTRIX_EXPORTER_BIND` (default `0.0.0.0:9101`).

## Usage

```bash
cargo run --release -p sentrix-prom-exporter
```

```bash
# systemd unit env (single line in real config):
SENTRIX_TARGETS=val1:mainnet:http://10.0.0.1:8545,val2:testnet:http://10.0.0.2:9545
SENTRIX_PROBE_INTERVAL_SEC=15
SENTRIX_EXPORTER_BIND=0.0.0.0:9101
```

Exported metrics:

| Metric | Labels |
|---|---|
| `sentrix_chain_height` | `validator`, `network` |
| `sentrix_block_age_seconds` | `validator`, `network` |
| `sentrix_chain_tip_hash_short` | `validator`, `network` |
| `sentrix_active_validators` | `network` |
| `sentrix_mempool_size` | `validator`, `network` |
| `sentrix_uptime_seconds` | `validator`, `network` |
| `sentrix_probe_failed_total` | `validator`, `network` |

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
