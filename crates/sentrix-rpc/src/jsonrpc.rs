// jsonrpc.rs - Sentrix — Ethereum-compatible JSON-RPC 2.0

use crate::routes::{ApiKey, SharedState};
use sentrix_primitives::transaction::Transaction;
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ── JSON-RPC types ───────────────────────────────────────
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub method: String,
    pub params: Option<Value>,
    pub id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: Some(result),
            error: None,
            id,
        }
    }
    pub fn err(id: Option<Value>, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.to_string(),
            }),
            id,
        }
    }
}

fn to_hex(n: u64) -> String {
    format!("0x{:x}", n)
}
fn to_hex_u128(n: u128) -> String {
    format!("0x{:x}", n)
}

/// M-11: validate a JSON-RPC address parameter before it is used as a
/// trie/DB lookup key. Accepts exactly `0x` + 40 hex lowercase and
/// returns the normalised string, or `Err` with an error message suitable
/// for JSON-RPC -32602 (Invalid params). Prevents oddly-shaped strings
/// (empty, too-long, non-hex) from reaching the account store where
/// they are merely a silent miss, wasting compute per malformed request
/// under adversarial load.
fn normalize_rpc_address(s: &str) -> Result<String, &'static str> {
    if s.len() != 42 {
        return Err("address must be 42 chars (0x + 40 hex)");
    }
    let lower = s.to_lowercase();
    if !lower.starts_with("0x") {
        return Err("address must start with 0x");
    }
    if !lower[2..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("address must be lowercase hex after 0x");
    }
    Ok(lower)
}

/// M-11: validate a JSON-RPC 32-byte hash parameter (tx hash, block
/// hash). Same rationale as `normalize_rpc_address` — keeps malformed
/// hex out of DB lookups.
fn normalize_rpc_hash(s: &str) -> Result<String, &'static str> {
    let stripped = s.strip_prefix("0x").unwrap_or(s);
    if stripped.len() != 64 {
        return Err("hash must be 32 bytes (64 hex chars)");
    }
    if !stripped.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("hash must be hex");
    }
    Ok(stripped.to_lowercase())
}

