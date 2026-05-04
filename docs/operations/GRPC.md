---
sidebar_position: 4
title: gRPC API
---

# Sentrix Chain — gRPC API

Sentrix Chain ships a Tonic-based gRPC interface as a parallel transport to the JSON-RPC `eth_*` endpoints. Same backend, same state, different wire format.

**When to use gRPC instead of JSON-RPC:**
- Binary protocol — smaller payloads, faster decode than JSON
- Strongly-typed schema via Protocol Buffers — no runtime parsing surprises
- HTTP/2 multiplexing — many in-flight calls over one connection
- Native server-streaming (when `StreamEvents` lands in v0.3)

**When JSON-RPC is still the right call:**
- MetaMask, ethers.js, hardhat, viem — all speak `eth_*` over JSON-RPC. Don't switch your dApp away from a working transport.
- Quick `curl` exploration without code generation
- Web pages that just need `eth_blockNumber` once on load

---

## Endpoints

| Network | Endpoint | chain_id |
|---|---|---|
| Mainnet | `grpc.sentrixchain.com:443` | 7119 |
| Testnet | `grpc-testnet.sentrixchain.com:443` | 7120 |

Both endpoints terminate TLS at the edge proxy and forward to the validator side-car over a private hop. Cloudflare proxy is enabled with the gRPC protocol toggle on, so HTTP/2 + `te: trailers` traverses the edge transparently.

Browser clients can hit the same hostnames over **gRPC-Web** (HTTP/1.1 or HTTP/2 with `application/grpc-web` content-type). Edge CORS is permissive (`Access-Control-Allow-Origin: *`) and exposes the `grpc-status` / `grpc-message` trailers so client libraries can read errors correctly.

---

## Schema

Canonical `.proto` lives in the repository at:

> `crates/sentrix-grpc/proto/sentrix.proto`

Service: **`sentrix.v1.Sentrix`** (note: `Sentrix`, not `SentrixService` or `BlockchainService`).

To generate clients, fetch the file directly from `main` and feed it to your codegen toolchain (`protoc`, `tonic-build`, `grpc-tools`, etc).

```bash
curl -O https://raw.githubusercontent.com/sentrix-labs/sentrix/main/crates/sentrix-grpc/proto/sentrix.proto
```

Reflection is **not** enabled on the side-car. `grpcurl` clients must be invoked with `-import-path` + `-proto`; `grpcurl list` will fail.

---

## Methods (v0.2)

### `GetBlock(GetBlockRequest) → Block`

Fetch a block by height, by hash, or via the `latest` / `finalized` selector. Returns `NOT_FOUND` if the block is outside the validator's in-memory chain window (currently 1000 blocks; older blocks need an indexer).

**Request:**

```protobuf
message GetBlockRequest {
  oneof selector {
    BlockHeight height = 1;   // { value: <uint64> }
    Hash hash = 2;             // { value: <32 bytes> }
    bool latest = 3;
    bool finalized = 4;
  }
}
```

**Response (`Block`):**

```protobuf
message Block {
  uint64 index = 1;
  Hash hash = 2;
  Hash parent_hash = 3;
  Hash state_root = 4;
  uint64 timestamp = 5;
  Address proposer = 6;
  uint32 round = 7;
  repeated Transaction transactions = 8;  // empty in v0.2
  bytes justification = 9;                 // bincoded BFT justification
}
```

> **v0.2 limitation:** `transactions` is returned empty. Full marshalling lands in v0.3 alongside `BroadcastTx`. To fetch transactions today, use the JSON-RPC `eth_getBlockByNumber` endpoint.

### `GetBalance(GetBalanceRequest) → Account`

