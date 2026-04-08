// sync.rs - Sentrix Chain

use crate::core::blockchain::Blockchain;
use crate::core::block::Block;
use crate::network::node::{Node, Message};
use crate::storage::db::Storage;
use crate::types::error::{SentrixError, SentrixResult};
use tokio::net::TcpStream;

pub struct ChainSync;

impl ChainSync {
    // Sync chain from a specific peer address
    // Safe protocol: validate in sandbox before replacing local state
    pub async fn sync_from_peer(
        peer_addr: &str,
        local: &mut Blockchain,
        storage: &Storage,
    ) -> SentrixResult<bool> {
        // Connect to peer
        let mut stream = TcpStream::connect(peer_addr).await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;

        // Request their chain
        Node::send_message(&mut stream, &Message::GetChain).await?;

        // Read response
        let response = Node::read_message(&mut stream).await?;
        let peer_blocks = match response {
            Message::ChainResponse { blocks } => blocks,
            _ => return Err(SentrixError::NetworkError(
                "unexpected response to GetChain".to_string()
            )),
        };

        // Only sync if peer has more blocks
        if peer_blocks.len() <= local.chain.len() {
            return Ok(false); // already up to date
        }

        // Validate received chain in sandbox
        Self::validate_and_apply(peer_blocks, local, storage).await
    }

    // Validate received blocks in a sandbox Blockchain instance
    // Only replace local state if ALL blocks validate successfully
    async fn validate_and_apply(
        peer_blocks: Vec<Block>,
        local: &mut Blockchain,
        storage: &Storage,
    ) -> SentrixResult<bool> {
        if peer_blocks.is_empty() {
            return Ok(false);
        }

        // Validate chain structure (hash links, integrity)
        Self::validate_chain_structure(&peer_blocks)?;

        // Create sandbox — fresh blockchain with same admin and validators
        let mut sandbox = Blockchain::new(local.authority.admin_address.clone());
        sandbox.authority = local.authority.clone();

        // Replay all blocks after genesis through sandbox
        // (genesis is created in Blockchain::new)
        for block in peer_blocks.iter().skip(1) {
            sandbox.add_block(block.clone())
                .map_err(|e| SentrixError::ChainValidationFailed(
                    format!("block {} failed: {}", block.index, e)
                ))?;
        }

        // All blocks validated — replace local state
        *local = sandbox;
        storage.save_blockchain(local)?;
        storage.save_height(local.height())?;

        Ok(true)
    }

    // Validate chain structure without applying state
    fn validate_chain_structure(blocks: &[Block]) -> SentrixResult<()> {
        if blocks.is_empty() {
            return Ok(());
        }

        // Check genesis
        let genesis = &blocks[0];
        if genesis.index != 0 {
            return Err(SentrixError::ChainValidationFailed(
                "first block must be genesis (index 0)".to_string()
            ));
        }

        // Check hash chain
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

    // Quick height check — ask peer for their current height
    pub async fn get_peer_height(peer_addr: &str) -> SentrixResult<u64> {
        let mut stream = TcpStream::connect(peer_addr).await
            .map_err(|e| SentrixError::NetworkError(e.to_string()))?;

        Node::send_message(&mut stream, &Message::GetHeight).await?;

        match Node::read_message(&mut stream).await? {
            Message::HeightResponse { height } => Ok(height),
            _ => Err(SentrixError::NetworkError("unexpected response".to_string())),
        }
    }
}

// No unit tests — requires live network connections.
// sync.rs is covered by integration tests when running actual nodes.
