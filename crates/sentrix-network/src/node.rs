// node.rs - Sentrix — Legacy P2P TCP Node (DEPRECATED)
//
// SECURITY: The raw TCP transport in this file is PLAINTEXT — no encryption.
// All production P2P traffic MUST use libp2p (Noise XX + Yamux) via
// `libp2p_node.rs`. This file is retained only for shared type definitions
// (NodeEvent, SharedBlockchain, DEFAULT_PORT) used by the rest of the codebase.
//
// The TCP listener/handler functions below are NOT called by main.rs.
// Do NOT re-enable raw TCP P2P without adding encryption.

use sentrix_primitives::block::Block;
use sentrix_core::blockchain::Blockchain;
use sentrix_primitives::transaction::Transaction;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, RwLock, mpsc};

pub const DEFAULT_PORT: u16 = 30303;
pub const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024; // 10MB

// Rate limiting and peer cap constants for network protection
pub const MAX_CONNECTIONS_PER_IP: u32 = 100;
pub const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(60);
pub const MAX_PEERS: usize = 50;

pub type ConnectionCounts = Arc<Mutex<HashMap<IpAddr, (u32, Instant)>>>;

// ── Message types ────────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum Message {
    Handshake {
        host: String,
        port: u16,
        height: u64,
        chain_id: u64,
    },
    NewBlock {
        block: Block,
    },
    NewTransaction {
        transaction: Transaction,
    },
    GetBlocks {
        from_height: u64,
    },
    BlocksResponse {
        blocks: Vec<Block>,
    },
    GetHeight,
    HeightResponse {
        height: u64,
    },
    Ping,
    Pong {
        height: u64,
    },
}

// ── Peer info ────────────────────────────────────────────
#[derive(Debug, Clone)]
pub struct Peer {
    pub host: String,
    pub port: u16,
    pub height: u64,
    pub chain_id: u64,
}

impl Peer {
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

// ── Shared state for P2P ─────────────────────────────────
pub type SharedBlockchain = Arc<RwLock<Blockchain>>;
pub type SharedPeers = Arc<RwLock<HashMap<String, Peer>>>;

// ── Node events ──────────────────────────────────────────
#[derive(Debug)]
pub enum NodeEvent {
    NewBlock(Block),
    NewTransaction(Transaction),
    PeerConnected(String),
    PeerDisconnected(String),
    SyncNeeded {
        peer_addr: String,
        peer_height: u64,
    },
    /// BFT: received a proposal from the network
    BftProposal(sentrix_bft::messages::Proposal),
    /// BFT: received a prevote from the network
    BftPrevote(sentrix_bft::messages::Prevote),
    /// BFT: received a precommit from the network
    BftPrecommit(sentrix_bft::messages::Precommit),
    /// BFT: received a round-status announcement for round synchronization
    BftRoundStatus(sentrix_bft::messages::RoundStatus),
}

// ── Node ─────────────────────────────────────────────────
pub struct Node {
    pub host: String,
    pub port: u16,
    pub peers: SharedPeers,
    pub blockchain: SharedBlockchain,
    pub event_tx: mpsc::Sender<NodeEvent>,
}

impl Node {
    pub fn new(
        host: String,
        port: u16,
        blockchain: SharedBlockchain,
        event_tx: mpsc::Sender<NodeEvent>,
    ) -> Self {
        Self {
            host,
            port,
            peers: Arc::new(RwLock::new(HashMap::new())),
            blockchain,
            event_tx,
        }
    }

    // ── Message encoding ─────────────────────────────────

    pub fn encode_message(msg: &Message) -> SentrixResult<Vec<u8>> {
        let json = serde_json::to_vec(msg)?;
        if json.len() > MAX_MESSAGE_SIZE {
            return Err(SentrixError::NetworkError("message too large".to_string()));
        }
        let len = json.len() as u32;
        let mut buf = len.to_be_bytes().to_vec();
        buf.extend(json);
        Ok(buf)
    }

    pub async fn read_message(stream: &mut TcpStream) -> SentrixResult<Message> {
        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;
        let len = u32::from_be_bytes(len_buf) as usize;

        if len > MAX_MESSAGE_SIZE {
            return Err(SentrixError::NetworkError("message too large".to_string()));
        }

        let mut buf = vec![0u8; len];
        stream
            .read_exact(&mut buf)
            .await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;

        let msg: Message = serde_json::from_slice(&buf)?;
        Ok(msg)
    }

    pub async fn send_message(stream: &mut TcpStream, msg: &Message) -> SentrixResult<()> {
        let encoded = Self::encode_message(msg)?;
        stream
            .write_all(&encoded)
            .await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;
        Ok(())
    }

