// bft_messages.rs — BFT vote types and justification structs (Voyager Phase 2a)
//
// Proposal, Prevote, Precommit, BlockJustification.
// All serializable with bincode to match P2P wire format.
// Signatures use secp256k1 ECDSA (same as transaction signing).

use serde::{Deserialize, Serialize};
use secp256k1::{Secp256k1, Message, SecretKey};
use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use sha2::{Sha256, Digest};
use crate::types::error::{SentrixError, SentrixResult};

// ── Proposal ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub height: u64,
    pub round: u32,
    pub block_hash: String,
    pub block_data: Vec<u8>, // bincode-encoded Block
    pub proposer: String,    // validator address
    pub signature: Vec<u8>,  // ed25519 signature over (height, round, block_hash)
}

impl Proposal {
    pub fn signing_payload(height: u64, round: u32, block_hash: &str) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&round.to_le_bytes());
        payload.extend_from_slice(block_hash.as_bytes());
        payload.push(0x01); // domain separator: proposal
        payload
    }
}

// ── Prevote ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Prevote {
    pub height: u64,
    pub round: u32,
    /// None = nil vote (no valid proposal received)
    pub block_hash: Option<String>,
    pub validator: String,
    pub signature: Vec<u8>,
}

impl Prevote {
    pub fn signing_payload(height: u64, round: u32, block_hash: &Option<String>) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&round.to_le_bytes());
        match block_hash {
            Some(h) => payload.extend_from_slice(h.as_bytes()),
            None => payload.extend_from_slice(b"NIL"),
        }
        payload.push(0x02); // domain separator: prevote
        payload
    }

    pub fn is_nil(&self) -> bool {
        self.block_hash.is_none()
    }
}

// ── Precommit ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Precommit {
    pub height: u64,
    pub round: u32,
    /// None = nil precommit (did not see 2/3+1 prevotes for any hash)
    pub block_hash: Option<String>,
    pub validator: String,
    pub signature: Vec<u8>,
}

impl Precommit {
    pub fn signing_payload(height: u64, round: u32, block_hash: &Option<String>) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&round.to_le_bytes());
        match block_hash {
            Some(h) => payload.extend_from_slice(h.as_bytes()),
            None => payload.extend_from_slice(b"NIL"),
        }
        payload.push(0x03); // domain separator: precommit
        payload
    }

    pub fn is_nil(&self) -> bool {
        self.block_hash.is_none()
    }
}

// ── Block Justification ──────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPrecommit {
    pub validator: String,
    pub block_hash: String,
    pub signature: Vec<u8>,
    pub stake_weight: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BlockJustification {
    pub height: u64,
    pub round: u32,
    pub block_hash: String,
    pub precommits: Vec<SignedPrecommit>,
}

impl BlockJustification {
    pub fn new(height: u64, round: u32, block_hash: String) -> Self {
        Self {
            height,
            round,
            block_hash,
            precommits: Vec::new(),
        }
    }

    pub fn add_precommit(&mut self, validator: String, signature: Vec<u8>, stake_weight: u64) {
        self.precommits.push(SignedPrecommit {
            validator,
            block_hash: self.block_hash.clone(),
            signature,
            stake_weight,
        });
    }

    /// Total stake weight of all precommits
    pub fn total_weight(&self) -> u64 {
        self.precommits.iter().map(|p| p.stake_weight).sum()
    }

    /// Check if we have supermajority (2/3+1 by stake weight)
    pub fn has_supermajority(&self, total_stake: u64) -> bool {
        if total_stake == 0 {
            return false;
        }
        self.total_weight() >= supermajority_threshold(total_stake)
    }

    pub fn signer_count(&self) -> usize {
        self.precommits.len()
    }
}

// ── Helpers ──────────────────────────────────────────────────

/// Calculate 2/3+1 threshold for a given total stake
pub fn supermajority_threshold(total_stake: u64) -> u64 {
    (total_stake as u128 * 2 / 3 + 1) as u64
}

