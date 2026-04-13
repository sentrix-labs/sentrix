// transaction.rs - Sentrix

use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use secp256k1::{Secp256k1, Message, PublicKey, SecretKey};
use secp256k1::ecdsa::Signature;
use crate::types::error::{SentrixError, SentrixResult};

pub const MIN_TX_FEE: u64 = 10_000; // 0.0001 SRX in sentri
pub const COINBASE_ADDRESS: &str = "COINBASE";
pub const TOKEN_OP_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

// ── Token operation types (encoded in Transaction.data field) ──
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TokenOp {
    Deploy { name: String, symbol: String, decimals: u8, supply: u64 },
    Transfer { contract: String, to: String, amount: u64 },
    Burn { contract: String, amount: u64 },
    Mint { contract: String, to: String, amount: u64 },
    Approve { contract: String, spender: String, amount: u64 },
}

impl TokenOp {
    pub fn encode(&self) -> SentrixResult<String> {
        serde_json::to_string(self)
            .map_err(|e| SentrixError::InvalidTransaction(e.to_string()))
    }

    pub fn decode(data: &str) -> Option<Self> {
        serde_json::from_str(data).ok()
    }

    pub fn is_token_op(data: &str) -> bool {
        data.contains("\"op\":")
    }
}

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
    pub chain_id: u64,      // replay protection across chains
    pub signature: String,  // hex encoded
    pub public_key: String, // hex encoded
}

