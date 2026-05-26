# sentrix-codec

[![crates.io](https://img.shields.io/crates/v/sentrix-codec.svg)](https://crates.io/crates/sentrix-codec)
[![docs.rs](https://docs.rs/sentrix-codec/badge.svg)](https://docs.rs/sentrix-codec)

Centralised bincode + hex encoding helpers for the Sentrix workspace.

## Why this crate exists

Eight files across the workspace were calling `bincode::serialize` / `bincode::deserialize`
directly, and `hex::encode` / `hex::decode` were used even more widely. A future format
change — bincode 1.x → 2.x, endianness, size limits — would have required hunting every
call site and patching them in lockstep. This crate is the single chokepoint so that
migration touches one file.

Consumed across most chain crates ([sentrix-core](../sentrix-core),
[sentrix-storage](../sentrix-storage), [sentrix-rpc](../sentrix-rpc), etc.). Wraps
bincode 1.3 with the workspace's default config (little-endian, varint ints, no byte
limit) — same on-the-wire bytes as direct `bincode::serialize` calls.

## Usage

```toml
[dependencies]
sentrix-codec = { path = "../sentrix-codec" }
```

```rust
use sentrix_codec::{encode, decode, hex_encode, hex_decode, hex_decode_fixed};

let bytes = encode(&("hello", 42u64)).unwrap();
let back: (String, u64) = decode(&bytes).unwrap();

let hex_str = hex_encode([0xde, 0xad, 0xbe, 0xef]);          // "deadbeef"
let raw = hex_decode("0xdeadbeef").unwrap();                  // accepts 0x prefix
let arr: [u8; 4] = hex_decode_fixed("deadbeef").unwrap();     // length-checked
```

Errors come back as `CodecError::Encode` / `CodecError::Decode` so callers can match
without a bincode dependency themselves.

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