    // ── Listener (accept incoming connections) ───────────

    // Per-IP rate limiting and max peer cap to prevent resource exhaustion
    pub async fn start_listener(
        port: u16,
        blockchain: SharedBlockchain,
        peers: SharedPeers,
        event_tx: mpsc::Sender<NodeEvent>,
    ) -> SentrixResult<()> {
        let addr = format!("0.0.0.0:{}", port);
        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;

        let connection_counts: ConnectionCounts = Arc::new(Mutex::new(HashMap::new()));

        tracing::info!("P2P listening on {}", addr);

        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    // Maximum simultaneous peer connections
                    let peer_count = peers.read().await.len();
                    if peer_count >= MAX_PEERS {
                        tracing::warn!(
                            "max peers reached ({}), rejecting {}",
                            MAX_PEERS,
                            peer_addr
                        );
                        continue;
                    }

                    // Rate limit per IP (message rate over a sliding window)
                    let peer_ip = peer_addr.ip();
                    {
                        let mut counts = connection_counts.lock().await;

                        // Evict stale entries to prevent unbounded HashMap growth
                        if counts.len() > 10_000 {
                            counts.retain(|_, (_, ts)| ts.elapsed() <= RATE_LIMIT_WINDOW);
                        }

                        let entry = counts.entry(peer_ip).or_insert((0, Instant::now()));
                        if entry.1.elapsed() > RATE_LIMIT_WINDOW {
                            *entry = (0, Instant::now());
                        }
                        entry.0 += 1;
                        if entry.0 > MAX_CONNECTIONS_PER_IP {
                            tracing::warn!(
                                "rate limit exceeded for IP {}: {} connections in window",
                                peer_ip,
                                entry.0
                            );
                            continue;
                        }
                    }

                    tracing::info!("Peer connected: {}", peer_addr);
                    let bc = blockchain.clone();
                    let peers = peers.clone();
                    let etx = event_tx.clone();

                    let peer_ip = peer_addr.ip().to_string();
                    tokio::spawn(async move {
                        if let Err(e) =
                            Self::handle_connection(stream, bc, peers, etx, peer_ip).await
                        {
                            tracing::warn!("Peer {} error: {}", peer_addr, e);
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!("Accept error: {}", e);
                }
            }
        }
    }

    // ── Handle a single peer connection ──────────────────

