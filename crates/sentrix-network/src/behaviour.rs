// behaviour.rs - Sentrix — libp2p composite NetworkBehaviour
//
// Combines:
//   - Identify  : exchange peer metadata (protocol version, pubkey, observed addr)
//   - Kademlia  : DHT-based automatic peer discovery
//   - Gossipsub : pub/sub block + transaction propagation
//   - RequestResponse : typed block sync / handshake protocol (bincode wire format)

#![allow(dead_code)]

use std::io;
use std::time::Duration;

use async_trait::async_trait;
use futures::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::{
    PeerId, gossipsub, identify,
    identity::PublicKey,
    kad,
    request_response::{self, ProtocolSupport},
    swarm::NetworkBehaviour,
};
use serde::{Deserialize, Serialize};

use sentrix_bft::messages::{Precommit, Prevote, Proposal};
use sentrix_primitives::block::Block;
use sentrix_primitives::transaction::Transaction;

// ── Protocol identifier ──────────────────────────────────
/// Protocol version string — bumped to 2.0.0 for bincode wire format.
pub const SENTRIX_PROTOCOL: &str = "/sentrix/2.0.0";

// ── Gossipsub topic names ────────────────────────────────
/// Topic for block propagation via gossipsub.
pub const BLOCKS_TOPIC: &str = "sentrix/blocks/1";
/// Topic for transaction propagation via gossipsub.
pub const TXS_TOPIC: &str = "sentrix/txs/1";

/// Hard cap on a single message (10 MiB) — matches `MAX_MESSAGE_SIZE` in node.rs.
const MAX_MESSAGE_BYTES: usize = 10 * 1024 * 1024;

// ── Tunable gossipsub + RR parameters ─────────────────────────
//
// Default values below are aggressively tuned for Sentrix's current small
// validator meshes (3 mainnet + 4 testnet, separate meshes per chain_id).
// When the active set grows (external validators onboarding, Voyager DPoS
// post-fork, hundreds-to-thousands of validators later), several of these
// become actively harmful — most notably `flood_publish`, which at 1000
// nodes turns block broadcast into a 1000-peer flood per publish (~500 MB/s
// per publishing validator at 1 block/s — self-DDoS).
//
// Every tunable is overridable via env var so operators can retune for
// their actual mesh size WITHOUT rebuilding the binary:
//
//   SENTRIX_GOSSIP_HEARTBEAT_MS      default 300   — raise to 1000+ at >100 nodes
//   SENTRIX_GOSSIP_FLOOD_PUBLISH     default true  — SET "false" at >~30 nodes
//   SENTRIX_GOSSIP_MESH_N            default 6     — standard libp2p value
//   SENTRIX_GOSSIP_MESH_N_LOW        default 2     — raise to 5 at >~30 nodes
//   SENTRIX_GOSSIP_MESH_N_HIGH       default 8     — raise to 12 at >~30 nodes
//   SENTRIX_GOSSIP_HISTORY_LENGTH    default 6     — raise to 10+ at scale (memory cheap, bandwidth dear)
//   SENTRIX_GOSSIP_HISTORY_GOSSIP    default 3     — standard libp2p value
//   SENTRIX_RR_REQUEST_TIMEOUT_SECS  default 15    — already safe at scale
//
// Recommended rollout at network growth:
//   3-30 nodes:   defaults (tiny-mesh tuning — what's shipped today)
//   30-100 nodes: FLOOD_PUBLISH=false, MESH_N_LOW=5, MESH_N_HIGH=12, HISTORY_LENGTH=10
//   >100 nodes:   + HEARTBEAT_MS=1000 (libp2p default, less CPU per-node)

/// Parse an env var as T, falling back to `default` on missing / unparseable input.
/// Emits a WARN trace if the value was set but didn't parse — otherwise silent.
fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    match std::env::var(key) {
        Ok(raw) => match raw.parse::<T>() {
            Ok(v) => v,
            Err(_) => {
                tracing::warn!(
                    "sentrix-network: {} set to {:?} but failed to parse — using default",
                    key, raw
                );
                default
            }
        },
        Err(_) => default,
    }
}

/// Parse a boolean env var — accepts "true" / "1" / "yes" (case-insensitive) as true,
/// "false" / "0" / "no" as false. Anything else falls back to `default`.
fn env_bool(key: &str, default: bool) -> bool {
    match std::env::var(key) {
        Ok(raw) => match raw.trim().to_lowercase().as_str() {
            "true" | "1" | "yes" => true,
            "false" | "0" | "no" => false,
            _ => {
                tracing::warn!(
                    "sentrix-network: {} set to {:?} but not a boolean — using default {}",
                    key, raw, default
                );
                default
            }
        },
        Err(_) => default,
    }
}

