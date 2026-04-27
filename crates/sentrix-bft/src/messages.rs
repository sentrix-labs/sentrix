// bft_messages.rs — BFT vote types and justification structs (Voyager Phase 2a)
//
// Proposal, Prevote, Precommit, BlockJustification.
// All serializable with bincode to match P2P wire format.
// Signatures use secp256k1 ECDSA (same as transaction signing).

use secp256k1::ecdsa::{RecoverableSignature, RecoveryId};
use secp256k1::{Message, Secp256k1, SecretKey};
use sentrix_primitives::{SentrixError, SentrixResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ── BFT signing v2 fork (chain_id in payload) ───────────────
//
// Bug A from `audits/bft-signing-fork-design.md`: BFT vote signing
// payloads currently lack `chain_id`, allowing a mainnet (7119) signature
// to cryptographically verify on testnet (7120) at the same height/round/
// hash. Practical exploit: nil-vote replay where `block_hash` is "NIL"
// — same payload across chains.
//
// v2 fix: prepend a magic byte (0x20, distinct from existing domain
// separators 0x01-0x04 and the MultiaddrAdvertisement separator 0x10)
// + the chain_id (big-endian u64) before the existing payload. Old
// payload format preserved verbatim after the v2 prefix so verifier
// dispatch is straightforward.
//
// Activation is hard-fork gated by `BFT_SIGNING_V2_FORK_HEIGHT`. Default
// `u64::MAX` = inert; the v2 path never fires in this binary. Operators
// flip the constant to a coordinated mainnet height in a separate
// fork-coordination session, after testnet bake.
//
// Phase 1 (this PR): add the constant + v2 payload helpers + tests.
// v1 sign/verify methods still take the old signature (no chain_id arg)
// and emit the v1 payload — no behavioural change at runtime.
//
// Phase 2 (next session): refactor every sign/verify call site to pass
// chain_id, dispatch v1 vs v2 payload internally based on height. Then
// remove the v1-only helpers below once all callers are migrated.
//
// Phase 5 (operator ceremony): set `BFT_SIGNING_V2_FORK_HEIGHT` to a
// coordinated mainnet height. v2 path activates at that block. Old
// validators (not on this binary) can no longer cross-verify v2-signed
// messages — that's the whole point.
//
// See operator runbooks for the full 5-phase migration plan.

/// Hard-fork height at which BFT signing v2 (chain_id-in-payload)
/// activates. `u64::MAX` = inert; v2 dispatch never fires in this binary.
/// Operators flip this in a coordinated mainnet fork session per
/// `audits/bft-signing-fork-design.md`. Until then, all sign/verify
/// paths use the legacy v1 payload format.
pub const BFT_SIGNING_V2_FORK_HEIGHT: u64 = u64::MAX;

/// v2 magic byte. Prepended to v2 signing payloads to make them
/// unambiguously distinct from v1 payloads + from any existing
/// domain-separated message type.
///
/// Existing separators in use:
/// - 0x01: Proposal (v1, last byte)
/// - 0x02: Prevote (v1, last byte)
/// - 0x03: Precommit (v1, last byte)
/// - 0x04: RoundStatus (v1, last byte)
/// - 0x10: MultiaddrAdvertisement (different signing context entirely)
/// - 0x20: BFT signing v2 (this — first byte of v2 payloads)
const BFT_V2_MAGIC: u8 = 0x20;

/// Build the v2 prefix that goes BEFORE the v1-format payload.
/// Layout: `[0x20 magic][chain_id BE u64]` (9 bytes total).
///
/// Used by all four BFT message types (Proposal/Prevote/Precommit/
/// RoundStatus) so a v2-signed Proposal cannot be replayed as a v2-signed
/// Prevote on another chain (the inner v1 payload still has the
/// per-message domain separator).
fn bft_v2_prefix(chain_id: u64) -> [u8; 9] {
    let mut prefix = [0u8; 9];
    prefix[0] = BFT_V2_MAGIC;
    prefix[1..9].copy_from_slice(&chain_id.to_be_bytes());
    prefix
}

/// Returns `true` if the given block height is at or past the v2 fork.
/// Centralised so call sites don't drift out of sync.
///
/// `clippy::absurd_extreme_comparisons` fires here because the default
/// `BFT_SIGNING_V2_FORK_HEIGHT = u64::MAX` makes `>=` trivially `==`.
/// Allowed deliberately: the operator flips the constant to a real
/// (much smaller) height at fork-coordination time, after which the
/// `>=` comparison is non-trivial. The semantics we want is "at or
/// past the fork", not "exactly at the fork", so `>=` is the correct
/// operator regardless of the current constant value.
#[inline]
#[allow(clippy::absurd_extreme_comparisons)]
pub fn is_bft_signing_v2_active(height: u64) -> bool {
    height >= BFT_SIGNING_V2_FORK_HEIGHT
}

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

    /// v2 signing payload: prepends `[BFT_V2_MAGIC][chain_id BE u64]` to the
    /// v1 layout. Use this only via the dispatch helper
    /// [`Proposal::signing_payload_for_height`] — calling it directly bypasses
    /// the fork-height gate.
    pub fn signing_payload_v2(height: u64, round: u32, block_hash: &str, chain_id: u64) -> Vec<u8> {
        let mut payload = Vec::with_capacity(9 + 8 + 4 + block_hash.len() + 1);
        payload.extend_from_slice(&bft_v2_prefix(chain_id));
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&round.to_le_bytes());
        payload.extend_from_slice(block_hash.as_bytes());
        payload.push(0x01);
        payload
    }

    /// Dispatch helper: returns v1 payload below the fork height, v2 payload
    /// at or above. Phase 2 of the migration plan switches all sign/verify
    /// call sites to this helper.
    pub fn signing_payload_for_height(
        height: u64,
        round: u32,
        block_hash: &str,
        chain_id: u64,
    ) -> Vec<u8> {
        if is_bft_signing_v2_active(height) {
            Self::signing_payload_v2(height, round, block_hash, chain_id)
        } else {
            Self::signing_payload(height, round, block_hash)
        }
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

    /// v2 signing payload — see [`Proposal::signing_payload_v2`] for shape rationale.
    pub fn signing_payload_v2(
        height: u64,
        round: u32,
        block_hash: &Option<String>,
        chain_id: u64,
    ) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&bft_v2_prefix(chain_id));
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&round.to_le_bytes());
        match block_hash {
            Some(h) => payload.extend_from_slice(h.as_bytes()),
            None => payload.extend_from_slice(b"NIL"),
        }
        payload.push(0x02);
        payload
    }

    pub fn signing_payload_for_height(
        height: u64,
        round: u32,
        block_hash: &Option<String>,
        chain_id: u64,
    ) -> Vec<u8> {
        if is_bft_signing_v2_active(height) {
            Self::signing_payload_v2(height, round, block_hash, chain_id)
        } else {
            Self::signing_payload(height, round, block_hash)
        }
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

    /// v2 signing payload — see [`Proposal::signing_payload_v2`] for shape rationale.
    pub fn signing_payload_v2(
        height: u64,
        round: u32,
        block_hash: &Option<String>,
        chain_id: u64,
    ) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&bft_v2_prefix(chain_id));
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&round.to_le_bytes());
        match block_hash {
            Some(h) => payload.extend_from_slice(h.as_bytes()),
            None => payload.extend_from_slice(b"NIL"),
        }
        payload.push(0x03);
        payload
    }

    pub fn signing_payload_for_height(
        height: u64,
        round: u32,
        block_hash: &Option<String>,
        chain_id: u64,
    ) -> Vec<u8> {
        if is_bft_signing_v2_active(height) {
            Self::signing_payload_v2(height, round, block_hash, chain_id)
        } else {
            Self::signing_payload(height, round, block_hash)
        }
    }

    pub fn is_nil(&self) -> bool {
        self.block_hash.is_none()
    }
}

