// address.rs — Ethereum-style address derivation from secp256k1 public key.

use secp256k1::PublicKey;
use sha3::{Digest, Keccak256};

/// Derive an Ethereum-compatible address (0x + 40 hex) from an uncompressed
/// secp256k1 public key. This is the Keccak-256 hash of the 64-byte public
/// key (without the 04 prefix), taking the last 20 bytes.
///
/// This function lives in primitives so it can be used by both the wallet
/// crate (Wallet::derive_address) and the transaction crate (signature
/// verification) without creating a circular dependency.
pub fn derive_address(public_key: &PublicKey) -> String {
    let pub_bytes = public_key.serialize_uncompressed();
    let mut hasher = Keccak256::new();
    hasher.update(&pub_bytes[1..]); // skip 04 prefix
    let hash = hasher.finalize();
    let address_bytes = &hash[12..]; // last 20 bytes
    format!("0x{}", hex::encode(address_bytes))
}
