// sentrix.rs — `sentrix_*` JSON-RPC namespace.
//
// Native Sentrix methods that expose chain features the `eth_*` namespace
// cannot represent: validator set (with PoA fallback), delegations,
// staking rewards, BFT status, finalized height, and a signed-tx send.
//
// Pulled out of the monolithic `mod.rs` during the backlog #11 phase 2
// refactor. Handler signature matches the other namespace modules so the
// top-level dispatcher in `mod.rs` can route by prefix.

use crate::routes::SharedState;
use sentrix_primitives::transaction::Transaction;
use serde_json::{Value, json};

use super::DispatchResult;
use super::helpers::to_hex_u128;

pub(super) async fn dispatch(method: &str, params: &Value, state: &SharedState) -> DispatchResult {
    match method {
        "sentrix_sendTransaction" => {
            // JSON-RPC token operations accept signed transactions only — no
            // private_key in params. params[0] must be a pre-signed
            // Transaction object (same fields as POST /transactions). Client
            // is responsible for signing the transaction locally before
            // sending.
            let tx: Transaction = match serde_json::from_value(params[0].clone()) {
                Ok(t) => t,
                Err(e) => {
                    return Err((-32602, format!("invalid transaction object: {}", e)));
                }
            };
            let txid = tx.txid.clone();
            let mut bc = state.write().await;
            match bc.add_to_mempool(tx) {
                Ok(()) => Ok(json!({ "txid": txid, "status": "pending_in_mempool" })),
                Err(e) => Err((-32603, e.to_string())),
            }
        }
        "sentrix_getBalance" => {
            // alias for eth_getBalance — returns SRX in wei hex.
            //
            // Normalise the address through the same path eth_getBalance
            // uses (`normalize_rpc_address`): enforces the 42-char
            // 0x-prefixed 40-hex format + lowercases. Without this,
            // malformed addresses silently returned `0 SRX`, masking
            // client bugs and wasting lookup cycles on pathological
            // strings.
            use super::helpers::normalize_rpc_address;
            let address = match normalize_rpc_address(params[0].as_str().unwrap_or("")) {
                Ok(a) => a,
                Err(e) => return Err((-32602, e.into())),
            };
            let bc = state.read().await;
            let balance = bc.accounts.get_balance(&address);
            let wei = balance as u128 * 10_000_000_000u128;
            Ok(json!(to_hex_u128(wei)))
        }
        "sentrix_getValidatorSet" => sentrix_get_validator_set(state).await,
        "sentrix_getDelegations" => sentrix_get_delegations(params, state).await,
        "sentrix_getStakingRewards" => sentrix_get_staking_rewards(params, state).await,
        "sentrix_getBftStatus" => sentrix_get_bft_status(state).await,
        "sentrix_getFinalizedHeight" => sentrix_get_finalized_height(state).await,
        _ => Err((-32601, format!("method not found: {}", method))),
    }
}

