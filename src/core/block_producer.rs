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

        // Take up to MAX_TX_PER_BLOCK from mempool (snapshot — do NOT drain here).
        // V6-H-01 FIX: drain() before add_block() means txs are permanently lost if
        // add_block() fails (e.g. stale prev_hash, validator race). Clone instead.
        // add_block() removes mined txs from mempool via retain() after a successful commit.
        let take = self.mempool.len().min(MAX_TX_PER_BLOCK - 1);
        let mempool_txs: Vec<Transaction> = self.mempool.iter().take(take).cloned().collect();
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

// ── Tests ─────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use secp256k1::{Secp256k1, SecretKey, PublicKey};
    use secp256k1::rand::rngs::OsRng;
    use crate::core::transaction::{Transaction, MIN_TX_FEE};
    use crate::core::blockchain::{Blockchain, CHAIN_ID};

    fn make_keypair() -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        secp.generate_keypair(&mut OsRng)
    }

    fn derive_addr(pk: &PublicKey) -> String {
        crate::wallet::wallet::Wallet::derive_address(pk)
    }

    fn setup() -> Blockchain {
        let mut bc = Blockchain::new("admin".to_string());
        bc.authority.add_validator_unchecked("v1".to_string(), "V1".to_string(), "pk1".to_string());
        bc
    }

    const RECV: &str = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

    // V6-H-01 test: create_block must NOT drain mempool (clone only)
    #[test]
    fn test_create_block_does_not_drain_mempool() {
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 1_000_000_000).unwrap();
        let tx = Transaction::new(
            sender, RECV.to_string(), 100_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        bc.add_to_mempool(tx).unwrap();
        assert_eq!(bc.mempool_size(), 1);

        // create_block should NOT remove the tx from mempool
        let _block = bc.create_block("v1").unwrap();
        assert_eq!(bc.mempool_size(), 1, "mempool must not be drained by create_block");
    }

    // V6-H-01 test: tx removed from mempool only after successful add_block
    #[test]
    fn test_mempool_cleared_only_after_add_block() {
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 1_000_000_000).unwrap();
        let tx = Transaction::new(
            sender, RECV.to_string(), 100_000, MIN_TX_FEE, 0, String::new(), CHAIN_ID, &sk, &pk,
        ).unwrap();
        bc.add_to_mempool(tx).unwrap();

        let block = bc.create_block("v1").unwrap();
        assert_eq!(bc.mempool_size(), 1); // still in mempool

        bc.add_block(block).unwrap();
        assert_eq!(bc.mempool_size(), 0); // cleared only after commit
    }

    // Unauthorized validator is rejected
    #[test]
    fn test_create_block_unauthorized_validator() {
        let mut bc = setup();
        let result = bc.create_block("not_a_validator");
        assert!(result.is_err());
    }
}