/// Build the gossipsub config used by both `new` and `new_with_keypair`.
/// Reads all tunables via env vars; see the module-level comment above for
/// the full parameter list + recommended values per mesh size.
fn gossipsub_config() -> gossipsub::Config {
    let heartbeat_ms: u64 = env_or("SENTRIX_GOSSIP_HEARTBEAT_MS", 300);
    let flood_publish = env_bool("SENTRIX_GOSSIP_FLOOD_PUBLISH", true);
    let mesh_n: usize = env_or("SENTRIX_GOSSIP_MESH_N", 6);
    let mesh_n_low: usize = env_or("SENTRIX_GOSSIP_MESH_N_LOW", 2);
    let mesh_n_high: usize = env_or("SENTRIX_GOSSIP_MESH_N_HIGH", 8);
    let history_length: usize = env_or("SENTRIX_GOSSIP_HISTORY_LENGTH", 6);
    let history_gossip: usize = env_or("SENTRIX_GOSSIP_HISTORY_GOSSIP", 3);

    gossipsub::ConfigBuilder::default()
        .heartbeat_interval(Duration::from_millis(heartbeat_ms))
        .heartbeat_initial_delay(Duration::from_millis(100))
        .flood_publish(flood_publish)
        .mesh_n(mesh_n)
        .mesh_n_low(mesh_n_low)
        .mesh_n_high(mesh_n_high)
        .mesh_outbound_min(1)
        .gossip_factor(0.25)
        .history_length(history_length)
        .history_gossip(history_gossip)
        .validation_mode(gossipsub::ValidationMode::Strict)
        .max_transmit_size(MAX_MESSAGE_BYTES)
        .build()
        .expect("valid gossipsub config")
}

/// Request-response timeout for the unified BFT-vote / block-sync protocol.
///
/// Overridable via `SENTRIX_RR_REQUEST_TIMEOUT_SECS`. Default 15s — BFT votes
/// get an immediate Ack from the peer, so 60s (the libp2p default) only
/// slowed dead-peer detection. 15s is comfortable for small-block sync
/// responses and already safe at scale — no need to retune at network growth.
fn rr_request_timeout_secs() -> u64 {
    env_or("SENTRIX_RR_REQUEST_TIMEOUT_SECS", 15u64)
}

// ── Request / Response enums ─────────────────────────────

/// Messages a node sends to a peer (requests).
///
/// Mirrors [`crate::network::node::Message`] but split into request/response
/// halves so libp2p's `RequestResponse` behaviour can track correlation.
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
    /// BFT: block proposal from the round proposer
    BftProposal { proposal: Box<Proposal> },
    /// BFT: prevote for a block (or nil)
    BftPrevote { prevote: Prevote },
    /// BFT: precommit for a block (or nil)
    BftPrecommit { precommit: Precommit },
    /// BFT: periodic round status announcement for round synchronization
    BftRoundStatus {
        status: sentrix_bft::messages::RoundStatus,
    },
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

// ── Wire codec ───────────────────────────────────────────
//
// Wire format: 4-byte big-endian length prefix + bincode body.
// Switched from JSON in v2.0.0 for ~3-5x smaller messages and faster ser/de.

/// Length-prefixed bincode codec for [`SentrixRequest`] / [`SentrixResponse`].
#[derive(Debug, Clone, Default)]
pub struct SentrixCodec;

#[async_trait]
impl request_response::Codec for SentrixCodec {
    type Protocol = String;
    type Request = SentrixRequest;
    type Response = SentrixResponse;

