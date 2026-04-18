// mempool.rs - Sentrix — Mempool management

use crate::blockchain::{
    Blockchain, MAX_MEMPOOL_PER_SENDER, MAX_MEMPOOL_SIZE, MEMPOOL_MAX_AGE_SECS,
    is_valid_sentrix_address,
};
use sentrix_primitives::transaction::{TOKEN_OP_ADDRESS, TokenOp, Transaction};
use sentrix_primitives::error::{SentrixError, SentrixResult};

impl Blockchain {
    pub fn add_to_mempool(&mut self, tx: Transaction) -> SentrixResult<()> {
        if tx.is_coinbase() {
            return Err(SentrixError::InvalidTransaction(
                "cannot manually add coinbase to mempool".to_string(),
            ));
        }

        // Global mempool size limit prevents RAM exhaustion under high transaction load
        if self.mempool.len() >= MAX_MEMPOOL_SIZE {
            return Err(SentrixError::InvalidTransaction(
                "mempool full — try again later".to_string(),
            ));
        }

        // Per-sender pending limit prevents one account from monopolizing the mempool
        let sender_pending = self.mempool_pending_count(&tx.from_address) as usize;
        if sender_pending >= MAX_MEMPOOL_PER_SENDER {
            return Err(SentrixError::InvalidTransaction(
                "too many pending transactions from this sender".to_string(),
            ));
        }

        // Reject malformed to_address before any state is touched
        if !is_valid_sentrix_address(&tx.to_address) {
            return Err(SentrixError::InvalidTransaction(format!(
                "invalid to_address: '{}'",
                tx.to_address
            )));
        }

        // Reject native SRX transfers to zero address — would silently destroy coins with no on-chain record.
        // TOKEN_OP_ADDRESS (0x000...0) is allowed when tx.data contains a valid TokenOp,
        // OR when this is an EVM CREATE tx (to=zero means contract creation).
        if tx.to_address == TOKEN_OP_ADDRESS && !TokenOp::is_token_op(&tx.data) && !tx.is_evm_tx() {
            return Err(SentrixError::InvalidTransaction(
                "cannot send SRX to zero address — use TokenOp::Burn to explicitly burn tokens"
                    .to_string(),
            ));
        }

        // Validate transaction timestamp: reject future-dated (clock skew attack)
        // and expired transactions (stale replay / mempool poisoning)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if tx.timestamp > now + 300 {
            return Err(SentrixError::InvalidTransaction(
                "transaction timestamp too far in the future (max +5 min)".to_string(),
            ));
        }
        if now > tx.timestamp.saturating_add(MEMPOOL_MAX_AGE_SECS) {
            return Err(SentrixError::InvalidTransaction(format!(
                "transaction too old — max age {} seconds",
                MEMPOOL_MAX_AGE_SECS
            )));
        }

        // Reject duplicate txid — same transaction must not enter the mempool twice
        if self.mempool.iter().any(|existing| existing.txid == tx.txid) {
            return Err(SentrixError::InvalidTransaction(format!(
                "duplicate txid in mempool: {}",
                tx.txid
            )));
        }

        // Basic validation
        let expected_nonce = self.accounts.get_nonce(&tx.from_address)
            + self.mempool_pending_count(&tx.from_address);
        tx.validate(expected_nonce, self.chain_id)?;

        // Check balance including pending mempool spends
        let pending_spend = self.mempool_pending_spend(&tx.from_address);
        let available = self
            .accounts
            .get_balance(&tx.from_address)
            .saturating_sub(pending_spend);
        // Checked addition prevents integer overflow on amount + fee
        let needed = tx
            .amount
            .checked_add(tx.fee)
            .ok_or_else(|| SentrixError::InvalidTransaction("amount + fee overflow".to_string()))?;

        if available < needed {
            return Err(SentrixError::InsufficientBalance {
                have: available,
                need: needed,
            });
        }

