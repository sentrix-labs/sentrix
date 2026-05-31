// trie/address.rs - Sentrix — Address ↔ trie key conversions

use crate::node::NodeHash;
use sha2::{Digest, Sha256};

/// Convert a Sentrix address string (e.g. "0x...") to a 32-byte trie key.
///
/// Normalisation (T-A / T-C):
/// - Strips the "0x" prefix (case-insensitive lookup: "0xDEAD" == "0xdead")
/// - Lowercases the remaining hex digits
/// - Hex-decodes to raw bytes (20 bytes for standard addresses)
/// - Falls back to the UTF-8 bytes of the stripped string for non-hex inputs
/// - SHA-256 of the raw bytes → uniform 32-byte trie key
pub fn address_to_key(address: &str) -> NodeHash {
    let addr = address.trim_start_matches("0x").to_lowercase();
    let bytes = hex::decode(&addr).unwrap_or_else(|_| addr.as_bytes().to_vec());
    let mut h = Sha256::new();
    h.update(&bytes);
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
    let nonce = u64::from_be_bytes(bytes[8..16].try_into().ok()?);
    Some((balance, nonce))
}

/// SIP-6 Bug A — trie key for a validator's `pending_rewards` accumulator.
///
/// Distinct domain from `address_to_key` via a fixed prefix
/// (`"sentrix/v1/pending_rewards/"`) so this namespace cannot collide
/// with account-balance keys regardless of SHA-256 input length.
///
/// Post-`STATE_IN_TRIE_HEIGHT` the apply path writes each validator's
/// `pending_rewards` to this key before state_root is taken, so any
/// drift surfaces immediately as a state_root mismatch instead of
/// silently diverging on the off-trie HashMap.
pub fn validator_pending_rewards_key(address: &str) -> NodeHash {
    const DOMAIN: &[u8] = b"sentrix/v1/pending_rewards/";
    let addr = address.trim_start_matches("0x").to_lowercase();
    let bytes = hex::decode(&addr).unwrap_or_else(|_| addr.as_bytes().to_vec());
    let mut h = Sha256::new();
    h.update(DOMAIN);
    h.update(&bytes);
    h.finalize().into()
}

/// Encode a `pending_rewards` value (u64 sentri) as 8 big-endian bytes
/// for trie value storage.
pub fn pending_rewards_value_bytes(rewards: u64) -> [u8; 8] {
    rewards.to_be_bytes()
}

/// Decode trie value bytes produced by [`pending_rewards_value_bytes`].
/// Returns `None` if the slice is shorter than 8 bytes.
pub fn pending_rewards_value_decode(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < 8 {
        return None;
    }
    Some(u64::from_be_bytes(bytes[0..8].try_into().ok()?))
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

    /// T-A: uppercase and lowercase hex addresses must map to the same trie key.
    #[test]
    fn test_address_to_key_case_insensitive() {
        let lower = address_to_key("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
        let upper = address_to_key("0xDEADBEEFDEADBEEFDEADBEEFDEADBEEFDEADBEEF");
        let mixed = address_to_key("0xDeAdBeEfDeAdBeEfDeAdBeEfDeAdBeEfDeAdBeEf");
        assert_eq!(
            lower, upper,
            "lowercase and uppercase address must yield same trie key"
        );
        assert_eq!(lower, mixed, "mixed-case address must yield same trie key");
    }

    /// T-C: address with and without "0x" prefix must map to the same trie key.
    #[test]
    fn test_address_to_key_strips_0x_prefix() {
        let with_prefix = address_to_key("0xdeadbeef");
        let without_prefix = address_to_key("deadbeef");
        assert_eq!(
            with_prefix, without_prefix,
            "0x prefix must be stripped before hashing"
        );
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

    // ── SIP-6 pending_rewards trie key tests ────────────────────────

    #[test]
    fn test_pending_rewards_key_deterministic() {
        let addr = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        assert_eq!(
            validator_pending_rewards_key(addr),
            validator_pending_rewards_key(addr)
        );
    }

    #[test]
    fn test_pending_rewards_key_case_insensitive() {
        let a = validator_pending_rewards_key("0xDEADBEEF");
        let b = validator_pending_rewards_key("0xdeadbeef");
        assert_eq!(a, b);
    }

    /// Domain separation guarantee: pending_rewards key for an address
    /// MUST differ from the balance trie key for the SAME address.
    /// Otherwise a balance write would clobber pending_rewards (and
    /// vice versa) at the same trie slot — silent state corruption.
    #[test]
    fn test_pending_rewards_key_distinct_from_balance_key() {
        let addr = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        let bal_key = address_to_key(addr);
        let rew_key = validator_pending_rewards_key(addr);
        assert_ne!(
            bal_key, rew_key,
            "pending_rewards and balance trie keys must be distinct \
             for the same address — otherwise writes collide"
        );
    }

    #[test]
    fn test_pending_rewards_value_roundtrip() {
        let rewards = 1_234_567_890u64;
        let encoded = pending_rewards_value_bytes(rewards);
        assert_eq!(encoded.len(), 8);
        assert_eq!(pending_rewards_value_decode(&encoded), Some(rewards));
    }

    #[test]
    fn test_pending_rewards_value_decode_short_returns_none() {
        assert!(pending_rewards_value_decode(&[0u8; 4]).is_none());
        assert!(pending_rewards_value_decode(&[]).is_none());
    }
}
