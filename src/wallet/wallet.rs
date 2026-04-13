// wallet.rs - Sentrix

use secp256k1::{Secp256k1, SecretKey, PublicKey};
use secp256k1::rand::rngs::OsRng;
use sha3::Keccak256;
use sha3::Digest;
use zeroize::Zeroizing;
use crate::types::error::{SentrixError, SentrixResult};

// M-05 FIX: no Clone — prevents accidental secret key duplication in memory
// L-04 FIX: secret_key_bytes is Zeroizing<[u8; 32]> — auto-zeroes on drop, no heap String
#[derive(Debug)]
pub struct Wallet {
    pub address: String,         // 0x + 40 hex chars (Ethereum style)
    pub public_key: String,      // hex encoded uncompressed pubkey (65 bytes)
    secret_key_bytes: Zeroizing<[u8; 32]>,  // private — Zeroizing handles drop automatically
}

// No manual Drop needed — Zeroizing<T> zeroes memory automatically when dropped

impl Wallet {
    // Generate a new random wallet
    pub fn generate() -> Self {
        let secp = Secp256k1::new();
        let (secret_key, public_key) = secp.generate_keypair(&mut OsRng);
        Self::from_keypair(&secret_key, &public_key)
    }

    // Create wallet from existing keypair
    pub fn from_keypair(secret_key: &SecretKey, public_key: &PublicKey) -> Self {
        let address = Self::derive_address(public_key);
        let public_key_hex = hex::encode(public_key.serialize_uncompressed());

        Self {
            address,
            public_key: public_key_hex,
            secret_key_bytes: Zeroizing::new(secret_key.secret_bytes()),
        }
    }

    // Import from private key hex
    pub fn from_private_key(private_key_hex: &str) -> SentrixResult<Self> {
        let bytes = hex::decode(private_key_hex)
            .map_err(|_| SentrixError::InvalidPrivateKey)?;
        let secret_key = SecretKey::from_slice(&bytes)
            .map_err(|_| SentrixError::InvalidPrivateKey)?;
        let secp = Secp256k1::new();
        let public_key = PublicKey::from_secret_key(&secp, &secret_key);
        Ok(Self::from_keypair(&secret_key, &public_key))
    }

    // Ethereum-style address derivation:
    // 1. Take uncompressed public key (65 bytes, skip first byte 0x04)
    // 2. Keccak-256 hash the 64 bytes
    // 3. Take last 20 bytes
    // 4. Prefix with 0x
    pub fn derive_address(public_key: &PublicKey) -> String {
        let pub_bytes = public_key.serialize_uncompressed();
        // Skip the 0x04 prefix byte, hash the remaining 64 bytes
        let mut hasher = Keccak256::new();
        hasher.update(&pub_bytes[1..]);
        let hash = hasher.finalize();
        // Take last 20 bytes
        let address_bytes = &hash[12..];
        format!("0x{}", hex::encode(address_bytes))
    }

    // Returns the private key as a hex string (use sparingly — creates a heap copy)
    pub fn secret_key_hex(&self) -> String {
        hex::encode(&*self.secret_key_bytes)
    }

    pub fn get_secret_key(&self) -> SentrixResult<SecretKey> {
        SecretKey::from_slice(&*self.secret_key_bytes)
            .map_err(|_| SentrixError::InvalidPrivateKey)
    }

    pub fn get_public_key(&self) -> SentrixResult<PublicKey> {
        let bytes = hex::decode(&self.public_key)
            .map_err(|_| SentrixError::InvalidSignature)?;
        PublicKey::from_slice(&bytes)
            .map_err(|_| SentrixError::InvalidSignature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_wallet() {
        let wallet = Wallet::generate();
        assert!(wallet.address.starts_with("0x"));
        assert_eq!(wallet.address.len(), 42); // 0x + 40 hex chars
        assert!(!wallet.public_key.is_empty());
        assert!(!wallet.secret_key_hex().is_empty());
    }

    #[test]
    fn test_address_deterministic() {
        let wallet = Wallet::generate();
        let sk = wallet.get_secret_key().unwrap();
        let pk = wallet.get_public_key().unwrap();
        let wallet2 = Wallet::from_keypair(&sk, &pk);
        assert_eq!(wallet.address, wallet2.address);
    }

    #[test]
    fn test_import_from_private_key() {
        let wallet = Wallet::generate();
        let imported = Wallet::from_private_key(&wallet.secret_key_hex()).unwrap();
        assert_eq!(wallet.address, imported.address);
        assert_eq!(wallet.public_key, imported.public_key);
    }

    #[test]
    fn test_invalid_private_key() {
        let result = Wallet::from_private_key("not_valid_hex");
        assert!(result.is_err());
    }

    #[test]
    fn test_address_format() {
        let wallet = Wallet::generate();
        // Must be 0x + 40 lowercase hex chars
        assert!(wallet.address.starts_with("0x"));
        let hex_part = &wallet.address[2..];
        assert_eq!(hex_part.len(), 40);
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_two_wallets_different_addresses() {
        let w1 = Wallet::generate();
        let w2 = Wallet::generate();
        assert_ne!(w1.address, w2.address);
        assert_ne!(w1.secret_key_hex(), w2.secret_key_hex());
    }

    #[test]
    fn test_l04_secret_key_bytes_not_public() {
        // Verify secret_key_bytes field is private (compile-time guarantee).
        // secret_key_hex() method provides access; field is not directly accessible.
        let wallet = Wallet::generate();
        let hex = wallet.secret_key_hex();
        assert_eq!(hex.len(), 64); // 32 bytes = 64 hex chars
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_l04_zeroizing_roundtrip() {
        // Import from hex, verify secret key round-trips via get_secret_key()
        let wallet = Wallet::generate();
        let sk = wallet.get_secret_key().unwrap();
        let wallet2 = Wallet::from_keypair(&sk, &wallet.get_public_key().unwrap());
        assert_eq!(wallet.secret_key_hex(), wallet2.secret_key_hex());
        assert_eq!(wallet.address, wallet2.address);
    }
}
