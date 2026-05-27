# sentrix-codec

[![crates.io](https://img.shields.io/crates/v/sentrix-codec.svg)](https://crates.io/crates/sentrix-codec)
[![docs.rs](https://docs.rs/sentrix-codec/badge.svg)](https://docs.rs/sentrix-codec)

Centralised bincode + hex encoding helpers for the Sentrix workspace.

## What it's good for

A thin compatibility layer that pins one bincode config + adds the missing
`0x`-prefix-aware hex helpers. Useful beyond Sentrix for any Rust project that:
- Mixes bincode 1.x serialization with hex encoding (RPC layers, on-disk
  fixtures, debug dumps) and wants the import surface in one place
- Needs to migrate bincode 1.x → 2.x later by touching one crate instead of
  every call site
- Wants `hex_decode` that transparently accepts both `"deadbeef"` and
  `"0xdeadbeef"`, plus length-checked `hex_decode_fixed::<N>` returning `[u8; N]`

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

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual-licensed as above, without any additional terms or conditions.