impl Transaction {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        from_address: String,
        to_address: String,
        amount: u64,
        fee: u64,
        nonce: u64,
        data: String,
        chain_id: u64,
        secret_key: &SecretKey,
        public_key: &PublicKey,
    ) -> SentrixResult<Self> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
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
            chain_id,
            signature: String::new(),
            public_key: public_key_hex,
        };

        let payload = tx.signing_payload();
        let secp = Secp256k1::signing_only();
        let msg = Self::payload_to_message(&payload)?;
        let sig = secp.sign_ecdsa(&msg, secret_key);
        tx.signature = hex::encode(sig.serialize_compact());
        tx.txid = tx.compute_txid();

        Ok(tx)
    }

    pub fn new_coinbase(to_address: String, amount: u64, block_index: u64) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
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
            chain_id: 0,
            signature: String::new(),
            public_key: String::new(),
        };
        tx.txid = tx.compute_txid();
        tx
    }

    pub fn is_coinbase(&self) -> bool {
        self.from_address == COINBASE_ADDRESS
    }

    // H-01 FIX: Canonical signing payload using BTreeMap for deterministic key ordering
    // and serde_json for proper escaping of special characters
    pub fn signing_payload(&self) -> String {
        let mut map = std::collections::BTreeMap::new();
        map.insert("amount", serde_json::Value::from(self.amount));
        map.insert("chain_id", serde_json::Value::from(self.chain_id));
        map.insert("data", serde_json::Value::from(self.data.as_str()));
        map.insert("fee", serde_json::Value::from(self.fee));
        map.insert("from", serde_json::Value::from(self.from_address.as_str()));
        map.insert("nonce", serde_json::Value::from(self.nonce));
        map.insert("timestamp", serde_json::Value::from(self.timestamp));
        map.insert("to", serde_json::Value::from(self.to_address.as_str()));
        // M-01 FIX: unwrap → unwrap_or_else to avoid panic in production path
        serde_json::to_string(&map).unwrap_or_else(|_| String::from("{}"))
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

    pub fn verify(&self) -> SentrixResult<()> {
        if self.is_coinbase() {
            // H-03 FIX: Coinbase must have empty signature and public_key
            if !self.signature.is_empty() || !self.public_key.is_empty() {
                return Err(SentrixError::InvalidTransaction(
                    "coinbase transaction must not have signature or public_key".to_string()
                ));
            }
            return Ok(());
        }

        let pub_key_bytes = hex::decode(&self.public_key)
            .map_err(|_| SentrixError::InvalidSignature)?;
        let secp = Secp256k1::verification_only();
        let public_key = PublicKey::from_slice(&pub_key_bytes)
            .map_err(|_| SentrixError::InvalidSignature)?;

        // C-01 FIX: Verify public key maps to from_address
        let derived_address = crate::wallet::wallet::Wallet::derive_address(&public_key);
        if derived_address != self.from_address {
            return Err(SentrixError::InvalidTransaction(
                format!("public key does not match from_address: expected {}, derived {}",
                        self.from_address, derived_address)
            ));
        }

        let sig_bytes = hex::decode(&self.signature)
            .map_err(|_| SentrixError::InvalidSignature)?;
        let sig = Signature::from_compact(&sig_bytes)
            .map_err(|_| SentrixError::InvalidSignature)?;

        let payload = self.signing_payload();
        let msg = Self::payload_to_message(&payload)?;
        secp.verify_ecdsa(&msg, &sig, &public_key)
            .map_err(|_| SentrixError::InvalidSignature)?;

        Ok(())
    }

    pub fn validate(&self, expected_nonce: u64, expected_chain_id: u64) -> SentrixResult<()> {
        if self.is_coinbase() {
            return Ok(());
        }

        if self.fee < MIN_TX_FEE {
            return Err(SentrixError::InvalidTransaction(
                format!("fee {} below minimum {}", self.fee, MIN_TX_FEE)
            ));
        }

        // amount=0 is allowed for token operations (data field carries the op)
        if self.amount == 0 && !TokenOp::is_token_op(&self.data) {
            return Err(SentrixError::InvalidTransaction(
                "amount must be > 0 (unless token operation)".to_string()
            ));
        }

        if self.nonce != expected_nonce {
            return Err(SentrixError::InvalidNonce {
                expected: expected_nonce,
                got: self.nonce,
            });
        }

        // Chain ID replay protection
        if self.chain_id != expected_chain_id {
            return Err(SentrixError::InvalidTransaction(
                format!("wrong chain_id: expected {}, got {}", expected_chain_id, self.chain_id)
            ));
        }

        self.verify()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secp256k1::rand::rngs::OsRng;

    const TEST_CHAIN_ID: u64 = 7119;

    fn make_keypair() -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        secp.generate_keypair(&mut OsRng)
    }

    fn derive_addr(pk: &PublicKey) -> String {
        crate::wallet::wallet::Wallet::derive_address(pk)
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
        let from = derive_addr(&pk);
        let tx = Transaction::new(
            from, "SRX_bob".to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), TEST_CHAIN_ID, &sk, &pk,
        ).unwrap();
        assert!(tx.verify().is_ok());
        assert!(!tx.txid.is_empty());
        assert!(!tx.signature.is_empty());
    }

    #[test]
    fn test_validate_correct_nonce() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);
        let tx = Transaction::new(
            from, "SRX_bob".to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), TEST_CHAIN_ID, &sk, &pk,
        ).unwrap();
        assert!(tx.validate(0, TEST_CHAIN_ID).is_ok());
    }

    #[test]
    fn test_validate_wrong_nonce() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);
        let tx = Transaction::new(
            from, "SRX_bob".to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), TEST_CHAIN_ID, &sk, &pk,
        ).unwrap();
        assert!(tx.validate(1, TEST_CHAIN_ID).is_err());
    }

    #[test]
    fn test_validate_wrong_chain_id() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);
        let tx = Transaction::new(
            from, "SRX_bob".to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), TEST_CHAIN_ID, &sk, &pk,
        ).unwrap();
        assert!(tx.validate(0, 9999).is_err()); // wrong chain
    }

    #[test]
    fn test_validate_fee_too_low() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);
        let tx = Transaction::new(
            from, "SRX_bob".to_string(),
            1_000_000, 1, 0, String::new(), TEST_CHAIN_ID, &sk, &pk,
        ).unwrap();
        assert!(tx.validate(0, TEST_CHAIN_ID).is_err());
    }

    #[test]
    fn test_tampered_signature_fails() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);
        let mut tx = Transaction::new(
            from, "SRX_bob".to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), TEST_CHAIN_ID, &sk, &pk,
        ).unwrap();
        tx.amount = 999_999_999;
        assert!(tx.verify().is_err());
    }

    #[test]
    fn test_c01_verify_rejects_mismatched_address() {
        let (sk, pk) = make_keypair();
        let real_address = derive_addr(&pk);

        // Create valid tx with correct from_address
        let mut tx = Transaction::new(
            real_address.clone(), "SRX_bob".to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), TEST_CHAIN_ID, &sk, &pk,
        ).unwrap();
        assert!(tx.verify().is_ok());

        // Tamper from_address to a different address
        tx.from_address = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string();
        // Should fail: public key doesn't match from_address
        assert!(tx.verify().is_err());
    }

    #[test]
    fn test_h01_signing_payload_canonical_and_escaped() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);

        // Test deterministic: same inputs → same payload
        let tx1 = Transaction::new(
            from.clone(), "0xbob".to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), TEST_CHAIN_ID, &sk, &pk,
        ).unwrap();
        let _tx2 = Transaction::new(
            from.clone(), "0xbob".to_string(),
            1_000_000, MIN_TX_FEE, 0, String::new(), TEST_CHAIN_ID, &sk, &pk,
        ).unwrap();
        // Timestamps differ, but let's verify the format is valid JSON
        let payload = tx1.signing_payload();
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["amount"], 1_000_000);
        assert_eq!(parsed["chain_id"], TEST_CHAIN_ID);
        assert_eq!(parsed["from"], from);

        // Test special chars in data field are properly escaped (not injected)
        let tx_xss = Transaction::new(
            from.clone(), "0xbob".to_string(),
            1_000_000, MIN_TX_FEE, 0,
            r#"<script>alert("xss")</script>"#.to_string(),
            TEST_CHAIN_ID, &sk, &pk,
        ).unwrap();
        let payload_xss = tx_xss.signing_payload();
        // Must be valid JSON — serde_json escapes the quotes in data field
        let parsed_xss: serde_json::Value = serde_json::from_str(&payload_xss).unwrap();
        assert_eq!(parsed_xss["data"], r#"<script>alert("xss")</script>"#);

        // Test with quote injection attempt in data
        let tx_inject = Transaction::new(
            from.clone(), "0xbob".to_string(),
            1_000_000, MIN_TX_FEE, 0,
            r#"","fee":0,"from":"attacker"#.to_string(),
            TEST_CHAIN_ID, &sk, &pk,
        ).unwrap();
        let payload_inject = tx_inject.signing_payload();
        // Must still be valid JSON with the injection attempt as a plain string value
        let parsed_inject: serde_json::Value = serde_json::from_str(&payload_inject).unwrap();
        assert!(parsed_inject["data"].as_str().unwrap().contains("attacker"));
        // The "from" field must still be the real address, not "attacker"
        assert_eq!(parsed_inject["from"], from);
    }
}
