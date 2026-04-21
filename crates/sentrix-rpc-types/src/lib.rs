//! sentrix-rpc-types — ETH ↔ Sentrix JSON-RPC type conversions + hex /
//! address validation helpers.
//!
//! Extracted from `crates/sentrix-rpc/src/jsonrpc/helpers.rs` during the
//! 45-crate split (see `founder-private/architecture/CRATE_SPLIT_PLAN.md`).
//! Lives outside `sentrix-rpc` so a future `sentrix-sdk` (JSON-RPC client)
//! can depend on the type conversions without pulling the axum/tokio
//! server stack.
//!
//! All helpers here are PURE — they operate on strings and primitive
//! numeric types, not on `Blockchain` or any node-state reference. Any
//! helper that needs node state (e.g. `resolve_block_tag`, `log_matches`,
//! `load_logs_for_tx`) stays in the consuming crate.

#![allow(missing_docs)]

use serde_json::Value;

/// Format a `u64` as an Ethereum JSON-RPC hex string: `"0x<lower-hex>"`.
///
/// Matches `web3.toHex` / `ethers.js utils.hexValue` behaviour. No leading
/// zeros beyond a single `0` for zero: `0x0`, not `0x00`.
pub fn to_hex(n: u64) -> String {
    format!("0x{:x}", n)
}

/// Format a `u128` as an Ethereum JSON-RPC hex string. Same format as
/// [`to_hex`] but for wider integers (wei balances, gas prices).
pub fn to_hex_u128(n: u128) -> String {
    format!("0x{:x}", n)
}

/// M-11: validate a JSON-RPC address parameter before it is used as a
/// trie/DB lookup key. Accepts exactly `0x` + 40 hex lowercase and
/// returns the normalised string, or `Err` with an error message suitable
/// for JSON-RPC -32602 (Invalid params). Prevents oddly-shaped strings
/// (empty, too-long, non-hex) from reaching the account store where
/// they are merely a silent miss, wasting compute per malformed request
/// under adversarial load.
pub fn normalize_rpc_address(s: &str) -> Result<String, &'static str> {
    if s.len() != 42 {
        return Err("address must be 42 chars (0x + 40 hex)");
    }
    let lower = s.to_lowercase();
    if !lower.starts_with("0x") {
        return Err("address must start with 0x");
    }
    if !lower[2..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("address must be lowercase hex after 0x");
    }
    Ok(lower)
}

/// M-11: validate a JSON-RPC 32-byte hash parameter (tx hash, block
/// hash). Same rationale as [`normalize_rpc_address`] — keeps malformed
/// hex out of DB lookups.
pub fn normalize_rpc_hash(s: &str) -> Result<String, &'static str> {
    let stripped = s.strip_prefix("0x").unwrap_or(s);
    if stripped.len() != 64 {
        return Err("hash must be 32 bytes (64 hex chars)");
    }
    if !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("hash must be hex");
    }
    Ok(stripped.to_lowercase())
}

/// Accept either a `String` (hex-encoded) or a JSON `Number` and return
/// it as `u64`. Used for the many JSON-RPC parameters that accept both
/// `"0x..."` hex strings AND plain numbers (`block tag`, gas limits).
pub fn parse_hex_u64(v: &Value) -> Option<u64> {
    match v {
        Value::String(s) => {
            let s = s.trim_start_matches("0x");
            u64::from_str_radix(s, 16).ok()
        }
        Value::Number(n) => n.as_u64(),
        _ => None,
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_to_hex_zero() {
        assert_eq!(to_hex(0), "0x0");
    }

    #[test]
    fn test_to_hex_nonzero() {
        assert_eq!(to_hex(255), "0xff");
        assert_eq!(to_hex(42), "0x2a");
    }

    #[test]
    fn test_to_hex_u128() {
        assert_eq!(
            to_hex_u128(u128::from(u64::MAX) + 1),
            "0x10000000000000000"
        );
    }

    #[test]
    fn test_normalize_address_valid() {
        let s = "0x4f3319a747fd564136209cd5d9e7d1a1e4d142be";
        assert_eq!(normalize_rpc_address(s).unwrap(), s);
    }

    #[test]
    fn test_normalize_address_uppercase_lowered() {
        let got = normalize_rpc_address("0x4F3319A747FD564136209CD5D9E7D1A1E4D142BE").unwrap();
        assert_eq!(got, "0x4f3319a747fd564136209cd5d9e7d1a1e4d142be");
    }

    #[test]
    fn test_normalize_address_too_short() {
        assert!(normalize_rpc_address("0x123").is_err());
    }

    #[test]
    fn test_normalize_address_no_prefix() {
        assert!(normalize_rpc_address("4f3319a747fd564136209cd5d9e7d1a1e4d142be").is_err());
    }

    #[test]
    fn test_normalize_address_bad_hex() {
        assert!(normalize_rpc_address("0xZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ").is_err());
    }

    #[test]
    fn test_normalize_hash_valid() {
        let s = "0x".to_string() + &"a".repeat(64);
        assert_eq!(normalize_rpc_hash(&s).unwrap(), "a".repeat(64));
    }

    #[test]
    fn test_normalize_hash_no_prefix() {
        // Stripping 0x is optional on input; output is always lowercase hex without prefix
        let s = "A".repeat(64);
        assert_eq!(normalize_rpc_hash(&s).unwrap(), "a".repeat(64));
    }

    #[test]
    fn test_normalize_hash_wrong_length() {
        assert!(normalize_rpc_hash("0xabcd").is_err());
    }

    #[test]
    fn test_parse_hex_u64_string() {
        assert_eq!(parse_hex_u64(&json!("0x2a")), Some(42));
        assert_eq!(parse_hex_u64(&json!("2a")), Some(42)); // prefix optional
    }

    #[test]
    fn test_parse_hex_u64_number() {
        assert_eq!(parse_hex_u64(&json!(42)), Some(42));
    }

    #[test]
    fn test_parse_hex_u64_invalid() {
        assert!(parse_hex_u64(&json!("0xZZ")).is_none());
        assert!(parse_hex_u64(&json!(null)).is_none());
    }
}
