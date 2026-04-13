// keystore.rs - Sentrix

use aes_gcm::{Aes256Gcm, Key, Nonce, aead::{Aead, KeyInit}};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use argon2::{Argon2, Algorithm, Version, Params};
use serde::{Deserialize, Serialize};
use rand::RngCore;
use rand::rngs::OsRng;
use crate::wallet::wallet::Wallet;
use crate::types::error::{SentrixError, SentrixResult};

#[cfg(test)]
const PBKDF2_ITERATIONS: u32 = 600_000; // NIST SP 800-132 recommended minimum (v1, tests only)

// I-02: Argon2id parameters (v2) — memory-hard KDF
const ARGON2_M_COST: u32 = 65_536; // 64 MiB
const ARGON2_T_COST: u32 = 3;      // 3 iterations
const ARGON2_P_COST: u32 = 4;      // 4 parallel lanes

const SALT_SIZE: usize = 16;
const NONCE_SIZE: usize = 12;
const KEY_SIZE: usize = 32; // 256 bits

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keystore {
    pub version: u8,
    pub address: String,
    pub crypto: KeystoreCrypto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeystoreCrypto {
    pub cipher: String,
    pub kdf: String,
    // PBKDF2 param (v1 only; 0 for v2 — always serialized for backward compat)
    pub kdf_iterations: u32,
    // I-02: Argon2id params (v2 only; absent in v1 keystores)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argon2_m_cost: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argon2_t_cost: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argon2_p_cost: Option<u32>,
    pub salt: String,   // hex
    pub nonce: String,  // hex
    pub ciphertext: String, // hex
    pub mac: String,    // hex — SHA-256(key[16..32] + ciphertext)
}

impl Keystore {
    // I-02: Default encryption now uses Argon2id (version 2).
    // Old keystores (PBKDF2, version 1) can still be decrypted via decrypt().
    pub fn encrypt(wallet: &Wallet, password: &str) -> SentrixResult<Self> {
        let mut salt = [0u8; SALT_SIZE];
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut salt);
        OsRng.fill_bytes(&mut nonce_bytes);

        // I-02: Derive key using Argon2id
        let mut key_bytes = [0u8; KEY_SIZE];
        let params = Params::new(ARGON2_M_COST, ARGON2_T_COST, ARGON2_P_COST, Some(KEY_SIZE))
            .map_err(|e| SentrixError::KeystoreError(e.to_string()))?;
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        argon2.hash_password_into(password.as_bytes(), &salt, &mut key_bytes)
            .map_err(|e| SentrixError::KeystoreError(e.to_string()))?;

        let private_key_bytes = hex::decode(wallet.secret_key_hex())
            .map_err(|_| SentrixError::KeystoreError("invalid private key".to_string()))?;

        let cipher_key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(cipher_key);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, private_key_bytes.as_ref())
            .map_err(|e| SentrixError::KeystoreError(e.to_string()))?;

        use sha2::{Sha256 as Sha256Hasher, Digest};
        let mut mac_input = Vec::new();
        mac_input.extend_from_slice(&key_bytes[16..32]);
        mac_input.extend_from_slice(&ciphertext);
        let mac = Sha256Hasher::digest(&mac_input);

        Ok(Self {
            version: 2,
            address: wallet.address.clone(),
            crypto: KeystoreCrypto {
                cipher: "aes-256-gcm".to_string(),
                kdf: "argon2id".to_string(),
                kdf_iterations: 0, // unused for v2
                argon2_m_cost: Some(ARGON2_M_COST),
                argon2_t_cost: Some(ARGON2_T_COST),
                argon2_p_cost: Some(ARGON2_P_COST),
                salt: hex::encode(salt),
                nonce: hex::encode(nonce_bytes),
                ciphertext: hex::encode(ciphertext),
                mac: hex::encode(mac),
            },
        })
    }

    // Decrypt keystore — handles both v1 (PBKDF2) and v2 (Argon2id).
    pub fn decrypt(&self, password: &str) -> SentrixResult<Wallet> {
        let salt = hex::decode(&self.crypto.salt)
            .map_err(|_| SentrixError::KeystoreError("invalid salt".to_string()))?;
        let nonce_bytes = hex::decode(&self.crypto.nonce)
            .map_err(|_| SentrixError::KeystoreError("invalid nonce".to_string()))?;
        let ciphertext = hex::decode(&self.crypto.ciphertext)
            .map_err(|_| SentrixError::KeystoreError("invalid ciphertext".to_string()))?;

        // I-02: Select KDF based on stored kdf field
        let mut key_bytes = [0u8; KEY_SIZE];
        match self.crypto.kdf.as_str() {
            "argon2id" => {
                let m = self.crypto.argon2_m_cost.unwrap_or(ARGON2_M_COST);
                let t = self.crypto.argon2_t_cost.unwrap_or(ARGON2_T_COST);
                let p = self.crypto.argon2_p_cost.unwrap_or(ARGON2_P_COST);
                let params = Params::new(m, t, p, Some(KEY_SIZE))
                    .map_err(|e| SentrixError::KeystoreError(e.to_string()))?;
                let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
                argon2.hash_password_into(password.as_bytes(), &salt, &mut key_bytes)
                    .map_err(|e| SentrixError::KeystoreError(e.to_string()))?;
            }
            "pbkdf2-sha256" => {
                pbkdf2_hmac::<Sha256>(
                    password.as_bytes(),
                    &salt,
                    self.crypto.kdf_iterations,
                    &mut key_bytes,
                );
            }
            other => {
                return Err(SentrixError::KeystoreError(
                    format!("unsupported KDF: {}", other)
                ));
            }
        }

        // Verify MAC before decryption
        use sha2::{Sha256 as Sha256Hasher, Digest};
        let mut mac_input = Vec::new();
        mac_input.extend_from_slice(&key_bytes[16..32]);
        mac_input.extend_from_slice(&ciphertext);
        let computed_mac = hex::encode(Sha256Hasher::digest(&mac_input));
        if computed_mac != self.crypto.mac {
            return Err(SentrixError::WrongPassword);
        }

        // Decrypt
        let cipher_key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(cipher_key);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let private_key_bytes = cipher
            .decrypt(nonce, ciphertext.as_ref())
            .map_err(|_| SentrixError::WrongPassword)?;

        let private_key_hex = hex::encode(private_key_bytes);
        Wallet::from_private_key(&private_key_hex)
    }

    // I-02: Migrate a v1 (PBKDF2) keystore to v2 (Argon2id).
    // Decrypts with the password, then re-encrypts using Argon2id.
    // Returns self unchanged if already v2.
    pub fn migrate_to_argon2id(&self, password: &str) -> SentrixResult<Self> {
        if self.version >= 2 && self.crypto.kdf == "argon2id" {
            return Ok(self.clone()); // already migrated
        }
        let wallet = self.decrypt(password)?;
        Self::encrypt(&wallet, password)
    }

    // Save keystore to JSON file
    pub fn save(&self, path: &str) -> SentrixResult<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    // Load keystore from JSON file
    pub fn load(path: &str) -> SentrixResult<Self> {
        let json = std::fs::read_to_string(path)?;
        let keystore: Self = serde_json::from_str(&json)?;
        Ok(keystore)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallet::wallet::Wallet;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let wallet = Wallet::generate();
        let password = "test_password_123";

        let keystore = Keystore::encrypt(&wallet, password).unwrap();
        let decrypted = keystore.decrypt(password).unwrap();

        assert_eq!(wallet.address, decrypted.address);
        assert_eq!(wallet.public_key, decrypted.public_key);
        assert_eq!(wallet.secret_key_hex(), decrypted.secret_key_hex());
    }

    #[test]
    fn test_wrong_password_fails() {
        let wallet = Wallet::generate();
        let keystore = Keystore::encrypt(&wallet, "correct_password").unwrap();
        let result = keystore.decrypt("wrong_password");
        assert!(result.is_err());
    }

    #[test]
    fn test_keystore_has_address() {
        let wallet = Wallet::generate();
        let keystore = Keystore::encrypt(&wallet, "password").unwrap();
        assert_eq!(keystore.address, wallet.address);
    }

    #[test]
    fn test_keystore_serialization() {
        let wallet = Wallet::generate();
        let keystore = Keystore::encrypt(&wallet, "password").unwrap();
        let json = serde_json::to_string(&keystore).unwrap();
        let loaded: Keystore = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.address, wallet.address);
        assert_eq!(loaded.crypto.cipher, "aes-256-gcm");
        // I-02: default is now argon2id
        assert_eq!(loaded.crypto.kdf, "argon2id");
    }

    #[test]
    fn test_save_and_load() {
        let wallet = Wallet::generate();
        let password = "file_test_pw";
        let keystore = Keystore::encrypt(&wallet, password).unwrap();

        let tmp_path = std::env::temp_dir().join("sentrix_test_keystore.json");
        let path_str = tmp_path.to_str().unwrap();

        keystore.save(path_str).unwrap();
        let loaded = Keystore::load(path_str).unwrap();
        let decrypted = loaded.decrypt(password).unwrap();

        assert_eq!(wallet.address, decrypted.address);
        assert_eq!(wallet.secret_key_hex(), decrypted.secret_key_hex());

        // Cleanup
        let _ = std::fs::remove_file(path_str);
    }

    #[test]
    fn test_different_passwords_different_ciphertext() {
        let wallet = Wallet::generate();
        let ks1 = Keystore::encrypt(&wallet, "password1").unwrap();
        let ks2 = Keystore::encrypt(&wallet, "password2").unwrap();
        // Different salt/nonce means different ciphertext even for same key
        assert_ne!(ks1.crypto.ciphertext, ks2.crypto.ciphertext);
    }

    // ── I-02: Argon2id migration tests ───────────────────────

    #[test]
    fn test_i02_new_keystore_uses_argon2id() {
        let wallet = Wallet::generate();
        let ks = Keystore::encrypt(&wallet, "password").unwrap();
        assert_eq!(ks.version, 2, "new keystores must be version 2");
        assert_eq!(ks.crypto.kdf, "argon2id", "new keystores must use argon2id");
        assert_eq!(ks.crypto.argon2_m_cost, Some(ARGON2_M_COST));
        assert_eq!(ks.crypto.argon2_t_cost, Some(ARGON2_T_COST));
        assert_eq!(ks.crypto.argon2_p_cost, Some(ARGON2_P_COST));
    }

    #[test]
    fn test_i02_argon2id_decrypt_roundtrip() {
        let wallet = Wallet::generate();
        let password = "argon2_test_pw";
        let ks = Keystore::encrypt(&wallet, password).unwrap();
        assert_eq!(ks.crypto.kdf, "argon2id");

        let decrypted = ks.decrypt(password).unwrap();
        assert_eq!(wallet.address, decrypted.address);
        assert_eq!(wallet.secret_key_hex(), decrypted.secret_key_hex());
    }

    #[test]
    fn test_i02_pbkdf2_backward_compat() {
        // Simulate a v1 keystore (PBKDF2) and verify it still decrypts correctly
        let wallet = Wallet::generate();
        let password = "legacy_pw";

        // Build a v1 keystore manually using the old PBKDF2 path
        let mut salt = [0u8; SALT_SIZE];
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut salt);
        OsRng.fill_bytes(&mut nonce_bytes);

        let mut key_bytes = [0u8; KEY_SIZE];
        pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, PBKDF2_ITERATIONS, &mut key_bytes);

        let private_key_bytes = hex::decode(wallet.secret_key_hex()).unwrap();
        let cipher_key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(cipher_key);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher.encrypt(nonce, private_key_bytes.as_ref()).unwrap();

        use sha2::{Sha256 as Sha256Hasher, Digest};
        let mut mac_input = Vec::new();
        mac_input.extend_from_slice(&key_bytes[16..32]);
        mac_input.extend_from_slice(&ciphertext);
        let mac = hex::encode(Sha256Hasher::digest(&mac_input));

        let v1_ks = Keystore {
            version: 1,
            address: wallet.address.clone(),
            crypto: KeystoreCrypto {
                cipher: "aes-256-gcm".to_string(),
                kdf: "pbkdf2-sha256".to_string(),
                kdf_iterations: PBKDF2_ITERATIONS,
                argon2_m_cost: None,
                argon2_t_cost: None,
                argon2_p_cost: None,
                salt: hex::encode(salt),
                nonce: hex::encode(nonce_bytes),
                ciphertext: hex::encode(ciphertext),
                mac,
            },
        };

        // Must still decrypt correctly
        let decrypted = v1_ks.decrypt(password).unwrap();
        assert_eq!(wallet.address, decrypted.address);
        assert_eq!(wallet.secret_key_hex(), decrypted.secret_key_hex());
    }

    #[test]
    fn test_i02_migrate_to_argon2id_upgrades_v1() {
        // Build a v1 keystore and migrate it to v2
        let wallet = Wallet::generate();
        let password = "migrate_pw";

        // Create v1 keystore using PBKDF2
        let mut salt = [0u8; SALT_SIZE];
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut salt);
        OsRng.fill_bytes(&mut nonce_bytes);

        let mut key_bytes = [0u8; KEY_SIZE];
        pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, PBKDF2_ITERATIONS, &mut key_bytes);

        let private_key_bytes = hex::decode(wallet.secret_key_hex()).unwrap();
        let cipher_key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(cipher_key);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher.encrypt(nonce, private_key_bytes.as_ref()).unwrap();

        use sha2::{Sha256 as Sha256Hasher, Digest};
        let mut mac_input = Vec::new();
        mac_input.extend_from_slice(&key_bytes[16..32]);
        mac_input.extend_from_slice(&ciphertext);
        let mac = hex::encode(Sha256Hasher::digest(&mac_input));

        let v1_ks = Keystore {
            version: 1,
            address: wallet.address.clone(),
            crypto: KeystoreCrypto {
                cipher: "aes-256-gcm".to_string(),
                kdf: "pbkdf2-sha256".to_string(),
                kdf_iterations: PBKDF2_ITERATIONS,
                argon2_m_cost: None,
                argon2_t_cost: None,
                argon2_p_cost: None,
                salt: hex::encode(salt),
                nonce: hex::encode(nonce_bytes),
                ciphertext: hex::encode(ciphertext),
                mac,
            },
        };

        assert_eq!(v1_ks.version, 1);
        assert_eq!(v1_ks.crypto.kdf, "pbkdf2-sha256");

        // Migrate
        let v2_ks = v1_ks.migrate_to_argon2id(password).unwrap();
        assert_eq!(v2_ks.version, 2, "migrated keystore must be version 2");
        assert_eq!(v2_ks.crypto.kdf, "argon2id", "migrated keystore must use argon2id");
        assert_eq!(v2_ks.address, wallet.address);

        // Decrypting the migrated keystore must yield the same key
        let decrypted = v2_ks.decrypt(password).unwrap();
        assert_eq!(wallet.address, decrypted.address);
        assert_eq!(wallet.secret_key_hex(), decrypted.secret_key_hex());
    }

    #[test]
    fn test_i02_migrate_noop_for_v2() {
        // migrate_to_argon2id on a v2 keystore must return it unchanged
        let wallet = Wallet::generate();
        let password = "noop_pw";
        let v2_ks = Keystore::encrypt(&wallet, password).unwrap();
        assert_eq!(v2_ks.version, 2);

        let migrated = v2_ks.migrate_to_argon2id(password).unwrap();
        assert_eq!(migrated.version, 2);
        assert_eq!(migrated.crypto.kdf, "argon2id");
        assert_eq!(migrated.address, wallet.address);
    }

    #[test]
    fn test_i02_v1_deserialization_without_argon2_fields() {
        // A v1 JSON without argon2_* fields must deserialize successfully
        let json = r#"{
            "version": 1,
            "address": "0xdeadbeef",
            "crypto": {
                "cipher": "aes-256-gcm",
                "kdf": "pbkdf2-sha256",
                "kdf_iterations": 600000,
                "salt": "aabbccdd00112233445566778899aabb",
                "nonce": "001122334455667788990011",
                "ciphertext": "deadbeef",
                "mac": "cafebabe"
            }
        }"#;
        let ks: Keystore = serde_json::from_str(json).unwrap();
        assert_eq!(ks.version, 1);
        assert_eq!(ks.crypto.kdf, "pbkdf2-sha256");
        assert_eq!(ks.crypto.argon2_m_cost, None);
        assert_eq!(ks.crypto.argon2_t_cost, None);
        assert_eq!(ks.crypto.argon2_p_cost, None);
    }

    #[test]
    fn test_i02_unsupported_kdf_returns_error() {
        // A keystore with an unknown KDF must return an error, not panic
        let wallet = Wallet::generate();
        let mut ks = Keystore::encrypt(&wallet, "pw").unwrap();
        ks.crypto.kdf = "scrypt".to_string(); // unknown
        let result = ks.decrypt("pw");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unsupported KDF"), "Expected unsupported KDF error, got: {}", err);
    }
}
