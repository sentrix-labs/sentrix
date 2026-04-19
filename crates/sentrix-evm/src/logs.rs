// logs.rs — Persisted EVM log + Ethereum-standard logs bloom helper.
//
// `StoredLog` decouples on-disk format from revm's in-memory `Log` so a future
// revm version bump cannot silently break MDBX reads. `compute_logs_bloom`
// implements the 2048-bit bloom per yellow paper section 4.4.3: for each
// loggable item (address + every topic) keccak-256 the bytes, take three
// pairs of bytes, mask each to 11 bits, and set that bit in the 256-byte
// filter. Used for both per-block prefilter and eth_getTransactionReceipt
// logsBloom output.

use alloy_primitives::{Address, B256, Bytes};
use serde::{Deserialize, Serialize};
use sha3::{Digest, Keccak256};

pub type LogsBloom = [u8; 256];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredLog {
    pub address: [u8; 20],
    pub topics: Vec<[u8; 32]>,
    pub data: Vec<u8>,
    pub block_number: u64,
    pub block_hash: [u8; 32],
    pub tx_hash: [u8; 32],
    pub tx_index: u32,
    pub log_index: u32,
}

impl StoredLog {
    pub fn from_revm(
        log: &revm::primitives::Log,
        block_number: u64,
        block_hash: [u8; 32],
        tx_hash: [u8; 32],
        tx_index: u32,
        log_index: u32,
    ) -> Self {
        let mut address = [0u8; 20];
        address.copy_from_slice(log.address.as_slice());
        let topics: Vec<[u8; 32]> = log
            .data
            .topics()
            .iter()
            .map(|t| {
                let mut a = [0u8; 32];
                a.copy_from_slice(t.as_slice());
                a
            })
            .collect();
        Self {
            address,
            topics,
            data: log.data.data.to_vec(),
            block_number,
            block_hash,
            tx_hash,
            tx_index,
            log_index,
        }
    }

    pub fn address_hex(&self) -> String {
        format!("0x{}", hex::encode(self.address))
    }

    pub fn to_rpc_json(&self) -> serde_json::Value {
        serde_json::json!({
            "removed": false,
            "logIndex": format!("0x{:x}", self.log_index),
            "transactionIndex": format!("0x{:x}", self.tx_index),
            "transactionHash": format!("0x{}", hex::encode(self.tx_hash)),
            "blockHash": format!("0x{}", hex::encode(self.block_hash)),
            "blockNumber": format!("0x{:x}", self.block_number),
            "address": self.address_hex(),
            "data": format!("0x{}", hex::encode(&self.data)),
            "topics": self.topics.iter().map(|t| format!("0x{}", hex::encode(t))).collect::<Vec<_>>(),
        })
    }
}

pub fn empty_bloom() -> LogsBloom {
    [0u8; 256]
}

fn add_to_bloom(bloom: &mut LogsBloom, bytes: &[u8]) {
    let hash = Keccak256::digest(bytes);
    for i in 0..3 {
        let bit_pos = (((hash[i * 2] as usize) << 8) | hash[i * 2 + 1] as usize) & 0x07FF;
        let byte_idx = 255 - (bit_pos / 8);
        let bit_idx = bit_pos % 8;
        bloom[byte_idx] |= 1 << bit_idx;
    }
}

pub fn add_log_to_bloom(bloom: &mut LogsBloom, address: &[u8; 20], topics: &[[u8; 32]]) {
    add_to_bloom(bloom, address);
    for t in topics {
        add_to_bloom(bloom, t);
    }
}

pub fn compute_logs_bloom(logs: &[StoredLog]) -> LogsBloom {
    let mut bloom = empty_bloom();
    for log in logs {
        add_log_to_bloom(&mut bloom, &log.address, &log.topics);
    }
    bloom
}

pub fn bloom_union(a: &LogsBloom, b: &LogsBloom) -> LogsBloom {
    let mut out = [0u8; 256];
    for i in 0..256 {
        out[i] = a[i] | b[i];
    }
    out
}

/// Check whether `needle` might be present in `bloom` — false positives
/// possible, false negatives impossible.
pub fn bloom_contains(bloom: &LogsBloom, needle: &[u8]) -> bool {
    let hash = Keccak256::digest(needle);
    for i in 0..3 {
        let bit_pos = (((hash[i * 2] as usize) << 8) | hash[i * 2 + 1] as usize) & 0x07FF;
        let byte_idx = 255 - (bit_pos / 8);
        let bit_idx = bit_pos % 8;
        if bloom[byte_idx] & (1 << bit_idx) == 0 {
            return false;
        }
    }
    true
}

pub fn log_key(height: u64, tx_index: u32, log_index: u32) -> [u8; 16] {
    let mut k = [0u8; 16];
    k[0..8].copy_from_slice(&height.to_be_bytes());
    k[8..12].copy_from_slice(&tx_index.to_be_bytes());
    k[12..16].copy_from_slice(&log_index.to_be_bytes());
    k
}

pub fn log_key_prefix(height: u64) -> [u8; 8] {
    height.to_be_bytes()
}

// Keep alloy types used by callers
pub use alloy_primitives::{Address as AlloyAddress, B256 as AlloyB256, Bytes as AlloyBytes};

// Silence unused import warnings if user imports full types
#[allow(dead_code)]
fn _force_import(_: Address, _: B256, _: Bytes) {}
