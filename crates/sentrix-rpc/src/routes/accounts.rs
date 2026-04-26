// accounts.rs — address-indexed read endpoints. Covers balance / nonce /
// tx history / combined-summary plus the state-trie proof endpoints that
// the frontend reaches via `/address/{addr}/proof` and
// `/chain/state-root/{height}`.
//
// Extracted from `routes/mod.rs` as part of backlog #12 phase 2e.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use sentrix_trie::address::{account_value_decode, address_to_key};
use std::collections::HashMap;

use super::SharedState;

pub(super) async fn get_balance(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    // Normalize address to lowercase for case-insensitive lookup (REST/RPC consistency)
    let address = address.to_lowercase();
    let bc = state.read().await;
    let balance = bc.accounts.get_balance(&address);
    Json(serde_json::json!({
        "address": address,
        "balance_sentri": balance,
        "balance_srx": balance as f64 / 100_000_000.0,
    }))
}

pub(super) async fn get_nonce(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let address = address.to_lowercase();
    let bc = state.read().await;
    let nonce = bc.accounts.get_nonce(&address);
    Json(serde_json::json!({ "address": address, "nonce": nonce }))
}

pub(super) async fn get_wallet_info(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let address = address.to_lowercase();
    let bc = state.read().await;
    let balance = bc.accounts.get_balance(&address);
    let nonce = bc.accounts.get_nonce(&address);
    // get_address_tx_count returns window-aware metadata; see chain_queries.rs for coverage details
    let tx_count_info = bc.get_address_tx_count(&address);
    Json(serde_json::json!({
        "address": address,
        "balance_sentri": balance,
        "balance_srx": balance as f64 / 100_000_000.0,
        "nonce": nonce,
        "tx_count": tx_count_info,
    }))
}

pub(super) async fn list_transactions(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let limit: usize = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(20)
        .min(100);
    let offset: usize = params
        .get("offset")
        .and_then(|o| o.parse().ok())
        .unwrap_or(0);
    let txs = bc.get_latest_transactions(limit, offset);
    let count = txs.len();
    Json(serde_json::json!({
        "transactions": txs,
        "count": count,
        "pagination": { "limit": limit, "offset": offset },
    }))
}

pub(super) async fn get_richlist(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let mut holders: Vec<serde_json::Value> = bc
        .accounts
        .accounts
        .iter()
        .filter(|(_, a)| a.balance > 0)
        .map(|(addr, a)| {
            let pct = a.balance as f64 / bc.max_supply_for(bc.height()) as f64 * 100.0;
            serde_json::json!({
                "address": addr,
                "balance_sentri": a.balance,
                "balance_srx": a.balance as f64 / 100_000_000.0,
                "percent_of_supply": pct,
            })
        })
        .collect();
    holders.sort_by(|a, b| {
        let ba = a["balance_sentri"].as_u64().unwrap_or(0);
        let bb = b["balance_sentri"].as_u64().unwrap_or(0);
        bb.cmp(&ba)
    });
    holders.truncate(50);
    let total = holders.len();
    Json(serde_json::json!({ "holders": holders, "total": total }))
}

// ── Address history ──────────────────────────────────────

pub(super) async fn get_address_history(
    State(state): State<SharedState>,
    Path(address): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let address = address.to_lowercase();
    let bc = state.read().await;
    let limit: usize = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(20)
        .min(100);
    let offset: usize = params
        .get("offset")
        .and_then(|o| o.parse().ok())
        .unwrap_or(0);
    let history = bc.get_address_history(&address, limit, offset);
    let count = history.len();
    Json(serde_json::json!({
        "address": address,
        "transactions": history,
        "count": count,
        "pagination": { "limit": limit, "offset": offset, "has_more": count == limit }
    }))
}

pub(super) async fn get_address_info(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let address = address.to_lowercase();
    let bc = state.read().await;
    let balance = bc.accounts.get_balance(&address);
    let nonce = bc.accounts.get_nonce(&address);
    let tx_count_info = bc.get_address_tx_count(&address);
    Json(serde_json::json!({
        "address": address,
        "balance_sentri": balance,
        "balance_srx": balance as f64 / 100_000_000.0,
        "nonce": nonce,
        "tx_count": tx_count_info,
    }))
}

// ── State-trie endpoints ──────────────────────────────────

/// GET /address/:address/proof
/// Returns a Merkle membership/non-membership proof for the address in
/// the current state trie. Requires the trie to be initialized (init_trie
/// called at node startup).
pub(super) async fn get_address_proof(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> impl IntoResponse {
    // Validate address format before acquiring any lock — fail fast on bad input
    if !sentrix_core::blockchain::is_valid_sentrix_address(&address) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid address format: expected 0x + 40 hex chars",
                "received": address,
            })),
        )
            .into_response();
    }

    // Read lock is sufficient for proof generation — prove() uses an
    // internal Mutex<LruCache> so it takes &self, never &mut self. This
    // prevents proof requests from blocking block production (which needs
    // a write lock).
    let bc = state.read().await;
    let key = address_to_key(&address);
    match bc.state_trie.as_ref() {
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "state trie not initialized",
                "hint": "node must be started with --trie flag"
            })),
        )
            .into_response(),
        Some(trie) => match trie.prove(&key) {
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response(),
            Ok(proof) => {
                let (balance, nonce) = if proof.found {
                    account_value_decode(&proof.value)
                        .map(|(b, n)| (Some(b), Some(n)))
                        .unwrap_or((None, None))
                } else {
                    (None, None)
                };
                Json(serde_json::json!({
                    "address": address,
                    "found": proof.found,
                    "balance_sentri": balance,
                    "nonce": nonce,
                    "key_hex": hex::encode(proof.key),
                    "depth": proof.depth,
                    "terminal_hash_hex": hex::encode(proof.terminal_hash),
                    "siblings_hex": proof.siblings.iter().map(hex::encode).collect::<Vec<_>>(),
                    "root_hex": hex::encode(trie.root_hash()),
                    // Proof covers native SRX state only — token balances are not committed to the trie
                    "scope": "native_srx_only",
                    "scope_note": "Proof covers native SRX balance and nonce only. SRC-20 token balances are not committed to the state root.",
                }))
                .into_response()
            }
        },
    }
}

/// GET /chain/state-root/:height
/// Returns the committed state root hash for the given block height.
pub(super) async fn get_state_root(
    State(state): State<SharedState>,
    Path(height): Path<u64>,
) -> impl IntoResponse {
    let bc = state.read().await;
    match bc.state_trie.as_ref() {
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "state trie not initialized"
            })),
        )
            .into_response(),
        Some(trie) => match trie.root_at_version(height) {
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response(),
            Ok(opt) => Json(serde_json::json!({
                "height": height,
                "state_root_hex": opt.map(hex::encode),
            }))
            .into_response(),
        },
    }
}
