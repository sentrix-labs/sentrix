# sentrix-proto

[![crates.io](https://img.shields.io/crates/v/sentrix-proto.svg)](https://crates.io/crates/sentrix-proto)
[![docs.rs](https://docs.rs/sentrix-proto/badge.svg)](https://docs.rs/sentrix-proto)

Generated tonic + prost protobuf types for the Sentrix Chain gRPC service (`sentrix.v1`).

## Why this crate exists

The Sentrix Chain gRPC service has one server-side implementation (the chain itself) and several client consumers (the Rust SDK, the WASM gRPC-Web client, the explorer-v2 frontend, future polyglot SDKs). Vendoring `sentrix.proto` separately into each consumer was producing schema drift — this crate is the single source of truth.

The chain server (`crates/sentrix-grpc` in [`sentrix-labs/sentrix`](https://github.com/sentrix-labs/sentrix)) consumes these types via the workspace path-dep; external clients depend on this crate via crates.io.

## Usage

```toml
[dependencies]
# Native (server, native client) — default features include `transport`:
sentrix-proto = "0.1"

# Browser / WASM target — disable `transport` so tokio-net / mio aren't pulled in:
# sentrix-proto = { version = "0.1", default-features = false }
```

### Cargo features

| Feature | Default | What it adds |
|---|---|---|
| `transport` | yes | Generated server stubs + tonic's hyper-based transport (Channel, Server). Needed for native server / native client; OFF for `wasm32`. |

```rust
use sentrix_proto::sentrix_client::SentrixClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = SentrixClient::connect("https://grpc.sentrixchain.com").await?;

    let block = client
        .get_block(sentrix_proto::GetBlockRequest {
            selector: Some(sentrix_proto::get_block_request::Selector::Latest(true)),
        })
        .await?
        .into_inner();

    println!("latest block: index={} timestamp={}", block.index, block.timestamp);
    Ok(())
}
```

## Schema versioning

The schema is namespaced `package sentrix.v1;`. Breaking field renames or type changes ship as a new `sentrix.v2` package, not in-place edits to v1. That gives client crates a deterministic boundary for major-version bumps.

## Build requirements

The build script invokes `protoc` (passed via `prost-build`). On Ubuntu 22.04 the apt-installed `protoc` is 3.12.x which treats proto3 `optional` fields as experimental — `build.rs` passes `--experimental_allow_proto3_optional` to keep the build green on older runners. Modern `protoc` (≥ 3.15) accepts the field by default, no change needed.

```bash
sudo apt install -y protobuf-compiler libprotobuf-dev
```

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