    async fn read_request<T>(&mut self, _: &Self::Protocol, io: &mut T) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        lp_read(io).await
    }

    async fn read_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        lp_read(io).await
    }

    async fn write_request<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        lp_write(io, &req).await
    }

    async fn write_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
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
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut buf = vec![0u8; len];
    io.read_exact(&mut buf).await?;
    bincode::deserialize(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

async fn lp_write<T, S>(io: &mut T, val: &S) -> io::Result<()>
where
    T: AsyncWrite + Unpin + Send,
    S: Serialize,
{
    let bytes =
        bincode::serialize(val).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let len = (bytes.len() as u32).to_be_bytes();
    io.write_all(&len).await?;
    io.write_all(&bytes).await?;
    io.flush().await?;
    Ok(())
}

// ── Composite behaviour ──────────────────────────────────

/// Combined libp2p behaviour for Sentrix P2P nodes.
///
/// Events are surfaced as `SentrixBehaviourEvent` (auto-generated by the derive macro):
/// - `SentrixBehaviourEvent::Identify(identify::Event)` — peer info updates
/// - `SentrixBehaviourEvent::Kademlia(kad::Event)` — DHT peer discovery
/// - `SentrixBehaviourEvent::Gossipsub(gossipsub::Event)` — pub/sub messages
/// - `SentrixBehaviourEvent::Rr(request_response::Event<...>)` — sync/handshake
#[derive(NetworkBehaviour)]
pub struct SentrixBehaviour {
    /// Identify protocol: exchange pubkey + observed addresses on connect.
    pub identify: identify::Behaviour,
    /// Kademlia DHT: automatic peer discovery.
    pub kademlia: kad::Behaviour<kad::store::MemoryStore>,
    /// Gossipsub: pub/sub for block and transaction propagation.
    pub gossipsub: gossipsub::Behaviour,
    /// Request-response: block sync, handshake, height queries.
    pub rr: request_response::Behaviour<SentrixCodec>,
}

impl SentrixBehaviour {
    /// Create behaviour for a node with the given local peer ID and public key.
    #[allow(clippy::expect_used)] // All expects use compile-time-known valid inputs
    pub fn new(local_peer_id: PeerId, local_public_key: PublicKey) -> Self {
        // Identify
        let identify = identify::Behaviour::new(identify::Config::new(
            SENTRIX_PROTOCOL.to_string(),
            local_public_key,
        ));

        // Kademlia DHT for peer discovery
        let store = kad::store::MemoryStore::new(local_peer_id);
        let kad_protocol = libp2p::StreamProtocol::try_from_owned("/sentrix/kad/1.0.0".to_string())
            .expect("valid protocol");
        let mut kad_config = kad::Config::new(kad_protocol);
        kad_config.set_query_timeout(Duration::from_secs(30));
        let kademlia = kad::Behaviour::with_config(local_peer_id, store, kad_config);

        // Gossipsub for block + tx propagation
        let mut gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(
                // Placeholder — real keypair injected via new_with_keypair
                libp2p::identity::Keypair::generate_ed25519(),
            ),
            gossipsub_config(),
        )
        .expect("valid gossipsub behaviour");

        // Subscribe to block and transaction topics
        let blocks_topic = gossipsub::IdentTopic::new(BLOCKS_TOPIC);
        let txs_topic = gossipsub::IdentTopic::new(TXS_TOPIC);
        gossipsub
            .subscribe(&blocks_topic)
            .expect("subscribe blocks");
        gossipsub.subscribe(&txs_topic).expect("subscribe txs");

        // Request-response for sync + handshake
        let rr_config = request_response::Config::default()
            .with_request_timeout(Duration::from_secs(rr_request_timeout_secs()));
        let rr = request_response::Behaviour::new(
            [(SENTRIX_PROTOCOL.to_string(), ProtocolSupport::Full)],
            rr_config,
        );

        Self {
            identify,
            kademlia,
            gossipsub,
            rr,
        }
    }

    /// Create behaviour with a specific keypair for gossipsub message signing.
    #[allow(clippy::expect_used)] // All expects use compile-time-known valid inputs
    pub fn new_with_keypair(local_peer_id: PeerId, keypair: &libp2p::identity::Keypair) -> Self {
        let local_public_key = keypair.public();

        // Identify
        let identify = identify::Behaviour::new(identify::Config::new(
            SENTRIX_PROTOCOL.to_string(),
            local_public_key,
        ));

        // Kademlia DHT
        let store = kad::store::MemoryStore::new(local_peer_id);
        let kad_protocol = libp2p::StreamProtocol::try_from_owned("/sentrix/kad/1.0.0".to_string())
            .expect("valid protocol");
        let mut kad_config = kad::Config::new(kad_protocol);
        kad_config.set_query_timeout(Duration::from_secs(30));
        let kademlia = kad::Behaviour::with_config(local_peer_id, store, kad_config);

        // Gossipsub with real keypair
        let mut gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(keypair.clone()),
            gossipsub_config(),
        )
        .expect("valid gossipsub behaviour");

        let blocks_topic = gossipsub::IdentTopic::new(BLOCKS_TOPIC);
        let txs_topic = gossipsub::IdentTopic::new(TXS_TOPIC);
        gossipsub
            .subscribe(&blocks_topic)
            .expect("subscribe blocks");
        gossipsub.subscribe(&txs_topic).expect("subscribe txs");

        // Request-response
        let rr_config = request_response::Config::default()
            .with_request_timeout(Duration::from_secs(rr_request_timeout_secs()));
        let rr = request_response::Behaviour::new(
            [(SENTRIX_PROTOCOL.to_string(), ProtocolSupport::Full)],
            rr_config,
        );

        Self {
            identify,
            kademlia,
            gossipsub,
            rr,
        }
    }
}

// ── Gossipsub message types ─────────────────────────────
// Gossipsub carries bincode-encoded envelopes on the two topics.

