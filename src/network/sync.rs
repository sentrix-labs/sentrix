// sync.rs - Sentrix — Chain synchronization

use crate::core::block::Block;
use crate::network::node::{Node, Message, SharedBlockchain};
use crate::types::error::{SentrixError, SentrixResult};
use tokio::net::TcpStream;

pub struct ChainSync;

impl ChainSync {
    /// Incremental sync: download only blocks we don't have.
    pub async fn sync_from_peer(
        peer_addr: &str,
        blockchain: &SharedBlockchain,
    ) -> SentrixResult<u64> {
        let mut stream = TcpStream::connect(peer_addr).await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;

        // Handshake
        let bc = blockchain.read().await;
        let handshake = Message::Handshake {
            host: "0.0.0.0".to_string(),
            port: 0,
            height: bc.height(),
            chain_id: bc.chain_id,
        };
        let our_height = bc.height();
        drop(bc);

        Node::send_message(&mut stream, &handshake).await?;

        // H-01 FIX: Validate chain_id in peer handshake response
        let (peer_height, peer_chain_id) = match Node::read_message(&mut stream).await? {
            Message::Handshake { height, chain_id, .. } => (height, chain_id),
            _ => return Err(SentrixError::NetworkError("expected handshake".to_string())),
        };
        let our_chain_id = blockchain.read().await.chain_id;
        if peer_chain_id != our_chain_id {
            return Err(SentrixError::NetworkError(
                format!("sync: chain_id mismatch — peer {} vs ours {}", peer_chain_id, our_chain_id)
            ));
        }
        let peer_height = peer_height;

        if peer_height <= our_height {
            return Ok(0);
        }

        // Request missing blocks in chunks of 100
        let mut total_synced = 0u64;
        let mut current = our_height + 1;

        while current <= peer_height {
            let get_blocks = Message::GetBlocks { from_height: current };
            Node::send_message(&mut stream, &get_blocks).await?;

            match Node::read_message(&mut stream).await? {
                Message::BlocksResponse { blocks } => {
                    if blocks.is_empty() {
                        break;
                    }
                    let mut bc = blockchain.write().await;
                    for block in &blocks {
                        match bc.add_block(block.clone()) {
                            Ok(()) => {
                                total_synced += 1;
                                current = block.index + 1;
                            }
                            Err(e) => {
                                tracing::warn!("Sync block {} failed: {}", block.index, e);
                                return Ok(total_synced);
                            }
                        }
                    }
                }
                _ => break,
            }
        }

        if total_synced > 0 {
            tracing::info!("Synced {} blocks from {}", total_synced, peer_addr);
        }

        Ok(total_synced)
    }

    /// Quick height check.
    pub async fn get_peer_height(peer_addr: &str) -> SentrixResult<u64> {
        let mut stream = TcpStream::connect(peer_addr).await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;

        Node::send_message(&mut stream, &Message::GetHeight).await?;

        match Node::read_message(&mut stream).await? {
            Message::HeightResponse { height } => Ok(height),
            _ => Err(SentrixError::NetworkError("unexpected response".to_string())),
        }
    }

    /// Validate chain structure (hash links only, no state).
    pub fn validate_chain_structure(blocks: &[Block]) -> SentrixResult<()> {
        for i in 1..blocks.len() {
            let block = &blocks[i];
            let prev = &blocks[i - 1];

            if block.index != prev.index + 1 {
                return Err(SentrixError::ChainValidationFailed(
                    format!("block index gap at {}", i)
                ));
            }
            if block.previous_hash != prev.hash {
                return Err(SentrixError::ChainValidationFailed(
                    format!("broken hash link at block {}", block.index)
                ));
            }
            if !block.is_valid_hash() {
                return Err(SentrixError::ChainValidationFailed(
                    format!("invalid hash at block {}", block.index)
                ));
            }
        }
        Ok(())
    }
}