// ── Block Justification — re-exported from sentrix-primitives ───

pub use sentrix_primitives::justification::{
    BlockJustification, SignedPrecommit, supermajority_threshold,
};

// ── BFT Network Message Wrapper ──────────────────────────────

// ── Round Status (convergence protocol) ─────────────────────

/// Periodically gossiped by validators so that peers returning from partition
/// can learn the current (height, round) without waiting for a vote.
///
/// Signed with the validator's secp256k1 key to close audit finding C-01:
/// an unsigned RoundStatus lets an attacker advance `self.state.round` or
/// trigger `SyncNeeded` with arbitrary heights, enabling round-manipulation
/// and block-sync-hijack attacks. `signature` is `#[serde(default)]` so
/// legacy encodings deserialize (as empty), but `verify_sig` rejects an
/// empty signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundStatus {
    pub height: u64,
    pub round: u32,
    pub validator: String,
    #[serde(default)]
    pub signature: Vec<u8>,
}

impl RoundStatus {
    /// Canonical signing payload — domain-separated to prevent cross-type
    /// signature reuse (proposal=0x01, prevote=0x02, precommit=0x03, this=0x04).
    pub fn signing_payload(height: u64, round: u32, validator: &str) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&round.to_le_bytes());
        payload.extend_from_slice(validator.as_bytes());
        payload.push(0x04); // domain separator: round_status
        payload
    }

    /// v2 signing payload — see [`Proposal::signing_payload_v2`] for shape rationale.
    pub fn signing_payload_v2(
        height: u64,
        round: u32,
        validator: &str,
        chain_id: u64,
    ) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&bft_v2_prefix(chain_id));
        payload.extend_from_slice(&height.to_le_bytes());
        payload.extend_from_slice(&round.to_le_bytes());
        payload.extend_from_slice(validator.as_bytes());
        payload.push(0x04);
        payload
    }

    pub fn signing_payload_for_height(
        height: u64,
        round: u32,
        validator: &str,
        chain_id: u64,
    ) -> Vec<u8> {
        if is_bft_signing_v2_active(height) {
            Self::signing_payload_v2(height, round, validator, chain_id)
        } else {
            Self::signing_payload(height, round, validator)
        }
    }

    /// Sign this status in place with the given secret key.
    pub fn sign(&mut self, secret_key: &SecretKey) {
        let payload = Self::signing_payload(self.height, self.round, &self.validator);
        self.signature = sign_payload(&payload, secret_key);
    }

    /// Verify the signature matches the claimed validator. An empty signature
    /// always fails — this is the C-01 barrier that stops legacy/forged
    /// RoundStatus messages from manipulating consensus state.
    pub fn verify_sig(&self) -> bool {
        if self.signature.is_empty() {
            tracing::warn!(
                "BFT: unsigned RoundStatus from {} — rejected (C-01)",
                &self.validator[..12.min(self.validator.len())]
            );
            return false;
        }
        let payload = Self::signing_payload(self.height, self.round, &self.validator);
        verify_vote_signature(&payload, &self.signature, &self.validator)
    }
}

