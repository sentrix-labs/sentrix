// bft_messages.rs — BFT vote types and justification structs (Voyager Phase 2a)
//
// Proposal, Prevote, Precommit, BlockJustification.
// All serializable with bincode to match P2P wire format.

use serde::{Deserialize, Serialize};

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
}
