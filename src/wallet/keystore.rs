// keystore.rs - Sentrix

use aes_gcm::{Aes256Gcm, Key, Nonce, aead::{Aead, KeyInit}};
use pbkdf2::pbkdf2_hmac;
use sha2::Sha256;
use serde::{Deserialize, Serialize};
use rand::RngCore;
use rand::rngs::OsRng;
use crate::wallet::wallet::Wallet;
use crate::types::error::{SentrixError, SentrixResult};

const PBKDF2_ITERATIONS: u32 = 600_000; // NIST SP 800-132 recommended minimum
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
    pub kdf_iterations: u32,
    pub salt: String,   // hex
    pub nonce: String,  // hex
    pub ciphertext: String, // hex
    pub mac: String,    // hex — SHA-256(key[16..32] + ciphertext)
}

impl Keystore {
    // Encrypt wallet private key with password
    pub fn encrypt(wallet: &Wallet, password: &str) -> SentrixResult<Self> {
        // Generate random salt and nonce
        let mut salt = [0u8; SALT_SIZE];
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        OsRng.fill_bytes(&mut salt);
        OsRng.fill_bytes(&mut nonce_bytes);

        // Derive key from password using PBKDF2-SHA256
        let mut key_bytes = [0u8; KEY_SIZE];
        pbkdf2_hmac::<Sha256>(
            password.as_bytes(),
            &salt,
            PBKDF2_ITERATIONS,
            &mut key_bytes,
        );

        // Encrypt private key using AES-256-GCM
        let private_key_bytes = hex::decode(wallet.secret_key_hex())
            .map_err(|_| SentrixError::KeystoreError("invalid private key".to_string()))?;

        let cipher_key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        let cipher = Aes256Gcm::new(cipher_key);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, private_key_bytes.as_ref())
            .map_err(|e| SentrixError::KeystoreError(e.to_string()))?;

        // Compute MAC: SHA-256(key[16..32] + ciphertext)
        use sha2::{Sha256 as Sha256Hasher, Digest};
        let mut mac_input = Vec::new();
        mac_input.extend_from_slice(&key_bytes[16..32]);
        mac_input.extend_from_slice(&ciphertext);
        let mac = Sha256Hasher::digest(&mac_input);

        Ok(Self {
            version: 1,
            address: wallet.address.clone(),
            crypto: KeystoreCrypto {
                cipher: "aes-256-gcm".to_string(),
                kdf: "pbkdf2-sha256".to_string(),
                kdf_iterations: PBKDF2_ITERATIONS,
                salt: hex::encode(salt),
                nonce: hex::encode(nonce_bytes),
                ciphertext: hex::encode(ciphertext),
                mac: hex::encode(mac),
            },
        })
    }

    // Decrypt keystore with password, return Wallet
    pub fn decrypt(&self, password: &str) -> SentrixResult<Wallet> {
        // Decode stored values
        let salt = hex::decode(&self.crypto.salt)
            .map_err(|_| SentrixError::KeystoreError("invalid salt".to_string()))?;
        let nonce_bytes = hex::decode(&self.crypto.nonce)
            .map_err(|_| SentrixError::KeystoreError("invalid nonce".to_string()))?;
        let ciphertext = hex::decode(&self.crypto.ciphertext)
            .map_err(|_| SentrixError::KeystoreError("invalid ciphertext".to_string()))?;

        // Re-derive key from password
        let mut key_bytes = [0u8; KEY_SIZE];
        pbkdf2_hmac::<Sha256>(
            password.as_bytes(),
            &salt,
            self.crypto.kdf_iterations,
            &mut key_bytes,
        );

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
        assert_eq!(loaded.crypto.kdf, "pbkdf2-sha256");
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
}
