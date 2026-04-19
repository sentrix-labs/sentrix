// explorer_api.rs - Sentrix
//
// New explorer / wallet REST endpoints requested by the sentrix-scan
// and sentrix-wallet-web frontends. Kept in its own module so the
// route-registration in routes.rs stays grep-able and the handler
// implementations don't clutter routes.rs further.
//
// CRITICAL (block explorer / wallet pages 404 without these):
//   GET /accounts/{address}/history?page=N&limit=M
//   GET /chain/blocks (existing, but response shape standardised here)
//
// HIGH (major explorer features):
//   GET /accounts/top?sort=balance&limit=N&page=1
//   GET /accounts/{address}/tokens
//   GET /tokens/{contract}/holders
//   GET /tokens/{contract}/transfers?page=N&limit=M
//
// NICE (analytics / charts):
//   GET /chain/performance?range=1h|6h|24h
//   GET /validators/{address}/delegators?page=N
//   GET /validators/{address}/rewards?page=N
//   GET /validators/{address}/blocks-over-time?range=1h|24h
//
// ENHANCEMENT:
//   GET /accounts/{address}/code

use crate::routes::SharedState;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use sentrix_core::blockchain::MAX_SUPPLY;
use std::collections::HashMap;

const SENTRI_PER_SRX: f64 = 100_000_000.0;

fn parse_page_limit(params: &HashMap<String, String>) -> (usize, usize) {
    let page: usize = params.get("page").and_then(|p| p.parse().ok()).unwrap_or(1).max(1);
    let limit: usize = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(20)
        .clamp(1, 100);
    (page, limit)
}

