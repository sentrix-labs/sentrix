// helpers.rs — shared helpers used by all JSON-RPC namespace modules
// (eth / net / web3 / sentrix). Pulled out of the monolithic
// `jsonrpc.rs` during the backlog #11 refactor so each namespace file
// can stay focused on its handlers.
//
// Pure hex / address / number conversion helpers moved to the new
// `sentrix-rpc-types` crate (2026-04-22 split per CRATE_SPLIT_PLAN.md
// Tier 1). Re-exported here as pub(super) shims so the existing
// callers in eth.rs / sentrix.rs / net.rs / web3.rs don't need their
// import paths changed in this PR. A follow-up PR will migrate call
// sites to `use sentrix_rpc_types::*` directly and delete the shims.

use serde_json::Value;

pub(super) use sentrix_rpc_types::{
    normalize_rpc_address, normalize_rpc_hash, parse_hex_u64, to_hex, to_hex_u128,
};

pub(super) fn resolve_block_tag(v: Option<&Value>, latest: u64) -> Result<u64, &'static str> {
    match v {
        None => Ok(latest),
        Some(Value::String(s)) => match s.as_str() {
            "latest" | "pending" | "safe" | "finalized" => Ok(latest),
            "earliest" => Ok(0),
            hex if hex.starts_with("0x") => {
                u64::from_str_radix(&hex[2..], 16).map_err(|_| "invalid hex block number")
            }
            _ => Err("invalid block tag"),
        },
        Some(Value::Number(n)) => n.as_u64().ok_or("invalid block number"),
        _ => Err("invalid block parameter"),
    }
}

/// Address filter accepts either a single string or an array. Normalizes to
/// lowercase 20-byte arrays; unparseable entries are silently skipped so
/// malformed filters still work against the rest of the query.
pub(super) fn parse_address_filter(v: Option<&Value>) -> Vec<[u8; 20]> {
    let mut out = Vec::new();
    let push = |s: &str, out: &mut Vec<[u8; 20]>| {
        let s = s.trim_start_matches("0x");
        if let Ok(bytes) = hex::decode(s)
            && bytes.len() == 20
        {
            let mut arr = [0u8; 20];
            arr.copy_from_slice(&bytes);
            out.push(arr);
        }
    };
    match v {
        None | Some(Value::Null) => {}
        Some(Value::String(s)) => push(s, &mut out),
        Some(Value::Array(arr)) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    push(s, &mut out);
                }
            }
        }
        _ => {}
    }
    out
}

/// Topics filter: outer index is position (topic0..topic3). Inner vec is the
/// OR-set for that position — empty means wildcard. `None` in topics[i]
/// means the position is omitted entirely (also wildcard).
pub(super) type TopicFilter = Vec<Option<Vec<[u8; 32]>>>;

pub(super) fn parse_topic_filter(v: Option<&Value>) -> TopicFilter {
    let mut out: TopicFilter = Vec::new();
    let parse_one = |s: &str| -> Option<[u8; 32]> {
        let s = s.trim_start_matches("0x");
        let bytes = hex::decode(s).ok()?;
        if bytes.len() != 32 {
            return None;
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Some(arr)
    };
    let arr = match v {
        Some(Value::Array(a)) => a,
        _ => return out,
    };
    for slot in arr {
        match slot {
            Value::Null => out.push(None),
            Value::String(s) => out.push(Some(parse_one(s).into_iter().collect())),
            Value::Array(inner) => {
                let set: Vec<[u8; 32]> = inner
                    .iter()
                    .filter_map(|x| x.as_str().and_then(parse_one))
                    .collect();
                out.push(Some(set));
            }
            _ => out.push(None),
        }
    }
    out
}

pub(super) fn log_matches(
    log: &sentrix_evm::StoredLog,
    addrs: &[[u8; 20]],
    topics: &TopicFilter,
) -> bool {
    if !addrs.is_empty() && !addrs.iter().any(|a| a == &log.address) {
        return false;
    }
    for (i, slot) in topics.iter().enumerate() {
        let Some(set) = slot else { continue };
        if set.is_empty() {
            continue;
        }
        let topic = match log.topics.get(i) {
            Some(t) => t,
            None => return false,
        };
        if !set.iter().any(|s| s == topic) {
            return false;
        }
    }
    true
}

pub(super) fn collect_logs(
    bc: &sentrix_core::blockchain::Blockchain,
    from: u64,
    to: u64,
    addrs: &[[u8; 20]],
    topics: &TopicFilter,
) -> Vec<Value> {
    let storage = match bc.mdbx_storage.as_ref() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let all = match storage.iter(sentrix_storage::tables::TABLE_LOGS) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for (k, v) in all {
        if k.len() < 16 {
            continue;
        }
        let mut h_bytes = [0u8; 8];
        h_bytes.copy_from_slice(&k[..8]);
        let height = u64::from_be_bytes(h_bytes);
        if height < from || height > to {
            continue;
        }
        if let Some(bloom_bytes) = storage
            .get(sentrix_storage::tables::TABLE_BLOOM, &h_bytes)
            .ok()
            .flatten()
            && bloom_bytes.len() == 256
            && !addrs.is_empty()
        {
            let mut bloom = [0u8; 256];
            bloom.copy_from_slice(&bloom_bytes);
            let any_hit = addrs.iter().any(|a| sentrix_evm::bloom_contains(&bloom, a));
            if !any_hit {
                continue;
            }
        }
        let Ok(log) = bincode::deserialize::<sentrix_evm::StoredLog>(&v) else {
            continue;
        };
        if log_matches(&log, addrs, topics) {
            out.push(log.to_rpc_json());
        }
    }
    out
}

pub(super) fn load_logs_for_tx(
    bc: &sentrix_core::blockchain::Blockchain,
    block_height: u64,
    txid: &str,
) -> (Vec<Value>, String) {
    let mut target_hash = [0u8; 32];
    if let Ok(decoded) = hex::decode(txid.trim_start_matches("0x")) {
        let n = decoded.len().min(32);
        target_hash[..n].copy_from_slice(&decoded[..n]);
    }
    let storage = match bc.mdbx_storage.as_ref() {
        Some(s) => s,
        None => return (Vec::new(), "0x".to_string() + &"00".repeat(256)),
    };
    let prefix = block_height.to_be_bytes();
    let entries = match storage.iter(sentrix_storage::tables::TABLE_LOGS) {
        Ok(v) => v,
        Err(_) => return (Vec::new(), "0x".to_string() + &"00".repeat(256)),
    };
    let mut logs = Vec::new();
    let mut bloom = sentrix_evm::empty_bloom();
    for (k, v) in entries {
        if k.len() < 8 || k[..8] != prefix {
            continue;
        }
        let Ok(log) = bincode::deserialize::<sentrix_evm::StoredLog>(&v) else {
            continue;
        };
        if log.tx_hash == target_hash {
            sentrix_evm::add_log_to_bloom(&mut bloom, &log.address, &log.topics);
            logs.push(log.to_rpc_json());
        }
    }
    (logs, format!("0x{}", hex::encode(bloom)))
}

pub(super) fn block_gas_used_ratio(bc: &sentrix_core::blockchain::Blockchain, height: u64) -> f64 {
    let block = match bc.chain.iter().find(|b| b.index == height) {
        Some(b) => b,
        None => return 0.0,
    };
    let total_gas: u64 = block
        .transactions
        .iter()
        .filter(|t| t.is_evm_tx())
        .map(|_| 21_000u64)
        .sum();
    (total_gas as f64) / (sentrix_evm::gas::BLOCK_GAS_LIMIT as f64)
}
