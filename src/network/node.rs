// node.rs - Sentrix Chain

use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{RwLock, mpsc};
use std::sync::Arc;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use crate::core::block::Block;
use crate::core::transaction::Transaction;
use crate::types::error::{SentrixError, SentrixResult};

pub const DEFAULT_PORT: u16 = 30303;
pub const MAX_MESSAGE_SIZE: usize = 10 * 1024 * 1024; // 10MB

// ── Message types ────────────────────────────────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum Message {
    Handshake { host: String, port: u16, height: u64 },
    NewBlock { block: Block },
    NewTransaction { transaction: Transaction },
    GetChain,
    ChainResponse { blocks: Vec<Block> },
    GetHeight,
    HeightResponse { height: u64 },
    Ping,
    Pong { height: u64 },
}

// ── Peer info ────────────────────────────────────────────
#[derive(Debug, Clone)]
pub struct Peer {
    pub host: String,
    pub port: u16,
    pub height: u64,
}

impl Peer {
    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

// ── Node ─────────────────────────────────────────────────
pub struct Node {
    pub host: String,
    pub port: u16,
    pub peers: Arc<RwLock<HashMap<String, Peer>>>,
    // Channel to send events to the main application
    pub event_tx: mpsc::Sender<NodeEvent>,
}

#[derive(Debug)]
pub enum NodeEvent {
    NewBlock(Block),
    NewTransaction(Transaction),
    PeerConnected(String),
    PeerDisconnected(String),
}

impl Node {
    pub fn new(host: String, port: u16, event_tx: mpsc::Sender<NodeEvent>) -> Self {
        Self {
            host,
            port,
            peers: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
        }
    }

    // Encode message with 4-byte length prefix
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

    // Decode message from stream (read 4-byte length then payload)
    pub async fn read_message(stream: &mut TcpStream) -> SentrixResult<Message> {
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;
        let len = u32::from_be_bytes(len_buf) as usize;

        if len > MAX_MESSAGE_SIZE {
            return Err(SentrixError::NetworkError("message too large".to_string()));
        }

        let mut buf = vec![0u8; len];
        stream.read_exact(&mut buf).await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;

        let msg: Message = serde_json::from_slice(&buf)?;
        Ok(msg)
    }

    // Send a message to a single stream
    pub async fn send_message(stream: &mut TcpStream, msg: &Message) -> SentrixResult<()> {
        let encoded = Self::encode_message(msg)?;
        stream.write_all(&encoded).await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;
        Ok(())
    }

    // Broadcast to all known peers (best-effort, skip failed peers)
    pub async fn broadcast(&self, msg: &Message, _current_height: u64) {
        let peers = self.peers.read().await;
        let encoded = match Self::encode_message(msg) {
            Ok(e) => e,
            Err(_) => return,
        };

        for (addr, _peer) in peers.iter() {
            if let Ok(mut stream) = TcpStream::connect(addr).await {
                let _ = stream.write_all(&encoded).await;
            }
        }
    }

    // Connect to a peer
    pub async fn connect_peer(&self, host: &str, port: u16, our_height: u64) -> SentrixResult<()> {
        let addr = format!("{}:{}", host, port);
        let mut stream = TcpStream::connect(&addr).await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;

        // Send handshake
        let handshake = Message::Handshake {
            host: self.host.clone(),
            port: self.port,
            height: our_height,
        };
        Self::send_message(&mut stream, &handshake).await?;

        // Read response
        match Self::read_message(&mut stream).await? {
            Message::Handshake { host, port, height } => {
                let peer = Peer { host: host.clone(), port, height };
                let peer_addr = peer.addr();
                self.peers.write().await.insert(peer_addr.clone(), peer);
                let _ = self.event_tx.send(NodeEvent::PeerConnected(peer_addr)).await;
                Ok(())
            }
            _ => Err(SentrixError::NetworkError("expected handshake response".to_string()))
        }
    }

    pub async fn peer_count(&self) -> usize {
        self.peers.read().await.len()
    }

    pub async fn peer_addresses(&self) -> Vec<String> {
        self.peers.read().await.keys().cloned().collect()
    }
}

// No tests for network module — requires live TCP connections.
// Integration tests will be added when running actual nodes.
