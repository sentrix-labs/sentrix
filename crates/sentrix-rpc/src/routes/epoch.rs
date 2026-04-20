// epoch.rs — Voyager Phase 2a epoch endpoints. `epoch_current` returns
// the live epoch; `epoch_history` returns the most recent N epochs (capped
// at 100) for dashboards that plot validator-set churn, total stake, and
// rewards over time.
//
// Extracted from `routes/mod.rs` as part of backlog #12 phase 2g —
// the final slice of the routes refactor.

use axum::{
    Json,
    extract::{Query, State},
};
use std::collections::HashMap;

use super::SharedState;

pub(super) async fn epoch_current(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let epoch = &bc.epoch_manager.current_epoch;
    Json(serde_json::json!({
        "epoch_number": epoch.epoch_number,
        "start_height": epoch.start_height,
        "end_height": epoch.end_height,
        "validator_set": epoch.validator_set,
        "total_staked": epoch.total_staked,
        "total_rewards": epoch.total_rewards,
        "total_blocks_produced": epoch.total_blocks_produced,
    }))
}

pub(super) async fn epoch_history(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let count: usize = params
        .get("count")
        .and_then(|c| c.parse().ok())
        .unwrap_or(10)
        .min(100);
    let epochs: Vec<serde_json::Value> = bc
        .epoch_manager
        .recent_epochs(count)
        .iter()
        .map(|e| {
            serde_json::json!({
                "epoch_number": e.epoch_number,
                "start_height": e.start_height,
                "end_height": e.end_height,
                "validator_count": e.validator_set.len(),
                "total_staked": e.total_staked,
                "total_rewards": e.total_rewards,
                "total_blocks_produced": e.total_blocks_produced,
            })
        })
        .collect();
    Json(serde_json::json!({
        "epochs": epochs,
        "count": epochs.len(),
    }))
}
