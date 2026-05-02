// block_producer.rs - Sentrix — Block creation (validator side)

use crate::blockchain::{Blockchain, MAX_TX_PER_BLOCK};
use sentrix_primitives::block::Block;
use sentrix_primitives::error::{SentrixError, SentrixResult};
use sentrix_primitives::transaction::Transaction;

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

        // Phase D: at epoch boundaries post-fork, emit JailEvidenceBundle
        // system tx with locally-computed downtime evidence. Helper returns
        // None pre-fork, at non-boundaries, or with no evidence — making this
        // a no-op on default builds (JAIL_CONSENSUS_HEIGHT=u64::MAX).
        if let Some(jail_tx) =
            self.build_jail_evidence_system_tx(next_height, block_timestamp)
        {
            transactions.push(jail_tx);
        }

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
        // Track per-sender expected nonce as we walk the queue so a
        // sender with multiple txs in flight contributes them in order
        // (n, n+1, n+2…) without us needing to re-fetch from the
        // accounts map each time. Starting value is the on-chain
        // nonce; we bump it as we include each tx from that sender.
        use std::collections::HashMap;
        let mut next_nonce_per_sender: HashMap<String, u64> = HashMap::new();
        for tx in self.mempool.iter().take(take) {
            // Skip stale-nonce txs at proposal time. A tx with nonce
            // strictly less than the chain's expected nonce will fail
            // pre-validate (`Invalid nonce: expected N, got M`) and
            // sink the entire block — every validator rejects, BFT
            // can't finalize, the chain stalls. Live discovery
            // 2026-05-02: a single stuck nonce-5 tx (whose sender's
            // account was already at nonce 7) repeatedly poisoned
            // proposals until manual `mempool clear` recovery. Pull
            // expected nonce on first sight, bump as we include
            // subsequent txs from the same sender.
            let expected_nonce = *next_nonce_per_sender
                .entry(tx.from_address.clone())
                .or_insert_with(|| self.accounts.get_nonce(&tx.from_address));
            if tx.nonce < expected_nonce {
                continue;
            }
            // Future-nonce gap (tx.nonce > expected) means a same-
            // sender lower-nonce tx must come first; skip until the
            // gap closes naturally on a later proposal.
            if tx.nonce > expected_nonce {
                continue;
            }

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
            // Bump per-sender expected nonce so the next tx from the
            // same sender (already validated to be tx.nonce + 1 by
            // mempool admission) lines up on inclusion.
            next_nonce_per_sender.insert(tx.from_address.clone(), expected_nonce + 1);
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
    use crate::test_util::env_test_lock;
    use secp256k1::{PublicKey, Secp256k1, SecretKey};
    use sentrix_primitives::transaction::{MIN_TX_FEE, Transaction};

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

    /// Phase D Step 3: pre-fork (default), no JailEvidenceBundle is emitted
    /// regardless of whether the block index lands on an epoch boundary.
    /// Block contains only coinbase (+ any mempool txs).
    #[test]
    fn test_create_block_no_jail_bundle_pre_fork() {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
        }
        let mut bc = setup();
        let block = bc.create_block("v1").unwrap();
        // Only coinbase, no system tx
        assert_eq!(block.transactions.len(), 1);
        assert!(block.transactions[0].is_coinbase());
    }

    /// Phase D Step 3: post-fork at epoch boundary with downtime evidence,
    /// proposer prepends JailEvidenceBundle as tx[1] (right after coinbase).
    /// This verifies the wire-up: helper invoked, tx structure correct.
    /// Note: this test exercises create_block_voyager + manually crafts the
    /// blockchain state to land on an epoch boundary.
    #[test]
    fn test_create_block_voyager_emits_jail_bundle_at_boundary() {
        use sentrix_primitives::transaction::PROTOCOL_TREASURY;

        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("JAIL_CONSENSUS_HEIGHT", "0");
        }

        let mut bc = setup();

        // Inject a downer into active_set + populate full liveness window
        let downer = "0xfeedfacefeedfacefeedfacefeedfacefeedface".to_string();
        bc.stake_registry.active_set = vec![downer.clone()];
        let _window = sentrix_staking::slashing::LIVENESS_WINDOW;
        // 2026-04-29 fix: under the new canonical-only LivenessTracker
        // recording, "downtime" is the absence of recent signed entries,
        // not a wall of explicit signed=false. Anchor the downer with
        // ONE signed entry at h=0 (proves "we've been watching them"),
        // then leave them silent. By the time we reach the epoch boundary
        // their window is empty → is_downtime_at fires.
        bc.slashing.liveness.record_signed(&downer, 0);

        // Force chain to height (EPOCH_LENGTH - 2) so next block lands at
        // EPOCH_LENGTH - 1 (boundary). We don't actually need to mine —
        // build_block reads self.height() + 1 from chain length.
        // Instead, monkey-pad the chain to the right height with empty blocks.
        let target_height = sentrix_staking::epoch::EPOCH_LENGTH - 2;
        let prev_hash = bc.latest_block().unwrap().hash.clone();
        let pad = sentrix_primitives::block::Block::new(
            target_height,
            prev_hash,
            vec![sentrix_primitives::transaction::Transaction::new_coinbase(
                "v1".into(),
                0,
                target_height,
                1_700_000_000,
            )],
            "v1".into(),
        );
        bc.chain.push(pad);

        let block = bc.create_block_voyager("v1").unwrap();

        // tx[0] = coinbase, tx[1] = JailEvidenceBundle system tx
        assert_eq!(block.transactions.len(), 2);
        assert!(block.transactions[0].is_coinbase());
        assert!(block.transactions[1].is_system_tx());
        assert_eq!(block.transactions[1].from_address, PROTOCOL_TREASURY);
        assert!(block.transactions[1].is_jail_evidence_bundle_tx());

        unsafe {
            std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
        }
    }
}
