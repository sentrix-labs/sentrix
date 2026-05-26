# sentrix-rpc-types

[![crates.io](https://img.shields.io/crates/v/sentrix-rpc-types.svg)](https://crates.io/crates/sentrix-rpc-types)
[![docs.rs](https://docs.rs/sentrix-rpc-types/badge.svg)](https://docs.rs/sentrix-rpc-types)

Pure ETH ↔ Sentrix JSON-RPC type conversions and hex / address validation helpers.

## Why this crate exists

Helpers like `to_hex(u64)`, `parse_hex_u64(Value)`, `normalize_rpc_address`, and
`normalize_rpc_hash` are needed by both the JSON-RPC server in
[sentrix-rpc](../sentrix-rpc) and any future Sentrix JSON-RPC client SDK. Keeping them
in a tiny crate that depends only on `serde_json` means SDK consumers don't pay for
the axum / tokio / revm transitive deps that come with the server crate.

Every helper here is pure — it operates on strings and primitive integers, never on
node state. Anything that needs a `Blockchain` reference (e.g. `resolve_block_tag`,
`log_matches`) stays in the consuming crate.

## Usage

```toml
[dependencies]
sentrix-rpc-types = { path = "../sentrix-rpc-types" }
```

```rust
use sentrix_rpc_types::{to_hex, to_hex_u128, parse_hex_u64, normalize_rpc_address, normalize_rpc_hash};
use serde_json::json;

assert_eq!(to_hex(255), "0xff");
assert_eq!(parse_hex_u64(&json!("0x2a")), Some(42));
assert_eq!(parse_hex_u64(&json!(42)), Some(42));

// normalize_rpc_address takes "0x" + 40 hex chars and returns the lowercased
// length-validated form; malformed input returns an `&'static str` error.
let addr = normalize_rpc_address(&caller_supplied_address)?;

// normalize_rpc_hash applies the same shape check for 32-byte hashes.
let hash = normalize_rpc_hash(&caller_supplied_hash)?;
```

Address normalisation enforces exactly `0x` + 40 hex characters; hash normalisation
enforces 32 bytes (64 hex). Malformed inputs return an `&'static str` error suitable
for JSON-RPC error code `-32602` (Invalid params), so adversarial requests don't
reach the account store as silent misses.

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
