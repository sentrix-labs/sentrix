// chain_queries.rs - Sentrix — Chain query methods

use crate::core::blockchain::{Blockchain, MAX_SUPPLY};
use crate::core::transaction::TokenOp;

impl Blockchain {
    // ── Transaction queries ──────────────────────────────

    pub fn get_transaction(&self, txid: &str) -> Option<serde_json::Value> {
        for block in self.chain.iter().rev() {
            for tx in &block.transactions {
                if tx.txid == txid {
                    return Some(serde_json::json!({
                        "transaction": tx,
                        "block_index": block.index,
                        "block_hash": block.hash,
                        "block_timestamp": block.timestamp,
                    }));
                }
            }
        }
        None
    }

    // L-03 FIX: paginated address history (limit + offset)
    // V6-L-02 FIX: standardized to newest-first (most recent at offset=0),
    // consistent with get_latest_transactions.
    pub fn get_address_history(&self, address: &str, limit: usize, offset: usize) -> Vec<serde_json::Value> {
        let mut history = Vec::new();
        let mut skipped = 0usize;
        // newest-first: reverse block order, reverse tx order within each block
        for block in self.chain.iter().rev() {
            for tx in block.transactions.iter().rev() {
                if tx.from_address == address || tx.to_address == address {
                    if skipped < offset {
                        skipped += 1;
                        continue;
                    }
                    if history.len() >= limit {
                        return history;
                    }
                    let direction = if tx.from_address == address {
                        if tx.is_coinbase() { "reward" } else { "out" }
                    } else {
                        "in"
                    };
                    history.push(serde_json::json!({
                        "txid": tx.txid,
                        "direction": direction,
                        "from": tx.from_address,
                        "to": tx.to_address,
                        "amount": tx.amount,
                        "fee": tx.fee,
                        "block_index": block.index,
                        "block_timestamp": block.timestamp,
                    }));
                }
            }
        }
        history
    }

    // V6-M-04 FIX: returns window-aware count with metadata indicating partial coverage.
    // After CHAIN_WINDOW_SIZE blocks, historical blocks are evicted from memory.
    // Use the returned `is_partial` flag to warn users the count may be incomplete.
    pub fn get_address_tx_count(&self, address: &str) -> serde_json::Value {
        let count = self.chain.iter()
            .flat_map(|b| &b.transactions)
            .filter(|tx| tx.from_address == address || tx.to_address == address)
            .count();
        let window_start = self.chain_window_start();
        serde_json::json!({
            "window_tx_count": count,
            "window_start_block": window_start,
            "is_partial": window_start > 0,
        })
    }

    pub fn get_latest_transactions(&self, limit: usize, offset: usize) -> Vec<serde_json::Value> {
        let mut result = Vec::new();
        let mut skipped = 0usize;
        for block in self.chain.iter().rev() {
            for tx in block.transactions.iter().rev() {
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                if result.len() >= limit {
                    return result;
                }
                result.push(serde_json::json!({
                    "txid": tx.txid,
                    "from": tx.from_address,
                    "to": tx.to_address,
                    "amount": tx.amount,
                    "fee": tx.fee,
                    "is_coinbase": tx.is_coinbase(),
                    "block_index": block.index,
                    "block_timestamp": block.timestamp,
                }));
            }
        }
        result
    }

    pub fn get_token_holders(&self, contract: &str) -> Option<Vec<serde_json::Value>> {
        self.contracts.get_holders_list(contract)
    }

    pub fn get_token_trades(&self, contract_addr: &str, limit: usize, offset: usize) -> Vec<serde_json::Value> {
        let mut result = Vec::new();
        let mut skipped = 0usize;
        for block in self.chain.iter().rev() {
            for tx in block.transactions.iter() {
                let entry = match TokenOp::decode(&tx.data) {
                    Some(TokenOp::Transfer { contract, to, amount }) if contract == contract_addr => {
                        Some(serde_json::json!({
                            "type": "transfer",
                            "from": tx.from_address,
                            "to": to,
                            "amount": amount,
                            "txid": tx.txid,
                            "block_index": block.index,
                            "block_timestamp": block.timestamp,
                        }))
                    }
                    Some(TokenOp::Burn { contract, amount }) if contract == contract_addr => {
                        Some(serde_json::json!({
                            "type": "burn",
                            "from": tx.from_address,
                            "to": serde_json::Value::Null,
                            "amount": amount,
                            "txid": tx.txid,
                            "block_index": block.index,
                            "block_timestamp": block.timestamp,
                        }))
                    }
                    Some(TokenOp::Mint { contract, to, amount }) if contract == contract_addr => {
                        Some(serde_json::json!({
                            "type": "mint",
                            "from": tx.from_address,
                            "to": to,
                            "amount": amount,
                            "txid": tx.txid,
                            "block_index": block.index,
                            "block_timestamp": block.timestamp,
                        }))
                    }
                    _ => None,
                };
                if let Some(e) = entry {
                    if skipped < offset {
                        skipped += 1;
                    } else {
                        result.push(e);
                        if result.len() >= limit {
                            return result;
                        }
                    }
                }
            }
        }
        result
    }

