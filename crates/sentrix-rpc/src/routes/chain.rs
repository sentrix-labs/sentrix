// chain.rs — chain-wide read endpoints: `/chain/info`, paginated blocks
// list, block-by-height, and the authenticated full-chain validate. The
// validate handler caches its last result per block height to keep the
// O(n) scan from running on every hit.
//
// Extracted from `routes/mod.rs` as part of backlog #12 phase 2d.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{ApiKey, SharedState};

pub(super) async fn chain_info(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    Json(bc.chain_stats())
}

/// REST alias for `sentrix_getFinalizedHeight` (JSON-RPC). Returns the
/// highest block index whose finality is established on the active
/// consensus profile:
///
/// * Pioneer PoA: every committed block is implicitly final → returns
///   the current chain tip.
/// * Voyager BFT: only blocks carrying a populated `justification`
///   qualify → walks back from the tip to find the newest justified
///   block.
///
/// Added so light clients and non-JSON-RPC consumers (curl, Prometheus
/// exporters, simple web dashboards) don't need to speak JSON-RPC to
/// learn how far behind finality they are.
pub(super) async fn get_finalized_height(
    State(state): State<SharedState>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let latest = match bc.latest_block() {
        Ok(b) => b.clone(),
        Err(_) => {
            return Json(serde_json::json!({
                "error": "chain empty",
            }));
        }
    };
    // BFT activity follows the runtime `voyager_activated` flag, NOT the
    // `is_voyager_height` fork-height check. `is_voyager_height` only
    // returns true when `next_height >= VOYAGER_FORK_HEIGHT` env var,
    // but mainnet activated Voyager via the chain.db `voyager_activated`
    // flag (set by activate_voyager()) while VOYAGER_FORK_HEIGHT remained
    // at u64::MAX as an operational safety. The fork-height check would
    // wrongly report `consensus=PoA` while runtime is actually BFT.
    let bft_active = bc.voyager_activated;
    let _ = latest.index.saturating_add(1); // formerly used for is_voyager_height

    // Fallback to latest when BFT is active but no justified block sits
    // in the sliding window yet. Mirrors `sentrix_getFinalizedHeight`
    // JSON-RPC semantics exactly — querying either endpoint must return
    // the same value, otherwise light clients hitting REST see
    // `finalized_height=0` on a chain JSON-RPC reports as finalized at tip.
    let (finalized_height, finalized_hash) = if !bft_active {
        (latest.index, latest.hash.clone())
    } else {
        let mut h = latest.index;
        let mut hash = latest.hash.clone();
        for b in bc.chain.iter().rev() {
            if b.justification.is_some() {
                h = b.index;
                hash = b.hash.clone();
                break;
            }
        }
        (h, hash)
    };

    Json(serde_json::json!({
        "finalized_height": finalized_height,
        "finalized_hash": finalized_hash,
        "latest_height": latest.index,
        "blocks_behind_finality": latest.index.saturating_sub(finalized_height),
        "consensus": if bft_active { "BFT" } else { "PoA" },
    }))
}

// Paginated block listing — default 20, max 100, newest first
pub(super) async fn get_blocks(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let page: u64 = params.get("page").and_then(|p| p.parse().ok()).unwrap_or(0);
    let limit: u64 = params
        .get("limit")
        .and_then(|l| l.parse().ok())
        .unwrap_or(20)
        .min(100); // hard cap at 100

    let total = bc.height() + 1; // true height from last block's index, not window size
    let start_skip = (page * limit) as usize;

    let blocks: Vec<serde_json::Value> = bc
        .chain
        .iter()
        .rev() // newest first (window only — last CHAIN_WINDOW_SIZE blocks)
        .skip(start_skip)
        .take(limit as usize)
        .map(|b| {
            serde_json::json!({
                "index": b.index,
                "hash": b.hash,
                "previous_hash": b.previous_hash,
                "timestamp": b.timestamp,
                "tx_count": b.tx_count(),
                "validator": b.validator,
                "merkle_root": b.merkle_root,
                "round": b.round,
                "has_justification": b.justification.is_some(),
            })
        })
        .collect();

    let has_more = (start_skip + blocks.len()) < total as usize;

    Json(serde_json::json!({
        "blocks": blocks,
        "pagination": {
            "page": page,
            "limit": limit,
            "total": total,
            "has_more": has_more
        }
    }))
}