    async fn handle_connection(
        mut stream: TcpStream,
        blockchain: SharedBlockchain,
        peers: SharedPeers,
        event_tx: mpsc::Sender<NodeEvent>,
        peer_ip: String,
    ) -> SentrixResult<()> {
        // Reject non-Handshake messages until the peer has completed the handshake
        let mut handshake_done = false;

        loop {
            let msg = match Self::read_message(&mut stream).await {
                Ok(m) => m,
                Err(_) => return Ok(()), // connection closed
            };

            // Drop messages from peers that haven't completed the handshake yet
            if !handshake_done && !matches!(msg, Message::Handshake { .. }) {
                tracing::warn!(
                    "Rejected pre-handshake message from {}: handshake not complete",
                    peer_ip
                );
                return Err(SentrixError::NetworkError(
                    "message received before handshake".to_string(),
                ));
            }

            match msg {
                Message::Handshake {
                    host: _,
                    port,
                    height,
                    chain_id,
                } => {
                    // Validate chain_id matches
                    let bc = blockchain.read().await;
                    if chain_id != bc.chain_id {
                        tracing::warn!(
                            "Rejected peer: chain_id mismatch (theirs: {}, ours: {})",
                            chain_id,
                            bc.chain_id
                        );
                        return Err(SentrixError::NetworkError(format!(
                            "chain_id mismatch: {} vs {}",
                            chain_id, bc.chain_id
                        )));
                    }

                    // Register peer using actual TCP IP + declared P2P port
                    let our_height = bc.height();
                    let our_chain_id = bc.chain_id;
                    drop(bc);

                    let peer = Peer {
                        host: peer_ip.clone(),
                        port,
                        height,
                        chain_id,
                    };
                    let peer_addr = peer.addr();
                    peers.write().await.insert(peer_addr.clone(), peer);
                    let _ = event_tx.send(NodeEvent::PeerConnected(peer_addr)).await;
                    let response = Message::Handshake {
                        host: "0.0.0.0".to_string(),
                        port: 0,
                        height: our_height,
                        chain_id: our_chain_id,
                    };
                    Self::send_message(&mut stream, &response).await?;
                    handshake_done = true;

                    // If peer has more blocks, trigger proactive sync via a dedicated connection.
                    // Do NOT send GetBlocks inline here — connect_peer callers only read one
                    // Handshake response and would close the stream, leaving our GetBlocks unanswered.
                    if height > our_height {
                        let sync_addr = format!("{}:{}", peer_ip, port);
                        let _ = event_tx
                            .send(NodeEvent::SyncNeeded {
                                peer_addr: sync_addr,
                                peer_height: height,
                            })
                            .await;
                    }
                }

                Message::NewBlock { block } => {
                    let mut bc = blockchain.write().await;
                    match bc.add_block(block.clone()) {
                        Ok(()) => {
                            tracing::info!("Received block {} from peer", block.index);
                            // Send the canonically committed block (with state_root set) rather than the pre-commit version
                            let chain_block = bc.chain.last().cloned().unwrap_or(block);
                            let _ = event_tx.send(NodeEvent::NewBlock(chain_block)).await;
                        }
                        Err(e) => {
                            tracing::warn!("Rejected block {}: {}", block.index, e);
                        }
                    }
                }

                Message::NewTransaction { transaction } => {
                    let mut bc = blockchain.write().await;
                    if let Ok(()) = bc.add_to_mempool(transaction.clone()) {
                        let _ = event_tx.send(NodeEvent::NewTransaction(transaction)).await;
                    }
                }

                Message::GetBlocks { from_height } => {
                    let bc = blockchain.read().await;
                    let mut blocks = Vec::new();
                    let to = bc.height().min(from_height + 100); // max 100 blocks per request
                    for i in from_height..=to {
                        if let Some(block) = bc.get_block(i) {
                            blocks.push(block.clone());
                        }
                    }
                    let response = Message::BlocksResponse { blocks };
                    Self::send_message(&mut stream, &response).await?;
                }

                Message::BlocksResponse { blocks } => {
                    let mut bc = blockchain.write().await;
                    let mut applied = 0;
                    for block in blocks {
                        match bc.add_block(block) {
                            Ok(()) => applied += 1,
                            Err(_) => break, // stop on first invalid
                        }
                    }
                    if applied > 0 {
                        tracing::info!("Synced {} blocks from peer", applied);
                    }
                }

                Message::GetHeight => {
                    let bc = blockchain.read().await;
                    let response = Message::HeightResponse {
                        height: bc.height(),
                    };
                    Self::send_message(&mut stream, &response).await?;
                }

                Message::HeightResponse { height } => {
                    let bc = blockchain.read().await;
                    if height > bc.height() {
                        tracing::info!("Peer has higher chain: {} vs our {}", height, bc.height());
                    }
                }

                Message::Ping => {
                    let bc = blockchain.read().await;
                    let response = Message::Pong {
                        height: bc.height(),
                    };
                    Self::send_message(&mut stream, &response).await?;
                }

                Message::Pong { .. } => {} // just ack
            }
        }
    }

    // ── Connect to a peer (outbound) ─────────────────────

    pub async fn connect_peer(&self, host: &str, port: u16) -> SentrixResult<()> {
        let addr = format!("{}:{}", host, port);
        let mut stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| SentrixError::NetworkError(format!("connect {}: {}", addr, e)))?;

        let bc = self.blockchain.read().await;
        let handshake = Message::Handshake {
            host: self.host.clone(),
            port: self.port,
            height: bc.height(),
            chain_id: bc.chain_id,
        };
        drop(bc);

        Self::send_message(&mut stream, &handshake).await?;

        // Read handshake response + verify chain_id
        match Self::read_message(&mut stream).await? {
            Message::Handshake {
                host: _,
                port: _,
                height,
                chain_id,
            } => {
                // Verify chain_id on outbound connections to prevent cross-network peer pollution
                let our_chain_id = self.blockchain.read().await.chain_id;
                if chain_id != our_chain_id {
                    return Err(SentrixError::NetworkError(format!(
                        "outbound peer {} chain_id mismatch: {} vs {}",
                        addr, chain_id, our_chain_id
                    )));
                }

                // Use the actual connection target (host:port) for broadcasting, not the handshake response
                let peer = Peer {
                    host: host.to_string(),
                    port,
                    height,
                    chain_id,
                };
                let peer_addr = peer.addr();
                self.peers.write().await.insert(peer_addr.clone(), peer);
                let _ = self
                    .event_tx
                    .send(NodeEvent::PeerConnected(peer_addr.clone()))
                    .await;
                tracing::info!("Connected to peer {} (height: {})", peer_addr, height);

                // If peer has more blocks, sync
                let bc = self.blockchain.read().await;
                if height > bc.height() {
                    let our_height = bc.height();
                    drop(bc);
                    let get_blocks = Message::GetBlocks {
                        from_height: our_height + 1,
                    };
                    Self::send_message(&mut stream, &get_blocks).await?;

                    // Read blocks response
                    if let Ok(Message::BlocksResponse { blocks }) =
                        Self::read_message(&mut stream).await
                    {
                        let mut bc = self.blockchain.write().await;
                        let mut applied = 0;
                        for block in blocks {
                            match bc.add_block(block) {
                                Ok(()) => applied += 1,
                                Err(_) => break,
                            }
                        }
                        if applied > 0 {
                            tracing::info!("Synced {} blocks from {}", applied, peer_addr);
                        }
                    }
                }

                Ok(())
            }
            _ => Err(SentrixError::NetworkError("expected handshake".to_string())),
        }
    }