async fn sentrix_get_validator_set(state: &SharedState) -> DispatchResult {
    let bc = state.read().await;
    let epoch = &bc.epoch_manager.current_epoch;
    let epoch_span = epoch.end_height.saturating_sub(epoch.start_height).max(1);

    // On a PoA chain (mainnet pre-Voyager) the DPoS stake_registry is
    // empty by design — validators live in AuthorityManager. Without the
    // fallback below this method returned [] on mainnet even though 3
    // validators (Foundation / Treasury / Core) are actively producing
    // blocks. "Consensus mode" detection: if the next block lands
    // post-Voyager, use the DPoS path; otherwise use PoA.
    let next_height = bc.latest_block().map(|b| b.index + 1).unwrap_or(1);
    let is_dpos = sentrix_core::blockchain::Blockchain::is_voyager_height(next_height)
        && !bc.stake_registry.validators.is_empty();

    if is_dpos {
        let active: std::collections::HashSet<String> =
            bc.stake_registry.active_set.iter().cloned().collect();
        let total_active_stake: u128 = bc
            .stake_registry
            .active_set
            .iter()
            .filter_map(|a| bc.stake_registry.get_validator(a))
            .map(|v| v.total_stake() as u128)
            .sum();

        let validators: Vec<serde_json::Value> = bc
            .stake_registry
            .validators
            .values()
            .map(|v| {
                let name = bc
                    .authority
                    .validators
                    .get(&v.address)
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                let total_stake = v.total_stake();
                let stake_wei = (total_stake as u128).saturating_mul(10_000_000_000u128);
                let commission = f64::from(v.commission_rate) / 10_000.0;
                let is_active = active.contains(&v.address);
                let status = if v.is_tombstoned {
                    "tombstoned"
                } else if v.is_jailed {
                    "jailed"
                } else if is_active {
                    "active"
                } else {
                    "unbonding"
                };
                let signed = v.blocks_signed;
                let attempted = signed.saturating_add(v.blocks_missed);
                let uptime = if attempted == 0 {
                    1.0
                } else {
                    signed as f64 / attempted as f64
                };
                let blocks_produced_epoch = signed.min(epoch_span);
                let voting_power_wei = if total_active_stake == 0 {
                    0u128
                } else {
                    (total_stake as u128).saturating_mul(10_000_000_000u128)
                };
                json!({
                    "address": v.address,
                    "name": name,
                    "stake": to_hex_u128(stake_wei),
                    "commission": commission,
                    "status": status,
                    "blocks_produced_epoch": blocks_produced_epoch,
                    "uptime": uptime,
                    "voting_power": to_hex_u128(voting_power_wei),
                })
            })
            .collect();

        Ok(json!({
            "consensus": "DPoS",
            "active_count": bc.stake_registry.active_count(),
            "total_count": bc.stake_registry.validators.len(),
            "total_active_stake": to_hex_u128(total_active_stake),
            "epoch_number": epoch.epoch_number,
            "validators": validators,
        }))
    } else {
        // PoA path: equal weight, zero commission, zero stake.
        // voting_power is a flat 1/N across the active set so clients
        // rendering a weight chart still get something meaningful.
        let active: Vec<_> = bc
            .authority
            .validators
            .values()
            .filter(|v| v.is_active)
            .collect();
        let active_count = active.len();
        let flat_weight = if active_count > 0 {
            1_000_000_000u128 / active_count as u128
        } else {
            0
        };
        let validators: Vec<serde_json::Value> = bc
            .authority
            .validators
            .values()
            .map(|v| {
                let status = if v.is_active { "active" } else { "unbonding" };
                json!({
                    "address": v.address,
                    "name": v.name,
                    "stake": "0x0",
                    "commission": 0.0,
                    "status": status,
                    "blocks_produced_epoch": v.blocks_produced.min(epoch_span),
                    "uptime": 1.0,
                    "voting_power": to_hex_u128(if v.is_active { flat_weight } else { 0 }),
                })
            })
            .collect();

        Ok(json!({
            "consensus": "PoA",
            "active_count": active_count,
            "total_count": bc.authority.validators.len(),
            "total_active_stake": "0x0",
            "epoch_number": epoch.epoch_number,
            "validators": validators,
        }))
    }
}

async fn sentrix_get_delegations(params: &Value, state: &SharedState) -> DispatchResult {
    let address = match params[0].as_str() {
        Some(a) => a.to_lowercase(),
        None => return Err((-32602, "address required".into())),
    };
    let bc = state.read().await;
    let delegations_raw = bc.stake_registry.get_delegations(&address).to_vec();
    let unbonding: Vec<_> = bc
        .stake_registry
        .get_pending_unbonding(&address)
        .into_iter()
        .cloned()
        .collect();

    // EPOCH_LENGTH is defined in sentrix-staking but sentrix-rpc does not
    // take a direct dep on it; the same constant is mirrored here
    // (staking::epoch::EPOCH_LENGTH = 28_800). If that constant ever
    // changes, this line must be updated in lockstep.
    const EPOCH_LENGTH: u64 = 28_800;
    let epoch_of = |h: u64| h / EPOCH_LENGTH;

    let mut rows: Vec<serde_json::Value> = Vec::new();
    for d in delegations_raw {
        let vstake = bc.stake_registry.get_validator(&d.validator);
        let validator_name = bc
            .authority
            .validators
            .get(&d.validator)
            .map(|v| v.name.clone())
            .unwrap_or_default();
        let amount_wei = (d.amount as u128).saturating_mul(10_000_000_000u128);
        // Pending reward share is pro-rated against the validator's
        // unclaimed pot by stake weight. It is an estimate — per-
        // delegator reward accounting lives in a staking sprint.
        let pending_reward_wei = match vstake {
            Some(v) if v.total_delegated > 0 => {
                let share = (d.amount as u128).saturating_mul(v.pending_rewards as u128)
                    / v.total_delegated as u128;
                share.saturating_mul(10_000_000_000u128)
            }
            _ => 0,
        };
        rows.push(json!({
            "validator": d.validator,
            "validator_name": validator_name,
            "amount": to_hex_u128(amount_wei),
            "pending_reward": to_hex_u128(pending_reward_wei),
            "delegated_at_epoch": epoch_of(d.height),
            "status": "active",
            "unbonding_complete_epoch": serde_json::Value::Null,
        }));
    }
    for u in unbonding {
        let validator_name = bc
            .authority
            .validators
            .get(&u.validator)
            .map(|v| v.name.clone())
            .unwrap_or_default();
        let amount_wei = (u.amount as u128).saturating_mul(10_000_000_000u128);
        rows.push(json!({
            "validator": u.validator,
            "validator_name": validator_name,
            "amount": to_hex_u128(amount_wei),
            "pending_reward": "0x0",
            "delegated_at_epoch": serde_json::Value::Null,
            "status": "unbonding",
            "unbonding_complete_epoch": epoch_of(u.completion_height),
        }));
    }
    Ok(json!({
        "delegator": address,
        "delegations": rows,
    }))
}

