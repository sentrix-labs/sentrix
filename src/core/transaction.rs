// transaction.rs - Sentrix Chain

use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use secp256k1::{Secp256k1, Message, PublicKey, SecretKey};
use secp256k1::ecdsa::Signature;
use crate::types::error::{SentrixError, SentrixResult};

pub const MIN_TX_FEE: u64 = 10_000; // 0.0001 SRX in sentri
pub const COINBASE_ADDRESS: &str = "COINBASE";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub txid: String,
    pub from_address: String,
    pub to_address: String,
    pub amount: u64,        // sentri
    pub fee: u64,           // sentri
    pub nonce: u64,
    pub data: String,
    pub timestamp: u64,     // unix timestamp seconds
    pub signature: String,  // hex encoded
    pub public_key: String, // hex encoded
}

impl Transaction {
    // Create and sign a new transaction
    pub fn new(
        from_address: String,
        to_address: String,
        amount: u64,
        fee: u64,
        nonce: u64,
        data: String,
        secret_key: &SecretKey,
        public_key: &PublicKey,
    ) -> SentrixResult<Self> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let public_key_hex = hex::encode(public_key.serialize_uncompressed());

        let mut tx = Self {
            txid: String::new(),
            from_address,
            to_address,
            amount,
            fee,
            nonce,
            data,
            timestamp,
            signature: String::new(),
            public_key: public_key_hex,
        };

        // Compute signing payload and sign
        let payload = tx.signing_payload();
        let secp = Secp256k1::signing_only();
        let msg = Self::payload_to_message(&payload)?;
        let sig = secp.sign_ecdsa(&msg, secret_key);
        tx.signature = hex::encode(sig.serialize_compact());
        tx.txid = tx.compute_txid();

        Ok(tx)
    }

    // Create coinbase transaction (block reward)
    pub fn new_coinbase(to_address: String, amount: u64, block_index: u64) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut tx = Self {
            txid: String::new(),
            from_address: COINBASE_ADDRESS.to_string(),
            to_address,
            amount,
            fee: 0,
            nonce: 0,
            data: format!("block_{}", block_index),
            timestamp,
            signature: String::new(),
            public_key: String::new(),
        };
        tx.txid = tx.compute_txid();
        tx
    }

    pub fn is_coinbase(&self) -> bool {
        self.from_address == COINBASE_ADDRESS
    }

    // Canonical signing payload — deterministic JSON, sorted keys
    pub fn signing_payload(&self) -> String {
        format!(
            r#"{{"amount":{},"data":"{}","fee":{},"from":"{}","nonce":{},"timestamp":{},"to":"{}"}}"#,
            self.amount,
            self.data,
            self.fee,
            self.from_address,
            self.nonce,
            self.timestamp,
            self.to_address,
        )
    }

    pub fn compute_txid(&self) -> String {
        let payload = self.signing_payload();
        let mut hasher = Sha256::new();
        hasher.update(payload.as_bytes());
        hex::encode(hasher.finalize())
    }

    fn payload_to_message(payload: &str) -> SentrixResult<Message> {
        let mut hasher = Sha256::new();
        hasher.update(payload.as_bytes());
        let hash = hasher.finalize();
        Message::from_digest_slice(&hash)
            .map_err(|e| SentrixError::InvalidTransaction(e.to_string()))
    }

    // Verify signature against the signing payload
    pub fn verify(&self) -> SentrixResult<()> {
        if self.is_coinbase() {
            return Ok(());
        }

        // Decode public key
        let pub_key_bytes = hex::decode(&self.public_key)
            .map_err(|_| SentrixError::InvalidSignature)?;
        let secp = Secp256k1::verification_only();
        let public_key = PublicKey::from_slice(&pub_key_bytes)
            .map_err(|_| SentrixError::InvalidSignature)?;

        // Decode signature
        let sig_bytes = hex::decode(&self.signature)
            .map_err(|_| SentrixError::InvalidSignature)?;
        let sig = Signature::from_compact(&sig_bytes)
            .map_err(|_| SentrixError::InvalidSignature)?;

        // Verify
        let payload = self.signing_payload();
        let msg = Self::payload_to_message(&payload)?;
        secp.verify_ecdsa(&msg, &sig, &public_key)
            .map_err(|_| SentrixError::InvalidSignature)?;

        Ok(())
    }

    pub fn validate(&self, expected_nonce: u64) -> SentrixResult<()> {
        if self.is_coinbase() {
            return Ok(());
        }

        // Check fee
        if self.fee < MIN_TX_FEE {
            return Err(SentrixError::InvalidTransaction(
                format!("fee {} below minimum {}", self.fee, MIN_TX_FEE)
            ));
        }

        // Check amount
        if self.amount == 0 {
            return Err(SentrixError::InvalidTransaction(
                "amount must be > 0".to_string()
            ));
        }

        // Check nonce
        if self.nonce != expected_nonce {
            return Err(SentrixError::InvalidNonce {
                expected: expected_nonce,
                got: self.nonce,
            });
        }

        // Verify signature
        self.verify()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::rand::rngs::OsRng;

    fn make_keypair() -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        secp.generate_keypair(&mut OsRng)
    }

    #[test]
    fn test_coinbase_transaction() {
        let tx = Transaction::new_coinbase("SRX_validator".to_string(), 100_000_000, 1);
        assert!(tx.is_coinbase());
        assert_eq!(tx.amount, 100_000_000);
        assert!(!tx.txid.is_empty());
    }

    #[test]
    fn test_sign_and_verify() {
        let (sk, pk) = make_keypair();
        let tx = Transaction::new(
            "SRX_alice".to_string(),
            "SRX_bob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            &sk,
            &pk,
        ).unwrap();

        assert!(tx.verify().is_ok());
        assert!(!tx.txid.is_empty());
        assert!(!tx.signature.is_empty());
    }

    #[test]
    fn test_validate_correct_nonce() {
        let (sk, pk) = make_keypair();
        let tx = Transaction::new(
            "SRX_alice".to_string(),
            "SRX_bob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            &sk,
            &pk,
        ).unwrap();
        assert!(tx.validate(0).is_ok());
    }

    #[test]
    fn test_validate_wrong_nonce() {
        let (sk, pk) = make_keypair();
        let tx = Transaction::new(
            "SRX_alice".to_string(),
            "SRX_bob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            &sk,
            &pk,
        ).unwrap();
        assert!(tx.validate(1).is_err());
    }

    #[test]
    fn test_validate_fee_too_low() {
        let (sk, pk) = make_keypair();
        let tx = Transaction::new(
            "SRX_alice".to_string(),
            "SRX_bob".to_string(),
            1_000_000,
            1, // below MIN_TX_FEE
            0,
            String::new(),
            &sk,
            &pk,
        ).unwrap();
        assert!(tx.validate(0).is_err());
    }

    #[test]
    fn test_tampered_signature_fails() {
        let (sk, pk) = make_keypair();
        let mut tx = Transaction::new(
            "SRX_alice".to_string(),
            "SRX_bob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            &sk,
            &pk,
        ).unwrap();

        // Tamper with amount after signing
        tx.amount = 999_999_999;
        assert!(tx.verify().is_err());
    }
}
