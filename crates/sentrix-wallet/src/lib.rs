//! sentrix-wallet — Wallet, keystore encryption, and signing for Sentrix.
//!
//! Provides:
//! - `Wallet` — key generation, import, address derivation (Keccak-256)
//! - `Keystore` — AES-256-GCM encrypted keystore files (Argon2id v2 default, PBKDF2 v1 compat)

#![allow(missing_docs)]

pub mod keystore;
pub mod wallet;

pub use keystore::Keystore;
pub use wallet::Wallet;
