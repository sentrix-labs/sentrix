<p align="center">
  <img src="https://cdn.jsdelivr.net/gh/sentrix-labs/brand-kit@master/png-transparent/sentrix-256.png" alt="Sentrix" width="120" />
</p>

<h1 align="center">sentrix-codec</h1>

<p align="center">
  <a href="https://crates.io/crates/sentrix-codec"><img src="https://img.shields.io/crates/v/sentrix-codec.svg" alt="crates.io" /></a>
  <a href="https://docs.rs/sentrix-codec"><img src="https://docs.rs/sentrix-codec/badge.svg" alt="docs.rs" /></a>
  <a href="https://github.com/sentrix-labs/sentrix/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/sentrix-labs/sentrix/ci.yml?branch=main&label=CI" alt="CI" /></a>
  <a href="https://codecov.io/gh/sentrix-labs/sentrix"><img src="https://codecov.io/gh/sentrix-labs/sentrix/branch/main/graph/badge.svg" alt="Coverage" /></a>
  <a href="https://crates.io/crates/sentrix-codec"><img src="https://img.shields.io/crates/d/sentrix-codec.svg?label=downloads" alt="downloads" /></a>
  <a href="https://github.com/sentrix-labs/sentrix/blob/main/crates/sentrix-codec/Cargo.toml"><img src="https://img.shields.io/badge/MSRV-1.95-blue.svg" alt="MSRV 1.95" /></a>
  <a href="#license"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg" alt="License: MIT OR Apache-2.0" /></a>
</p>

Centralized serialization and hex encoding helpers for the Sentrix workspace.

## What it is

A thin, stable wrapper around `bincode 1.3` and `hex 0.4`. Five pure functions
and one error type — `encode` / `decode` for bincode, `hex_encode` /
`hex_decode` / `hex_decode_fixed` for hex, all returning `Result<_, CodecError>`.

No I/O, no `unsafe`, no panics on the production path. Pulls in only the three
crates it wraps (`bincode`, `hex`, `serde`).

## Why this crate exists

A workspace that calls `bincode::serialize` and `hex::decode` from dozens of
call sites turns any format change — bincode 1.x → 2.x, hex prefix rules, byte
limits — into a workspace-wide grep-and-patch. This crate is the chokepoint:
edit the encoder once, every consumer follows in one step.

The same logic applies outside Sentrix: any Rust project that wants to commit
to one bincode config (and stop re-implementing the `0x`-prefix-tolerant
`hex_decode` everyone eventually writes) can use it the same way.

## Format guarantees

- **bincode**: `1.3` default config — little-endian, variable-length integer
  encoding, no byte-length limit. Output is byte-identical to a direct
  `bincode::serialize(val)` call against the same crate version.
- **hex encode**: lowercase, no `0x` prefix (matches `hex::encode`).
- **hex decode**: tolerates a leading `0x` prefix; fails on non-hex characters
  or odd length.
- **hex decode fixed**: tolerates `0x`, then requires exactly `2 * N`
  characters; fails with a length-mismatch error otherwise.

These are stable for the `0.x` line. Any breaking change to the on-the-wire
bytes will be a major version bump.

## API

```rust
// bincode
pub fn encode<T: Serialize>(val: &T) -> Result<Vec<u8>, CodecError>;
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError>;

// hex
pub fn hex_encode<T: AsRef<[u8]>>(bytes: T) -> String;
pub fn hex_decode(s: &str) -> Result<Vec<u8>, CodecError>;
pub fn hex_decode_fixed<const N: usize>(s: &str) -> Result<[u8; N], CodecError>;

// error
pub enum CodecError {
    Encode(String),
    Decode(String),
}
```

`CodecError` implements `std::error::Error` and `Display`. Pattern-match on the
variants if you need to distinguish encode vs decode failures without taking a
direct dependency on `bincode` yourself.

## Usage

Add to your `Cargo.toml`:

```toml
[dependencies]
sentrix-codec = "0.1"
```

This crate has **no optional features** and **no `default-features`** to disable — the API surface is the same for every consumer.

## Examples

```rust
use sentrix_codec::{encode, decode, hex_encode, hex_decode, hex_decode_fixed};
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Block {
    height: u64,
    data: Vec<u8>,
}

// bincode round-trip
let b = Block { height: 42, data: vec![1, 2, 3] };
let bytes = encode(&b)?;
let back: Block = decode(&bytes)?;
assert_eq!(b, back);

// hex helpers
let s = hex_encode([0xde, 0xad, 0xbe, 0xef]);           // "deadbeef"
let v = hex_decode("0xdeadbeef")?;                       // accepts 0x prefix
let arr: [u8; 4] = hex_decode_fixed("deadbeef")?;        // length-checked
# Ok::<(), sentrix_codec::CodecError>(())
```

## Safety and security notes

- **`#![forbid(unsafe_code)]`** — the crate cannot contain `unsafe` blocks;
  enforced at compile time.
- **No I/O.** All functions are pure — no filesystem, network, environment, or
  process access. The crate has no side channels of its own.
- **No panics on the production path.** Every fallible operation returns
  `CodecError`; the workspace lint config denies `unwrap_used`, `expect_used`,
  and `panic`.
- **Length-checked decode.** `hex_decode_fixed` validates the byte length
  before copying into the output array.
- **Secrets caveat.** If you serialize a private key or other signing material
  with `encode`, the resulting `Vec<u8>` is plain bytes — treat it with the
  same care as the original secret (zero on drop, avoid logging, avoid
  unencrypted transmission). The crate does not zeroize on its own.
- **Not `no_std`-compatible.** The crate uses `Vec<u8>` and `String` from
  `alloc` / `std`; embedded targets without an allocator are not supported.

## Testing

Nine unit tests cover all five public functions plus the error paths
(non-hex input, odd-length input, wrong fixed length, garbage bincode bytes).

```sh
cargo test -p sentrix-codec
```

## Minimum supported Rust version

**Rust 1.95** (declared via `rust-version` in this crate's `Cargo.toml`).
MSRV bumps are treated as a minor-version bump on this crate (`0.1.x`
→ `0.2.0`) so downstream consumers can pin around a Rust release if
they need to.

## Versioning

Pre-`1.0`: the `0.x` line follows the de-facto crates.io convention —
**minor bumps may include breaking changes**, patch bumps are non-breaking
fixes. Any change to the on-the-wire bincode bytes or hex behavior is
treated as breaking. A `1.0` cut will follow once the format guarantees
above have a few stable releases of bake-in.

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for per-version notes; release tags live
at the [sentrix-labs/sentrix releases](https://github.com/sentrix-labs/sentrix/releases)
page (`sentrix-codec-v<version>`).

## Security

Report vulnerabilities privately to
**`security@sentrixchain.com`** — please **do not** file a public issue
for a suspected security bug. See
[`SECURITY.md`](https://github.com/sentrix-labs/sentrix/blob/main/SECURITY.md)
for the full disclosure policy and response timeline.

## Contributing

PRs and issues welcome. See the workspace
[`CONTRIBUTING.md`](https://github.com/sentrix-labs/sentrix/blob/main/CONTRIBUTING.md)
for branch / commit / clippy expectations. Reproduce a build locally:

```sh
git clone https://github.com/sentrix-labs/sentrix.git
cd sentrix
cargo test -p sentrix-codec
cargo clippy -p sentrix-codec --all-targets -- -D warnings
```

## License

Licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be
dual-licensed as above, without any additional terms or conditions.
