# sentrix-rpc

[![crates.io](https://img.shields.io/crates/v/sentrix-rpc.svg)](https://crates.io/crates/sentrix-rpc)
[![docs.rs](https://docs.rs/sentrix-rpc/badge.svg)](https://docs.rs/sentrix-rpc)

REST API, JSON-RPC (`eth_*`), WebSocket subscriptions, and block-explorer endpoints for Sentrix Chain.

## Why this crate exists

This is the ecosystem-facing surface — the contract wallets, dApps, and the public
explorer depend on. It exposes axum routes for the `eth_*` JSON-RPC namespace,
WebSocket subscriptions (`eth_subscribe` newHeads + logs), the explorer REST
endpoints, and the Sentrix-specific `/sentrix_status` health endpoint that the
[sentrix-prom-exporter](../sentrix-prom-exporter) scrapes.

Builds on [sentrix-core](../sentrix-core) for blockchain state,
[sentrix-evm](../sentrix-evm) for `eth_call` and `eth_estimateGas`,
[sentrix-rpc-types](../sentrix-rpc-types) for the pure hex/address helpers, and
[sentrix-trie](../sentrix-trie) for `eth_getProof`. The shared `EventBus` exported
here is also consumed by [sentrix-grpc](../sentrix-grpc) so gRPC stream subscribers
and WebSocket subscribers see the same ordering.

## Usage

```toml
[dependencies]
sentrix-rpc = { path = "../sentrix-rpc" }
```

```rust
use sentrix_rpc::{create_router, SharedState, EventBus};
use std::sync::Arc;

// The validator main builds an Arc<RwLock<Blockchain>>, hands it to the router,
// and serves the result with axum. Same shared state the gRPC side-car holds.
let state: SharedState = /* Arc<RwLock<Blockchain>> */;
let bus = Arc::new(EventBus::new());
let app = create_router(state, bus);
axum::serve(listener, app).await?;
```

Key re-exports: `create_router`, `SharedState`, `EventBus`, `NewHeadEvent`.
Submodules: `jsonrpc` (the `eth_*` handlers), `routes` (axum routing + CORS),
`ws` (WebSocket subscriptions), `explorer` + `explorer_api` (block / tx / log
queries), `events` (broadcast channels).

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
