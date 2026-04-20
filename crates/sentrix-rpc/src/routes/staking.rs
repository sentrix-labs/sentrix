// staking.rs — validator + staking (DPoS) REST endpoints. Four handlers:
// the PoA authority set (`/validators`) and the three Voyager DPoS views
// (`/staking/validators`, `/staking/delegations/{addr}`,
// `/staking/unbonding/{addr}`).
//
// Extracted from `routes/mod.rs` as part of backlog #12 phase 2b. Paired
// with the `sentrix_getValidatorSet` / `sentrix_getDelegations` JSON-RPC
// methods — wallets / explorers can take either path.

use axum::{
    Json,
    extract::{Path, State},
};

use super::SharedState;

pub(super) async fn get_validators(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let validators: Vec<serde_json::Value> = bc
        .authority
        .validators
        .values()
        .map(|v| {
            serde_json::json!({
                "address": v.address,
                "name": v.name,
                "is_active": v.is_active,
                "blocks_produced": v.blocks_produced,
                "registered_at": v.registered_at,
            })
        })
        .collect();
    Json(serde_json::json!({
        "validators": validators,
        "active": bc.authority.active_count(),
        "total": bc.authority.validator_count(),
    }))
}

pub(super) async fn staking_validators(
    State(state): State<SharedState>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let validators: Vec<serde_json::Value> = bc
        .stake_registry
        .validators
        .values()
        .map(|v| {
            serde_json::json!({
                "address": v.address,
                "self_stake": v.self_stake,
                "total_delegated": v.total_delegated,
                "total_stake": v.total_stake(),
                "commission_rate": v.commission_rate,
                "is_jailed": v.is_jailed,
                "is_tombstoned": v.is_tombstoned,
                "is_active": bc.stake_registry.is_active(&v.address),
                "blocks_signed": v.blocks_signed,
                "blocks_missed": v.blocks_missed,
                "pending_rewards": v.pending_rewards,
            })
        })
        .collect();
    Json(serde_json::json!({
        "validators": validators,
        "active_count": bc.stake_registry.active_count(),
        "total_count": bc.stake_registry.validators.len(),
    }))
}

pub(super) async fn staking_delegations(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let addr = address.to_lowercase();
    let delegations: Vec<serde_json::Value> = bc
        .stake_registry
        .get_delegations(&addr)
        .iter()
        .map(|d| {
            serde_json::json!({
                "validator": d.validator,
                "amount": d.amount,
                "height": d.height,
            })
        })
        .collect();
    Json(serde_json::json!({
        "delegator": addr,
        "delegations": delegations,
        "count": delegations.len(),
    }))
}

pub(super) async fn staking_unbonding(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Json<serde_json::Value> {
    let bc = state.read().await;
    let addr = address.to_lowercase();
    let entries: Vec<serde_json::Value> = bc
        .stake_registry
        .get_pending_unbonding(&addr)
        .iter()
        .map(|u| {
            serde_json::json!({
                "validator": u.validator,
                "amount": u.amount,
                "completion_height": u.completion_height,
            })
        })
        .collect();
    Json(serde_json::json!({
        "delegator": addr,
        "unbonding": entries,
        "count": entries.len(),
    }))
}
