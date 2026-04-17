// evm/precompiles.rs — Precompiled contracts for Sentrix EVM
//
// Standard Ethereum precompiles (0x01-0x09) are provided by revm's EthPrecompiles.
// Sentrix-specific precompiles (0x100 staking, 0x101 slashing) will be added
// when DPoS staking is integrated with EVM execution.
//
// Standard precompiles included automatically:
//   0x01 ecRecover     — ECDSA public key recovery
//   0x02 SHA256        — SHA-256 hash
//   0x03 RIPEMD160     — RIPEMD-160 hash
//   0x04 identity      — Data copy (returns input as output)
//   0x05 modexp        — Modular exponentiation
//   0x06 ecAdd         — BN256 elliptic curve addition
//   0x07 ecMul         — BN256 elliptic curve scalar multiplication
//   0x08 ecPairing     — BN256 elliptic curve pairing check
//   0x09 blake2f       — BLAKE2 compression function F

use alloy_primitives::Address;

/// Sentrix staking precompile address (0x0100).
/// Allows smart contracts to interact with the DPoS staking system.
pub const STAKING_PRECOMPILE: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x00,
]);

/// Sentrix slashing evidence precompile address (0x0101).
/// Allows submitting double-sign evidence from smart contracts.
pub const SLASHING_PRECOMPILE: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x01,
]);

/// Check if an address is a Sentrix-specific precompile.
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
