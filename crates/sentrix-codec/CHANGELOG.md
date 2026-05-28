# Changelog

All notable changes to `sentrix-codec` are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and this crate adheres
to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Pre-`1.0`: minor bumps (`0.x.0`) may include breaking changes, patch bumps
(`0.x.y`) are non-breaking fixes. Any change to the on-the-wire bincode bytes
or hex behavior is treated as breaking.

## [Unreleased]

## [0.1.0] - 2026-05-28

### Added

- Initial release.
- `encode<T>` / `decode<T>` — `bincode 1.3` default-config wrappers that
  return a single error type (`CodecError`) instead of `bincode::Error`.
- `hex_encode` — lowercase hex with no `0x` prefix, matches `hex::encode`.
- `hex_decode` — tolerates a leading `0x` prefix; fails on non-hex chars
  or odd length.
- `hex_decode_fixed<const N: usize>` — `0x`-tolerant length-checked
  variant that returns `[u8; N]`.
- `CodecError` with `Encode(String)` and `Decode(String)` variants;
  implements `Display` and `std::error::Error`.
- Crate-level safety pins: `#![forbid(unsafe_code)]` + `#![deny(missing_docs)]`.
- Dual `MIT OR Apache-2.0` license.

[Unreleased]: https://github.com/sentrix-labs/sentrix/compare/sentrix-codec-v0.1.0...HEAD
[0.1.0]: https://github.com/sentrix-labs/sentrix/releases/tag/sentrix-codec-v0.1.0