    // ── Stats + window info ──────────────────────────────
    pub fn chain_stats(&self) -> serde_json::Value {
        serde_json::json!({
            "height": self.height(),
            "total_blocks": self.height() + 1, // I-01 FIX: chain window may be < total blocks
            "total_minted_srx": self.total_minted as f64 / 100_000_000.0,
            "max_supply_srx": MAX_SUPPLY as f64 / 100_000_000.0,
            "total_burned_srx": self.accounts.total_burned as f64 / 100_000_000.0,
            "mempool_size": self.mempool.len(),
            "active_validators": self.authority.active_count(),
            "deployed_tokens": self.contracts.contract_count(),
            "chain_id": self.chain_id,
            "next_block_reward_srx": self.get_block_reward() as f64 / 100_000_000.0,
            // V6-I-01: expose window metadata so callers know query coverage
            "window_start_block": self.chain_window_start(),
            "window_is_partial": self.chain_window_start() > 0,
        })
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

    // V6-M-04 test: get_address_tx_count returns window-aware metadata
    #[test]
    fn test_get_address_tx_count_returns_metadata() {
        let bc = setup();
        let result = bc.get_address_tx_count("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef");
        // Must return a JSON object with window_tx_count and is_partial fields
        assert!(result["window_tx_count"].is_number());
        assert!(result["is_partial"].is_boolean());
        assert!(result["window_start_block"].is_number());
    }

    // V6-L-02 test: get_address_history returns newest first (consistent with get_latest_transactions)
    #[test]
    fn test_address_history_newest_first() {
        let mut bc = setup();
        let (sk, pk) = make_keypair();
        let sender = derive_addr(&pk);
        bc.accounts.credit(&sender, 10_000_000_000).unwrap();

        // Add two transactions and mine them in separate blocks
        for i in 0..2 {
            let tx = Transaction::new(
                sender.clone(), RECV.to_string(),
                100_000, MIN_TX_FEE, i, String::new(), CHAIN_ID, &sk, &pk,
            ).unwrap();
            bc.add_to_mempool(tx).unwrap();
            let block = bc.create_block("v1").unwrap();
            bc.add_block(block).unwrap();
        }

        let history = bc.get_address_history(&sender, 10, 0);
        assert_eq!(history.len(), 2);
        // Newest first: second tx (block 2) should come before first tx (block 1)
        let first_block = history[0]["block_index"].as_u64().unwrap();
        let second_block = history[1]["block_index"].as_u64().unwrap();
        assert!(first_block >= second_block, "history must be newest-first");
    }

    // get_latest_transactions: pagination offset works correctly
    #[test]
    fn test_get_latest_transactions_pagination() {
        let mut bc = setup();
        // Mine 3 empty blocks (coinbase only)
        for _ in 0..3 {
            let block = bc.create_block("v1").unwrap();
            bc.add_block(block).unwrap();
        }
        let page1 = bc.get_latest_transactions(2, 0);
        let page2 = bc.get_latest_transactions(2, 2);
        // All coinbase txs are unique (different block heights)
        assert_eq!(page1.len(), 2);
        assert!(!page2.is_empty());
        // No overlap between pages
        let ids1: Vec<&str> = page1.iter().map(|t| t["txid"].as_str().unwrap()).collect();
        let ids2: Vec<&str> = page2.iter().map(|t| t["txid"].as_str().unwrap()).collect();
        for id in &ids2 {
            assert!(!ids1.contains(id), "pagination pages must not overlap");
        }
    }
}