// ── Main handler ─────────────────────────────────────────
pub async fn jsonrpc_handler(
    State(state): State<SharedState>,
    Json(req): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    let id = req.id.clone();
    let params = req.params.unwrap_or(json!([]));

    let result = match req.method.as_str() {
        "eth_chainId" => {
            let bc = state.read().await;
            Ok(json!(to_hex(bc.chain_id)))
        }
        "net_version" => {
            let bc = state.read().await;
            Ok(json!(bc.chain_id.to_string()))
        }
        "net_listening" => Ok(json!(true)),
        "web3_clientVersion" => Ok(json!(format!("Sentrix/{}/Rust", env!("CARGO_PKG_VERSION")))),
        "eth_blockNumber" => {
            let bc = state.read().await;
            Ok(json!(to_hex(bc.height())))
        }
        "eth_gasPrice" => Ok(json!(to_hex(1_000_000_000))),
        "eth_estimateGas" => {
            // Gas estimation — 21000 for transfers, higher for contract calls
            let call_obj = &params[0];
            let data_hex = call_obj["data"].as_str().unwrap_or("0x");
            if data_hex.len() > 2 {
                Ok(json!(to_hex(100_000)))
            } else {
                Ok(json!(to_hex(21_000)))
            }
        }
        "eth_getBalance" => {
            // M-11: validate address before DB lookup.
            let address = match normalize_rpc_address(params[0].as_str().unwrap_or("")) {
                Ok(a) => a,
                Err(e) => return Json(JsonRpcResponse::err(id, -32602, e)),
            };
            let bc = state.read().await;
            let balance = bc.accounts.get_balance(&address);
            let wei = balance as u128 * 10_000_000_000u128;
            Ok(json!(to_hex_u128(wei)))
        }
        "eth_getTransactionCount" => {
            let address = match normalize_rpc_address(params[0].as_str().unwrap_or("")) {
                Ok(a) => a,
                Err(e) => return Json(JsonRpcResponse::err(id, -32602, e)),
            };
            let bc = state.read().await;
            let nonce = bc.accounts.get_nonce(&address);
            Ok(json!(to_hex(nonce)))
        }
        "eth_getBlockByNumber" => {
            let bc = state.read().await;
            let block_param = params[0].as_str().unwrap_or("latest");
            let index = if block_param == "latest" {
                bc.height()
            } else if block_param == "earliest" {
                0
            } else {
                u64::from_str_radix(block_param.trim_start_matches("0x"), 16).unwrap_or(0)
            };
            match bc.get_block(index) {
                Some(block) => Ok(json!({
                    "number": to_hex(block.index),
                    "hash": format!("0x{}", block.hash),
                    "parentHash": format!("0x{}", block.previous_hash),
                    "timestamp": to_hex(block.timestamp),
                    "miner": block.validator,
                    "transactions": block.transactions.iter().map(|tx| format!("0x{}", tx.txid)).collect::<Vec<_>>(),
                    "transactionsRoot": format!("0x{}", block.merkle_root),
                    "gasLimit": to_hex(30_000_000),
                    "gasUsed": to_hex(0),
                    "difficulty": "0x0",
                    "totalDifficulty": "0x0",
                    "size": to_hex(1000),
                    "extraData": "0x",
                    "nonce": "0x0000000000000000",
                })),
                None => Ok(json!(null)),
            }
        }
        "eth_getBlockByHash" => {
            let hash = params[0]
                .as_str()
                .unwrap_or("")
                .trim_start_matches("0x")
                .to_string();
            let bc = state.read().await;
            match bc.get_block_by_hash(&hash) {
                Some(block) => Ok(json!({
                    "number": to_hex(block.index),
                    "hash": format!("0x{}", block.hash),
                    "parentHash": format!("0x{}", block.previous_hash),
                    "timestamp": to_hex(block.timestamp),
                    "miner": block.validator,
                    "transactions": block.transactions.iter().map(|tx| format!("0x{}", tx.txid)).collect::<Vec<_>>(),
                    "transactionsRoot": format!("0x{}", block.merkle_root),
                    "gasLimit": to_hex(30_000_000),
                    "gasUsed": to_hex(0),
                })),
                None => Ok(json!(null)),
            }
        }
        "eth_getTransactionByHash" => {
            // M-11: validate tx hash format before DB lookup.
            let txid = match normalize_rpc_hash(params[0].as_str().unwrap_or("")) {
                Ok(h) => h,
                Err(e) => return Json(JsonRpcResponse::err(id, -32602, e)),
            };
            let bc = state.read().await;
            match bc.get_transaction(&txid) {
                Some(tx_data) => Ok(tx_data),
                None => Ok(json!(null)),
            }
        }
        "eth_getTransactionReceipt" => {
            let txid = match normalize_rpc_hash(params[0].as_str().unwrap_or("")) {
                Ok(h) => h,
                Err(e) => return Json(JsonRpcResponse::err(id, -32602, e)),
            };
            let bc = state.read().await;
            match bc.get_transaction(&txid) {
                Some(tx_data) => {
                    let block_index = tx_data["block_index"].as_u64().unwrap_or(0);
                    // A2: failed EVM tx → status=0x0 (reverted), success → 0x1.
                    // Native (non-EVM) txs that reach a block always succeeded — they are
                    // validated atomically in Pass 1 and only committed if Pass 2 succeeds,
                    // so they are never recorded as failed.
                    let status = if bc.accounts.is_evm_tx_failed(&txid) {
                        "0x0"
                    } else {
                        "0x1"
                    };
                    Ok(json!({
                        "transactionHash": format!("0x{}", txid),
                        "blockNumber": to_hex(block_index),
                        "blockHash": tx_data["block_hash"],
                        "status": status,
                        "gasUsed": to_hex(21_000),
                        "cumulativeGasUsed": to_hex(21_000),
                        "logs": [],
                        "logsBloom": "0x00",
                    }))
                }
                None => Ok(json!(null)),
            }
        }
        "sentrix_sendTransaction" => {
            // JSON-RPC token operations accept signed transactions only — no private_key in params.
            // params[0] must be a pre-signed Transaction object (same fields as POST /transactions).
            // Client is responsible for signing the transaction locally before sending.
            let tx: Transaction = match serde_json::from_value(params[0].clone()) {
                Ok(t) => t,
                Err(e) => {
                    return Json(JsonRpcResponse::err(
                        id,
                        -32602,
                        &format!("invalid transaction object: {}", e),
                    ));
                }
            };
            let txid = tx.txid.clone();
            let mut bc = state.write().await;
            match bc.add_to_mempool(tx) {
                Ok(()) => Ok(json!({ "txid": txid, "status": "pending_in_mempool" })),
                Err(e) => return Json(JsonRpcResponse::err(id, -32603, &e.to_string())),
            }
        }
        "sentrix_getBalance" => {
            // alias for eth_getBalance — returns SRX as float string
            let address = params[0].as_str().unwrap_or("").to_lowercase();
            let bc = state.read().await;
            let balance = bc.accounts.get_balance(&address);
            let wei = balance as u128 * 10_000_000_000u128;
            Ok(json!(to_hex_u128(wei)))
        }
        "eth_sendRawTransaction" => {
            // Decode RLP-encoded signed Ethereum transaction (legacy or EIP-1559/2930/4844).
            // Recover sender, convert to Sentrix Transaction format, add to mempool.
            if !state.read().await.is_evm_active() {
                return Json(JsonRpcResponse::err(id, -32000, "EVM not active yet"));
            }
            let raw_hex = params[0].as_str().unwrap_or("").trim_start_matches("0x");
            let raw_bytes = match hex::decode(raw_hex) {
                Ok(b) => b,
                Err(_) => return Json(JsonRpcResponse::err(id, -32602, "invalid hex")),
            };

            use alloy_consensus::TxEnvelope;
            use alloy_eips::eip2718::Decodable2718;

            let envelope: TxEnvelope = match TxEnvelope::decode_2718(&mut raw_bytes.as_slice()) {
                Ok(env) => env,
                Err(e) => {
                    return Json(JsonRpcResponse::err(
                        id,
                        -32602,
                        &format!("RLP decode failed: {}", e),
                    ));
                }
            };

            // Recover sender address from signature
            use alloy_consensus::Transaction as AlloyTx;
            use alloy_consensus::transaction::SignerRecoverable;
            let sender: alloy_primitives::Address = match envelope.recover_signer() {
                Ok(addr) => addr,
                Err(e) => {
                    return Json(JsonRpcResponse::err(
                        id,
                        -32602,
                        &format!("signer recovery failed: {}", e),
                    ));
                }
            };
            let sender_str = format!("0x{}", hex::encode(sender.as_slice()));

            // Extract tx fields
            let nonce = envelope.nonce();
            let gas_limit = envelope.gas_limit();
            let value_u256: alloy_primitives::U256 = envelope.value();
            let data_bytes = envelope.input().to_vec();
            let to_kind = envelope.kind();
            let chain_id = envelope.chain_id().unwrap_or(0);

            // Convert Ethereum value (wei) to Sentrix sentri (1 SRX = 1e18 wei = 1e8 sentri)
            // 1 sentri = 1e10 wei
            //
            // P1: reject instead of saturating on U256→u128 overflow.
            // Pre-fix, a caller could set `value = U256::MAX` and have
            // it silently saturate to `u128::MAX`, then divide by 1e10
            // to produce a nonsensical u64 amount. Surface the
            // out-of-range condition as a JSON-RPC error so the client
            // sees the rejection rather than a mangled amount.
            let value_wei: u128 = match value_u256.try_into() {
                Ok(v) => v,
                Err(_) => {
                    return Json(JsonRpcResponse::err(
                        id,
                        -32602,
                        "tx value exceeds u128 (not representable on Sentrix)",
                    ));
                }
            };
            let amount_sentri = (value_wei / 10_000_000_000u128) as u64;

            // Build Sentrix Transaction. txid = keccak256 of raw bytes (Ethereum tx hash)
            use sha3::{Digest as _, Keccak256};
            let tx_hash = Keccak256::digest(&raw_bytes);
            let txid = hex::encode(tx_hash);

            let to_str = match to_kind {
                alloy_primitives::TxKind::Call(addr) => {
                    format!("0x{}", hex::encode(addr.as_slice()))
                }
                alloy_primitives::TxKind::Create => {
                    sentrix_primitives::transaction::TOKEN_OP_ADDRESS.to_string()
                }
            };

            // Encode EVM call data as hex in the data field (will be decoded by block_executor)
            let evm_data = format!("EVM:{}:{}", gas_limit, hex::encode(&data_bytes));

            let sentrix_tx = Transaction {
                txid: txid.clone(),
                from_address: sender_str,
                to_address: to_str,
                amount: amount_sentri,
                fee: sentrix_primitives::transaction::MIN_TX_FEE,
                nonce,
                data: evm_data,
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                chain_id,
                signature: hex::encode(&raw_bytes), // store full raw tx for re-execution
                public_key: String::new(),          // not needed — sender derived from signature
            };

            let mut bc = state.write().await;
            match bc.add_to_mempool(sentrix_tx) {
                Ok(()) => Ok(json!(format!("0x{}", txid))),
                Err(e) => {
                    return Json(JsonRpcResponse::err(id, -32603, &e.to_string()));
                }
            }
        }
        "eth_call" => {
            // Execute a read-only EVM call without state mutation.
            // params[0] = {from, to, data, value, gas}
            if !state.read().await.is_evm_active() {
                return Json(JsonRpcResponse::err(id, -32000, "EVM not active yet"));
            }
            let call_obj = &params[0];
            let from_str = call_obj["from"]
                .as_str()
                .unwrap_or("0x0000000000000000000000000000000000000000");
            let to_str = call_obj["to"].as_str().unwrap_or("");
            let data_hex = call_obj["data"]
                .as_str()
                .unwrap_or("0x")
                .trim_start_matches("0x");
            let data_bytes = hex::decode(data_hex).unwrap_or_default();
            // P1: cap eth_call gas_limit at BLOCK_GAS_LIMIT. Without the
            // cap a client can request `u64::MAX` gas and force the EVM
            // to run until it naturally OOGs, which at current
            // INITIAL_BASE_FEE is a free long-running compute request
            // against the validator — an asymmetric DoS: cheap for the
            // client, expensive for the node.
            let gas_limit = call_obj["gas"]
                .as_str()
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
                .unwrap_or(sentrix_evm::gas::BLOCK_GAS_LIMIT)
                .min(sentrix_evm::gas::BLOCK_GAS_LIMIT);

            let bc = state.read().await;
            use sentrix_evm::database::parse_sentrix_address;

            // Snapshot chain_id before bc is dropped — execute_call below needs it.
            let chain_id = bc.chain_id;

            let from_addr =
                parse_sentrix_address(from_str).unwrap_or(alloy_primitives::Address::ZERO);
            let to_addr = parse_sentrix_address(to_str);

            let tx_kind = match to_addr {
                Some(addr) => revm::primitives::TxKind::Call(addr),
                None => revm::primitives::TxKind::Create,
            };

            let tx = revm::context::TxEnv::builder()
                .caller(from_addr)
                .kind(tx_kind)
                .data(alloy_primitives::Bytes::from(data_bytes))
                .gas_limit(gas_limit)
                .gas_price(0)
                .nonce(bc.accounts.get_nonce(from_str))
                .chain_id(Some(chain_id))
                .build()
                .unwrap_or_default();

            let base_fee = sentrix_evm::gas::INITIAL_BASE_FEE;

            // Populate InMemoryDB with sender account (so gas payment works).
            // Also load target contract if it has code.
            let mut in_mem_db = revm::database::InMemoryDB::default();
            let sender_balance = bc.accounts.get_balance(from_str);
            let sender_nonce = bc.accounts.get_nonce(from_str);
            in_mem_db.insert_account_info(
                from_addr,
                revm::state::AccountInfo {
                    balance: alloy_primitives::U256::from(sender_balance)
                        .saturating_mul(alloy_primitives::U256::from(10_000_000_000u64)),
                    nonce: sender_nonce,
                    code_hash: revm::primitives::KECCAK_EMPTY,
                    account_id: None,
                    code: None,
                },
            );
            // Load target contract if present
            if let Some(target) = to_addr
                && let Some(target_account) = bc.accounts.accounts.get(to_str)
                && target_account.is_contract()
            {
                let code_hash_hex = hex::encode(target_account.code_hash);
                if let Some(code_bytes) = bc.accounts.get_contract_code(&code_hash_hex) {
                    let bytecode = revm::state::Bytecode::new_raw(alloy_primitives::Bytes::from(
                        code_bytes.clone(),
                    ));
                    let code_hash = alloy_primitives::B256::from(target_account.code_hash);
                    in_mem_db.insert_account_info(
                        target,
                        revm::state::AccountInfo {
                            balance: alloy_primitives::U256::from(target_account.balance),
                            nonce: target_account.nonce,
                            code_hash,
                            account_id: None,
                            code: Some(bytecode),
                        },
                    );
                }
            }
            drop(bc);

            match sentrix_evm::executor::execute_call(in_mem_db, tx, base_fee, chain_id) {
                Ok(receipt) => {
                    let output_hex = format!("0x{}", hex::encode(&receipt.output));
                    Ok(json!(output_hex))
                }
                Err(e) => {
                    tracing::warn!("eth_call EVM execution failed: {}", e);
                    // Return empty result instead of error for compatibility
                    Ok(json!("0x"))
                }
            }
        }
        "sentrix_getValidatorSet" => {
            let bc = state.read().await;
            let active: std::collections::HashSet<String> =
                bc.stake_registry.active_set.iter().cloned().collect();
            let epoch = &bc.epoch_manager.current_epoch;
            let epoch_span = epoch
                .end_height
                .saturating_sub(epoch.start_height)
                .max(1);
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
                    // The registry tracks lifetime blocks_signed only. For the
                    // "epoch" count we expose the portion signed since epoch
                    // start — clamped to epoch_span so a fresh validator can't
                    // report > 100 % of the current epoch.
                    let blocks_produced_epoch = signed.min(epoch_span);
                    let voting_power_wei = if total_active_stake == 0 {
                        0u128
                    } else {
                        (total_stake as u128).saturating_mul(10_000_000_000u128)
                    };
                    serde_json::json!({
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
                "active_count": bc.stake_registry.active_count(),
                "total_count": bc.stake_registry.validators.len(),
                "total_active_stake": to_hex_u128(total_active_stake),
                "epoch_number": epoch.epoch_number,
                "validators": validators,
            }))
        }
        "sentrix_getDelegations" => {
            let address = match params[0].as_str() {
                Some(a) => a.to_lowercase(),
                None => return Json(JsonRpcResponse::err(id, -32602, "address required")),
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
                        let share = (d.amount as u128)
                            .saturating_mul(v.pending_rewards as u128)
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
        "sentrix_getStakingRewards" => {
            let address = match params[0].as_str() {
                Some(a) => a.to_lowercase(),
                None => return Json(JsonRpcResponse::err(id, -32602, "address required")),
            };
            let bc = state.read().await;
            let cur = bc.epoch_manager.current_epoch.epoch_number;
            let default_from = cur.saturating_sub(29);

            let (from_epoch, to_epoch) = if let Some(opts) = params.get(1).filter(|v| v.is_object())
            {
                let from = opts
                    .get("from_epoch")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(default_from);
                let to = opts
                    .get("to_epoch")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(cur);
                (from, to)
            } else {
                (default_from, cur)
            };

            // Per-epoch, per-delegator reward accounting is not persisted
            // anywhere on-chain today; the validator pending pot and the
            // epoch-wide total are the only historical signals we can
            // reconstruct. Callers rendering a reward chart therefore see
            // the aggregate credited to validators the delegator picked,
            // not an exact claim-by-claim ledger.
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
                let share_sentri = (d.amount as u128)
                    .saturating_mul(vstake.pending_rewards as u128)
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
        "sentrix_getBftStatus" => {
            let bc = state.read().await;
            let latest = match bc.latest_block() {
                Ok(b) => b.clone(),
                Err(_) => {
                    return Json(JsonRpcResponse::err(id, -32603, "chain empty"));
                }
            };
            let next_height = latest.index.saturating_add(1);
            let consensus = if sentrix_core::blockchain::Blockchain::is_voyager_height(
                next_height,
            ) {
                "BFT"
            } else {
                "PoA"
            };
            // Live BFT round/phase state is owned by the validator loop's
            // BftEngine and not yet published into Blockchain. For now we
            // expose the chain-level finality view (last block carrying a
            // BFT justification) and, in BFT mode, the weighted proposer
            // the engine WOULD select for the next round-0.
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
        "sentrix_getFinalizedHeight" => {
            let bc = state.read().await;
            let latest = match bc.latest_block() {
                Ok(b) => b.clone(),
                Err(_) => {
                    return Json(JsonRpcResponse::err(id, -32603, "chain empty"));
                }
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
        "eth_syncing" => Ok(json!(false)),
        "eth_accounts" => Ok(json!([])),
        "eth_getCode" => {
            // Return contract bytecode for an address
            let address = params[0].as_str().unwrap_or("").to_lowercase();
            let bc = state.read().await;
            if let Some(account) = bc.accounts.accounts.get(&address) {
                if account.is_contract() {
                    let code_hash_hex = hex::encode(account.code_hash);
                    if let Some(code) = bc.accounts.get_contract_code(&code_hash_hex) {
                        Ok(json!(format!("0x{}", hex::encode(code))))
                    } else {
                        Ok(json!("0x"))
                    }
                } else {
                    Ok(json!("0x"))
                }
            } else {
                Ok(json!("0x"))
            }
        }
        "eth_getStorageAt" => {
            // Return contract storage at slot
            let address = params[0].as_str().unwrap_or("").to_lowercase();
            let slot = params[1].as_str().unwrap_or("0x0");
            let slot_hex = slot.trim_start_matches("0x");
            let bc = state.read().await;
            if let Some(value) = bc.accounts.get_contract_storage(&address, slot_hex) {
                Ok(json!(format!("0x{}", hex::encode(value))))
            } else {
                Ok(json!(
                    "0x0000000000000000000000000000000000000000000000000000000000000000"
                ))
            }
        }
        _ => Err((-32601, "Method not found")),
    };

    Json(match result {
        Ok(val) => JsonRpcResponse::ok(id, val),
        Err((code, msg)) => JsonRpcResponse::err(id, code, msg),
    })
}

// Hard cap on batch size to prevent CPU saturation from oversized batch requests
const MAX_BATCH_SIZE: usize = 100;

// ── Smart dispatcher (single + batch) ────────────────────
pub async fn rpc_dispatcher(
    _auth: ApiKey,
    State(state): State<SharedState>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return Json(JsonRpcResponse::err(None, -32700, "Parse error")).into_response(),
    };

    if let Some(arr) = value.as_array() {
        // M-03: reject oversize batches BEFORE the per-element typed
        // deserialisation allocates a second copy. The raw `Value` parse
        // above is bounded by axum's body-size limit; this guard closes
        // the second amplification where 100 MB of arbitrary JSON would
        // round-trip through `Vec<JsonRpcRequest>` before being rejected.
        if arr.len() > MAX_BATCH_SIZE {
            return Json(JsonRpcResponse::err(
                None,
                -32600,
                &format!(
                    "batch too large: max {} requests, got {}",
                    MAX_BATCH_SIZE,
                    arr.len()
                ),
            ))
            .into_response();
        }

        let requests: Vec<JsonRpcRequest> = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(_) => {
                return Json(JsonRpcResponse::err(None, -32600, "Invalid Request")).into_response();
            }
        };

        let mut responses = Vec::new();
        for req in requests {
            let resp = jsonrpc_handler(State(state.clone()), Json(req)).await;
            responses.push(resp.0);
        }
        Json(responses).into_response()
    } else {
        let req: JsonRpcRequest = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(_) => {
                return Json(JsonRpcResponse::err(None, -32600, "Invalid Request")).into_response();
            }
        };
        jsonrpc_handler(State(state), Json(req))
            .await
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_m03_max_batch_size_constant() {
        // Verify the constant is set and reasonable
        assert_eq!(MAX_BATCH_SIZE, 100);
    }

    #[tokio::test]
    async fn test_m03_batch_too_large_rejected() {
        use sentrix_core::blockchain::Blockchain;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let bc = Blockchain::new("admin".to_string());
        let state: crate::routes::SharedState = Arc::new(RwLock::new(bc));

        // Build a batch of 101 requests (exceeds MAX_BATCH_SIZE)
        let mut requests = Vec::new();
        for i in 0..101 {
            requests.push(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "eth_chainId",
                "params": [],
                "id": i
            }));
        }
        let body = axum::body::Bytes::from(serde_json::to_vec(&requests).unwrap());

        let response = rpc_dispatcher(ApiKey, axum::extract::State(state), body).await;

        // Response should be an error about batch too large
        let body_bytes = axum::body::to_bytes(response.into_body(), 10_000)
            .await
            .unwrap();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(
            body_str.contains("batch too large"),
            "Expected batch too large error, got: {}",
            body_str
        );
    }
}
