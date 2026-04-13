// trie/address.rs - Sentrix — Address ↔ trie key conversions

use sha2::{Sha256, Digest};
use crate::core::trie::node::NodeHash;

/// Convert a Sentrix address string (e.g. "0x...") to a 32-byte trie key.
/// Uses SHA-256 of the address bytes to spread keys uniformly across the 256-level tree.
pub fn address_to_key(address: &str) -> NodeHash {
    let mut h = Sha256::new();
    h.update(address.as_bytes());
    h.finalize().into()
}

/// Encode account state (balance, nonce) as 16 raw bytes for trie value storage.
/// Layout: [balance: 8 bytes BE] [nonce: 8 bytes BE]
pub fn account_value_bytes(balance: u64, nonce: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(16);
    v.extend_from_slice(&balance.to_be_bytes());
    v.extend_from_slice(&nonce.to_be_bytes());
    v
}

/// Decode account state from trie value bytes.
/// Returns (balance, nonce) or None if the byte slice is shorter than 16.
pub fn account_value_decode(bytes: &[u8]) -> Option<(u64, u64)> {
    if bytes.len() < 16 {
        return None;
    }
    let balance = u64::from_be_bytes(bytes[0..8].try_into().ok()?);
    let nonce   = u64::from_be_bytes(bytes[8..16].try_into().ok()?);
    Some((balance, nonce))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_address_to_key_deterministic() {
        let addr = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(address_to_key(addr), address_to_key(addr));
    }

    #[test]
    fn test_address_to_key_different_addresses() {
        let a = address_to_key("0xaaaa");
        let b = address_to_key("0xbbbb");
        assert_ne!(a, b);
    }

    #[test]
    fn test_account_value_roundtrip() {
        let balance = 1_234_567_890u64;
        let nonce = 42u64;
        let encoded = account_value_bytes(balance, nonce);
        assert_eq!(encoded.len(), 16);
        let (b2, n2) = account_value_decode(&encoded).unwrap();
        assert_eq!(b2, balance);
        assert_eq!(n2, nonce);
    }

    #[test]
    fn test_account_value_decode_short_returns_none() {
        assert!(account_value_decode(&[0u8; 8]).is_none());
        assert!(account_value_decode(&[]).is_none());
    }
}