// ── BFT Network Message Wrapper ──────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BftMessage {
    Propose(Proposal),
    Prevote(Prevote),
    Precommit(Precommit),
}

// ── Vote Signing (secp256k1 ECDSA) ──────────────────────────

/// Sign arbitrary bytes with a secp256k1 secret key.
/// Returns 65-byte recoverable signature (64-byte compact + 1-byte recovery_id).
pub fn sign_payload(payload: &[u8], secret_key: &SecretKey) -> Vec<u8> {
    let secp = Secp256k1::signing_only();
    let hash = Sha256::digest(payload);
    // SHA-256 always produces 32 bytes — from_digest_slice cannot fail
    #[allow(clippy::expect_used)]
    let msg = Message::from_digest_slice(&hash).expect("SHA-256 always 32 bytes");
    let sig = secp.sign_ecdsa_recoverable(&msg, secret_key);
    let (rec_id, compact) = sig.serialize_compact();
    let mut out = compact.to_vec(); // 64 bytes
    out.push(rec_id.to_i32() as u8);   // 1 byte recovery id
    out
}

/// Verify a recoverable signature and return the signer's Ethereum-style address.
/// Returns Err if signature is invalid or malformed.
pub fn recover_signer(payload: &[u8], signature: &[u8]) -> SentrixResult<String> {
    if signature.len() != 65 {
        return Err(SentrixError::InvalidSignature);
    }
    let secp = Secp256k1::verification_only();
    let hash = Sha256::digest(payload);
    let msg = Message::from_digest_slice(&hash)
        .map_err(|_| SentrixError::InvalidSignature)?;
    let rec_id = RecoveryId::from_i32(signature[64] as i32)
        .map_err(|_| SentrixError::InvalidSignature)?;
    let sig = RecoverableSignature::from_compact(&signature[..64], rec_id)
        .map_err(|_| SentrixError::InvalidSignature)?;
    let pubkey = secp.recover_ecdsa(&msg, &sig)
        .map_err(|_| SentrixError::InvalidSignature)?;
    Ok(crate::wallet::wallet::Wallet::derive_address(&pubkey))
}

/// Verify that a signature was produced by the claimed validator address.
pub fn verify_vote_signature(payload: &[u8], signature: &[u8], expected_validator: &str) -> bool {
    if signature.is_empty() {
        return false; // unsigned votes are invalid
    }
    match recover_signer(payload, signature) {
        Ok(ref addr) if addr == expected_validator => true,
        Ok(addr) => {
            tracing::warn!(
                "BFT sig mismatch: expected={} recovered={} sig_len={}",
                &expected_validator[..12], &addr[..12], signature.len(),
            );
            false
        }
        Err(e) => {
            tracing::warn!("BFT sig recovery failed: {} sig_len={}", e, signature.len());
            false
        }
    }
}

impl Prevote {
    /// Sign this prevote with the given secret key, filling the signature field.
    pub fn sign(&mut self, secret_key: &SecretKey) {
        let payload = Self::signing_payload(self.height, self.round, &self.block_hash);
        self.signature = sign_payload(&payload, secret_key);
    }

    /// Verify this prevote's signature matches the claimed validator.
    pub fn verify_sig(&self) -> bool {
        let payload = Self::signing_payload(self.height, self.round, &self.block_hash);
        verify_vote_signature(&payload, &self.signature, &self.validator)
    }
}

impl Precommit {
    /// Sign this precommit with the given secret key, filling the signature field.
    pub fn sign(&mut self, secret_key: &SecretKey) {
        let payload = Self::signing_payload(self.height, self.round, &self.block_hash);
        self.signature = sign_payload(&payload, secret_key);
    }

    /// Verify this precommit's signature matches the claimed validator.
    pub fn verify_sig(&self) -> bool {
        let payload = Self::signing_payload(self.height, self.round, &self.block_hash);
        verify_vote_signature(&payload, &self.signature, &self.validator)
    }
}

