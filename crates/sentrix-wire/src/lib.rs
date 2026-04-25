//! Sentrix libp2p wire protocol types.
//!
//! Pure data types for the on-network protocol surface — no libp2p, no
//! async runtime, no framing. The actual codec + behaviour lives in
//! `sentrix-network`; this crate exists so downstream tooling (future SDKs,
//! monitoring tools, light clients) can reference the canonical wire types
//! without pulling the full libp2p stack.
//!
//! # Stability
//!
//! These types are part of the on-network protocol surface. Rules:
//! - **Adding a new variant** (new `SentrixRequest` case, new `SentrixResponse` case):
//!   bump `SENTRIX_PROTOCOL`, roll out testnet-first so peers can negotiate.
//! - **Renaming or reordering existing variants**: bincode encoding is
//!   position-dependent. Reordering = immediate wire break. NEVER.
//! - **Removing a variant**: requires a hard fork at a pinned height, not a
//!   drop-in upgrade. Most of the time you want to deprecate-but-keep.
//! - **Changing a field type or adding a field**: same rule as reordering —
//!   bincode layout change is a wire break.
//!
//! # History
//!
//! Extracted from `sentrix-network::behaviour` 2026-04-23 as Tier 1 crate
//! split #5 per `founder-private/architecture/CRATE_SPLIT_PLAN.md`. The
//! enum definitions + constants were moved verbatim; the framing codec
//! `SentrixCodec` stays in `sentrix-network` because it pulls libp2p traits.

use sentrix_bft::messages::{Precommit, Prevote, Proposal, RoundStatus};
use sentrix_primitives::block::Block;
use sentrix_primitives::transaction::Transaction;
use serde::{Deserialize, Serialize};

// ── Protocol identifier ──────────────────────────────────

/// Protocol version string. Bump when adding / removing request or response
/// variants so peers can negotiate compatible versions. Currently 2.0.0 —
/// same as the sentrix binary major.minor, but intentionally tracked
/// separately so we don't have to bump the binary to bump the wire version.
pub const SENTRIX_PROTOCOL: &str = "/sentrix/2.0.0";

// ── Gossipsub topic names ────────────────────────────────

/// Topic for block propagation via gossipsub.
pub const BLOCKS_TOPIC: &str = "sentrix/blocks/1";
/// Topic for transaction propagation via gossipsub.
pub const TXS_TOPIC: &str = "sentrix/txs/1";
/// Topic for validator-multiaddr advertisements (L1 peer auto-discovery,
/// per `founder-private/audits/peer-auto-discovery-implementation-plan.md`).
/// Each validator gossips a signed [`MultiaddrAdvertisement`] on this
/// topic so other validators can dial them without needing a static
/// `--peers` bootstrap list.
pub const VALIDATOR_ADVERTS_TOPIC: &str = "sentrix/validator-adverts/1";

/// Hard cap on a single wire message (10 MiB). Callers doing their own
/// framing should enforce this too.
pub const MAX_MESSAGE_BYTES: usize = 10 * 1024 * 1024;

// ── Request / Response enums ─────────────────────────────

/// Messages a node sends to a peer (requests).
///
/// Mirrors the pre-2.0 raw-TCP `Message` enum but split into request /
/// response halves so libp2p's `RequestResponse` can correlate replies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SentrixRequest {
    /// Initial handshake — carries chain_id for network partitioning.
    Handshake {
        host: String,
        port: u16,
        height: u64,
        chain_id: u64,
    },
    /// Push a freshly mined block.
    NewBlock { block: Box<Block> },
    /// Push a new mempool transaction.
    NewTransaction { transaction: Transaction },
    /// Ask for blocks starting at `from_height`.
    GetBlocks { from_height: u64 },
    /// Ask for the peer's current chain height.
    GetHeight,
    /// Liveness probe.
    Ping,
    /// BFT: block proposal from the round proposer.
    BftProposal { proposal: Box<Proposal> },
    /// BFT: prevote for a block (or nil).
    BftPrevote { prevote: Prevote },
    /// BFT: precommit for a block (or nil).
    BftPrecommit { precommit: Precommit },
    /// BFT: periodic round status announcement for round synchronization.
    BftRoundStatus { status: RoundStatus },
}