// ── BFT Network Message Wrapper ─────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BftMessage {
    Propose(Proposal),
    Prevote(Prevote),
    Precommit(Precommit),
    RoundStatus(RoundStatus),
}

// ── Vote Signing (secp256k1 ECDSA) ──────────────────────────

/// Sign arbitrary bytes with a secp256k1 secret key.
/// Returns 65-byte recoverable signature (64-byte compact + 1-byte recovery_id).
pub fn sign_payload(payload: &[u8], secret_key: &SecretKey) -> Vec<u8> {
    let secp = Secp256k1::signing_only();
    let hash: [u8; 32] = Sha256::digest(payload).into();
    let msg = Message::from_digest(hash);
    let sig = secp.sign_ecdsa_recoverable(msg, secret_key);
    let (rec_id, compact) = sig.serialize_compact();
    let mut out = compact.to_vec(); // 64 bytes
    out.push(i32::from(rec_id) as u8); // 1 byte recovery id
    out
}

/// Verify a recoverable signature and return the signer's Ethereum-style address.
/// Returns Err if signature is invalid or malformed.
pub fn recover_signer(payload: &[u8], signature: &[u8]) -> SentrixResult<String> {
    if signature.len() != 65 {
        return Err(SentrixError::InvalidSignature);
    }
    let secp = Secp256k1::verification_only();
    let hash: [u8; 32] = Sha256::digest(payload).into();
    let msg = Message::from_digest(hash);
    let rec_id =
        RecoveryId::try_from(signature[64] as i32).map_err(|_| SentrixError::InvalidSignature)?;
    let sig = RecoverableSignature::from_compact(&signature[..64], rec_id)
        .map_err(|_| SentrixError::InvalidSignature)?;
    let pubkey = secp
        .recover_ecdsa(msg, &sig)
        .map_err(|_| SentrixError::InvalidSignature)?;
    Ok(sentrix_wallet::Wallet::derive_address(&pubkey))
}