impl Proposal {
    /// Sign this proposal with the given secret key.
    pub fn sign(&mut self, secret_key: &SecretKey) {
        let payload = Self::signing_payload(self.height, self.round, &self.block_hash);
        self.signature = sign_payload(&payload, secret_key);
    }

    /// Verify this proposal's signature matches the claimed proposer.
    pub fn verify_sig(&self) -> bool {
        let payload = Self::signing_payload(self.height, self.round, &self.block_hash);
        verify_vote_signature(&payload, &self.signature, &self.proposer)
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supermajority_threshold() {
        // 21 equal validators, each 1 unit
        assert_eq!(supermajority_threshold(21), 15); // ceil(21*2/3) + 1 = 14+1 = 15
        assert_eq!(supermajority_threshold(100), 67);
        assert_eq!(supermajority_threshold(3), 3);
        assert_eq!(supermajority_threshold(1), 1);
        assert_eq!(supermajority_threshold(0), 1); // edge: 0 + 1
    }

    #[test]
    fn test_proposal_signing_payload() {
        let p1 = Proposal::signing_payload(100, 0, "hash_abc");
        let p2 = Proposal::signing_payload(100, 1, "hash_abc");
        assert_ne!(p1, p2); // different round

        let p3 = Proposal::signing_payload(100, 0, "hash_def");
        assert_ne!(p1, p3); // different hash
    }

    #[test]
    fn test_prevote_signing_payload_nil() {
        let p1 = Prevote::signing_payload(100, 0, &Some("hash".into()));
        let p2 = Prevote::signing_payload(100, 0, &None);
        assert_ne!(p1, p2);
    }

    #[test]
    fn test_prevote_domain_separation() {
        let prevote = Prevote::signing_payload(100, 0, &Some("hash".into()));
        let precommit = Precommit::signing_payload(100, 0, &Some("hash".into()));
        // Different domain separators (0x02 vs 0x03)
        assert_ne!(prevote, precommit);
    }

    #[test]
    fn test_prevote_is_nil() {
        let nil = Prevote {
            height: 1, round: 0, block_hash: None,
            validator: "v1".into(), signature: vec![],
        };
        assert!(nil.is_nil());

        let vote = Prevote {
            height: 1, round: 0, block_hash: Some("hash".into()),
            validator: "v1".into(), signature: vec![],
        };
        assert!(!vote.is_nil());
    }

    #[test]
    fn test_precommit_is_nil() {
        let nil = Precommit {
            height: 1, round: 0, block_hash: None,
            validator: "v1".into(), signature: vec![],
        };
        assert!(nil.is_nil());
    }

    #[test]
    fn test_justification_supermajority() {
        let mut just = BlockJustification::new(100, 0, "hash".into());

        // Total stake = 21, threshold = 15
        for i in 0..14 {
            just.add_precommit(format!("val{}", i), vec![], 1);
        }
        assert!(!just.has_supermajority(21)); // 14 < 15

        just.add_precommit("val14".into(), vec![], 1);
        assert!(just.has_supermajority(21)); // 15 >= 15
    }

    #[test]
    fn test_justification_weighted() {
        let mut just = BlockJustification::new(100, 0, "hash".into());

        // One big validator with most stake
        just.add_precommit("whale".into(), vec![], 70);
        assert!(just.has_supermajority(100)); // 70 >= 67
    }

    #[test]
    fn test_justification_total_weight() {
        let mut just = BlockJustification::new(100, 0, "hash".into());
        just.add_precommit("v1".into(), vec![], 10);
        just.add_precommit("v2".into(), vec![], 20);
        just.add_precommit("v3".into(), vec![], 30);
        assert_eq!(just.total_weight(), 60);
        assert_eq!(just.signer_count(), 3);
    }

    #[test]
    fn test_justification_zero_stake() {
        let just = BlockJustification::new(100, 0, "hash".into());
        assert!(!just.has_supermajority(0));
    }

    #[test]
    fn test_bft_message_enum() {
        let msg = BftMessage::Prevote(Prevote {
            height: 1, round: 0, block_hash: Some("h".into()),
            validator: "v".into(), signature: vec![],
        });
        // Just verify it's constructible and matchable
        if let BftMessage::Prevote(v) = msg {
            assert_eq!(v.height, 1);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn test_bincode_roundtrip() {
        let prevote = Prevote {
            height: 12345, round: 3,
            block_hash: Some("abc123def456".into()),
            validator: "0xval1".into(),
            signature: vec![1, 2, 3, 4],
        };
        let encoded = bincode::serialize(&prevote).unwrap();
        let decoded: Prevote = bincode::deserialize(&encoded).unwrap();
        assert_eq!(prevote, decoded);
    }

    #[test]
    fn test_bincode_roundtrip_precommit() {
        let pc = Precommit {
            height: 999, round: 0,
            block_hash: None,
            validator: "0xval2".into(),
            signature: vec![5, 6, 7],
        };
        let encoded = bincode::serialize(&pc).unwrap();
        let decoded: Precommit = bincode::deserialize(&encoded).unwrap();
        assert_eq!(pc, decoded);
    }

    // ── Signature tests ──────────────────────────────────────

    fn make_wallet() -> crate::wallet::wallet::Wallet {
        crate::wallet::wallet::Wallet::generate()
    }

    #[test]
    fn test_prevote_sign_verify() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let mut pv = Prevote {
            height: 100, round: 0,
            block_hash: Some("hash_abc".into()),
            validator: wallet.address.clone(),
            signature: vec![],
        };
        pv.sign(&sk);
        assert_eq!(pv.signature.len(), 65);
        assert!(pv.verify_sig());
    }

    #[test]
    fn test_precommit_sign_verify() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let mut pc = Precommit {
            height: 200, round: 1,
            block_hash: Some("hash_def".into()),
            validator: wallet.address.clone(),
            signature: vec![],
        };
        pc.sign(&sk);
        assert_eq!(pc.signature.len(), 65);
        assert!(pc.verify_sig());
    }

    #[test]
    fn test_proposal_sign_verify() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let mut prop = Proposal {
            height: 300, round: 0,
            block_hash: "hash_ghi".into(),
            block_data: vec![1, 2, 3],
            proposer: wallet.address.clone(),
            signature: vec![],
        };
        prop.sign(&sk);
        assert_eq!(prop.signature.len(), 65);
        assert!(prop.verify_sig());
    }

