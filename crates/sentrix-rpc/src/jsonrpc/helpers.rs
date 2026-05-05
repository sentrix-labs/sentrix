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

/// Gate state-read methods (`eth_getBalance`, `eth_getCode`,
/// `eth_getStorageAt`, `eth_getTransactionCount`, `eth_call`) against
/// historical-specific block heights.
///
/// Sentrix doesn't yet have MDBX snapshot isolation, so account state
/// reads always serve current-tip data regardless of the block tag
/// the caller passed. Pre-2026-05-05 these handlers silently ignored
/// `params[block_tag_index]` and returned latest, so a caller probing
/// "balance at h=1M" got the same answer as "balance at h=tip" — no
/// way to tell. The 2026-05-05 audit caught it for `eth_getBalance`;
/// this helper extends the same honesty to the rest of the namespace.
///
/// Returns Ok(()) when the read can be served from current state:
///   - `None` / `Null` (tag arg omitted)
///   - `""` / `"latest"` / `"pending"` / `"safe"` / `"finalized"`
///   - `"earliest"` only when chain is at h=0
///   - hex block number that equals current `latest`
///
/// Returns -32004 for any specific historical height. Returns -32602
/// for malformed inputs.
///
/// Block-content methods (`eth_getBlockByNumber`, etc.) do NOT use
/// this gate — historical block bodies live in chain.db and can be
/// served correctly. Only the account-state subset is gated.
pub(super) fn require_latest_state_read(
    block_tag: Option<&Value>,
    latest: u64,
) -> Result<(), (i32, String)> {
    let tag = match block_tag {
        None | Some(Value::Null) => return Ok(()),
        Some(Value::String(s)) => s.as_str(),
        Some(Value::Number(n)) => {
            return match n.as_u64() {
                Some(h) if h == latest => Ok(()),
                Some(_) => Err((
                    -32004,
                    "historical state reads not yet supported; use 'latest'".into(),
                )),
                None => Err((-32602, "invalid block number".into())),
            };
        }
        Some(other) => {
            return Err((-32602, format!("invalid block tag: {other}")));
        }
    };
    match tag {
        "" | "latest" | "pending" | "safe" | "finalized" => Ok(()),
        "earliest" => {
            if latest == 0 {
                Ok(())
            } else {
                Err((
                    -32004,
                    "historical state reads not yet supported; use 'latest'".into(),
                ))
            }
        }
        hex if hex.starts_with("0x") => match u64::from_str_radix(&hex[2..], 16) {
            Ok(h) if h == latest => Ok(()),
            Ok(_) => Err((
                -32004,
                "historical state reads not yet supported; use 'latest'".into(),
            )),
            Err(_) => Err((-32602, format!("invalid hex block number: {hex}"))),
        },
        _ => Err((-32602, format!("invalid block tag: {tag}"))),
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
        let Ok(log) = sentrix_codec::decode::<sentrix_evm::StoredLog>(&v) else {
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
        let Ok(log) = sentrix_codec::decode::<sentrix_evm::StoredLog>(&v) else {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Locks down the historical-state gate behavior so regressions
    // surface in CI rather than in a wallet returning stale data.
    #[test]
    fn require_latest_state_read_passes_when_no_tag() {
        assert!(require_latest_state_read(None, 1_000_000).is_ok());
    }

    #[test]
    fn require_latest_state_read_passes_for_string_tags() {
        let latest = 1_000_000;
        for tag in ["", "latest", "pending", "safe", "finalized"] {
            let v = json!(tag);
            assert!(
                require_latest_state_read(Some(&v), latest).is_ok(),
                "tag {tag:?} should pass"
            );
        }
    }

    #[test]
    fn require_latest_state_read_passes_for_hex_at_tip() {
        let v = json!("0xf4240"); // 1_000_000
        assert!(require_latest_state_read(Some(&v), 1_000_000).is_ok());
    }

    #[test]
    fn require_latest_state_read_passes_for_number_at_tip() {
        let v = json!(1_000_000);
        assert!(require_latest_state_read(Some(&v), 1_000_000).is_ok());
    }

    #[test]
    fn require_latest_state_read_rejects_historical_hex() {
        let v = json!("0x1");
        let err = require_latest_state_read(Some(&v), 1_000_000).unwrap_err();
        assert_eq!(err.0, -32004);
    }

    #[test]
    fn require_latest_state_read_rejects_historical_number() {
        let v = json!(1);
        let err = require_latest_state_read(Some(&v), 1_000_000).unwrap_err();
        assert_eq!(err.0, -32004);
    }

    #[test]
    fn require_latest_state_read_rejects_earliest_when_chain_progressed() {
        let v = json!("earliest");
        let err = require_latest_state_read(Some(&v), 1_000_000).unwrap_err();
        assert_eq!(err.0, -32004);
    }

    #[test]
    fn require_latest_state_read_passes_earliest_at_genesis() {
        let v = json!("earliest");
        assert!(require_latest_state_read(Some(&v), 0).is_ok());
    }

    #[test]
    fn require_latest_state_read_rejects_invalid_hex() {
        let v = json!("0xZZ");
        let err = require_latest_state_read(Some(&v), 1_000_000).unwrap_err();
        assert_eq!(err.0, -32602);
    }

    #[test]
    fn require_latest_state_read_rejects_garbage_tag() {
        let v = json!("not-a-block");
        let err = require_latest_state_read(Some(&v), 1_000_000).unwrap_err();
        assert_eq!(err.0, -32602);
    }
}