        // Insert sorted by fee descending (highest fee = front of queue).
        // A3: position lookup via partition_point — O(log n) comparisons
        // instead of the previous O(n) linear scan. The trailing memmove
        // from VecDeque::insert is a single hardware-accelerated memcpy
        // and stays well below comparison cost at MAX_MEMPOOL_SIZE=10_000.
        // We deliberately keep VecDeque (not BinaryHeap) because callers
        // depend on ordered iteration: block_producer.create_block takes
        // the first N by fee, explorer/API list mempool in fee order, and
        // tests index `mempool[0]`. BinaryHeap iteration is unordered and
        // would force a sort on every read — worse trade for read-heavy
        // access patterns.
        // TODO: RBF (Replace-By-Fee) not yet implemented.
        let pos = self
            .mempool
            .partition_point(|existing| existing.fee >= tx.fee);
        self.mempool.insert(pos, tx);
        Ok(())
    }

    fn mempool_pending_count(&self, address: &str) -> u64 {
        self.mempool
            .iter()
            .filter(|tx| tx.from_address == address)
            .count() as u64
    }

    fn mempool_pending_spend(&self, address: &str) -> u64 {
        self.mempool
            .iter()
            .filter(|tx| tx.from_address == address)
            .map(|tx| tx.amount.saturating_add(tx.fee))
            .fold(0u64, |acc, v| acc.saturating_add(v))
    }

    pub fn mempool_size(&self) -> usize {
        self.mempool.len()
    }

    /// Drop all pending transactions from the mempool. Used by the CLI
    /// `sentrix mempool clear` command to recover from a stuck-mempool
    /// incident (e.g. batch of bad-nonce txs blocking block production).
    pub fn clear_mempool(&mut self) {
        self.mempool.clear();
    }

    /// Removes transactions older than MEMPOOL_MAX_AGE_SECS.
    /// Called automatically after each block is added; also callable manually.
    pub fn prune_mempool(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.mempool
            .retain(|tx| now <= tx.timestamp.saturating_add(MEMPOOL_MAX_AGE_SECS));
    }
}

// ── Tests ─────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use crate::blockchain::{Blockchain, CHAIN_ID};
    use sentrix_primitives::transaction::{MIN_TX_FEE, Transaction};
    use secp256k1::rand::rngs::OsRng;
    use secp256k1::{PublicKey, Secp256k1, SecretKey};

    fn make_keypair() -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        secp.generate_keypair(&mut OsRng)
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

    // SRX transfers to zero address must be rejected (silent coin destruction)
    #[test]
    fn test_zero_address_srx_transfer_rejected() {
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 1_000_000_000).unwrap();
        let tx = Transaction::new(
            sender,
            TOKEN_OP_ADDRESS.to_string(),
            100_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        let result = bc.add_to_mempool(tx);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("zero address") || err.contains("TokenOp"));
    }

    // Token op transactions to TOKEN_OP_ADDRESS are allowed when tx.data contains a valid TokenOp
    #[test]
    fn test_zero_address_token_op_allowed() {
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 1_000_000_000).unwrap();
        // Deploy a dummy token first so the contract exists for transfer
        let token_op = TokenOp::Deploy {
            name: "TestToken".to_string(),
            symbol: "TTK".to_string(),
            decimals: 8,
            supply: 1_000_000,
            max_supply: 0,
        };
        let data = token_op.encode().unwrap();
        let tx = Transaction::new(
            sender,
            TOKEN_OP_ADDRESS.to_string(),
            0,
            MIN_TX_FEE,
            0,
            data,
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        // This should succeed — to_address is zero address but data is a valid TokenOp
        assert!(bc.add_to_mempool(tx).is_ok());
    }

    // Timestamp validation: future timestamp rejected
    #[test]
    fn test_future_timestamp_rejected() {
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 1_000_000_000).unwrap();
        let mut tx = Transaction::new(
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
        // Manually set timestamp 10 minutes into the future
        tx.timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + 600;
        assert!(bc.add_to_mempool(tx).is_err());
    }

    // Fee-priority ordering: higher fee tx should be inserted before lower fee tx
    #[test]
    fn test_fee_priority_ordering() {
        let mut bc = setup();
        let (sk1, pk1) = make_keypair();
        let (sk2, pk2) = make_keypair();
        let sender1 = derive_addr(&pk1);
        let sender2 = derive_addr(&pk2);
        bc.accounts.credit(&sender1, 1_000_000_000).unwrap();
        bc.accounts.credit(&sender2, 1_000_000_000).unwrap();

        let low_fee = Transaction::new(
            sender1,
            RECV.to_string(),
            100_000,
            MIN_TX_FEE,
            0,
            String::new(),
            CHAIN_ID,
            &sk1,
            &pk1,
        )
        .unwrap();
        let high_fee = Transaction::new(
            sender2,
            RECV.to_string(),
            100_000,
            MIN_TX_FEE * 10,
            0,
            String::new(),
            CHAIN_ID,
            &sk2,
            &pk2,
        )
        .unwrap();

        bc.add_to_mempool(low_fee).unwrap();
        bc.add_to_mempool(high_fee).unwrap();

        // Highest fee should be first in mempool
        assert!(bc.mempool[0].fee > bc.mempool[1].fee);
    }
}
