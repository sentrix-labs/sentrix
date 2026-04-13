// behaviour.rs - Sentrix — libp2p composite NetworkBehaviour
//
// Combines:
//   - Identify  : exchange peer metadata (protocol version, pubkey, observed addr)
//   - RequestResponse : typed block/tx/handshake protocol over the Noise+Yamux transport
//
// Message types mirror node.rs exactly so the two paths stay compatible during the
// migration.  node.rs is untouched until Step 3c.

#![allow(dead_code)]

use std::io;

use async_trait::async_trait;
use futures::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::{
    identify,
    identity::PublicKey,
    request_response::{self, ProtocolSupport},
    swarm::NetworkBehaviour,
};
use serde::{Deserialize, Serialize};

use crate::core::block::Block;
use crate::core::transaction::Transaction;

// ── Protocol identifier ──────────────────────────────────
/// Sent as the protocol string during Noise handshake and Identify.
pub const SENTRIX_PROTOCOL: &str = "/sentrix/1.0.0";

/// Hard cap on a single message (10 MiB) — matches `MAX_MESSAGE_SIZE` in node.rs.
const MAX_MESSAGE_BYTES: usize = 10 * 1024 * 1024;

// ── Request / Response enums ─────────────────────────────

/// Messages a node sends to a peer (requests).
///
/// Mirrors [`crate::network::node::Message`] but split into request/response
/// halves so libp2p's `RequestResponse` behaviour can track correlation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum SentrixRequest {
    /// Initial handshake — carries chain_id for network partitioning.
    Handshake { host: String, port: u16, height: u64, chain_id: u64 },
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
}

/// Responses returned by a peer for the above requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum SentrixResponse {
    /// Handshake acknowledgement — peer echoes their own chain state.
    Handshake { host: String, port: u16, height: u64, chain_id: u64 },
    /// Batch of blocks answering a `GetBlocks` request.
    BlocksResponse { blocks: Vec<Block> },
    /// Answer to `GetHeight`.
    HeightResponse { height: u64 },
    /// Answer to `Ping`.
    Pong { height: u64 },
    /// Generic acknowledgement for fire-and-forget messages (NewBlock, NewTx).
    Ack,
}

// ── Wire codec ───────────────────────────────────────────
//
// Wire format: 4-byte big-endian length prefix + JSON body.
// Matches the existing framing in `node.rs` so legacy nodes can interop.

/// Length-prefixed JSON codec for [`SentrixRequest`] / [`SentrixResponse`].
#[derive(Debug, Clone, Default)]
pub struct SentrixCodec;

#[async_trait]
impl request_response::Codec for SentrixCodec {
    type Protocol = String;
    type Request  = SentrixRequest;
    type Response = SentrixResponse;

    async fn read_request<T>(&mut self, _: &Self::Protocol, io: &mut T)
        -> io::Result<Self::Request>
    where T: AsyncRead + Unpin + Send
    {
        lp_read(io).await
    }

    async fn read_response<T>(&mut self, _: &Self::Protocol, io: &mut T)
        -> io::Result<Self::Response>
    where T: AsyncRead + Unpin + Send
    {
        lp_read(io).await
    }

    async fn write_request<T>(&mut self, _: &Self::Protocol, io: &mut T, req: Self::Request)
        -> io::Result<()>
    where T: AsyncWrite + Unpin + Send
    {
        lp_write(io, &req).await
    }

    async fn write_response<T>(&mut self, _: &Self::Protocol, io: &mut T, res: Self::Response)
        -> io::Result<()>
    where T: AsyncWrite + Unpin + Send
    {
        lp_write(io, &res).await
    }
}

// ── Framing helpers ──────────────────────────────────────

