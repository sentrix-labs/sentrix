// block.rs - Sentrix

use crate::error::{SentrixError, SentrixResult};
use crate::merkle::{merkle_root, sha256_hex};
use crate::transaction::Transaction;
use hex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Block height at which `state_root` is included in `calculate_hash()`.
///
/// Before this height: hash = SHA-256(index ‖ prev_hash ‖ merkle ‖ timestamp ‖ validator)
/// At/after this height: hash = SHA-256(index ‖ prev_hash ‖ merkle ‖ timestamp ‖ validator ‖ state_root_hex)
///
/// **Hard fork**: all validators must upgrade before this height is reached.
/// Current chain height at time of writing: ~97 K — buffer of ~3 K blocks.
pub const STATE_ROOT_FORK_HEIGHT: u64 = 100_000;

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

    // ── Voyager BFT fields (Phase 2a) ────────────────────
    /// BFT round number (0 for Pioneer blocks)
    #[serde(default)]
    pub round: u32,
    /// BFT justification (precommit signatures from 2/3+1 validators).
    /// None for Pioneer blocks.
    #[serde(default)]
    pub justification: Option<crate::justification::BlockJustification>,
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
            round: 0,
            justification: None,
        };
        block.hash = block.calculate_hash();
        block
    }

    pub fn calculate_hash(&self) -> String {
        if self.index >= STATE_ROOT_FORK_HEIGHT {
            // Include state_root in the block hash to cryptographically commit the account state.
            // state_root = None → 64 zero hex chars (pre-trie or genesis sentinel).
            let state_root_hex = self
                .state_root
                .map(hex::encode)
                .unwrap_or_else(|| "0".repeat(64));
            let payload = format!(
                "{}{}{}{}{}{}",
                self.index,
                self.previous_hash,
                self.merkle_root,
                self.timestamp,
                self.validator,
                state_root_hex,
            );
            sha256_hex(payload.as_bytes())
        } else {
            // Legacy format — backward compatible with all existing blocks < FORK_HEIGHT.
            let payload = format!(
                "{}{}{}{}{}",
                self.index, self.previous_hash, self.merkle_root, self.timestamp, self.validator,
            );
            sha256_hex(payload.as_bytes())
        }
    }

    pub fn is_valid_hash(&self) -> bool {
        self.hash == self.calculate_hash()
    }

    // Genesis block — block 0, no previous hash
    // Hardcoded genesis timestamp ensures all nodes derive an identical genesis block.
    pub fn genesis() -> Self {
        const GENESIS_TIMESTAMP: u64 = 1_712_620_800; // 2024-04-09 00:00:00 UTC
        let genesis_tx = Transaction::new_coinbase("GENESIS".to_string(), 0, 0, GENESIS_TIMESTAMP);

        let txids: Vec<String> = vec![genesis_tx.txid.clone()];
        let merkle = merkle_root(&txids);

        let mut block = Self {
            index: 0,
            previous_hash: "0".repeat(64),
            transactions: vec![genesis_tx],
            timestamp: GENESIS_TIMESTAMP,
            merkle_root: merkle,
            validator: "GENESIS".to_string(),
            hash: String::new(),
            state_root: None,
            round: 0,
            justification: None,
        };
        block.hash = block.calculate_hash();
        block
    }

    pub fn tx_count(&self) -> usize {
        self.transactions.len()
    }

    // Get coinbase transaction (always first)
    pub fn coinbase(&self) -> Option<&Transaction> {
        self.transactions.first().filter(|tx| tx.is_coinbase())
    }

    // Validate block structure
    pub fn validate_structure(
        &self,
        expected_index: u64,
        expected_prev_hash: &str,
    ) -> SentrixResult<()> {
        // Check index
        if self.index != expected_index {
            return Err(SentrixError::InvalidBlock(format!(
                "expected index {}, got {}",
                expected_index, self.index
            )));
        }

        // Check previous hash link
        if self.previous_hash != expected_prev_hash {
            return Err(SentrixError::InvalidBlock(
                "invalid previous hash".to_string(),
            ));
        }

        // Check hash integrity
        if !self.is_valid_hash() {
            return Err(SentrixError::InvalidBlock(
                "block hash is invalid".to_string(),
            ));
        }

        // Must have at least coinbase
        if self.transactions.is_empty() {
            return Err(SentrixError::InvalidBlock(
                "block has no transactions".to_string(),
            ));
        }

        // First transaction must be coinbase
        if !self.transactions[0].is_coinbase() {
            return Err(SentrixError::InvalidBlock(
                "first transaction must be coinbase".to_string(),
            ));
        }

        // C-04: no subsequent transaction may be a coinbase. A forged block
        // with duplicate coinbase txs is otherwise only blocked incidentally
        // by the COINBASE-account balance check in the executor; enforce it
        // structurally so the guarantee can't be lost by later refactors.
        for tx in self.transactions.iter().skip(1) {
            if tx.is_coinbase() {
                return Err(SentrixError::InvalidBlock(
                    "only the first transaction may be coinbase".to_string(),
                ));
            }
        }

        // C-05: reject duplicate txids. The current merkle construction
        // duplicates the last element when a level has odd length, so
        // a tree [A, B, C] and a tree [A, B, C, C] hash to the same root
        // (Bitcoin CVE-2012-2459). Without this check, a proposer could
        // submit a block whose `transactions` vec contains duplicate leaves
        // and still pass merkle verification against either the 3-leaf or
        // 4-leaf root, enabling a block-validity bypass. Rejecting
        // duplicate txids at the structural layer closes that vector
        // independently of the merkle implementation, so the primitive
        // does not need a consensus-breaking switch to RFC 6962 on the
        // live chain.
        let mut seen_txids: HashSet<&str> = HashSet::with_capacity(self.transactions.len());
        for tx in &self.transactions {
            if !seen_txids.insert(tx.txid.as_str()) {
                return Err(SentrixError::InvalidBlock(format!(
                    "duplicate txid {} in block",
                    tx.txid
                )));
            }
        }

        // Verify merkle root
        let txids: Vec<String> = self.transactions.iter().map(|tx| tx.txid.clone()).collect();
        let computed_merkle = merkle_root(&txids);
        if computed_merkle != self.merkle_root {
            return Err(SentrixError::InvalidBlock(
                "merkle root mismatch".to_string(),
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

    // C-05: duplicate txids in a block must be rejected before merkle
    // verification is reached. Without this guard, the CVE-2012-2459
    // odd-duplication merkle property (see merkle.rs) would let a
    // malicious proposer pass merkle verification with a duplicated-leaf
    // block while the non-duplicated leaf set would have the same root.
    #[test]
    fn test_c05_duplicate_txids_rejected_at_block_layer() {
        let genesis = Block::genesis();
        let coinbase = Transaction::new_coinbase("v1".to_string(), 100_000_000, 1, 1_712_620_800);
        // Two structurally-identical tx records share a txid by construction
        // (Transaction::new_coinbase is deterministic for given inputs).
        let dup_a = Transaction::new_coinbase("v2".to_string(), 1, 2, 1_712_620_800);
        let dup_b = dup_a.clone();

        // Note: the dup_* leaves are themselves coinbase txs, so the
        // "only-first-coinbase" check fires first with this particular
        // fixture. Recreate them as non-coinbase clones by swapping the
        // sender field — we only need two txs with identical txids to
        // exercise the seen_txids guard.
        let mut tx_a = dup_a;
        tx_a.from_address = "0x1111111111111111111111111111111111111111".to_string();
        tx_a.txid = "deadbeef".to_string();
        let mut tx_b = dup_b;
        tx_b.from_address = "0x1111111111111111111111111111111111111111".to_string();
        tx_b.txid = "deadbeef".to_string(); // same as tx_a

        let block = Block::new(
            1,
            genesis.hash.clone(),
            vec![coinbase, tx_a, tx_b],
            "v1".to_string(),
        );
        let err = block.validate_structure(1, &genesis.hash).unwrap_err();
        assert!(
            format!("{err:?}").contains("duplicate txid"),
            "expected duplicate-txid rejection, got: {err:?}"
        );
    }

    #[test]
    fn test_c04_duplicate_coinbase_rejected() {
        // C-04: any tx at index > 0 with from=COINBASE must be rejected
        // by validate_structure. Previously only blocked by the incidental
        // balance check in the executor (COINBASE account has 0 balance).
        let genesis = Block::genesis();
        let cb1 = Transaction::new_coinbase("v1".to_string(), 100_000_000, 1, 1_712_620_801);
        let cb2 = Transaction::new_coinbase("attacker".to_string(), 999_999_999, 1, 1_712_620_801);
        let block1 = Block::new(1, genesis.hash.clone(), vec![cb1, cb2], "v1".to_string());
        let err = block1.validate_structure(1, &genesis.hash).unwrap_err();
        assert!(
            format!("{err:?}").contains("only the first transaction may be coinbase"),
            "expected duplicate-coinbase rejection, got: {err:?}"
        );
    }

    #[test]
    fn test_block_chain_link() {
        let genesis = Block::genesis();
        let block1 = Block::new(
            1,
            genesis.hash.clone(),
            vec![Transaction::new_coinbase(
                "validator1".to_string(),
                100_000_000,
                1,
                1_712_620_800,
            )],
            "validator1".to_string(),
        );
        assert!(block1.validate_structure(1, &genesis.hash).is_ok());
    }

    #[test]
    fn test_hash_below_fork_height_does_not_include_state_root() {
        // Blocks below STATE_ROOT_FORK_HEIGHT must use old hash format (no state_root).
        // This ensures backward compatibility with all existing chain history.
        let mut block = Block::genesis(); // index = 0 < 100_000
        block.state_root = Some([0xABu8; 32]);
        // Hash must NOT change when state_root is modified (old format ignores it)
        let hash_with_root = block.calculate_hash();
        block.state_root = None;
        let hash_without_root = block.calculate_hash();
        assert_eq!(
            hash_with_root, hash_without_root,
            "blocks below fork height must not include state_root in hash"
        );
    }

    #[test]
    fn test_hash_at_fork_height_includes_state_root() {
        // Blocks AT fork height must include state_root in hash.
        let mut block = Block::new(
            STATE_ROOT_FORK_HEIGHT,
            "0".repeat(64),
            vec![Transaction::new_coinbase(
                "v1".to_string(),
                100_000_000,
                STATE_ROOT_FORK_HEIGHT,
                1_712_620_800,
            )],
            "v1".to_string(),
        );
        // state_root = None → "0"*64 sentinel in hash
        let hash_no_root = block.calculate_hash();
        // Setting state_root to Some changes the hash
        block.state_root = Some([0xFFu8; 32]);
        let hash_with_root = block.calculate_hash();
        assert_ne!(
            hash_no_root, hash_with_root,
            "blocks at fork height must include state_root in hash"
        );
        // is_valid_hash must still work (block.hash was computed with old state_root=None)
        assert!(
            !block.is_valid_hash(),
            "is_valid_hash must fail when state_root changed after hash was set"
        );
    }
}
