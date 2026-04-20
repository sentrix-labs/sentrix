// mempool.rs - Sentrix — Mempool management

use crate::blockchain::{
    Blockchain, MAX_MEMPOOL_PER_SENDER, MAX_MEMPOOL_SIZE, MEMPOOL_MAX_AGE_SECS,
    is_valid_sentrix_address,
};
use sentrix_primitives::error::{SentrixError, SentrixResult};
use sentrix_primitives::transaction::{TOKEN_OP_ADDRESS, TokenOp, Transaction};

/// M-10: per-transaction size ceiling. Bounds worst-case memory impact
/// of a single accepted mempool entry and caps block bloat from any
/// single author. 128 KiB is generous for SRX transfers (< 1 KB each)
/// and realistic SRC-20 ops (~2–4 KB) while well below the point where
/// bincode deserialisation would become a parking lot for a slow peer.
pub const MAX_TX_SIZE: usize = 128 * 1024;

impl Blockchain {
    pub fn add_to_mempool(&mut self, tx: Transaction) -> SentrixResult<()> {
        if tx.is_coinbase() {
            return Err(SentrixError::InvalidTransaction(
                "cannot manually add coinbase to mempool".to_string(),
            ));
        }

        // M-10: reject oversize txs at mempool boundary. The size estimate
        // uses `bincode::serialized_size` so it matches what libp2p would
        // actually ship over the wire, avoiding the trap where a tx looks
        // small in Rust-struct form but serialises to megabytes after
        // signatures + data expand.
        if let Ok(size) = bincode::serialized_size(&tx)
            && (size as usize) > MAX_TX_SIZE
        {
            return Err(SentrixError::InvalidTransaction(format!(
                "tx size {} bytes exceeds limit {}",
                size, MAX_TX_SIZE
            )));
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

        // Insert sorted by fee descending (highest fee at the front), with a
        // per-sender nonce constraint overriding fee when the two conflict.
        //
        // Why the constraint: the mempool's own nonce check above guarantees
        // that every new tx from a given sender has the *highest* nonce
        // among that sender's pending txs. Block production iterates the
        // mempool in order — if we let fee ordering put the high-nonce tx
        // before a same-sender lower-nonce tx already in the queue, block
        // validation rejects the high-nonce one ("expected nonce N, got
        // N+1") and the block is lost. Backlog #10 fix.
        //
        // Algorithm:
        //   1. partition_point by fee desc → O(log n), same as before.
        //   2. scan mempool for the last tx from the same sender
        //      (O(k) where k ≤ MAX_MEMPOOL_PER_SENDER ≈ 250).
        //   3. insert at max(fee_pos, same_sender_last_pos + 1).
        //
        // We deliberately keep VecDeque (not BinaryHeap) because callers
        // depend on ordered iteration: block_producer.create_block takes
        // the first N, explorer/API list mempool in order, and tests index
        // mempool[0]. BinaryHeap iteration is unordered and would force a
        // sort on every read — worse trade for read-heavy access patterns.
        // TODO: RBF (Replace-By-Fee) still not implemented.
        let fee_pos = self
            .mempool
            .partition_point(|existing| existing.fee >= tx.fee);
        let same_sender_last = self
            .mempool
            .iter()
            .rposition(|existing| existing.from_address == tx.from_address)
            .map(|i| i + 1);
        let pos = match same_sender_last {
            Some(min_pos) => fee_pos.max(min_pos),
            None => fee_pos,
        };
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
        // M-13 note: the saturating_adds here are safe under the
        // add_to_mempool invariants — any single tx with amount+fee
        // overflow is already rejected, and thousands of near-max-value
        // txs reaching the same sender would require bypassing the
        // MAX_MEMPOOL_PER_SENDER cap. Saturating at u64::MAX in the
        // pathological case is the conservative answer anyway: new-tx
        // admission will reject because `available < needed`.
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

    /// #10 — same-sender nonce order must not be reordered by a later
    /// higher-fee tx from the same sender. Without the fix, a sender who
    /// submits nonce=0 low-fee then nonce=1 high-fee would see nonce=1
    /// placed in front by fee-priority, and block production would reject
    /// it with "expected nonce 0, got 1" every round.
    #[test]
    fn test_same_sender_nonce_order_preserved_under_fee_priority() {
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 1_000_000_000).unwrap();

        let tx_nonce0_low_fee = Transaction::new(
            sender.clone(),
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
        let tx_nonce1_high_fee = Transaction::new(
            sender,
            RECV.to_string(),
            100_000,
            MIN_TX_FEE * 100,
            1,
            String::new(),
            CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();

        bc.add_to_mempool(tx_nonce0_low_fee).unwrap();
        bc.add_to_mempool(tx_nonce1_high_fee).unwrap();

        assert_eq!(
            bc.mempool[0].nonce, 0,
            "nonce=0 must stay in front of nonce=1 from same sender despite lower fee"
        );
        assert_eq!(bc.mempool[1].nonce, 1);
    }

    /// #10 — cross-sender fee priority should still work alongside the
    /// per-sender nonce constraint. When a high-fee tx arrives from a
    /// different sender, it should jump ahead of low-fee txs from others
    /// — the nonce constraint only binds txs from the *same* sender.
    #[test]
    fn test_cross_sender_fee_priority_preserved() {
        let mut bc = setup();
        let (sk_a, pk_a) = make_keypair();
        let (sk_b, pk_b) = make_keypair();
        let sender_a = derive_addr(&pk_a);
        let sender_b = derive_addr(&pk_b);
        bc.accounts.credit(&sender_a, 1_000_000_000).unwrap();
        bc.accounts.credit(&sender_b, 1_000_000_000).unwrap();

        // Sender A submits 2 txs at MIN_TX_FEE: nonce=0, nonce=1.
        for nonce in 0..2 {
            let tx = Transaction::new(
                sender_a.clone(),
                RECV.to_string(),
                100_000,
                MIN_TX_FEE,
                nonce,
                String::new(),
                CHAIN_ID,
                &sk_a,
                &pk_a,
            )
            .unwrap();
            bc.add_to_mempool(tx).unwrap();
        }

        // Sender B submits a high-fee tx.
        let tx_b_high_fee = Transaction::new(
            sender_b.clone(),
            RECV.to_string(),
            100_000,
            MIN_TX_FEE * 50,
            0,
            String::new(),
            CHAIN_ID,
            &sk_b,
            &pk_b,
        )
        .unwrap();
        bc.add_to_mempool(tx_b_high_fee).unwrap();

        // B's high-fee tx should be at the front; A's txs follow in nonce order.
        assert_eq!(bc.mempool[0].from_address, sender_b);
        assert_eq!(bc.mempool[1].from_address, sender_a);
        assert_eq!(bc.mempool[1].nonce, 0);
        assert_eq!(bc.mempool[2].from_address, sender_a);
        assert_eq!(bc.mempool[2].nonce, 1);
    }
}