async fn lp_read<T, D>(io: &mut T) -> io::Result<D>
where
    T: AsyncRead + Unpin + Send,
    D: for<'de> Deserialize<'de>,
{
    let mut len_buf = [0u8; 4];
    io.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    let mut buf = vec![0u8; len];
    io.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

async fn lp_write<T, S>(io: &mut T, val: &S) -> io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
    S: Serialize,
{
    let json = serde_json::to_vec(val)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if json.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    let len = (json.len() as u32).to_be_bytes();
    io.write_all(&len).await?;
    io.write_all(&json).await?;
    io.flush().await?;
    Ok(())
}

// ── Composite behaviour ──────────────────────────────────

/// Combined libp2p behaviour for Sentrix P2P nodes.
///
/// Events are surfaced as `SentrixBehaviourEvent` (auto-generated by the derive macro):
/// - `SentrixBehaviourEvent::Identify(identify::Event)` — peer info updates
/// - `SentrixBehaviourEvent::Rr(request_response::Event<...>)` — incoming messages
#[derive(NetworkBehaviour)]
pub struct SentrixBehaviour {
    /// Identify protocol: exchange pubkey + observed addresses on connect.
    pub identify: identify::Behaviour,
    /// Request-response: block sync, handshake, tx exchange.
    pub rr: request_response::Behaviour<SentrixCodec>,
}

impl SentrixBehaviour {
    /// Create behaviour for a node with the given local public key.
    ///
    /// Typically called from `SwarmBuilder::with_behaviour(|key| SentrixBehaviour::new(key.public()))`.
    pub fn new(local_public_key: PublicKey) -> Self {
        let identify = identify::Behaviour::new(
            identify::Config::new(SENTRIX_PROTOCOL.to_string(), local_public_key),
        );

        let rr = request_response::Behaviour::new(
            [(SENTRIX_PROTOCOL.to_string(), ProtocolSupport::Full)],
            request_response::Config::default(),
        );

        Self { identify, rr }
    }
}

// ── Tests ────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::identity;
    use libp2p::request_response::Codec;

    fn make_keypair() -> identity::Keypair {
        identity::Keypair::generate_ed25519()
    }

    // ── Codec round-trip tests ───────────────────────────

    #[tokio::test]
    async fn test_codec_roundtrip_get_height() {
        let req = SentrixRequest::GetHeight;
        let mut buf = Vec::<u8>::new();
        let mut codec = SentrixCodec;
        codec.write_request(&SENTRIX_PROTOCOL.to_string(), &mut buf, req.clone()).await
            .expect("write_request failed");

        let decoded: SentrixRequest = codec
            .read_request(&SENTRIX_PROTOCOL.to_string(), &mut buf.as_slice())
            .await
            .expect("read_request failed");

        assert!(matches!(decoded, SentrixRequest::GetHeight));
    }

    #[tokio::test]
    async fn test_codec_roundtrip_handshake_request() {
        let req = SentrixRequest::Handshake {
            host: "127.0.0.1".to_string(),
            port: 30303,
            height: 42,
            chain_id: 7119,
        };
        let mut buf = Vec::<u8>::new();
        let mut codec = SentrixCodec;
        codec.write_request(&SENTRIX_PROTOCOL.to_string(), &mut buf, req).await
            .expect("write failed");

        let decoded = codec
            .read_request(&SENTRIX_PROTOCOL.to_string(), &mut buf.as_slice())
            .await
            .expect("read failed");

        match decoded {
            SentrixRequest::Handshake { height, chain_id, port, .. } => {
                assert_eq!(height, 42);
                assert_eq!(chain_id, 7119);
                assert_eq!(port, 30303);
            }
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_codec_roundtrip_blocks_response() {
        let res = SentrixResponse::BlocksResponse { blocks: vec![] };
        let mut buf = Vec::<u8>::new();
        let mut codec = SentrixCodec;
        codec.write_response(&SENTRIX_PROTOCOL.to_string(), &mut buf, res).await
            .expect("write failed");

        let decoded = codec
            .read_response(&SENTRIX_PROTOCOL.to_string(), &mut buf.as_slice())
            .await
            .expect("read failed");

        assert!(matches!(decoded, SentrixResponse::BlocksResponse { .. }));
    }

    #[tokio::test]
    async fn test_codec_roundtrip_pong() {
        let res = SentrixResponse::Pong { height: 100 };
        let mut buf = Vec::<u8>::new();
        let mut codec = SentrixCodec;
        codec.write_response(&SENTRIX_PROTOCOL.to_string(), &mut buf, res).await
            .expect("write failed");

        let decoded = codec
            .read_response(&SENTRIX_PROTOCOL.to_string(), &mut buf.as_slice())
            .await
            .expect("read failed");

        assert!(matches!(decoded, SentrixResponse::Pong { height: 100 }));
    }

    #[tokio::test]
    async fn test_codec_rejects_oversized_message() {
        // Write a fake 4-byte length > MAX_MESSAGE_BYTES
        let huge_len: u32 = (MAX_MESSAGE_BYTES + 1) as u32;
        let buf = huge_len.to_be_bytes().to_vec();
        let mut codec = SentrixCodec;
        let result: io::Result<SentrixRequest> = codec
            .read_request(&SENTRIX_PROTOCOL.to_string(), &mut buf.as_slice())
            .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    // ── Behaviour construction ───────────────────────────

    #[test]
    fn test_behaviour_new_succeeds() {
        let kp = make_keypair();
        // Should not panic
        let _behaviour = SentrixBehaviour::new(kp.public());
    }

    #[test]
    fn test_peer_ids_are_unique() {
        let kp1 = make_keypair();
        let kp2 = make_keypair();
        let pid1 = libp2p::PeerId::from_public_key(&kp1.public());
        let pid2 = libp2p::PeerId::from_public_key(&kp2.public());
        assert_ne!(pid1, pid2, "each keypair must yield a unique PeerId");
    }
}