pub(super) async fn get_block(
    State(state): State<SharedState>,
    Path(index): Path<u64>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let bc = state.read().await;
    match bc.get_block_any(index) {
        Some(block) => serde_json::to_value(block)
            .map(Json)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR),
        None => Err(StatusCode::NOT_FOUND),
    }
}

// Cache last validation result per block height to avoid O(n) recompute
// on every call.
static VALIDATE_CACHE_HEIGHT: AtomicU64 = AtomicU64::new(u64::MAX);
static VALIDATE_CACHE_RESULT: AtomicBool = AtomicBool::new(false);

// validate_chain requires X-API-Key authentication — an O(n) full chain
// scan per unauthenticated request would be a viable DoS vector on a
// long chain.
pub(super) async fn validate_chain(
    _auth: ApiKey,
    State(state): State<SharedState>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let height = bc.height();

    // Return cached result if chain height hasn't changed since last run
    if VALIDATE_CACHE_HEIGHT.load(Ordering::Relaxed) == height {
        let cached_valid = VALIDATE_CACHE_RESULT.load(Ordering::Relaxed);
        return Json(serde_json::json!({
            "valid": cached_valid,
            "height": height,
            "total_blocks": bc.height() + 1, // true total from block index, not window length
            "cached": true,
        }));
    }

    // Full O(n) chain scan — only runs when height has changed
    let valid = bc.is_valid_chain_window();
    VALIDATE_CACHE_HEIGHT.store(height, Ordering::Relaxed);
    VALIDATE_CACHE_RESULT.store(valid, Ordering::Relaxed);

    Json(serde_json::json!({
        "valid": valid,
        "height": height,
        "total_blocks": bc.height() + 1, // match cached path: true total from block index
        "cached": false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── M-07: validate_chain cache logic ──────────────────

    #[test]
    fn test_m07_validate_cache_statics_initialized() {
        // VALIDATE_CACHE_HEIGHT starts at u64::MAX (sentinel = never cached)
        // VALIDATE_CACHE_RESULT starts at false
        // After a fresh start, loading the statics should not panic.
        let h = VALIDATE_CACHE_HEIGHT.load(std::sync::atomic::Ordering::Relaxed);
        let r = VALIDATE_CACHE_RESULT.load(std::sync::atomic::Ordering::Relaxed);
        // Height is either u64::MAX (never run) or a valid height from a previous test.
        // Either way, the atomics must be readable without panic.
        let _ = h;
        let _ = r;
    }

    #[test]
    fn test_m07_validate_cache_update() {
        // Simulate the cache update logic used by validate_chain handler.
        let test_height: u64 = 12_345;
        let test_valid = true;

        VALIDATE_CACHE_HEIGHT.store(test_height, std::sync::atomic::Ordering::Relaxed);
        VALIDATE_CACHE_RESULT.store(test_valid, std::sync::atomic::Ordering::Relaxed);

        // Reading back should match
        assert_eq!(
            VALIDATE_CACHE_HEIGHT.load(std::sync::atomic::Ordering::Relaxed),
            test_height
        );
        assert_eq!(
            VALIDATE_CACHE_RESULT.load(std::sync::atomic::Ordering::Relaxed),
            test_valid
        );

        // Different height means cache miss (simulate)
        let different_height = test_height + 1;
        let is_cache_hit =
            VALIDATE_CACHE_HEIGHT.load(std::sync::atomic::Ordering::Relaxed) == different_height;
        assert!(!is_cache_hit);

        // Same height is a cache hit
        let is_same_hit =
            VALIDATE_CACHE_HEIGHT.load(std::sync::atomic::Ordering::Relaxed) == test_height;
        assert!(is_same_hit);
    }
}
