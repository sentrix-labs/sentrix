// block_producer.rs - Sentrix — Block creation (validator side)

use crate::core::blockchain::{Blockchain, MAX_TX_PER_BLOCK};
use crate::core::block::Block;
use crate::core::transaction::Transaction;
use crate::types::error::{SentrixError, SentrixResult};

impl Blockchain {
    // ── Block creation (validator calls this) ────────────
    pub fn create_block(&mut self, validator_address: &str) -> SentrixResult<Block> {
        let next_height = self.height() + 1;

        // Check authorization
        if !self.authority.is_authorized(validator_address, next_height)? {
            return Err(SentrixError::NotYourTurn);
        }

        // Build transaction list — coinbase first
        let reward = self.get_block_reward();
        let coinbase = Transaction::new_coinbase(
            validator_address.to_string(),
            reward,
            next_height,
        );

        let mut transactions = vec![coinbase];

        // Take up to MAX_TX_PER_BLOCK from mempool
        let take = self.mempool.len().min(MAX_TX_PER_BLOCK - 1);
        let mempool_txs: Vec<Transaction> = self.mempool.drain(..take).collect();
        transactions.extend(mempool_txs);

        let block = Block::new(
            next_height,
            self.latest_block()?.hash.clone(),
            transactions,
            validator_address.to_string(),
        );

        Ok(block)
    }
}