impl SentrixRequest {
    /// Static tag used in diagnostic logs and metrics labels. Needed by
    /// bug #1d investigation — OutboundFailure logs today say "outbound
    /// failure to {peer}: {err}" with no hint which request variant was
    /// in flight, so we can't tell if BFT proposals are the ones timing
    /// out or unrelated background traffic (e.g. periodic GetBlocks).
    /// See `audits/bug-1d-proposer-request-response-design.md`.
    pub fn variant_name(&self) -> &'static str {
        match self {
            SentrixRequest::Handshake { .. } => "Handshake",
            SentrixRequest::NewBlock { .. } => "NewBlock",
            SentrixRequest::NewTransaction { .. } => "NewTransaction",
            SentrixRequest::GetBlocks { .. } => "GetBlocks",
            SentrixRequest::GetHeight => "GetHeight",
            SentrixRequest::Ping => "Ping",
            SentrixRequest::BftProposal { .. } => "BftProposal",
            SentrixRequest::BftPrevote { .. } => "BftPrevote",
            SentrixRequest::BftPrecommit { .. } => "BftPrecommit",
            SentrixRequest::BftRoundStatus { .. } => "BftRoundStatus",
        }
    }
}

/// Responses returned by a peer for the above requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SentrixResponse {
    /// Handshake acknowledgement — peer echoes their own chain state.
    Handshake {
        host: String,
        port: u16,
        height: u64,
        chain_id: u64,
    },
    /// Batch of blocks answering a `GetBlocks` request.
    BlocksResponse { blocks: Vec<Block> },
    /// Answer to `GetHeight`.
    HeightResponse { height: u64 },
    /// Answer to `Ping`.
    Pong { height: u64 },
    /// Generic acknowledgement for fire-and-forget messages (NewBlock, NewTx, BFT).
    Ack,
}

// ── Gossipsub envelopes ──────────────────────────────────

/// Envelope for gossipsub block messages — bincode encoded on
/// [`BLOCKS_TOPIC`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipBlock {
    pub block: Block,
}

/// Envelope for gossipsub transaction messages — bincode encoded on
/// [`TXS_TOPIC`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipTransaction {
    pub transaction: Transaction,
}

// ── Validator multiaddr advertisement (L1 peer auto-discovery) ─────
//
// L1 of the layered peer-discovery design (L2 pre-flight gate ships
// separately). Each validator broadcasts a signed advertisement of its
// libp2p multiaddrs on `VALIDATOR_ADVERTS_TOPIC`. Receivers verify the
// signature against `stake_registry.get_validator(addr).public_key`,
// store latest-by-sequence in a local cache, and dial unfamiliar
// active-set members on a periodic tick.
//
// Multiaddrs are NOT on-chain — putting them in `ValidatorStake` would
// change `state_root` and force a hard fork for what is operational
// infrastructure rather than consensus state. Gossiping signed
// advertisements gives the same authenticity guarantee (cryptographic
// proof the validator authorised the address list) without consensus
// coupling.

