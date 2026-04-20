// eth.rs — `eth_*` JSON-RPC namespace (Ethereum compatibility).
//
// Spec: https://ethereum.org/en/developers/docs/apis/json-rpc/
//
// Pulled out of the monolithic `mod.rs` during the backlog #11 phase 2
// refactor. Handler signature matches the other namespace modules so the
// top-level dispatcher in `mod.rs` can route by prefix.

use crate::routes::SharedState;
use sentrix_primitives::transaction::Transaction;
use serde_json::{Value, json};

use super::DispatchResult;
use super::helpers::{
    block_gas_used_ratio, collect_logs, load_logs_for_tx, normalize_rpc_address,
    normalize_rpc_hash, parse_address_filter, parse_hex_u64, parse_topic_filter, resolve_block_tag,
    to_hex, to_hex_u128,
};

pub(super) async fn dispatch(method: &str, params: &Value, state: &SharedState) -> DispatchResult {
    match method {
        "eth_chainId" => {
            let bc = state.read().await;
            Ok(json!(to_hex(bc.chain_id)))
        }
        "eth_blockNumber" => {
            let bc = state.read().await;
            Ok(json!(to_hex(bc.height())))
        }
        "eth_gasPrice" => Ok(json!(to_hex(1_000_000_000))),
        "eth_estimateGas" => {
            let call_obj = &params[0];
            let data_hex = call_obj["data"].as_str().unwrap_or("0x");
            if data_hex.len() > 2 {
                Ok(json!(to_hex(100_000)))
            } else {
                Ok(json!(to_hex(21_000)))
            }
        }
        "eth_getBalance" => {
            let address = match normalize_rpc_address(params[0].as_str().unwrap_or("")) {
                Ok(a) => a,
                Err(e) => return Err((-32602, e.into())),
            };
            let bc = state.read().await;
            let balance = bc.accounts.get_balance(&address);
            let wei = balance as u128 * 10_000_000_000u128;
            Ok(json!(to_hex_u128(wei)))
        }
        "eth_getTransactionCount" => {
            let address = match normalize_rpc_address(params[0].as_str().unwrap_or("")) {
                Ok(a) => a,
                Err(e) => return Err((-32602, e.into())),
            };
            let bc = state.read().await;
            let nonce = bc.accounts.get_nonce(&address);
            Ok(json!(to_hex(nonce)))
        }
        "eth_getBlockByNumber" => eth_get_block_by_number(params, state).await,
        "eth_getBlockByHash" => eth_get_block_by_hash(params, state).await,
        "eth_getTransactionByHash" => eth_get_transaction_by_hash(params, state).await,
        "eth_getTransactionReceipt" => eth_get_transaction_receipt(params, state).await,
        "eth_getBlockReceipts" => eth_get_block_receipts(params, state).await,
        "eth_sendRawTransaction" => eth_send_raw_transaction(params, state).await,
        "eth_call" => eth_call(params, state).await,
        "eth_getLogs" => eth_get_logs(params, state).await,
        "eth_feeHistory" => eth_fee_history(params, state).await,
        "eth_maxPriorityFeePerGas" => Ok(json!(to_hex(sentrix_evm::INITIAL_BASE_FEE))),
        "eth_syncing" => Ok(json!(false)),
        "eth_accounts" => Ok(json!([])),
        "eth_getCode" => eth_get_code(params, state).await,
        "eth_getStorageAt" => eth_get_storage_at(params, state).await,
        _ => Err((-32601, format!("method not found: {}", method))),
    }
}