async fn sentrix_get_staking_rewards(params: &Value, state: &SharedState) -> DispatchResult {
    let address = match params[0].as_str() {
        Some(a) => a.to_lowercase(),
        None => return Err((-32602, "address required".into())),
    };
    let bc = state.read().await;
    let cur = bc.epoch_manager.current_epoch.epoch_number;
    let default_from = cur.saturating_sub(29);

    let (from_epoch, to_epoch) = if let Some(opts) = params.get(1).filter(|v| v.is_object()) {
        let from = opts
            .get("from_epoch")
            .and_then(|v| v.as_u64())
            .unwrap_or(default_from);
        let to = opts.get("to_epoch").and_then(|v| v.as_u64()).unwrap_or(cur);
        (from, to)
    } else {
        (default_from, cur)
    };

    // Per-epoch, per-delegator reward accounting is not persisted anywhere
    // on-chain today; the validator pending pot and the epoch-wide total
    // are the only historical signals we can reconstruct. Callers
    // rendering a reward chart therefore see the aggregate credited to
    // validators the delegator picked, not an exact claim-by-claim ledger.
    let delegations = bc.stake_registry.get_delegations(&address);
    let mut by_epoch: Vec<serde_json::Value> = Vec::new();
    let mut total_pending_sentri: u128 = 0;
    for d in delegations {
        let vstake = match bc.stake_registry.get_validator(&d.validator) {
            Some(v) => v,
            None => continue,
        };
        if vstake.total_delegated == 0 {
            continue;
        }
        let share_sentri = (d.amount as u128).saturating_mul(vstake.pending_rewards as u128)
            / vstake.total_delegated as u128;
        total_pending_sentri = total_pending_sentri.saturating_add(share_sentri);
        if cur >= from_epoch && cur <= to_epoch && share_sentri > 0 {
            by_epoch.push(json!({
                "epoch": cur,
                "validator": d.validator,
                "reward": to_hex_u128(share_sentri.saturating_mul(10_000_000_000u128)),
                "claimed": false,
            }));
        }
    }

    let pending_claimable_wei = total_pending_sentri.saturating_mul(10_000_000_000u128);

    Ok(json!({
        "total_lifetime": to_hex_u128(pending_claimable_wei),
        "pending_claimable": to_hex_u128(pending_claimable_wei),
        "from_epoch": from_epoch,
        "to_epoch": to_epoch,
        "by_epoch": by_epoch,
    }))
}

async fn sentrix_get_bft_status(state: &SharedState) -> DispatchResult {
    let bc = state.read().await;
    let latest = match bc.latest_block() {
        Ok(b) => b.clone(),
        Err(_) => return Err((-32603, "chain empty".into())),
    };
    let next_height = latest.index.saturating_add(1);
    let consensus = if sentrix_core::blockchain::Blockchain::is_voyager_height(next_height) {
        "BFT"
    } else {
        "PoA"
    };
    // Live BFT round/phase state is owned by the validator loop's
    // BftEngine and not yet published into Blockchain. For now we expose
    // the chain-level finality view (last block carrying a BFT
    // justification) and, in BFT mode, the weighted proposer the engine
    // WOULD select for the next round-0.
    let (finalized_height, finalized_hash) = if consensus == "PoA" {
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
    let current_leader = if consensus == "PoA" {
        bc.authority
            .expected_validator(next_height)
            .map(|v| v.address.clone())
            .unwrap_or_default()
    } else {
        bc.stake_registry
            .weighted_proposer(next_height, 0)
            .unwrap_or_default()
    };
    let rounds_since_last_block = if consensus == "BFT" {
        latest.round as u64
    } else {
        0
    };

    if consensus == "PoA" {
        Ok(json!({
            "consensus": "PoA",
            "current_leader": current_leader,
            "last_finalized_height": finalized_height,
            "last_finalized_hash": finalized_hash,
        }))
    } else {
        Ok(json!({
            "consensus": "BFT",
            "current_round": serde_json::Value::Null,
            "current_view": serde_json::Value::Null,
            "current_leader": current_leader,
            "phase": serde_json::Value::Null,
            "rounds_since_last_block": rounds_since_last_block,
            "last_finalized_height": finalized_height,
            "last_finalized_hash": finalized_hash,
        }))
    }
}

async fn sentrix_get_finalized_height(state: &SharedState) -> DispatchResult {
    let bc = state.read().await;
    let latest = match bc.latest_block() {
        Ok(b) => b.clone(),
        Err(_) => return Err((-32603, "chain empty".into())),
    };
    let next_height = latest.index.saturating_add(1);
    let bft = sentrix_core::blockchain::Blockchain::is_voyager_height(next_height);
    let (finalized_height, finalized_hash) = if !bft {
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
    Ok(json!({
        "finalized_height": finalized_height,
        "finalized_hash": finalized_hash,
        "latest_height": latest.index,
        "blocks_behind_finality": latest.index.saturating_sub(finalized_height),
    }))
}
