// block_producer.rs - Sentrix — Block creation (validator side)

use sentrix_primitives::block::Block;
use crate::blockchain::{Blockchain, MAX_TX_PER_BLOCK};
use sentrix_primitives::transaction::Transaction;
use sentrix_primitives::error::{SentrixError, SentrixResult};

impl Blockchain {
    // ── Block creation (validator calls this) ────────────
    pub fn create_block(&mut self, validator_address: &str) -> SentrixResult<Block> {
        let next_height = self.height() + 1;

        // Check authorization (Pioneer round-robin)
        if !self
            .authority
            .is_authorized(validator_address, next_height)?
        {
            return Err(SentrixError::NotYourTurn);
        }

        self.build_block(validator_address)
    }

    /// Create a block without Pioneer authority check.
    /// Used in Voyager BFT mode where proposer is selected by DPoS weighted round-robin.
    pub fn create_block_voyager(&mut self, validator_address: &str) -> SentrixResult<Block> {
        self.build_block(validator_address)
    }

    fn build_block(&mut self, validator_address: &str) -> SentrixResult<Block> {
        let next_height = self.height() + 1;

        // Build transaction list — coinbase first
        // Coinbase uses the block's timestamp — deterministic across all nodes for the same block.
        let block_timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let reward = self.get_block_reward();
        let coinbase = Transaction::new_coinbase(
            validator_address.to_string(),
            reward,
            next_height,
            block_timestamp,
        );

        let mut transactions = vec![coinbase];

        // Take up to MAX_TX_PER_BLOCK from mempool (snapshot — do NOT drain here).
        // Clone mempool transactions into the block — do NOT drain before add_block succeeds.
        // add_block() removes mined txs from mempool via retain() after a successful commit.
        //
        // P1: additionally bound the block by total EVM gas. The tx-count
        // limit (MAX_TX_PER_BLOCK) is an upper bound on batch size but
        // says nothing about compute cost — a 5000-tx block of
        // contract-heavy EVM calls could exceed BLOCK_GAS_LIMIT and
        // stall validators. For each EVM tx we parse the per-tx
        // gas_limit from its `EVM:{gas_limit}:{calldata}` data field
        // and stop including once the running total would exceed the
        // block ceiling. Native Sentrix txs are charged a nominal
        // 21_000 so they still participate in the accumulator.
        let take = self.mempool.len().min(MAX_TX_PER_BLOCK - 1);
        let mut current_gas_used: u64 = 0;
        for tx in self.mempool.iter().take(take) {
            let tx_gas = if tx.is_evm_tx() {
                tx.data
                    .split(':')
                    .nth(1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(sentrix_evm::gas::BLOCK_GAS_LIMIT)
                    .min(sentrix_evm::gas::BLOCK_GAS_LIMIT)
            } else {
                21_000
            };
            if !sentrix_evm::gas::fits_in_block(current_gas_used, tx_gas) {
                break;
            }
            current_gas_used = current_gas_used.saturating_add(tx_gas);
            transactions.push(tx.clone());
        }

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
    use crate::blockchain::{Blockchain, CHAIN_ID};
    use sentrix_primitives::transaction::{MIN_TX_FEE, Transaction};
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    fn make_keypair() -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        let mut rng = secp256k1::rand::rng();
        secp.generate_keypair(&mut rng)
    }

    fn derive_addr(pk: &PublicKey) -> String {
        sentrix_wallet::Wallet::derive_address(pk)
    }

    fn setup() -> Blockchain {
        let mut bc = Blockchain::new("admin".to_string());
        bc.authority
            .add_validator_unchecked("v1".to_string(), "V1".to_string(), "pk1".to_string());
        bc
    }

    const RECV: &str = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";

    // create_block must clone mempool transactions — not drain them — so they can retry on failure
    #[test]
    fn test_create_block_does_not_drain_mempool() {
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 1_000_000_000).unwrap();
        let tx = Transaction::new(
            sender,
            RECV.to_string(),
            100_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        bc.add_to_mempool(tx).unwrap();
        assert_eq!(bc.mempool_size(), 1);

        // create_block should NOT remove the tx from mempool
        let _block = bc.create_block("v1").unwrap();
        assert_eq!(
            bc.mempool_size(),
            1,
            "mempool must not be drained by create_block"
        );
    }

    // Transactions are removed from mempool only after successful add_block
    #[test]
    fn test_mempool_cleared_only_after_add_block() {
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 1_000_000_000).unwrap();
        let tx = Transaction::new(
            sender,
            RECV.to_string(),
            100_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
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