async fn eth_get_block_by_number(params: &Value, state: &SharedState) -> DispatchResult {
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

async fn eth_get_block_by_hash(params: &Value, state: &SharedState) -> DispatchResult {
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

async fn eth_get_transaction_by_hash(params: &Value, state: &SharedState) -> DispatchResult {
    let txid = match normalize_rpc_hash(params[0].as_str().unwrap_or("")) {
        Ok(h) => h,
        Err(e) => return Err((-32602, e.into())),
    };
    let bc = state.read().await;
    match bc.get_transaction(&txid) {
        Some(tx_data) => Ok(tx_data),
        None => Ok(json!(null)),
    }
}

async fn eth_get_transaction_receipt(params: &Value, state: &SharedState) -> DispatchResult {
    let txid = match normalize_rpc_hash(params[0].as_str().unwrap_or("")) {
        Ok(h) => h,
        Err(e) => return Err((-32602, e.into())),
    };
    let bc = state.read().await;
    match bc.get_transaction(&txid) {
        Some(tx_data) => {
            let block_index = tx_data["block_index"].as_u64().unwrap_or(0);
            let status = if bc.accounts.is_evm_tx_failed(&txid) {
                "0x0"
            } else {
                "0x1"
            };
            let (logs, bloom_hex) = load_logs_for_tx(&bc, block_index, &txid);
            Ok(json!({
                "transactionHash": format!("0x{}", txid),
                "blockNumber": to_hex(block_index),
                "blockHash": tx_data["block_hash"],
                "status": status,
                "gasUsed": to_hex(21_000),
                "cumulativeGasUsed": to_hex(21_000),
                "logs": logs,
                "logsBloom": bloom_hex,
            }))
        }
        None => Ok(json!(null)),
    }
}

async fn eth_get_block_receipts(params: &Value, state: &SharedState) -> DispatchResult {
    // Batch receipt query. Input is a block tag (latest/earliest/0x-hex)
    // OR a block hash. Returns an array of receipt objects with the same
    // shape as eth_getTransactionReceipt. Explorers that today fan out N
    // single-receipt calls per block can collapse them to one round trip.
    let bc = state.read().await;
    let block = if let Some(s) = params[0].as_str() {
        if s.strip_prefix("0x").unwrap_or(s).len() == 64 {
            let hash = s.trim_start_matches("0x").to_lowercase();
            bc.get_block_by_hash(&hash).cloned()
        } else {
            let latest = bc.height();
            let height = match resolve_block_tag(Some(&params[0]), latest) {
                Ok(h) => h,
                Err(e) => return Err((-32602, e.into())),
            };
            bc.get_block(height).cloned()
        }
    } else if let Some(obj) = params[0].as_object() {
        if let Some(h) = obj.get("blockHash").and_then(|v| v.as_str()) {
            let hash = h.trim_start_matches("0x").to_lowercase();
            bc.get_block_by_hash(&hash).cloned()
        } else if let Some(n) = obj.get("blockNumber") {
            let latest = bc.height();
            let height = match resolve_block_tag(Some(n), latest) {
                Ok(h) => h,
                Err(e) => return Err((-32602, e.into())),
            };
            bc.get_block(height).cloned()
        } else {
            return Err((-32602, "expected blockHash or blockNumber".into()));
        }
    } else {
        return Err((
            -32602,
            "expected block tag, block hash, or { blockHash | blockNumber } object".into(),
        ));
    };

    let block = match block {
        Some(b) => b,
        None => return Ok(json!(null)),
    };

    let mut receipts = Vec::with_capacity(block.transactions.len());
    let mut cumulative: u64 = 0;
    for (idx, tx) in block.transactions.iter().enumerate() {
        let status = if bc.accounts.is_evm_tx_failed(&tx.txid) {
            "0x0"
        } else {
            "0x1"
        };
        let (logs, bloom_hex) = load_logs_for_tx(&bc, block.index, &tx.txid);
        let gas_used: u64 = 21_000;
        cumulative = cumulative.saturating_add(gas_used);
        receipts.push(json!({
            "transactionHash": format!("0x{}", tx.txid),
            "transactionIndex": to_hex(idx as u64),
            "blockNumber": to_hex(block.index),
            "blockHash": format!("0x{}", block.hash),
            "from": tx.from_address,
            "to": tx.to_address,
            "status": status,
            "gasUsed": to_hex(gas_used),
            "cumulativeGasUsed": to_hex(cumulative),
            "logs": logs,
            "logsBloom": bloom_hex,
        }));
    }
    Ok(json!(receipts))
}

async fn eth_send_raw_transaction(params: &Value, state: &SharedState) -> DispatchResult {
    // Decode RLP-encoded signed Ethereum transaction (legacy or
    // EIP-1559/2930/4844). Recover sender, convert to Sentrix
    // Transaction format, add to mempool.
    if !state.read().await.is_evm_active() {
        return Err((-32000, "EVM not active yet".into()));
    }
    let raw_hex = params[0].as_str().unwrap_or("").trim_start_matches("0x");
    let raw_bytes = match hex::decode(raw_hex) {
        Ok(b) => b,
        Err(_) => return Err((-32602, "invalid hex".into())),
    };

    use alloy_consensus::TxEnvelope;
    use alloy_eips::eip2718::Decodable2718;

    let envelope: TxEnvelope = match TxEnvelope::decode_2718(&mut raw_bytes.as_slice()) {
        Ok(env) => env,
        Err(e) => return Err((-32602, format!("RLP decode failed: {}", e))),
    };

    // Recover sender address from signature
    use alloy_consensus::Transaction as AlloyTx;
    use alloy_consensus::transaction::SignerRecoverable;
    let sender: alloy_primitives::Address = match envelope.recover_signer() {
        Ok(addr) => addr,
        Err(e) => return Err((-32602, format!("signer recovery failed: {}", e))),
    };
    let sender_str = format!("0x{}", hex::encode(sender.as_slice()));

    let nonce = envelope.nonce();
    let gas_limit = envelope.gas_limit();
    let value_u256: alloy_primitives::U256 = envelope.value();
    let data_bytes = envelope.input().to_vec();
    let to_kind = envelope.kind();
    let chain_id = envelope.chain_id().unwrap_or(0);

    // Convert Ethereum value (wei) to Sentrix sentri (1 SRX = 1e18 wei =
    // 1e8 sentri). 1 sentri = 1e10 wei.
    //
    // P1: reject instead of saturating on U256→u128 overflow. Pre-fix, a
    // caller could set `value = U256::MAX` and have it silently saturate
    // to `u128::MAX`, then divide by 1e10 to produce a nonsensical u64
    // amount. Surface the out-of-range condition as a JSON-RPC error so
    // the client sees the rejection rather than a mangled amount.
    let value_wei: u128 = match value_u256.try_into() {
        Ok(v) => v,
        Err(_) => {
            return Err((
                -32602,
                "tx value exceeds u128 (not representable on Sentrix)".into(),
            ));
        }
    };
    let amount_sentri = (value_wei / 10_000_000_000u128) as u64;

    use sha3::{Digest as _, Keccak256};
    let tx_hash = Keccak256::digest(&raw_bytes);
    let txid = hex::encode(tx_hash);

    let to_str = match to_kind {
        alloy_primitives::TxKind::Call(addr) => format!("0x{}", hex::encode(addr.as_slice())),
        alloy_primitives::TxKind::Create => {
            sentrix_primitives::transaction::TOKEN_OP_ADDRESS.to_string()
        }
    };

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
        signature: hex::encode(&raw_bytes),
        public_key: String::new(),
    };

    let mut bc = state.write().await;
    match bc.add_to_mempool(sentrix_tx) {
        Ok(()) => Ok(json!(format!("0x{}", txid))),
        Err(e) => Err((-32603, e.to_string())),
    }
}

async fn eth_call(params: &Value, state: &SharedState) -> DispatchResult {
    // Execute a read-only EVM call without state mutation.
    // params[0] = {from, to, data, value, gas}
    if !state.read().await.is_evm_active() {
        return Err((-32000, "EVM not active yet".into()));
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
    // P1: cap eth_call gas_limit at BLOCK_GAS_LIMIT. Without the cap a
    // client can request `u64::MAX` gas and force the EVM to run until
    // it naturally OOGs, which at current INITIAL_BASE_FEE is a free
    // long-running compute request against the validator — an
    // asymmetric DoS: cheap for the client, expensive for the node.
    let gas_limit = call_obj["gas"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(sentrix_evm::gas::BLOCK_GAS_LIMIT)
        .min(sentrix_evm::gas::BLOCK_GAS_LIMIT);

    let bc = state.read().await;
    use sentrix_evm::database::parse_sentrix_address;

    let chain_id = bc.chain_id;

    let from_addr = parse_sentrix_address(from_str).unwrap_or(alloy_primitives::Address::ZERO);
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
    if let Some(target) = to_addr
        && let Some(target_account) = bc.accounts.accounts.get(to_str)
        && target_account.is_contract()
    {
        let code_hash_hex = hex::encode(target_account.code_hash);
        if let Some(code_bytes) = bc.accounts.get_contract_code(&code_hash_hex) {
            let bytecode =
                revm::state::Bytecode::new_raw(alloy_primitives::Bytes::from(code_bytes.clone()));
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

async fn eth_get_logs(params: &Value, state: &SharedState) -> DispatchResult {
    let filter = match params.get(0) {
        Some(v) if v.is_object() => v,
        _ => return Err((-32602, "filter object required".into())),
    };
    let bc = state.read().await;
    let latest = bc.height();
    let from_block = match resolve_block_tag(filter.get("fromBlock"), latest) {
        Ok(h) => h,
        Err(e) => return Err((-32602, e.into())),
    };
    let to_block = match resolve_block_tag(filter.get("toBlock"), latest) {
        Ok(h) => h,
        Err(e) => return Err((-32602, e.into())),
    };
    if to_block < from_block {
        return Err((-32602, "toBlock < fromBlock".into()));
    }
    if to_block.saturating_sub(from_block) >= 10_000 {
        return Err((-32005, "query returned more than 10000 results".into()));
    }
    let address_filter = parse_address_filter(filter.get("address"));
    let topic_filter = parse_topic_filter(filter.get("topics"));
    let logs = collect_logs(&bc, from_block, to_block, &address_filter, &topic_filter);
    Ok(json!(logs))
}

async fn eth_fee_history(params: &Value, state: &SharedState) -> DispatchResult {
    let block_count = params.get(0).and_then(parse_hex_u64).unwrap_or(1).min(1024);
    let bc = state.read().await;
    let latest = bc.height();
    let newest = params
        .get(1)
        .and_then(|v| resolve_block_tag(Some(v), latest).ok())
        .unwrap_or(latest);
    let percentiles: Vec<f64> = params
        .get(2)
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_f64()).collect())
        .unwrap_or_default();
    let base = sentrix_evm::INITIAL_BASE_FEE;
    let oldest = newest.saturating_sub(block_count.saturating_sub(1));
    let mut base_fees = Vec::with_capacity((block_count + 1) as usize);
    for _ in 0..=block_count {
        base_fees.push(to_hex(base));
    }
    let mut gas_used_ratios = Vec::with_capacity(block_count as usize);
    let mut rewards: Vec<Vec<String>> = Vec::with_capacity(block_count as usize);
    for h in oldest..=newest {
        let ratio = block_gas_used_ratio(&bc, h);
        gas_used_ratios.push(ratio);
        rewards.push(percentiles.iter().map(|_| to_hex(base)).collect());
    }
    Ok(json!({
        "oldestBlock": to_hex(oldest),
        "baseFeePerGas": base_fees,
        "gasUsedRatio": gas_used_ratios,
        "reward": rewards,
    }))
}

async fn eth_get_code(params: &Value, state: &SharedState) -> DispatchResult {
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

async fn eth_get_storage_at(params: &Value, state: &SharedState) -> DispatchResult {
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