    // ── Broadcast to all peers ───────────────────────────

    // Non-blocking broadcast with per-peer timeout to prevent slow peers from stalling propagation
    pub async fn broadcast(&self, msg: &Message) {
        let peers = self.peers.read().await;
        let encoded = match Self::encode_message(msg) {
            Ok(e) => e,
            Err(_) => return,
        };

        for (addr, _) in peers.iter() {
            let data = encoded.clone();
            let peer_addr = addr.clone();
            tokio::spawn(async move {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    TcpStream::connect(&peer_addr),
                )
                .await
                {
                    Ok(Ok(mut stream)) => {
                        let _ = stream.write_all(&data).await;
                    }
                    Ok(Err(e)) => {
                        tracing::warn!("broadcast: connect to {} failed: {}", peer_addr, e);
                    }
                    Err(_) => {
                        tracing::warn!("broadcast: connect to {} timed out", peer_addr);
                    }
                }
            });
        }
    }

    pub async fn broadcast_block(&self, block: &Block) {
        self.broadcast(&Message::NewBlock {
            block: block.clone(),
        })
        .await;
    }

    pub async fn broadcast_transaction(&self, tx: &Transaction) {
        self.broadcast(&Message::NewTransaction {
            transaction: tx.clone(),
        })
        .await;
    }

    // ── Queries ──────────────────────────────────────────

    pub async fn peer_count(&self) -> usize {
        self.peers.read().await.len()
    }

    pub async fn peer_list(&self) -> Vec<(String, u64)> {
        self.peers
            .read()
            .await
            .iter()
            .map(|(addr, p)| (addr.clone(), p.height))
            .collect()
    }

    /// Check if IP is rate limited (for testing rate limit behavior)
    pub fn check_rate_limit(counts: &mut HashMap<IpAddr, (u32, Instant)>, ip: IpAddr) -> bool {
        let entry = counts.entry(ip).or_insert((0, Instant::now()));
        if entry.1.elapsed() > RATE_LIMIT_WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
        entry.0 <= MAX_CONNECTIONS_PER_IP
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_m02_rate_limit_per_ip() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let mut counts: HashMap<IpAddr, (u32, Instant)> = HashMap::new();

        // First MAX_CONNECTIONS_PER_IP connections should pass
        for _ in 0..MAX_CONNECTIONS_PER_IP {
            assert!(Node::check_rate_limit(&mut counts, ip));
        }

        // Next connection should be rejected
        assert!(!Node::check_rate_limit(&mut counts, ip));
        assert!(!Node::check_rate_limit(&mut counts, ip));

        // Different IP should still pass
        let ip2: IpAddr = "192.168.1.1".parse().unwrap();
        assert!(Node::check_rate_limit(&mut counts, ip2));
    }

    #[test]
    fn test_m02_max_peers_constant() {
        assert_eq!(MAX_PEERS, 50);
        assert_eq!(MAX_CONNECTIONS_PER_IP, 100);
    }

    #[tokio::test]
    async fn test_m02_chain_id_mismatch_rejected() {
        // Create two blockchains with different chain IDs
        let bc1 = sentrix_core::blockchain::Blockchain::new("admin".to_string());
        let shared1: SharedBlockchain = Arc::new(RwLock::new(bc1));

        let (tx1, _rx1) = mpsc::channel(16);
        let node1 = Node::new("127.0.0.1".to_string(), 0, shared1.clone(), tx1);

        // Start a listener on a random port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        // Spawn listener that sends a handshake with wrong chain_id
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Read the incoming handshake
            let _msg = Node::read_message(&mut stream).await.unwrap();
            // Reply with wrong chain_id
            let bad_handshake = Message::Handshake {
                host: "127.0.0.1".to_string(),
                port: 0,
                height: 0,
                chain_id: 9999, // wrong!
            };
            Node::send_message(&mut stream, &bad_handshake)
                .await
                .unwrap();
        });

        // Node1 connects — should fail due to chain_id mismatch
        let result = node1.connect_peer("127.0.0.1", port).await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("chain_id mismatch"),
            "Expected chain_id error, got: {}",
            err
        );
    }
}
