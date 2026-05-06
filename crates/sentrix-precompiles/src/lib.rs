//! sentrix-precompiles — Sentrix-specific EVM precompile addresses.
//!
//! Standard Ethereum precompiles (0x01-0x09) are provided by revm's
//! EthPrecompiles at the EVM execution layer. This crate defines the
//! Sentrix-reserved precompile addresses that chain-specific features
//! (DPoS staking, slashing evidence, ...) will be wired to when EVM
//! execution integrates with those subsystems.
//!
//! Extracted from `crates/sentrix-evm/src/precompiles.rs` during the
//! 45-crate split (Tier 2 per CRATE_SPLIT_PLAN.md). Isolated so a
//! future `sentrix-sdk` or governance tooling can reference the canonical
//! precompile addresses without pulling the whole `sentrix-evm` stack.
//!
//! **Consensus note:** the numeric addresses defined here are part of
//! Sentrix's contract surface post-Voyager-EVM activation. Smart
//! contracts that encode these addresses as constants rely on them being
//! stable. NEVER change an address — introduce a new one.
//!
//! Standard precompiles included automatically by revm:
//!   0x01 ecRecover     — ECDSA public key recovery
//!   0x02 SHA256        — SHA-256 hash
//!   0x03 RIPEMD160     — RIPEMD-160 hash
//!   0x04 identity      — Data copy (returns input as output)
//!   0x05 modexp        — Modular exponentiation
//!   0x06 ecAdd         — BN256 elliptic curve addition
//!   0x07 ecMul         — BN256 elliptic curve scalar multiplication
//!   0x08 ecPairing     — BN256 elliptic curve pairing check
//!   0x09 blake2f       — BLAKE2 compression function F

#![allow(missing_docs)]

use alloy_primitives::Address;

/// **Reserved** address for the future Sentrix staking precompile (0x0100).
///
/// Audit L7 (2026-05-06): the address is reserved + advertised, but
/// **no handler is wired into revm's `PrecompilesContext`**. A
/// contract that calls 0x0100 today goes through revm's normal CALL
/// flow → empty-code account → returns success with empty output. A
/// contract author who treats `is_sentrix_precompile(...) == true` as
/// proof a real handler exists will silently misinterpret that
/// empty-success as "stake recorded".
///
/// Before wiring an actual handler: gate behind a fork height
/// (`STAKING_PRECOMPILE_FORK_HEIGHT` or similar) so historical
/// chain.db state stays reproducible, and add a regression test that
/// CALLs to 0x0100 return data in the expected ABI shape.
pub const STAKING_PRECOMPILE: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x00,
]);

/// **Reserved** address for the future Sentrix slashing-evidence
/// precompile (0x0101). Same caveat as `STAKING_PRECOMPILE` — no
/// handler is currently wired into revm; a contract calling 0x0101
/// receives revm's default empty-success.
pub const SLASHING_PRECOMPILE: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x01,
]);

/// Check if an address matches a **reserved** Sentrix precompile slot.
///
/// Returning `true` here does NOT mean a handler exists. As of
/// v2.1.80 no handler is wired into revm for either reserved address;
/// a contract calling 0x0100 or 0x0101 receives revm's default
/// empty-success. This helper exists so the future wire-up has a
/// canonical address-set to gate on — do not treat it as proof that
/// the call executed precompile logic.
pub fn is_sentrix_precompile(address: &Address) -> bool {
    *address == STAKING_PRECOMPILE || *address == SLASHING_PRECOMPILE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_precompile_addresses() {
        assert_eq!(
            STAKING_PRECOMPILE,
            Address::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0])
        );
        assert_eq!(
            SLASHING_PRECOMPILE,
            Address::from([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1])
        );
    }

    #[test]
    fn test_is_sentrix_precompile() {
        assert!(is_sentrix_precompile(&STAKING_PRECOMPILE));
        assert!(is_sentrix_precompile(&SLASHING_PRECOMPILE));
        assert!(!is_sentrix_precompile(&Address::ZERO));
        assert!(!is_sentrix_precompile(&Address::from([0x01u8; 20])));
    }
}