    #[test]
    fn test_tampered_prevote_fails_verify() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let mut pv = Prevote {
            height: 100, round: 0,
            block_hash: Some("original".into()),
            validator: wallet.address.clone(),
            signature: vec![],
        };
        pv.sign(&sk);
        // Tamper with the block_hash
        pv.block_hash = Some("tampered".into());
        assert!(!pv.verify_sig());
    }

    #[test]
    fn test_wrong_validator_fails_verify() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let mut pv = Prevote {
            height: 100, round: 0,
            block_hash: Some("hash".into()),
            validator: wallet.address.clone(),
            signature: vec![],
        };
        pv.sign(&sk);
        // Change claimed validator
        pv.validator = "0xwrongaddress000000000000000000000000000".into();
        assert!(!pv.verify_sig());
    }

    #[test]
    fn test_nil_prevote_sign_verify() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let mut pv = Prevote {
            height: 100, round: 0,
            block_hash: None, // nil vote
            validator: wallet.address.clone(),
            signature: vec![],
        };
        pv.sign(&sk);
        assert!(pv.verify_sig());
    }

    #[test]
    fn test_empty_signature_fails() {
        let pv = Prevote {
            height: 100, round: 0,
            block_hash: Some("hash".into()),
            validator: "0xsome_address".into(),
            signature: vec![], // unsigned
        };
        assert!(!pv.verify_sig());
    }

    #[test]
    fn test_recover_signer() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let payload = b"test message";
        let sig = sign_payload(payload, &sk);
        let recovered = recover_signer(payload, &sig).unwrap();
        assert_eq!(recovered, wallet.address);
    }
}
