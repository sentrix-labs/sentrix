// block.rs - Sentrix

use serde::{Deserialize, Serialize};
use crate::core::transaction::Transaction;
use crate::core::merkle::{merkle_root, sha256_hex};
use crate::types::error::{SentrixError, SentrixResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub index: u64,
    pub previous_hash: String,
    pub transactions: Vec<Transaction>,
    pub timestamp: u64,
    pub merkle_root: String,
    pub validator: String,
    pub hash: String,
    /// State root after this block's transactions are applied.
    /// None for genesis and blocks produced before SentrixTrie was initialized.
    /// Not included in calculate_hash() — backward-compatible with all existing blocks.
    #[serde(default)]
    pub state_root: Option<[u8; 32]>,
}

impl Block {
    pub fn new(
        index: u64,
        previous_hash: String,
        transactions: Vec<Transaction>,
        validator: String,
    ) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let txids: Vec<String> = transactions.iter().map(|tx| tx.txid.clone()).collect();
        let merkle = merkle_root(&txids);

        let mut block = Self {
            index,
            previous_hash,
            transactions,
            timestamp,
            merkle_root: merkle,
            validator,
            hash: String::new(),
            state_root: None,
        };
        block.hash = block.calculate_hash();
        block
    }

    pub fn calculate_hash(&self) -> String {
        let payload = format!(
            "{}{}{}{}{}",
            self.index,
            self.previous_hash,
            self.merkle_root,
            self.timestamp,
            self.validator,
        );
        sha256_hex(payload.as_bytes())
    }

    pub fn is_valid_hash(&self) -> bool {
        self.hash == self.calculate_hash()
    }

    // Genesis block — block 0, no previous hash
    pub fn genesis() -> Self {
        let genesis_tx = Transaction::new_coinbase(
            "GENESIS".to_string(),
            0,
            0,
        );
        Self::new(
            0,
            "0".repeat(64),
            vec![genesis_tx],
            "GENESIS".to_string(),
        )
    }

    pub fn tx_count(&self) -> usize {
        self.transactions.len()
    }

    // Get coinbase transaction (always first)
    pub fn coinbase(&self) -> Option<&Transaction> {
        self.transactions.first().filter(|tx| tx.is_coinbase())
    }

    // Validate block structure
    pub fn validate_structure(&self, expected_index: u64, expected_prev_hash: &str) -> SentrixResult<()> {
        // Check index
        if self.index != expected_index {
            return Err(SentrixError::InvalidBlock(
                format!("expected index {}, got {}", expected_index, self.index)
            ));
        }

        // Check previous hash link
        if self.previous_hash != expected_prev_hash {
            return Err(SentrixError::InvalidBlock(
                "invalid previous hash".to_string()
            ));
        }

        // Check hash integrity
        if !self.is_valid_hash() {
            return Err(SentrixError::InvalidBlock(
                "block hash is invalid".to_string()
            ));
        }

        // Must have at least coinbase
        if self.transactions.is_empty() {
            return Err(SentrixError::InvalidBlock(
                "block has no transactions".to_string()
            ));
        }

        // First transaction must be coinbase
        if !self.transactions[0].is_coinbase() {
            return Err(SentrixError::InvalidBlock(
                "first transaction must be coinbase".to_string()
            ));
        }

        // Verify merkle root
        let txids: Vec<String> = self.transactions.iter().map(|tx| tx.txid.clone()).collect();
        let computed_merkle = merkle_root(&txids);
        if computed_merkle != self.merkle_root {
            return Err(SentrixError::InvalidBlock(
                "merkle root mismatch".to_string()
            ));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_block() {
        let genesis = Block::genesis();
        assert_eq!(genesis.index, 0);
        assert_eq!(genesis.previous_hash, "0".repeat(64));
        assert!(!genesis.hash.is_empty());
        assert!(genesis.is_valid_hash());
    }

    #[test]
    fn test_block_hash_deterministic() {
        let genesis = Block::genesis();
        let hash1 = genesis.calculate_hash();
        let hash2 = genesis.calculate_hash();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_block_has_coinbase() {
        let genesis = Block::genesis();
        assert!(genesis.coinbase().is_some());
        assert!(genesis.coinbase().unwrap().is_coinbase());
    }

    #[test]
    fn test_tampered_block_invalid_hash() {
        let mut block = Block::genesis();
        block.index = 999; // tamper
        assert!(!block.is_valid_hash());
    }

    #[test]
    fn test_validate_structure_valid() {
        let genesis = Block::genesis();
        assert!(genesis.validate_structure(0, &"0".repeat(64)).is_ok());
    }

    #[test]
    fn test_validate_structure_wrong_index() {
        let genesis = Block::genesis();
        assert!(genesis.validate_structure(1, &"0".repeat(64)).is_err());
    }

    #[test]
    fn test_validate_structure_wrong_prev_hash() {
        let genesis = Block::genesis();
        assert!(genesis.validate_structure(0, "wronghash").is_err());
    }

    #[test]
    fn test_block_chain_link() {
        let genesis = Block::genesis();
        let block1 = Block::new(
            1,
            genesis.hash.clone(),
            vec![Transaction::new_coinbase("validator1".to_string(), 100_000_000, 1)],
            "validator1".to_string(),
        );
        assert!(block1.validate_structure(1, &genesis.hash).is_ok());
    }
}