// ─── #1 GET /accounts/{address}/history?page=N&limit=M ───────────────
pub async fn accounts_history(
    State(state): State<SharedState>,
    Path(address): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let address = address.to_lowercase();
    let (page, limit) = parse_page_limit(&params);
    let offset = (page - 1) * limit;
    let bc = state.read().await;
    // get_address_history uses offset-based pagination — fetch one extra
    // to compute `total` cheaply within the window, else estimate
    // window-wide by counting.
    let history = bc.get_address_history(&address, limit, offset);
    // `total` for this response is the window-scan count; the query
    // returns is_partial when CHAIN_WINDOW_SIZE caps it.
    let total_info = bc.get_address_tx_count(&address);
    let total = total_info
        .get("window_tx_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(history.len() as u64);

    // Enrich each row with the shape the frontend expects.
    let transactions: Vec<serde_json::Value> = history
        .into_iter()
        .map(|tx| {
            let txid = tx.get("txid").cloned().unwrap_or_default();
            let from = tx.get("from").cloned().unwrap_or_default();
            let to = tx.get("to").cloned().unwrap_or_default();
            let amount_sentri = tx.get("amount").and_then(|v| v.as_u64()).unwrap_or(0);
            let fee_sentri = tx.get("fee").and_then(|v| v.as_u64()).unwrap_or(0);
            let block_height = tx.get("block_index").and_then(|v| v.as_u64()).unwrap_or(0);
            let timestamp = tx
                .get("block_timestamp")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            serde_json::json!({
                "id": txid,
                "from": from,
                "to": to,
                "amount": amount_sentri as f64 / SENTRI_PER_SRX,
                "fee": fee_sentri as f64 / SENTRI_PER_SRX,
                "timestamp": timestamp,
                "nonce": tx.get("nonce"),
                "status": "success",
                "block_height": block_height,
                "direction": tx.get("direction"),
            })
        })
        .collect();

    Json(serde_json::json!({
        "transactions": transactions,
        "total": total,
        "page": page,
        "limit": limit,
        "is_partial": total_info.get("is_partial").and_then(|v| v.as_bool()).unwrap_or(false),
    }))
}

// ─── #3 GET /accounts/top?sort=balance&limit=N&page=1 ────────────────
pub async fn accounts_top(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let (page, limit) = parse_page_limit(&params);
    let sort = params
        .get("sort")
        .cloned()
        .unwrap_or_else(|| "balance".to_string());
    let bc = state.read().await;

    let mut all: Vec<(String, u64)> = bc
        .accounts
        .accounts
        .iter()
        .filter(|(_, a)| a.balance > 0)
        .map(|(addr, a)| (addr.clone(), a.balance))
        .collect();

    // Only "balance" sort supported today; other `sort` values fall
    // through to balance-descending rather than erroring so the frontend
    // can probe new sort keys without 500-ing.
    let _ = sort;
    all.sort_by_key(|e| std::cmp::Reverse(e.1));

    let total = all.len();
    let offset = (page - 1) * limit;

    let accounts: Vec<serde_json::Value> = all
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(address, balance)| {
            let tx_count = bc
                .get_address_tx_count(&address)
                .get("window_tx_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let pct = balance as f64 / MAX_SUPPLY as f64 * 100.0;
            serde_json::json!({
                "address": address,
                "balance_srx": balance as f64 / SENTRI_PER_SRX,
                "balance_sentri": balance,
                "percentage": pct,
                "tx_count": tx_count,
                "name": serde_json::Value::Null,
            })
        })
        .collect();

    Json(serde_json::json!({
        "accounts": accounts,
        "total": total,
        "page": page,
        "limit": limit,
    }))
}

// ─── #4 GET /accounts/{address}/tokens ───────────────────────────────
pub async fn accounts_tokens(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let address = address.to_lowercase();
    let bc = state.read().await;
    // Iterate the contract registry and collect non-zero balances.
    let tokens: Vec<serde_json::Value> = bc
        .list_tokens()
        .into_iter()
        .filter_map(|token_info| {
            let contract = token_info
                .get("contract_address")
                .and_then(|v| v.as_str())?
                .to_string();
            let balance = bc.contracts.get_token_balance(&contract, &address);
            if balance == 0 {
                return None;
            }
            Some(serde_json::json!({
                "contract_address": contract,
                "name": token_info.get("name"),
                "symbol": token_info.get("symbol"),
                "decimals": token_info.get("decimals"),
                "balance": balance,
            }))
        })
        .collect();

    Json(serde_json::json!({
        "tokens": tokens,
        "total": tokens.len(),
    }))
}

// ─── #5 GET /tokens/{contract}/holders (standardised response) ───────
pub async fn tokens_holders(
    State(state): State<SharedState>,
    Path(contract): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let (_page, limit) = parse_page_limit(&params);
    let bc = state.read().await;
    match bc.get_token_holders(&contract) {
        Some(mut holders) => {
            // Compute total supply for percentage calculation.
            let total_supply: u64 = holders
                .iter()
                .map(|h| h.get("balance").and_then(|v| v.as_u64()).unwrap_or(0))
                .sum();
            // Sort desc by balance.
            holders.sort_by_key(|h| {
                std::cmp::Reverse(h.get("balance").and_then(|v| v.as_u64()).unwrap_or(0))
            });
            let total_holders = holders.len();
            let rows: Vec<serde_json::Value> = holders
                .into_iter()
                .take(limit)
                .map(|h| {
                    let bal = h.get("balance").and_then(|v| v.as_u64()).unwrap_or(0);
                    let pct = if total_supply > 0 {
                        bal as f64 / total_supply as f64 * 100.0
                    } else {
                        0.0
                    };
                    serde_json::json!({
                        "address": h.get("address"),
                        "balance": bal,
                        "percentage": pct,
                    })
                })
                .collect();
            Ok(Json(serde_json::json!({
                "holders": rows,
                "total": total_holders,
            })))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

// ─── #6 GET /tokens/{contract}/transfers?page=N&limit=M ──────────────
pub async fn tokens_transfers(
    State(state): State<SharedState>,
    Path(contract): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let (page, limit) = parse_page_limit(&params);
    let offset = (page - 1) * limit;
    let bc = state.read().await;
    let trades = bc.get_token_trades(&contract, limit, offset);

    let transfers: Vec<serde_json::Value> = trades
        .into_iter()
        .map(|t| {
            serde_json::json!({
                "tx_hash": t.get("txid"),
                "from": t.get("from"),
                "to": t.get("to"),
                "amount": t.get("amount"),
                "timestamp": t.get("block_timestamp"),
                "block_height": t.get("block_index"),
            })
        })
        .collect();

    let count = transfers.len();
    Json(serde_json::json!({
        "transfers": transfers,
        "pagination": {
            "page": page,
            "limit": limit,
            "returned": count,
            "has_more": count == limit,
        },
    }))
}

// ─── #7 GET /chain/performance?range=1h|6h|24h ───────────────────────
pub async fn chain_performance(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let range = params
        .get("range")
        .cloned()
        .unwrap_or_else(|| "1h".to_string());
    let window_secs: u64 = match range.as_str() {
        "6h" => 6 * 3600,
        "24h" => 24 * 3600,
        _ => 3600,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cutoff = now.saturating_sub(window_secs);

    let bc = state.read().await;
    // Pick blocks whose timestamp falls in the window (in-memory window only).
    let mut samples: Vec<(u64, u64, u64)> = bc
        .chain
        .iter()
        .filter(|b| b.timestamp >= cutoff)
        .map(|b| (b.timestamp, b.tx_count() as u64, b.index))
        .collect();
    samples.sort_by_key(|s| s.0);

    // Aggregate to ~30 datapoints so charts don't blow up.
    let bucket_secs = (window_secs / 30).max(1);
    let mut buckets: std::collections::BTreeMap<u64, (u64, u64, u64)> =
        std::collections::BTreeMap::new();
    for (ts, tx, _h) in &samples {
        let key = (ts / bucket_secs) * bucket_secs;
        let entry = buckets.entry(key).or_insert((0, 0, 0));
        entry.0 += 1; // block count
        entry.1 += *tx; // tx count
        entry.2 = *ts; // latest ts in bucket
    }

    let points: Vec<serde_json::Value> = buckets
        .iter()
        .map(|(ts, (block_count, tx_count, _))| {
            let tps = *tx_count as f64 / bucket_secs as f64;
            let block_time = bucket_secs as f64 / (*block_count).max(1) as f64;
            serde_json::json!({
                "timestamp": ts,
                "tps": tps,
                "block_time_sec": block_time,
                "tx_count": tx_count,
                "block_count": block_count,
            })
        })
        .collect();

    let total_tx: u64 = samples.iter().map(|s| s.1).sum();
    let total_blocks = samples.len() as u64;
    let avg_tps = total_tx as f64 / window_secs as f64;
    let peak_tps = points
        .iter()
        .filter_map(|p| p.get("tps").and_then(|v| v.as_f64()))
        .fold(0.0_f64, |m, v| m.max(v));

    Json(serde_json::json!({
        "points": points,
        "range": range,
        "peak_tps": peak_tps,
        "avg_tps": avg_tps,
        "total_blocks": total_blocks,
        "total_tx": total_tx,
    }))
}

// ─── #8 GET /validators/{address}/delegators?page=N ──────────────────
pub async fn validator_delegators(
    State(state): State<SharedState>,
    Path(address): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let address = address.to_lowercase();
    let (page, limit) = parse_page_limit(&params);
    let offset = (page - 1) * limit;
    let bc = state.read().await;

    let mut rows: Vec<(String, u64, u64)> = bc
        .stake_registry
        .delegations
        .iter()
        .flat_map(|(delegator, entries)| {
            entries
                .iter()
                .filter(|e| e.validator == address)
                .map(move |e| (delegator.clone(), e.amount, e.height))
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));

    let total_delegated: u64 = rows.iter().map(|r| r.1).sum();
    let total = rows.len();

    let delegators: Vec<serde_json::Value> = rows
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|(addr, staked, since)| {
            let share = if total_delegated > 0 {
                staked as f64 / total_delegated as f64
            } else {
                0.0
            };
            serde_json::json!({
                "address": addr,
                "staked": staked,
                "staked_srx": staked as f64 / SENTRI_PER_SRX,
                "share": share,
                "since": since,
            })
        })
        .collect();

    Json(serde_json::json!({
        "delegators": delegators,
        "total": total,
        "total_delegated_sentri": total_delegated,
        "total_delegated_srx": total_delegated as f64 / SENTRI_PER_SRX,
        "page": page,
        "limit": limit,
    }))
}

// ─── #9 GET /validators/{address}/rewards?page=N ─────────────────────
pub async fn validator_rewards(
    State(state): State<SharedState>,
    Path(address): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let address = address.to_lowercase();
    let (page, limit) = parse_page_limit(&params);
    let offset = (page - 1) * limit;
    let bc = state.read().await;

    let mut rewards: Vec<serde_json::Value> = Vec::new();
    let mut total_earned_sentri: u64 = 0;
    let mut skipped = 0usize;
    for block in bc.chain.iter().rev() {
        if block.validator.to_lowercase() != address {
            continue;
        }
        if let Some(cb) = block.coinbase() {
            total_earned_sentri = total_earned_sentri.saturating_add(cb.amount);
            if skipped < offset {
                skipped += 1;
                continue;
            }
            if rewards.len() >= limit {
                continue;
            }
            rewards.push(serde_json::json!({
                "block_height": block.index,
                "amount_sentri": cb.amount,
                "amount": cb.amount as f64 / SENTRI_PER_SRX,
                "timestamp": block.timestamp,
            }));
        }
    }

    Json(serde_json::json!({
        "rewards": rewards,
        "total_earned_sentri": total_earned_sentri,
        "total_earned_srx": total_earned_sentri as f64 / SENTRI_PER_SRX,
        "pagination": {
            "page": page,
            "limit": limit,
            "returned": rewards.len(),
            "has_more": rewards.len() == limit,
        },
    }))
}

// ─── #10 GET /validators/{address}/blocks-over-time?range=1h|24h ─────
pub async fn validator_blocks_over_time(
    State(state): State<SharedState>,
    Path(address): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let address = address.to_lowercase();
    let range = params
        .get("range")
        .cloned()
        .unwrap_or_else(|| "1h".to_string());
    let window_secs: u64 = match range.as_str() {
        "24h" => 24 * 3600,
        _ => 3600,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cutoff = now.saturating_sub(window_secs);

    let bc = state.read().await;
    let bucket_secs = (window_secs / 30).max(1);
    let mut buckets: std::collections::BTreeMap<u64, u64> = std::collections::BTreeMap::new();
    for b in bc
        .chain
        .iter()
        .filter(|b| b.timestamp >= cutoff && b.validator.to_lowercase() == address)
    {
        let key = (b.timestamp / bucket_secs) * bucket_secs;
        *buckets.entry(key).or_insert(0) += 1;
    }

    let points: Vec<serde_json::Value> = buckets
        .into_iter()
        .map(|(ts, count)| serde_json::json!({ "timestamp": ts, "count": count }))
        .collect();

    Json(serde_json::json!({
        "points": points,
        "range": range,
    }))
}

// ─── #13 GET /accounts/{address}/code ────────────────────────────────
pub async fn accounts_code(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let address = address.to_lowercase();
    let bc = state.read().await;
    let (is_contract, code_hex) = if let Some(acct) = bc.accounts.accounts.get(&address) {
        if acct.is_contract() {
            let code_hash_hex = hex::encode(acct.code_hash);
            let code = bc
                .accounts
                .get_contract_code(&code_hash_hex)
                .map(|c| format!("0x{}", hex::encode(c)))
                .unwrap_or_else(|| "0x".to_string());
            (true, code)
        } else {
            (false, "0x".to_string())
        }
    } else {
        (false, "0x".to_string())
    };

    Json(serde_json::json!({
        "is_contract": is_contract,
        "bytecode": code_hex,
        "abi": serde_json::Value::Null,
        "compiler": serde_json::Value::Null,
    }))
}
