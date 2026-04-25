//! sentrix-codec — centralised encoding helpers for Sentrix.
//!
//! Why this crate exists:
//!   - 8 files across the workspace call `bincode::serialize` /
//!     `bincode::deserialize` directly. If we ever need to change bincode
//!     config (e.g. bincode 1.x → 2.x migration, endianness, size limit),
//!     every call site needs to be found and updated.
//!   - `hex::encode` / `hex::decode` is used even more widely.
//!
//! This crate is the single chokepoint. Future format migrations edit
//! ONE file (this one) instead of scanning the whole workspace.
//!
//! Extracted during the 45-crate split, Tier 1 item #2. See
//! `internal design doc`.
//!
//! # Design
//!
//! We stay with bincode 1.3 for now (matches the existing workspace
//! pin). Migration to bincode 2.x (which has a different API surface)
//! would happen here first with unit-tests confirming byte-identical
//! output for the fixed serialization formats. Not part of this PR.

#![allow(missing_docs)]

use serde::{Serialize, de::DeserializeOwned};

// ── bincode ──────────────────────────────────────────────────────────

/// Error type returned by this crate's encode/decode helpers.
/// Wraps `bincode::Error` but doesn't leak the underlying crate, so
/// callers can match on `CodecError::*` without a bincode dep themselves.
#[derive(Debug)]
pub enum CodecError {
    Encode(String),
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

/// Serialize a value to `Vec<u8>` using bincode 1.3 default config
/// (little-endian, varint ints, no byte limit). Matches the behaviour
/// of `bincode::serialize(..)` already in use across the workspace —
/// this is a direct wrapper, not a new format.
pub fn encode<T: Serialize>(val: &T) -> Result<Vec<u8>, CodecError> {
    bincode::serialize(val).map_err(|e| CodecError::Encode(e.to_string()))
}

/// Deserialize a value from bytes using bincode 1.3 default config.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    bincode::deserialize(bytes).map_err(|e| CodecError::Decode(e.to_string()))
}

// ── hex ──────────────────────────────────────────────────────────────

/// Hex-encode bytes as lowercase string (no `0x` prefix — matches the
/// existing `hex::encode` behaviour that the workspace expects).
pub fn hex_encode<T: AsRef<[u8]>>(bytes: T) -> String {
    hex::encode(bytes)
}

/// Hex-decode a string (tolerates a leading `0x` prefix). Returns
/// `CodecError::Decode` on invalid hex or odd length.
pub fn hex_decode(s: &str) -> Result<Vec<u8>, CodecError> {
    let stripped = s.strip_prefix("0x").unwrap_or(s);
    hex::decode(stripped).map_err(|e| CodecError::Decode(e.to_string()))
}

/// Hex-decode into a fixed-size byte array. Errors if the input isn't
/// exactly `N` bytes (2*N hex chars after optional `0x` prefix).
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
        assert_eq!(hex_decode("deadbeef").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn test_hex_decode_with_prefix() {
        assert_eq!(hex_decode("0xdeadbeef").unwrap(), vec![0xde, 0xad, 0xbe, 0xef]);
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
