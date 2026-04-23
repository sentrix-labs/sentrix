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
}