Single round-trip for `eth_getBalance` + `eth_getTransactionCount`. Includes mempool-pending nonce (matches the chain's pending-aware nonce behaviour).

**Request:**

```protobuf
message GetBalanceRequest {
  Address address = 1;                       // { value: <20 bytes> }
  optional BlockHeight at_height = 2;       // historical reads — see limitation
}
```

**Response (`Account`):**

```protobuf
message Account {
  Address address = 1;
  Amount balance = 2;     // { sentri: <uint64> }
  uint64 nonce = 3;
  Hash storage_root = 4;  // not populated in v0.2
  Hash code_hash = 5;     // not populated in v0.2
}
```

> **v0.2 limitation:** `at_height` historical reads return `FAILED_PRECONDITION`. Snapshot-isolated reads need an MDBX-snapshot refactor; tracked for v0.3.

### `BroadcastTx(BroadcastTxRequest) → BroadcastTxResponse`

Submit a signed transaction to the local mempool. **Returns `UNIMPLEMENTED` in v0.2.** Use JSON-RPC `eth_sendRawTransaction` for now.

### `StreamEvents(StreamEventsRequest) → stream ChainEvent`

Server-streaming subscription replacing N separate `eth_subscribe` calls. **Returns `UNIMPLEMENTED` in v0.2.** Use the WebSocket `eth_subscribe` endpoint for now.

---

## Quickstart

### `grpcurl` (CLI)

```bash
# fetch the proto
curl -O https://raw.githubusercontent.com/sentrix-labs/sentrix/main/crates/sentrix-grpc/proto/sentrix.proto

# latest block on mainnet
grpcurl -import-path . -proto sentrix.proto \
  -d '{"latest":true}' \
  grpc.sentrixchain.com:443 sentrix.v1.Sentrix/GetBlock

# specific height
grpcurl -import-path . -proto sentrix.proto \
  -d '{"height":{"value":1440000}}' \
  grpc.sentrixchain.com:443 sentrix.v1.Sentrix/GetBlock

# balance — Address.value is base64-encoded 20 bytes
grpcurl -import-path . -proto sentrix.proto \
  -d '{"address":{"value":"<base64-of-20-byte-address>"}}' \
  grpc.sentrixchain.com:443 sentrix.v1.Sentrix/GetBalance
```

### Rust (Tonic)

```toml
# Cargo.toml
[dependencies]
tonic = "0.12"
prost = "0.13"
tokio = { version = "1", features = ["full"] }

[build-dependencies]
tonic-build = "0.12"
```

```rust
// build.rs
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::compile_protos("proto/sentrix.proto")?;
    Ok(())
}

// src/main.rs
pub mod sentrix_proto { tonic::include_proto!("sentrix.v1"); }

use sentrix_proto::sentrix_client::SentrixClient;
use sentrix_proto::GetBlockRequest;
use sentrix_proto::get_block_request::Selector;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = SentrixClient::connect("https://grpc.sentrixchain.com").await?;
    let resp = client.get_block(GetBlockRequest {
        selector: Some(Selector::Latest(true)),
    }).await?;
    println!("latest block index = {}", resp.into_inner().index);
    Ok(())
}
```

### TypeScript / Node (`@grpc/grpc-js`)

```bash
npm install @grpc/grpc-js @grpc/proto-loader
```

```typescript
import { credentials, loadPackageDefinition } from "@grpc/grpc-js";
import { loadSync } from "@grpc/proto-loader";

const pkgDef = loadSync("./sentrix.proto", { keepCase: true, longs: String });
const proto = loadPackageDefinition(pkgDef) as any;

const client = new proto.sentrix.v1.Sentrix(
  "grpc.sentrixchain.com:443",
  credentials.createSsl(),
);

client.GetBlock({ latest: true }, (err: any, block: any) => {
  if (err) return console.error(err);
  console.log("latest block index =", block.index);
});
```

### Browser / Next.js (`@grpc/grpc-web` or `@protobuf-ts/grpcweb-transport`)

The gRPC-Web codec is enabled on the same host — browsers can call directly without a separate proxy.

```bash
npm install @protobuf-ts/grpcweb-transport @protobuf-ts/runtime-rpc
# (plus your codegen pipeline of choice — e.g. ts-proto, protobuf-ts plugin for protoc)
```

```typescript
import { GrpcWebFetchTransport } from "@protobuf-ts/grpcweb-transport";
import { SentrixClient } from "./generated/sentrix.client";

const transport = new GrpcWebFetchTransport({
  baseUrl: "https://grpc.sentrixchain.com",
});

const client = new SentrixClient(transport);

const { response } = await client.getBlock({ selector: { oneofKind: "latest", latest: true } });
console.log("latest block index =", response.index);
```

### Python (`grpcio`)

```bash
pip install grpcio grpcio-tools
python -m grpc_tools.protoc -I. --python_out=. --grpc_python_out=. sentrix.proto
```

```python
import grpc
import sentrix_pb2 as pb
import sentrix_pb2_grpc as svc

channel = grpc.secure_channel("grpc.sentrixchain.com:443", grpc.ssl_channel_credentials())
client = svc.SentrixStub(channel)

resp = client.GetBlock(pb.GetBlockRequest(latest=True))
print("latest block index =", resp.index)
```

### Go (`google.golang.org/grpc`)

```bash
go get google.golang.org/grpc google.golang.org/grpc/credentials
protoc --go_out=. --go-grpc_out=. sentrix.proto
```

```go
package main

import (
    "context"
    "log"

    "google.golang.org/grpc"
    "google.golang.org/grpc/credentials"
    pb "yourmodule/proto"
)

func main() {
    creds := credentials.NewClientTLSFromCert(nil, "")
    conn, err := grpc.Dial("grpc.sentrixchain.com:443", grpc.WithTransportCredentials(creds))
    if err != nil { log.Fatal(err) }
    defer conn.Close()

    client := pb.NewSentrixClient(conn)
    block, err := client.GetBlock(context.Background(), &pb.GetBlockRequest{
        Selector: &pb.GetBlockRequest_Latest{Latest: true},
    })
    if err != nil { log.Fatal(err) }
    log.Printf("latest block index = %d", block.Index)
}
```

---

## CORS (browser clients)

The edge proxy adds the following headers on every gRPC-Web response:

```
Access-Control-Allow-Origin: *
Access-Control-Allow-Methods: POST, OPTIONS
Access-Control-Allow-Headers: Content-Type, X-Grpc-Web, X-User-Agent, Grpc-Timeout
Access-Control-Expose-Headers: Grpc-Status, Grpc-Message, Grpc-Encoding, Grpc-Accept-Encoding
Access-Control-Max-Age: 86400
```

Preflight `OPTIONS` requests are answered with `204 No Content`. Standard gRPC-Web client libraries work without any custom configuration.

---

## Limitations

- **Chain window:** Validators serve blocks within their last ~1000-block in-memory window. Older blocks need to come from an indexer (none operated by Sentrix Labs at this time — community indexers welcome).
- **No reflection:** `grpcurl list` won't work. Use the `.proto` file.
- **No history reads:** `at_height` on `GetBalance` returns `FAILED_PRECONDITION`.
- **Read-only:** `BroadcastTx` and `StreamEvents` are stubs in v0.2; use JSON-RPC `eth_sendRawTransaction` and the WebSocket `eth_subscribe` endpoint until v0.3.
- **Single validator per network:** The published endpoint forwards to a single validator side-car. If that validator restarts, expect a brief connection reset; clients should implement standard gRPC retry with exponential backoff.

---

## Roadmap

- **v0.3** — full transaction marshalling (`BroadcastTx`), event-bus subscription (`StreamEvents`), MDBX snapshot reads (`at_height`).
- **v0.4** — multi-validator load balancing on the edge, optional gRPC compression negotiation, server reflection toggle for tooling.

Track progress at the canonical design doc in the repo: `crates/sentrix-grpc/proto/sentrix.proto` is updated as methods come online.
