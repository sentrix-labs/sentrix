// mempool.rs - Sentrix — Mempool management

use crate::core::blockchain::{
    Blockchain, is_valid_sentrix_address,
    MAX_MEMPOOL_SIZE, MAX_MEMPOOL_PER_SENDER, MEMPOOL_MAX_AGE_SECS,
};
use crate::core::transaction::Transaction;
use crate::types::error::{SentrixError, SentrixResult};

impl Blockchain {
    pub fn add_to_mempool(&mut self, tx: Transaction) -> SentrixResult<()> {
        if tx.is_coinbase() {
            return Err(SentrixError::InvalidTransaction(
                "cannot manually add coinbase to mempool".to_string()
            ));
        }

        // C-03 FIX: Global mempool size limit
        if self.mempool.len() >= MAX_MEMPOOL_SIZE {
            return Err(SentrixError::InvalidTransaction(
                "mempool full — try again later".to_string()
            ));
        }

        // C-03 FIX: Per-sender pending tx limit
        let sender_pending = self.mempool_pending_count(&tx.from_address) as usize;
        if sender_pending >= MAX_MEMPOOL_PER_SENDER {
            return Err(SentrixError::InvalidTransaction(
                "too many pending transactions from this sender".to_string()
            ));
        }

        // H-04 FIX: Validate to_address is a well-formed Sentrix address
        if !is_valid_sentrix_address(&tx.to_address) {
            return Err(SentrixError::InvalidTransaction(
                format!("invalid to_address: '{}'", tx.to_address)
            ));
        }

        // M-03/M-04 FIX: Validate transaction timestamp
        // Reject timestamps too far in the future (clock skew / pre-signed attack)
        // Reject timestamps too old (replay of stale transactions / mempool poisoning)
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if tx.timestamp > now + 300 {
            return Err(SentrixError::InvalidTransaction(
                "transaction timestamp too far in the future (max +5 min)".to_string()
            ));
        }
        if now > tx.timestamp.saturating_add(MEMPOOL_MAX_AGE_SECS) {
            return Err(SentrixError::InvalidTransaction(
                format!("transaction too old — max age {} seconds", MEMPOOL_MAX_AGE_SECS)
            ));
        }

        // Basic validation
        let expected_nonce = self.accounts.get_nonce(&tx.from_address)
            + self.mempool_pending_count(&tx.from_address);
        tx.validate(expected_nonce, self.chain_id)?;

        // Check balance including pending mempool spends
        let pending_spend = self.mempool_pending_spend(&tx.from_address);
        let available = self.accounts.get_balance(&tx.from_address)
            .saturating_sub(pending_spend);
        // H-02 FIX: checked addition to prevent overflow
        let needed = tx.amount.checked_add(tx.fee)
            .ok_or_else(|| SentrixError::InvalidTransaction(
                "amount + fee overflow".to_string()
            ))?;

        if available < needed {
            return Err(SentrixError::InsufficientBalance {
                have: available,
                need: needed,
            });
        }

        // Insert sorted by fee descending (highest fee = front of queue)
        // V5-07 TODO: RBF (Replace-By-Fee) not yet implemented — a sender cannot replace
        // a pending tx with a higher-fee version. Adding RBF requires nonce-keyed lookup
        // and per-sender replacement logic. Track in BIBLE.md under "Future Work".
        let pos = self.mempool.iter()
            .position(|existing| existing.fee < tx.fee)
            .unwrap_or(self.mempool.len());
        self.mempool.insert(pos, tx);
        Ok(())
    }

    fn mempool_pending_count(&self, address: &str) -> u64 {
        self.mempool.iter()
            .filter(|tx| tx.from_address == address)
            .count() as u64
    }

    fn mempool_pending_spend(&self, address: &str) -> u64 {
        self.mempool.iter()
            .filter(|tx| tx.from_address == address)
            .map(|tx| tx.amount.saturating_add(tx.fee))
            .fold(0u64, |acc, v| acc.saturating_add(v))
    }

    pub fn mempool_size(&self) -> usize {
        self.mempool.len()
    }

    /// M-04 FIX: Remove transactions older than MEMPOOL_MAX_AGE_SECS.
    /// Called automatically after each block is added; also callable manually.
    pub fn prune_mempool(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.mempool.retain(|tx| now <= tx.timestamp.saturating_add(MEMPOOL_MAX_AGE_SECS));
    }
}
