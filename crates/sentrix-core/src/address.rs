//! Sentrix address format validation + protocol-reserved constants.
//!
//! Extracted from `crate::blockchain` so the validation helpers and the
//! handful of compiled-in protocol addresses live in one focused module.
//! All symbols are re-exported from `crate::blockchain::*` so existing
//! caller paths continue to resolve unchanged.

/// Sentrix addresses are 42-char hex strings (0x + 40 hex digits).
///
/// This is the only structural check — case is not normalised here
/// (callers that need a canonical form lowercase the string before
/// indexing). Use [`is_spendable_sentrix_address`] when the address
/// is the target of a value transfer to reject the burn sentinel
/// silently.
pub fn is_valid_sentrix_address(addr: &str) -> bool {
    addr.len() == 42 && addr.starts_with("0x") && addr[2..].chars().all(|c| c.is_ascii_hexdigit())
}

/// Canonical zero address. Used as a burn sink by `AuthorityManager`
/// and as an invalid target for value-bearing token operations (M-02).
pub const ZERO_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// Sentrix address that is valid-format AND not the burn sentinel. Use
/// this for value-bearing targets (token transfers, mints, SRX sends)
/// where the zero address would silently burn tokens without setting
/// the protocol's `total_burned` counter. [`is_valid_sentrix_address`]
/// alone remains the right guard for addresses that legitimately
/// include the zero sentinel (e.g. internal tracking fields).
pub fn is_spendable_sentrix_address(addr: &str) -> bool {
    is_valid_sentrix_address(addr) && addr != ZERO_ADDRESS
}

// ── Protocol-reserved addresses ──────────────────────────────────────
// The canonical premine allocations live in `genesis/mainnet.toml` and
// load via [`crate::Genesis`]. Only constants still referenced at
// runtime (outside of initialisation) live here.

/// Ecosystem Fund receives the ecosystem share of token-operation fees
/// (see `token_ops.rs`). Kept as a const because it is a compiled-in
/// protocol parameter, not just a premine recipient.
pub const ECOSYSTEM_FUND_ADDRESS: &str = "0xeb70fdefd00fdb768dec06c478f450c351499f14";

/// Total premine across all genesis allocations, in sentri units. An
/// economic invariant of the mainnet spec; surfaced for tests / tooling.
/// Drift against `genesis/mainnet.toml` is caught by
/// `test_total_premine_matches_hardcoded` in `genesis.rs`.
pub const TOTAL_PREMINE: u64 = 63_000_000 * 100_000_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_address_accepts_42_char_hex() {
        assert!(is_valid_sentrix_address(
            "0x0123456789abcdef0123456789ABCDEF01234567"
        ));
    }

    #[test]
    fn valid_address_rejects_wrong_length() {
        assert!(!is_valid_sentrix_address("0xabcd"));
        assert!(!is_valid_sentrix_address(
            "0x0123456789abcdef0123456789abcdef0123456789"
        ));
    }

    #[test]
    fn valid_address_rejects_missing_prefix() {
        assert!(!is_valid_sentrix_address(
            "0123456789abcdef0123456789abcdef01234567"
        ));
    }

    #[test]
    fn valid_address_rejects_non_hex() {
        assert!(!is_valid_sentrix_address(
            "0x0123456789ABCDEFGHIJ0123456789abcdef0123"
        ));
    }

    #[test]
    fn spendable_address_rejects_zero() {
        assert!(is_valid_sentrix_address(ZERO_ADDRESS));
        assert!(!is_spendable_sentrix_address(ZERO_ADDRESS));
    }

    #[test]
    fn spendable_accepts_real_address() {
        let real = "0x753f2f68829fbe76a0132295624f48b27ce2e2d9";
        assert!(is_valid_sentrix_address(real));
        assert!(is_spendable_sentrix_address(real));
    }

    #[test]
    fn protocol_constants_self_consistent() {
        assert_eq!(ZERO_ADDRESS.len(), 42);
        assert!(is_valid_sentrix_address(ECOSYSTEM_FUND_ADDRESS));
        assert!(is_spendable_sentrix_address(ECOSYSTEM_FUND_ADDRESS));
        assert_eq!(TOTAL_PREMINE, 63_000_000_u64 * 100_000_000);
    }
}