/// Verify that a signature was produced by the claimed validator address.
pub fn verify_vote_signature(payload: &[u8], signature: &[u8], expected_validator: &str) -> bool {
    if signature.is_empty() {
        tracing::warn!(
            "BFT: UNSIGNED vote from {} (empty signature)",
            &expected_validator[..12.min(expected_validator.len())]
        );
        return false;
    }
    match recover_signer(payload, signature) {
        Ok(ref addr) if addr == expected_validator => true,
        Ok(addr) => {
            tracing::warn!(
                "BFT sig mismatch: expected={} recovered={} sig_len={}",
                &expected_validator[..12],
                &addr[..12],
                signature.len(),
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
            height: 1,
            round: 0,
            block_hash: None,
            validator: "v1".into(),
            signature: vec![],
        };
        assert!(nil.is_nil());

        let vote = Prevote {
            height: 1,
            round: 0,
            block_hash: Some("hash".into()),
            validator: "v1".into(),
            signature: vec![],
        };
        assert!(!vote.is_nil());
    }

    #[test]
    fn test_precommit_is_nil() {
        let nil = Precommit {
            height: 1,
            round: 0,
            block_hash: None,
            validator: "v1".into(),
            signature: vec![],
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
            height: 1,
            round: 0,
            block_hash: Some("h".into()),
            validator: "v".into(),
            signature: vec![],
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
            height: 12345,
            round: 3,
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
            height: 999,
            round: 0,
            block_hash: None,
            validator: "0xval2".into(),
            signature: vec![5, 6, 7],
        };
        let encoded = bincode::serialize(&pc).unwrap();
        let decoded: Precommit = bincode::deserialize(&encoded).unwrap();
        assert_eq!(pc, decoded);
    }

    // ── Signature tests ──────────────────────────────────────

    fn make_wallet() -> sentrix_wallet::Wallet {
        sentrix_wallet::Wallet::generate()
    }

    #[test]
    fn test_prevote_sign_verify() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let mut pv = Prevote {
            height: 100,
            round: 0,
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
            height: 200,
            round: 1,
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
            height: 300,
            round: 0,
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
            height: 100,
            round: 0,
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
            height: 100,
            round: 0,
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
            height: 100,
            round: 0,
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
            height: 100,
            round: 0,
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

    // ── RoundStatus signature tests (C-01) ──────────────────

    #[test]
    fn test_round_status_sign_verify_positive() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let mut status = RoundStatus {
            height: 42,
            round: 3,
            validator: wallet.address.clone(),
            signature: Vec::new(),
        };
        status.sign(&sk);
        assert!(status.verify_sig());
    }

    #[test]
    fn test_round_status_unsigned_rejected() {
        // C-01 barrier: empty signature must fail — this is what stops
        // pre-upgrade nodes from injecting unsigned round manipulations.
        let status = RoundStatus {
            height: 42,
            round: 3,
            validator: "0xsome_address".into(),
            signature: Vec::new(),
        };
        assert!(!status.verify_sig());
    }

    #[test]
    fn test_round_status_tampered_height_rejected() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let mut status = RoundStatus {
            height: 42,
            round: 3,
            validator: wallet.address.clone(),
            signature: Vec::new(),
        };
        status.sign(&sk);
        status.height = 999_999; // attacker rewrites height after signing
        assert!(!status.verify_sig());
    }

    #[test]
    fn test_round_status_tampered_round_rejected() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let mut status = RoundStatus {
            height: 42,
            round: 3,
            validator: wallet.address.clone(),
            signature: Vec::new(),
        };
        status.sign(&sk);
        status.round = 50; // attacker rewrites round to force catch-up
        assert!(!status.verify_sig());
    }

    #[test]
    fn test_round_status_wrong_validator_rejected() {
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        let mut status = RoundStatus {
            height: 42,
            round: 3,
            validator: wallet.address.clone(),
            signature: Vec::new(),
        };
        status.sign(&sk);
        status.validator = "0xwrongaddress0000000000000000000000000000".into();
        assert!(!status.verify_sig());
    }

    // ── BFT signing v2 (chain_id) tests ──────────────────────

    #[test]
    fn test_v2_dispatch_inert_when_fork_height_unset() {
        // With BFT_SIGNING_V2_FORK_HEIGHT = u64::MAX, no realistic block
        // height ever activates v2. Dispatch helper must always return v1.
        // This is the load-bearing safety property: the binary ships with
        // the v2 path code dead until operators flip the fork height.
        let proposal_v1 = Proposal::signing_payload(100, 0, "hash");
        let proposal_dispatched = Proposal::signing_payload_for_height(100, 0, "hash", 7119);
        assert_eq!(proposal_v1, proposal_dispatched);

        let prevote_v1 = Prevote::signing_payload(100, 0, &Some("hash".into()));
        let prevote_dispatched =
            Prevote::signing_payload_for_height(100, 0, &Some("hash".into()), 7119);
        assert_eq!(prevote_v1, prevote_dispatched);

        let precommit_v1 = Precommit::signing_payload(100, 0, &Some("hash".into()));
        let precommit_dispatched =
            Precommit::signing_payload_for_height(100, 0, &Some("hash".into()), 7119);
        assert_eq!(precommit_v1, precommit_dispatched);

        let status_v1 = RoundStatus::signing_payload(100, 0, "0xval");
        let status_dispatched = RoundStatus::signing_payload_for_height(100, 0, "0xval", 7119);
        assert_eq!(status_v1, status_dispatched);

        // And dispatch for height u64::MAX-1 (just below fork) must also be v1.
        assert_eq!(
            Proposal::signing_payload(u64::MAX - 1, 0, "hash"),
            Proposal::signing_payload_for_height(u64::MAX - 1, 0, "hash", 7119)
        );
    }

    #[test]
    fn test_v2_payload_starts_with_magic_byte() {
        // v2 payloads begin with [0x20][chain_id BE u64][...v1 layout].
        let v2 = Proposal::signing_payload_v2(100, 0, "hash", 7119);
        assert_eq!(v2[0], 0x20);
        // chain_id 7119 = 0x1bcf, big-endian fills bytes 1..9 as 8 bytes.
        let chain_id_bytes: [u8; 8] = v2[1..9].try_into().unwrap();
        assert_eq!(u64::from_be_bytes(chain_id_bytes), 7119);
    }

    #[test]
    fn test_v2_chain_id_separates_mainnet_from_testnet() {
        // Same height/round/hash, different chain_id → different payload.
        // This is the cross-chain replay protection: a mainnet-signed
        // payload cannot verify on testnet because chain_id is in the
        // signed bytes.
        let mainnet = Proposal::signing_payload_v2(1_000_000, 0, "hash", 7119);
        let testnet = Proposal::signing_payload_v2(1_000_000, 0, "hash", 7120);
        assert_ne!(mainnet, testnet);
        // But everything except the chain_id bytes is identical.
        assert_eq!(mainnet[0], testnet[0]); // magic byte same
        assert_eq!(mainnet[9..], testnet[9..]); // post-prefix v1 layout same
        assert_ne!(mainnet[1..9], testnet[1..9]); // only chain_id bytes differ
    }

    #[test]
    fn test_v2_magic_byte_does_not_collide_with_v1() {
        // v1 payloads CANNOT start with 0x20 because they start with
        // height bytes (LE u64). For v1 to collide, height would need to
        // have its lowest byte = 0x20 (any height % 256 == 0x20 = 32).
        // BUT — the v1 payload length differs from a v2 payload at the
        // SAME starting bytes, AND the trailing domain separator on v2
        // is at a different position. So even with byte-level collision
        // at index 0, the full payloads cannot be confused by a verifier
        // because verify always uses the same dispatch logic for sign+verify.
        let v1 = Proposal::signing_payload(0x20, 0, ""); // height=32, empty hash
        let v2 = Proposal::signing_payload_v2(100, 0, "", 7119);
        // Payloads are different lengths even if first byte matches.
        assert_ne!(v1.len(), v2.len());
    }

    #[test]
    fn test_v2_domain_separators_preserved() {
        // The four message types must remain distinct under v2 — the
        // per-message domain separator (0x01-0x04) is at the same
        // relative position (last byte). A v2-Proposal sig must not
        // verify as a v2-Prevote even if chain_id, height, round match.
        let proposal = Proposal::signing_payload_v2(100, 0, "hash", 7119);
        let prevote = Prevote::signing_payload_v2(100, 0, &Some("hash".into()), 7119);
        let precommit = Precommit::signing_payload_v2(100, 0, &Some("hash".into()), 7119);
        let status = RoundStatus::signing_payload_v2(100, 0, "hash", 7119);
        assert_ne!(proposal, prevote);
        assert_ne!(proposal, precommit);
        assert_ne!(proposal, status);
        assert_ne!(prevote, precommit);
        assert_ne!(prevote, status);
        assert_ne!(precommit, status);
    }

    #[test]
    fn test_v2_nil_block_hash_produces_distinct_payload() {
        // Nil-vote replay was the specific exploit called out in the
        // design doc — same NIL payload across chains. Under v2,
        // nil-prevote-mainnet ≠ nil-prevote-testnet.
        let nil_mainnet = Prevote::signing_payload_v2(100, 0, &None, 7119);
        let nil_testnet = Prevote::signing_payload_v2(100, 0, &None, 7120);
        assert_ne!(nil_mainnet, nil_testnet);
    }

    #[test]
    fn test_is_bft_signing_v2_active_const_dispatch() {
        // Sanity: the helper is consistent with the constant.
        assert!(!is_bft_signing_v2_active(0));
        assert!(!is_bft_signing_v2_active(1_000_000));
        assert!(!is_bft_signing_v2_active(BFT_SIGNING_V2_FORK_HEIGHT - 1));
        assert!(is_bft_signing_v2_active(BFT_SIGNING_V2_FORK_HEIGHT));
        assert!(is_bft_signing_v2_active(u64::MAX));
    }

    #[test]
    fn test_round_status_domain_separation_prevents_reuse() {
        // Ensure a signature over a Prevote-shaped payload does NOT verify
        // as a RoundStatus signature (cross-type reuse attack).
        let wallet = make_wallet();
        let sk = wallet.get_secret_key().unwrap();
        // Sign a prevote-domain payload
        let mut pv = Prevote {
            height: 42,
            round: 3,
            block_hash: Some(wallet.address.clone()), // shape of validator field
            validator: wallet.address.clone(),
            signature: vec![],
        };
        pv.sign(&sk);
        // Plug the prevote signature into a RoundStatus with matching fields
        let status = RoundStatus {
            height: 42,
            round: 3,
            validator: wallet.address.clone(),
            signature: pv.signature,
        };
        // Domain separator byte (0x02 vs 0x04) makes these incompatible.
        assert!(!status.verify_sig());
    }
}
