<p align="center">
  <img src="https://cdn.jsdelivr.net/gh/sentrix-labs/brand-kit@master/png-transparent/sentrix-256.png" alt="Sentrix" width="120" />
</p>

# sentrix-codec

[![crates.io](https://img.shields.io/crates/v/sentrix-codec.svg)](https://crates.io/crates/sentrix-codec)
[![docs.rs](https://docs.rs/sentrix-codec/badge.svg)](https://docs.rs/sentrix-codec)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

Centralized serialization and hex encoding helpers used by Sentrix internal components. A thin wrapper over `bincode 1.3` and `hex 0.4` with a single error type for both surfaces.

## What it is

`sentrix-codec` is a small Rust crate providing centralized serialization and hex encoding helpers used by Sentrix internal components.

It wraps:

- `bincode 1.3`
- `hex 0.4`
- `serde`

Main API surface:

- `encode`
- `decode`
- `hex_encode`
- `hex_decode`
- `hex_decode_fixed`
- `CodecError`

## Why this crate exists

Direct, scattered usage of `bincode::serialize`, `bincode::deserialize`, `hex::encode`, and `hex::decode` across many call sites makes future format changes harder — every consumer has to be found and patched in lockstep.

This crate acts as a single chokepoint for serialization and hex behavior across the Sentrix workspace. A change to the underlying format lives in one file, and every consumer follows.

## Format guarantees

- `encode` / `decode` use `bincode 1.3` default serialization.
- Encoded bytes are byte-compatible with a direct `bincode::serialize` call against the same crate version and the same type.
- `hex_encode` returns lowercase hex with no `0x` prefix.
- `hex_decode` accepts strings with or without a leading `0x` prefix.
- `hex_decode_fixed<const N: usize>` accepts strings with or without a leading `0x` prefix and requires exactly `N` decoded bytes.
- Any future change that alters encoded bytes is treated as compatibility-sensitive.

## What this crate is not

- This crate is **not** an Ethereum ABI encoder.
- This crate is **not** an RLP codec.
- This crate is **not** an EVM transaction envelope codec.
- This crate is **not** SSZ, SCALE, protobuf, or a replacement for any externally specified protocol format.
- This crate is an internal Sentrix serialization and hex helper crate.

## API

```rust
pub fn encode<T: serde::Serialize>(val: &T) -> Result<Vec<u8>, CodecError>;

pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError>;

pub fn hex_encode<T: AsRef<[u8]>>(bytes: T) -> String;

pub fn hex_decode(s: &str) -> Result<Vec<u8>, CodecError>;

pub fn hex_decode_fixed<const N: usize>(s: &str) -> Result<[u8; N], CodecError>;

pub enum CodecError {
    Encode(String),
    Decode(String),
}
```

`CodecError` implements `std::fmt::Display` and `std::error::Error`, so it composes with `?` in any `Result` chain.

## Usage

Add the dependency inside the workspace using a path dependency:

```toml
[dependencies]
sentrix-codec = { path = "../sentrix-codec" }
```

Encode and decode a small struct, then exercise the hex helpers:

```rust
use sentrix_codec::{encode, decode, hex_encode, hex_decode, hex_decode_fixed};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Block {
    height: u64,
    data: Vec<u8>,
}

let b = Block { height: 42, data: vec![1, 2, 3] };
let bytes = encode(&b)?;
let back: Block = decode(&bytes)?;
assert_eq!(b, back);

let s = hex_encode([0xde, 0xad, 0xbe, 0xef]);            // "deadbeef"
let v = hex_decode("0xdeadbeef")?;                        // accepts 0x prefix
let arr: [u8; 4] = hex_decode_fixed("deadbeef")?;         // length-checked
# Ok::<(), sentrix_codec::CodecError>(())
```

## Safety and security notes

- `#![forbid(unsafe_code)]` is set at the crate root — `unsafe` blocks are rejected at compile time.
- No filesystem, network, environment, or process access — the crate is pure functions over its inputs.
- All fallible operations return `CodecError`.
- `hex_decode_fixed` checks the decoded byte length before copying into the output array.

## Testing

```sh
cargo test -p sentrix-codec
```

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual-licensed as above, without any additional terms or conditions.