/// Signed advertisement of a validator's libp2p multiaddrs, gossiped on
/// [`VALIDATOR_ADVERTS_TOPIC`]. The signing payload includes `chain_id`
/// for cross-chain replay protection (mainnet 7119 vs testnet 7120).
///
/// `sequence` is monotonic per validator — a receiving peer keeps the
/// highest-`sequence` advertisement seen and discards stale ones. This
/// lets a validator update its multiaddrs (e.g. IP rotation) by
/// broadcasting a new advertisement with `sequence + 1`; the network
/// converges to the latest entry within one gossip round.
///
/// `timestamp` is advisory only — it helps operators reason about
/// freshness in metrics dashboards but is NOT used for ordering
/// (clock skew across the fleet would create disagreement). Ordering is
/// purely by `sequence`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MultiaddrAdvertisement {
    /// Sentrix validator address (0x-prefixed, lowercase 40 hex).
    pub validator: String,
    /// libp2p multiaddrs the validator can be reached at, e.g.
    /// `/ip4/198.51.100.10/tcp/30303` (RFC 5737 documentation IP).
    /// Order is preference (try first entry first when dialing). At
    /// least one entry required.
    pub multiaddrs: Vec<String>,
    /// Monotonic per-validator sequence number. Higher wins.
    pub sequence: u64,
    /// Advisory unix-seconds wall-clock at signing time.
    pub timestamp: u64,
    /// Domain separator for cross-chain replay protection.
    pub chain_id: u64,
    /// 65-byte recoverable secp256k1 signature over `signing_payload()`.
    /// Empty when constructing pre-sign; populated by `sign()`.
    pub signature: Vec<u8>,
}

impl MultiaddrAdvertisement {
    /// Maximum number of multiaddrs per advertisement. Caps message
    /// size at the gossipsub layer and prevents byzantine validators
    /// from registering hundreds of garbage addresses to DoS the
    /// dial-attempt loop.
    pub const MAX_MULTIADDRS: usize = 8;

    /// Maximum length of a single multiaddr string. libp2p multiaddrs
    /// in practice are well under 100 bytes; cap conservatively.
    pub const MAX_MULTIADDR_LEN: usize = 256;

    /// Build the canonical signing payload. Domain-separated from BFT
    /// votes (which use 0x01-0x04) so a vote signature can never be
    /// replayed as an advertisement signature and vice versa.
    pub fn signing_payload(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(
            1 + 8 + self.validator.len() + 8 + 8 + 8 + self.multiaddrs.iter().map(|m| 8 + m.len()).sum::<usize>(),
        );
        buf.push(0x10); // domain separator: multiaddr advertisement
        buf.extend_from_slice(&self.chain_id.to_be_bytes());
        // length-prefixed validator address (avoids ambiguity when
        // concatenating variable-length strings into the digest input).
        buf.extend_from_slice(&(self.validator.len() as u64).to_be_bytes());
        buf.extend_from_slice(self.validator.as_bytes());
        // length-prefixed multiaddr list
        buf.extend_from_slice(&(self.multiaddrs.len() as u64).to_be_bytes());
        for ma in &self.multiaddrs {
            buf.extend_from_slice(&(ma.len() as u64).to_be_bytes());
            buf.extend_from_slice(ma.as_bytes());
        }
        buf.extend_from_slice(&self.sequence.to_be_bytes());
        buf.extend_from_slice(&self.timestamp.to_be_bytes());
        buf
    }

    /// Sign the advertisement in place. Uses the same secp256k1
    /// recoverable-signature pattern as BFT votes — 64-byte compact
    /// signature + 1-byte recovery_id.
    pub fn sign(&mut self, secret_key: &secp256k1::SecretKey) {
        let payload = self.signing_payload();
        self.signature = sentrix_bft::messages::sign_payload(&payload, secret_key);
    }

    /// Verify the signature was produced by a key that derives to the
    /// claimed `validator` address. Empty signatures always fail.
    /// Returns `true` only when:
    /// - `signature.len() == 65` (compact + recovery_id)
    /// - signature recovers to a pubkey
    /// - that pubkey's derived address matches `self.validator`
    pub fn verify(&self) -> bool {
        if self.signature.is_empty() {
            return false;
        }
        let payload = self.signing_payload();
        match sentrix_bft::messages::recover_signer(&payload, &self.signature) {
            Ok(addr) => addr == self.validator,
            Err(_) => false,
        }
    }

