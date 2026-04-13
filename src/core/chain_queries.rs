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
    pub fn get_address_history(&self, address: &str, limit: usize, offset: usize) -> Vec<serde_json::Value> {
        let mut history = Vec::new();
        let mut skipped = 0usize;
        for block in &self.chain {
            for tx in &block.transactions {
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

    pub fn get_address_tx_count(&self, address: &str) -> usize {
        self.chain.iter()
            .flat_map(|b| &b.transactions)
            .filter(|tx| tx.from_address == address || tx.to_address == address)
            .count()
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

    // ── Stats ────────────────────────────────────────────
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
        })
    }
}
