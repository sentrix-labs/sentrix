//! sentrix-codec — centralised encoding helpers.
//!
//! A small, stable wrapper around `bincode 1.3` and `hex 0.4` that gives
//! consumers a single import surface and one error type (`CodecError`) for
//! both serialization and hex conversion.
//!
//! # Design
//!
//! Bincode is pinned to 1.3 (the workspace stays on 1.x for now). A future
//! migration to bincode 2.x — which has a different API surface — would
//! happen here first with byte-equality tests so every consumer follows in
//! one step instead of a workspace-wide grep.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use serde::{Serialize, de::DeserializeOwned};

// ── bincode ──────────────────────────────────────────────────────────

/// Error type returned by every function in this crate. Implements
/// `std::error::Error` so it composes with `?` in any `Result` chain.
#[derive(Debug)]
pub enum CodecError {
    /// Serialization failed (returned by [`encode`]). The wrapped string
    /// is the underlying `bincode` error message — opaque to callers, fine
    /// for logging or surfacing to a user.
    Encode(String),
    /// Deserialization or hex-decoding failed (returned by [`decode`],
    /// [`hex_decode`], and [`hex_decode_fixed`]). The wrapped string is the
    /// underlying error message from `bincode` or `hex`.
    Decode(String),
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodecError::Encode(e) => write!(f, "codec encode error: {e}"),
            CodecError::Decode(e) => write!(f, "codec decode error: {e}"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Serialize a value to `Vec<u8>` using `bincode 1.3` default config
/// (little-endian, varint integers, no byte-length limit). Output is
/// byte-identical to a direct `bincode::serialize(val)` call.
pub fn encode<T: Serialize>(val: &T) -> Result<Vec<u8>, CodecError> {
    bincode::serialize(val).map_err(|e| CodecError::Encode(e.to_string()))
}

/// Deserialize a value from bytes using `bincode 1.3` default config.
/// Returns [`CodecError::Decode`] on malformed input or type mismatch.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    bincode::deserialize(bytes).map_err(|e| CodecError::Decode(e.to_string()))
}

// ── hex ──────────────────────────────────────────────────────────────

/// Hex-encode bytes as a lowercase string with no `0x` prefix. Matches
/// `hex::encode` exactly — kept here so consumers don't take a direct
/// dependency on the `hex` crate.
pub fn hex_encode<T: AsRef<[u8]>>(bytes: T) -> String {
    hex::encode(bytes)
}

/// Hex-decode a string into a `Vec<u8>`. Tolerates a leading `0x` prefix
/// (`"deadbeef"` and `"0xdeadbeef"` both decode to the same bytes).
/// Returns [`CodecError::Decode`] on non-hex characters or odd length.
pub fn hex_decode(s: &str) -> Result<Vec<u8>, CodecError> {
    let stripped = s.strip_prefix("0x").unwrap_or(s);
    hex::decode(stripped).map_err(|e| CodecError::Decode(e.to_string()))
}

/// Hex-decode a string into a fixed-size `[u8; N]`. Tolerates a leading
/// `0x` prefix and then requires exactly `2 * N` hex characters. Returns
/// [`CodecError::Decode`] on a length mismatch or non-hex input.
pub fn hex_decode_fixed<const N: usize>(s: &str) -> Result<[u8; N], CodecError> {
    let bytes = hex_decode(s)?;
    if bytes.len() != N {
        return Err(CodecError::Decode(format!(
            "expected {N} bytes, got {}",
            bytes.len()
        )));
    }
    let mut out = [0u8; N];
    out.copy_from_slice(&bytes);
    Ok(out)
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Fix {
        a: u64,
        b: String,
    }

    #[test]
    fn test_bincode_roundtrip() {
        let v = Fix {
            a: 42,
            b: "hello".into(),
        };
        let bytes = encode(&v).unwrap();
        let decoded: Fix = decode(&bytes).unwrap();
        assert_eq!(v, decoded);
    }

    #[test]
    fn test_bincode_decode_error_on_garbage() {
        let err: Result<Fix, _> = decode(&[0xff, 0xff, 0xff]);
        assert!(matches!(err, Err(CodecError::Decode(_))));
    }

    #[test]
    fn test_hex_encode_empty() {
        assert_eq!(hex_encode(&[] as &[u8]), "");
    }

    #[test]
    fn test_hex_encode_bytes() {
        assert_eq!(hex_encode([0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

    #[test]
    fn test_hex_decode_no_prefix() {
        assert_eq!(
            hex_decode("deadbeef").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn test_hex_decode_with_prefix() {
        assert_eq!(
            hex_decode("0xdeadbeef").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn test_hex_decode_invalid() {
        assert!(matches!(hex_decode("0xZZ"), Err(CodecError::Decode(_))));
        assert!(matches!(hex_decode("abc"), Err(CodecError::Decode(_)))); // odd length
    }

    #[test]
    fn test_hex_decode_fixed_ok() {
        let bytes: [u8; 4] = hex_decode_fixed("deadbeef").unwrap();
        assert_eq!(bytes, [0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn test_hex_decode_fixed_wrong_length() {
        let err: Result<[u8; 4], _> = hex_decode_fixed("deadbe");
        assert!(matches!(err, Err(CodecError::Decode(_))));
    }
}