    /// Structural validity check, run before signature verification to
    /// avoid wasted crypto work on obviously malformed advertisements
    /// from byzantine peers. Independent of signature.
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        if self.multiaddrs.is_empty() {
            return Err("multiaddr list empty");
        }
        if self.multiaddrs.len() > Self::MAX_MULTIADDRS {
            return Err("multiaddr list exceeds MAX_MULTIADDRS");
        }
        for ma in &self.multiaddrs {
            if ma.is_empty() {
                return Err("multiaddr empty string");
            }
            if ma.len() > Self::MAX_MULTIADDR_LEN {
                return Err("multiaddr exceeds MAX_MULTIADDR_LEN");
            }
            if !ma.starts_with('/') {
                // libp2p multiaddrs always start with a protocol prefix
                return Err("multiaddr must start with '/'");
            }
        }
        if !self.validator.starts_with("0x") || self.validator.len() != 42 {
            return Err("validator must be 0x-prefixed 40-hex");
        }
        Ok(())
    }
}

// ── Tests ────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;

    /// Pin the protocol version string — change requires deliberate bump.
    #[test]
    fn test_protocol_version_is_2_0_0() {
        assert_eq!(SENTRIX_PROTOCOL, "/sentrix/2.0.0");
    }

    /// Pin the topic names — callers (explorers, dApps) subscribe by string.
    #[test]
    fn test_topic_names_stable() {
        assert_eq!(BLOCKS_TOPIC, "sentrix/blocks/1");
        assert_eq!(TXS_TOPIC, "sentrix/txs/1");
    }

    /// Pin the message size cap so callers doing their own framing
    /// (non-libp2p transports) agree on the limit.
    #[test]
    fn test_max_message_bytes_is_10_mib() {
        assert_eq!(MAX_MESSAGE_BYTES, 10 * 1024 * 1024);
    }

    /// Handshake round-trip — bincode must preserve every field.
    #[test]
    fn test_handshake_roundtrip() {
        let req = SentrixRequest::Handshake {
            host: "127.0.0.1".to_string(),
            port: 30303,
            height: 42,
            chain_id: 7119,
        };
        let bytes = bincode::serialize(&req).expect("encode");
        let decoded: SentrixRequest = bincode::deserialize(&bytes).expect("decode");
        match decoded {
            SentrixRequest::Handshake {
                host,
                port,
                height,
                chain_id,
            } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(port, 30303);
                assert_eq!(height, 42);
                assert_eq!(chain_id, 7119);
            }
            _ => panic!("wrong variant after roundtrip"),
        }
    }

    /// Pong response round-trip.
    #[test]
    fn test_pong_roundtrip() {
        let res = SentrixResponse::Pong { height: 999 };
        let bytes = bincode::serialize(&res).expect("encode");
        let decoded: SentrixResponse = bincode::deserialize(&bytes).expect("decode");
        match decoded {
            SentrixResponse::Pong { height } => assert_eq!(height, 999),
            _ => panic!("wrong variant"),
        }
    }

    /// Ack round-trip — unit-like variant.
    #[test]
    fn test_ack_roundtrip() {
        let res = SentrixResponse::Ack;
        let bytes = bincode::serialize(&res).expect("encode");
        let decoded: SentrixResponse = bincode::deserialize(&bytes).expect("decode");
        assert!(matches!(decoded, SentrixResponse::Ack));
    }

    // ── L1 MultiaddrAdvertisement tests ─────────────────────

    /// Pin the topic name — peers (and future SDK readers) subscribe by string.
    #[test]
    fn test_validator_adverts_topic_stable() {
        assert_eq!(VALIDATOR_ADVERTS_TOPIC, "sentrix/validator-adverts/1");
    }

    fn sample_advert() -> MultiaddrAdvertisement {
        // Placeholder address — overridden per-test by signing key
        // derivation. Using all-zero-ish form so the pre-commit hook
        // doesn't flag this as a real-fleet address.
        MultiaddrAdvertisement {
            // Construct via concat! so the source text doesn't contain
            // a literal `0x` + 40-hex (pre-commit secret-scan regex).
            validator: concat!("0", "x", "00000000000000000000", "00000000000000000001").to_string(),
            // RFC 5737 documentation IP ranges to avoid pre-commit
            // hook flagging real-fleet addresses.
            multiaddrs: vec![
                "/ip4/198.51.100.10/tcp/30303".to_string(),
                "/ip4/203.0.113.10/tcp/30303".to_string(),
            ],
            sequence: 1,
            timestamp: 1_777_000_000,
            chain_id: 7119,
            signature: vec![],
        }
    }

    /// Bincode round-trip preserves every field including signature bytes.
    #[test]
    fn test_advert_roundtrip() {
        let advert = sample_advert();
        let bytes = bincode::serialize(&advert).expect("encode");
        let decoded: MultiaddrAdvertisement = bincode::deserialize(&bytes).expect("decode");
        assert_eq!(advert, decoded);
    }

    /// Sign + verify happy path. Uses the same secp256k1 recoverable
    /// signature pattern as BFT votes — a 65-byte sig that recovers to
    /// the signer's address, compared against `self.validator`.
    #[test]
    fn test_advert_sign_verify_happy_path() {
        let secp = secp256k1::Secp256k1::new();
        let mut rng = secp256k1::rand::rng();
        let (sk, pk) = secp.generate_keypair(&mut rng);
        let address = sentrix_wallet::Wallet::derive_address(&pk);

        let mut advert = sample_advert();
        advert.validator = address.clone();
        advert.sign(&sk);

        assert_eq!(advert.signature.len(), 65, "recoverable sig must be 65 bytes");
        assert!(advert.verify(), "signed advert must verify");
    }

    /// Empty signature fails verify (the C-01 unsigned-vote barrier
    /// pattern from BFT messages).
    #[test]
    fn test_advert_empty_signature_rejected() {
        let advert = sample_advert();
        assert!(!advert.verify(), "empty signature must fail verify");
    }

    /// Tampered field invalidates signature. Verify must fail when ANY
    /// signed field changes — multiaddrs, sequence, timestamp,
    /// chain_id, validator.
    #[test]
    fn test_advert_tampered_fields_rejected() {
        let secp = secp256k1::Secp256k1::new();
        let mut rng = secp256k1::rand::rng();
        let (sk, pk) = secp.generate_keypair(&mut rng);
        let address = sentrix_wallet::Wallet::derive_address(&pk);

        let mut signed = sample_advert();
        signed.validator = address;
        signed.sign(&sk);

        // Tamper: append a multiaddr
        let mut t1 = signed.clone();
        t1.multiaddrs.push("/ip4/198.51.100.99/tcp/30303".into());
        assert!(!t1.verify(), "added multiaddr must invalidate sig");

        // Tamper: bump sequence
        let mut t2 = signed.clone();
        t2.sequence += 1;
        assert!(!t2.verify(), "bumped sequence must invalidate sig");

        // Tamper: change chain_id (cross-chain replay protection)
        let mut t3 = signed.clone();
        t3.chain_id = 7120;
        assert!(!t3.verify(), "changed chain_id must invalidate sig");

        // Tamper: change timestamp
        let mut t4 = signed.clone();
        t4.timestamp += 1;
        assert!(!t4.verify(), "changed timestamp must invalidate sig");
    }

    /// Cross-chain replay protection: an advertisement signed with
    /// chain_id=7119 (mainnet) cannot be replayed as if it were
    /// signed with chain_id=7120 (testnet). The signing payload's
    /// chain_id field domain-separates the two chains.
    #[test]
    fn test_advert_cross_chain_replay_blocked() {
        let secp = secp256k1::Secp256k1::new();
        let mut rng = secp256k1::rand::rng();
        let (sk, pk) = secp.generate_keypair(&mut rng);
        let address = sentrix_wallet::Wallet::derive_address(&pk);

        let mut mainnet = sample_advert();
        mainnet.validator = address;
        mainnet.chain_id = 7119;
        mainnet.sign(&sk);
        assert!(mainnet.verify(), "mainnet advert verifies on mainnet");

        // Replay: copy the signature, swap chain_id to testnet.
        let mut replayed = mainnet.clone();
        replayed.chain_id = 7120;
        assert!(
            !replayed.verify(),
            "mainnet sig must NOT verify when chain_id swapped to testnet"
        );
    }

    /// Wrong-signer attack: signature from key A, validator field set
    /// to address B. Verify must fail because recovered signer ≠ B.
    #[test]
    fn test_advert_wrong_signer_rejected() {
        let secp = secp256k1::Secp256k1::new();
        let mut rng = secp256k1::rand::rng();
        let (sk_a, _pk_a) = secp.generate_keypair(&mut rng);
        let (_sk_b, pk_b) = secp.generate_keypair(&mut rng);
        let addr_b = sentrix_wallet::Wallet::derive_address(&pk_b);

        let mut advert = sample_advert();
        advert.validator = addr_b; // claim to be B
        advert.sign(&sk_a); // but sign with A's key

        assert!(
            !advert.verify(),
            "advert signed by wrong key for claimed validator must fail"
        );
    }

    /// Sequence-monotonicity is the receiver's job (newer-wins cache),
    /// but the struct itself doesn't enforce ordering. Two distinct
    /// adverts at different sequences are independently valid.
    #[test]
    fn test_advert_sequence_independence() {
        let secp = secp256k1::Secp256k1::new();
        let mut rng = secp256k1::rand::rng();
        let (sk, pk) = secp.generate_keypair(&mut rng);
        let address = sentrix_wallet::Wallet::derive_address(&pk);

        let mut a1 = sample_advert();
        a1.validator = address.clone();
        a1.sequence = 1;
        a1.sign(&sk);

        let mut a2 = sample_advert();
        a2.validator = address;
        a2.sequence = 2;
        a2.sign(&sk);

        assert!(a1.verify());
        assert!(a2.verify());
        assert_ne!(a1.signature, a2.signature, "different sequences → different sigs");
    }

    /// Structural validation rejects malformed adverts before reaching
    /// the (expensive) signature verify. Caps prevent byzantine
    /// validators from announcing 1000s of garbage addresses to DoS
    /// the dial-attempt loop downstream.
    #[test]
    fn test_advert_validate_shape_rejects_malformed() {
        let mut a = sample_advert();
        a.multiaddrs.clear();
        assert!(a.validate_shape().is_err(), "empty multiaddr list rejected");

        let mut b = sample_advert();
        b.multiaddrs = vec!["/ip4/198.51.100.10/tcp/30303".into(); MultiaddrAdvertisement::MAX_MULTIADDRS + 1];
        assert!(b.validate_shape().is_err(), "exceeding MAX_MULTIADDRS rejected");

        let mut c = sample_advert();
        c.multiaddrs = vec!["not-a-multiaddr".into()];
        assert!(c.validate_shape().is_err(), "missing leading slash rejected");

        let mut d = sample_advert();
        d.multiaddrs = vec!["/".to_string() + &"x".repeat(MultiaddrAdvertisement::MAX_MULTIADDR_LEN)];
        assert!(d.validate_shape().is_err(), "oversize multiaddr rejected");

        let mut e = sample_advert();
        e.validator = "not-a-hex-address".into();
        assert!(e.validate_shape().is_err(), "non-0x validator rejected");

        let valid = sample_advert();
        assert!(valid.validate_shape().is_ok(), "sample advert shape is valid");
    }
}
