# sentrix-grpc

[![crates.io](https://img.shields.io/crates/v/sentrix-grpc.svg)](https://crates.io/crates/sentrix-grpc)
[![docs.rs](https://docs.rs/sentrix-grpc/badge.svg)](https://docs.rs/sentrix-grpc)

Tonic gRPC supplement transport for Sentrix Chain — parallel to the JSON-RPC `eth_*` interface.

## Why this crate exists

JSON-RPC stays the ecosystem-facing contract for wallets and dApps; gRPC is the
side-car for SentrisCloud internal monitoring, the Rust SDK, and clients that prefer
a binary protocol over WebSocket polling. Both transports share the same
`Arc<RwLock<Blockchain>>` and the same `EventBus` — gRPC is just another reader.

Generated proto types live in the sibling [sentrix-proto](../sentrix-proto) crate (its
own crates.io semver). This crate carries only server-side handlers and the chain-state
plumbing that depends on [sentrix-core](../sentrix-core), [sentrix-rpc](../sentrix-rpc),
and [sentrix-staking](../sentrix-staking).

## Usage

```toml
[dependencies]
sentrix-grpc = { path = "../sentrix-grpc" }
```

```rust
use sentrix_grpc::{SentrixServiceImpl, server_factory, SharedState};

// In the validator main: spawn the gRPC server next to the axum JSON-RPC
// router, sharing the same Blockchain handle and EventBus.
let svc = server_factory(shared_state.clone(), event_bus.clone());
tokio::spawn(async move {
    tonic::transport::Server::builder()
        .add_service(svc)
        .serve("0.0.0.0:50051".parse().unwrap())
        .await
        .ok();
});
```

Implemented RPCs: `GetBlock`, `GetBalance`, `GetValidatorSet`, `GetSupply`,
`GetMempool`, and server-streaming `StreamEvents` (subscribes to the same broadcast
bus the WebSocket `eth_subscribe` path uses). `BroadcastTx` is currently
`Status::unimplemented` pending the proto↔chain `Transaction` marshalling.

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