/// Envelope for gossipsub block messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipBlock {
    pub block: Block,
}

/// Envelope for gossipsub transaction messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GossipTransaction {
    pub transaction: Transaction,
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

    // ── Codec round-trip tests (bincode) ────────────────

    #[tokio::test]
    async fn test_codec_roundtrip_get_height() {
        let req = SentrixRequest::GetHeight;
        let mut buf = Vec::<u8>::new();
        let mut codec = SentrixCodec;
        codec
            .write_request(&SENTRIX_PROTOCOL.to_string(), &mut buf, req.clone())
            .await
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
        codec
            .write_request(&SENTRIX_PROTOCOL.to_string(), &mut buf, req)
            .await
            .expect("write failed");

        let decoded = codec
            .read_request(&SENTRIX_PROTOCOL.to_string(), &mut buf.as_slice())
            .await
            .expect("read failed");

        match decoded {
            SentrixRequest::Handshake {
                height,
                chain_id,
                port,
                ..
            } => {
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
        codec
            .write_response(&SENTRIX_PROTOCOL.to_string(), &mut buf, res)
            .await
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
        codec
            .write_response(&SENTRIX_PROTOCOL.to_string(), &mut buf, res)
            .await
            .expect("write failed");

        let decoded = codec
            .read_response(&SENTRIX_PROTOCOL.to_string(), &mut buf.as_slice())
            .await
            .expect("read failed");

        assert!(matches!(decoded, SentrixResponse::Pong { height: 100 }));
    }

    #[tokio::test]
    async fn test_codec_rejects_oversized_message() {
        let huge_len: u32 = (MAX_MESSAGE_BYTES + 1) as u32;
        let buf = huge_len.to_be_bytes().to_vec();
        let mut codec = SentrixCodec;
        let result: io::Result<SentrixRequest> = codec
            .read_request(&SENTRIX_PROTOCOL.to_string(), &mut buf.as_slice())
            .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_bincode_smaller_than_json() {
        // Verify bincode produces smaller output than JSON for the same message.
        let req = SentrixRequest::Handshake {
            host: "127.0.0.1".to_string(),
            port: 30303,
            height: 140_000,
            chain_id: 7119,
        };
        let bincode_bytes = bincode::serialize(&req).expect("bincode");
        let json_bytes = serde_json::to_vec(&req).expect("json");
        assert!(
            bincode_bytes.len() < json_bytes.len(),
            "bincode ({} bytes) should be smaller than JSON ({} bytes)",
            bincode_bytes.len(),
            json_bytes.len()
        );
    }

    // ── Gossipsub message round-trip ────────────────────

    #[test]
    fn test_gossip_block_roundtrip() {
        let block = Block::new(1, "0".repeat(64), vec![], "validator1".into());
        let msg = GossipBlock { block };
        let encoded = bincode::serialize(&msg).expect("encode");
        let decoded: GossipBlock = bincode::deserialize(&encoded).expect("decode");
        assert_eq!(decoded.block.index, msg.block.index);
    }

    #[test]
    fn test_gossip_transaction_roundtrip() {
        // Construct a minimal transaction for serialization testing (no signature needed).
        let tx = Transaction {
            txid: "test_tx".into(),
            from_address: "addr1".into(),
            to_address: "addr2".into(),
            amount: 100,
            fee: 1,
            nonce: 0,
            data: String::new(),
            timestamp: 0,
            chain_id: 7119,
            signature: String::new(),
            public_key: String::new(),
        };
        let msg = GossipTransaction { transaction: tx };
        let encoded = bincode::serialize(&msg).expect("encode");
        let decoded: GossipTransaction = bincode::deserialize(&encoded).expect("decode");
        assert_eq!(decoded.transaction.txid, msg.transaction.txid);
    }

    // ── Behaviour construction ───────────────────────────

    #[test]
    fn test_behaviour_new_with_keypair_succeeds() {
        let kp = make_keypair();
        let pid = libp2p::PeerId::from_public_key(&kp.public());
        let _behaviour = SentrixBehaviour::new_with_keypair(pid, &kp);
    }

    #[test]
    fn test_peer_ids_are_unique() {
        let kp1 = make_keypair();
        let kp2 = make_keypair();
        let pid1 = libp2p::PeerId::from_public_key(&kp1.public());
        let pid2 = libp2p::PeerId::from_public_key(&kp2.public());
        assert_ne!(pid1, pid2, "each keypair must yield a unique PeerId");
    }

    #[test]
    fn test_topic_constants() {
        assert_eq!(BLOCKS_TOPIC, "sentrix/blocks/1");
        assert_eq!(TXS_TOPIC, "sentrix/txs/1");
    }
}
