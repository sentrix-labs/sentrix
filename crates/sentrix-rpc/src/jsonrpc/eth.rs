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
        "eth_estimateGas" => eth_estimate_gas(params, state).await,
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
            // Standard EVM semantics for the second arg: `latest` returns
            // the finalized account nonce; `pending` adds count of mempool
            // entries from this account so the caller can sign the
            // next-usable nonce. Without this distinction, faucets +
            // dapps that fetched the nonce, signed, and submitted in
            // quick succession would all sign the same nonce — chain
            // accepted only the first, the rest piled up rejected
            // mid-block. Live discovery 2026-05-02.
            let block_tag = params[1].as_str().unwrap_or("latest");
            let bc = state.read().await;
            let mut nonce = bc.accounts.get_nonce(&address);
            if block_tag == "pending" {
                nonce = nonce.saturating_add(bc.mempool_pending_count(&address));
            }
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
        // Subscriptions only make sense on a long-lived connection. HTTP is
        // request/response; clients that try to subscribe over HTTP get a
        // pointer to the right transport instead of a silent error.
        "eth_subscribe" | "eth_unsubscribe" => Err((
            -32601,
            "subscriptions only available on the WebSocket endpoint at /ws".into(),
        )),
        _ => Err((-32601, format!("method not found: {}", method))),
    }
}

async fn eth_get_block_by_number(params: &Value, state: &SharedState) -> DispatchResult {
    let bc = state.read().await;
    let block_param = params[0].as_str().unwrap_or("latest");
    // Silently mapping invalid hex to block 0 returned genesis on typos like
    // `0xZZ` or `"not a number"`, which users then took at face value. Reject
    // the parse error so the caller sees the mistake instead of genesis data.
    let index = if block_param == "latest" {
        bc.height()
    } else if block_param == "earliest" {
        0
    } else {
        match u64::from_str_radix(block_param.trim_start_matches("0x"), 16) {
            Ok(n) => n,
            Err(_) => {
                return Err((-32602, format!("invalid block number: {block_param:?}")));
            }
        }
    };
    // BACKLOG #15: get_block_any so historical blocks outside the
    // in-memory sliding window are served from MDBX, not silently
    // returned as null.
    match bc.get_block_any(index) {
        Some(block) => Ok(build_block_json(&block)),
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
        Some(block) => Ok(build_block_json(block)),
        None => Ok(json!(null)),
    }
}

// Standard EVM block fields beyond what Sentrix natively tracks. Off-the-shelf
// EVM tooling (Blockscout indexer, etc.) pattern-matches these — missing keys
// trigger FunctionClauseError on parse. Sentrix has no PoW/uncles, so most are
// constants; `stateRoot` surfaces the real on-chain commitment when available.
const ZERO_HASH_HEX: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
// Keccak256 of empty RLP list — the canonical "no uncles" sentinel every EVM
// chain emits.
const EMPTY_SHA3_UNCLES: &str =
    "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347";
// Empty 256-byte logs bloom (2 hex chars per byte → 512 zeros after 0x).
const EMPTY_LOGS_BLOOM: &str = "0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000";

fn build_block_json(block: &sentrix_primitives::Block) -> Value {
    let state_root = match block.state_root {
        Some(bytes) => format!("0x{}", hex::encode(bytes)),
        None => ZERO_HASH_HEX.to_string(),
    };
    json!({
        "number": to_hex(block.index),
        "hash": format!("0x{}", block.hash),
        "parentHash": format!("0x{}", block.previous_hash),
        "timestamp": to_hex(block.timestamp),
        "miner": block.validator,
        "transactions": block.transactions.iter().map(|tx| format!("0x{}", tx.txid)).collect::<Vec<_>>(),
        "transactionsRoot": format!("0x{}", block.merkle_root),
        "stateRoot": state_root,
        "receiptsRoot": ZERO_HASH_HEX,
        "logsBloom": EMPTY_LOGS_BLOOM,
        "sha3Uncles": EMPTY_SHA3_UNCLES,
        "mixHash": ZERO_HASH_HEX,
        "uncles": [],
        "gasLimit": to_hex(30_000_000),
        "gasUsed": to_hex(0),
        "difficulty": "0x0",
        "totalDifficulty": "0x0",
        "size": to_hex(1000),
        "extraData": "0x",
        "nonce": "0x0000000000000000",
        "baseFeePerGas": to_hex(sentrix_evm::gas::INITIAL_BASE_FEE),
    })
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
            // EVM tx vs native tx: `data` starts with "EVM:" for alloy-decoded
            // envelopes (see primitives/transaction.rs:198). Sentrix's EVM
            // txs go through the EIP-1559 base_fee pipeline, so we surface
            // type 0x2 + effectiveGasPrice = INITIAL_BASE_FEE for those. Native
            // txs have no EIP-1559 semantics — type 0x0 + effectiveGasPrice 0.
            let is_evm = tx_data["transaction"]["data"]
                .as_str()
                .map(|d| d.starts_with("EVM:"))
                .unwrap_or(false);
            let tx_type = if is_evm { "0x2" } else { "0x0" };
            let effective_gas_price = if is_evm {
                to_hex(sentrix_evm::gas::INITIAL_BASE_FEE)
            } else {
                "0x0".to_string()
            };

            // 2026-04-29: pull the originating tx + transaction index out
            // of the same get_transaction lookup so receipts include
            // from / to / transactionIndex / contractAddress. Off-the-
            // shelf indexers (Blockscout, etherscan-style tooling) need
            // these to wire up address history and detect contract
            // creations — without them eth_getTransactionReceipt is just
            // a txid → gas/status echo and the indexer can't populate
            // its smart_contracts table.
            let from = tx_data["transaction"]["from_address"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let to_raw = tx_data["transaction"]["to_address"]
                .as_str()
                .unwrap_or("")
                .to_string();
            // EVM contract creations carry to=null. In the Sentrix
            // primitive Tx model that surfaces as the zero address; map
            // that back to null so off-the-shelf indexers can spot
            // creations the way they expect.
            let to_value: Value = if to_raw.is_empty()
                || to_raw == "0x0000000000000000000000000000000000000000"
                || to_raw == "0000000000000000000000000000000000000000"
            {
                Value::Null
            } else {
                Value::String(if to_raw.starts_with("0x") {
                    to_raw.clone()
                } else {
                    format!("0x{to_raw}")
                })
            };
            // Resolve transactionIndex by scanning the txs in the block.
            // Cheap because blocks carry a handful of txs — if this ever
            // becomes a hotspot we'd index txid → idx alongside the
            // txid → block lookup. Also drag the full tx slice up because
            // we need it for cumulativeGasUsed below.
            let block_txs: Vec<sentrix_primitives::transaction::Transaction> = bc
                .get_block_any(block_index)
                .map(|b| b.transactions.clone())
                .unwrap_or_default();
            let tx_idx_opt = block_txs.iter().position(|t| t.txid == txid);
            let tx_index = tx_idx_opt
                .map(|i| to_hex(i as u64))
                .unwrap_or_else(|| "0x0".to_string());
            // Re-stamp blockHash with the 0x prefix that EVM tooling
            // expects. The stored `block_hash` is bare hex.
            let block_hash = tx_data["block_hash"]
                .as_str()
                .map(|h| {
                    if h.starts_with("0x") {
                        h.to_string()
                    } else {
                        format!("0x{h}")
                    }
                })
                .unwrap_or_default();

            // Look up the persisted EVM receipt — block_executor writes one
            // per EVM tx (v2.1.56). Pre-fix we returned 21_000 unconditionally;
            // now we surface the real gas + contractAddress + (post-2026-05-02
            // backfill) revert reason bytes. Native (non-EVM) txs don't have
            // a receipt row → 21_000 fallback is the right answer for them.
            let stored_receipt: Option<sentrix_evm::StoredReceipt> = bc
                .mdbx_storage
                .as_ref()
                .and_then(|s| sentrix_evm::receipt_key(&txid).map(|k| (s, k)))
                .and_then(|(s, k)| {
                    s.get_bincode(sentrix_storage::tables::TABLE_RECEIPTS, &k).ok().flatten()
                });

            let gas_used: u64 = stored_receipt.as_ref().map(|r| r.gas_used).unwrap_or(21_000);

            // cumulativeGasUsed = sum of gas_used for all txs in this block
            // up to and including this one. We sum from the receipt store
            // so the number reflects real EVM gas; missing rows fall back
            // to 21_000 each (native txs).
            let cumulative_gas_used: u64 = {
                let upto = tx_idx_opt.unwrap_or(usize::MAX);
                let mut sum: u64 = 0;
                for (i, t) in block_txs.iter().enumerate() {
                    if i > upto {
                        break;
                    }
                    let g = bc
                        .mdbx_storage
                        .as_ref()
                        .and_then(|s| sentrix_evm::receipt_key(&t.txid).map(|k| (s, k)))
                        .and_then(|(s, k)| {
                            s.get_bincode::<sentrix_evm::StoredReceipt>(
                                sentrix_storage::tables::TABLE_RECEIPTS,
                                &k,
                            )
                            .ok()
                            .flatten()
                        })
                        .map(|r| r.gas_used)
                        .unwrap_or(21_000);
                    sum = sum.saturating_add(g);
                }
                sum
            };

            // contractAddress: surface from the stored receipt if the EVM tx
            // was a CREATE. Pre-fix we always returned null; off-the-shelf
            // indexers (Blockscout, etherscan-style) couldn't detect contract
            // creations and the smart_contracts table stayed empty.
            let contract_address: Value = stored_receipt
                .as_ref()
                .and_then(|r| r.contract_address)
                .map(|a| Value::String(format!("0x{}", hex::encode(a))))
                .unwrap_or(Value::Null);

            // Pull the from address through the same 0x-prefix
            // normaliser so receipt consumers don't have to special-case
            // either form. Coinbase txs carry the literal string
            // "COINBASE" in tx.from_address; surface that as the EVM
            // zero address (which is the convention every block explorer
            // expects for the block-reward minter).
            let from_value: Value = if from.is_empty() || from == "COINBASE" {
                Value::String(ZERO_HASH_HEX[..42].to_string())
            } else if from.starts_with("0x") {
                Value::String(from)
            } else {
                Value::String(format!("0x{from}"))
            };

            Ok(json!({
                "transactionHash": format!("0x{}", txid),
                "transactionIndex": tx_index,
                "blockNumber": to_hex(block_index),
                "blockHash": block_hash,
                "from": from_value,
                "to": to_value,
                "contractAddress": contract_address,
                "status": status,
                "gasUsed": to_hex(gas_used),
                "cumulativeGasUsed": to_hex(cumulative_gas_used),
                "effectiveGasPrice": effective_gas_price,
                "type": tx_type,
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
            // BACKLOG #15: MDBX fallback for historical queries.
            bc.get_block_any(height)
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
            bc.get_block_any(height)
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
        // Surface real EVM gas + contractAddress from the receipt store
        // (v2.1.56). Native txs don't have a row → fall back to 21_000.
        let stored_receipt: Option<sentrix_evm::StoredReceipt> = bc
            .mdbx_storage
            .as_ref()
            .and_then(|s| sentrix_evm::receipt_key(&tx.txid).map(|k| (s, k)))
            .and_then(|(s, k)| {
                s.get_bincode(sentrix_storage::tables::TABLE_RECEIPTS, &k).ok().flatten()
            });
        let gas_used: u64 = stored_receipt.as_ref().map(|r| r.gas_used).unwrap_or(21_000);
        cumulative = cumulative.saturating_add(gas_used);
        let contract_address: Value = stored_receipt
            .as_ref()
            .and_then(|r| r.contract_address)
            .map(|a| Value::String(format!("0x{}", hex::encode(a))))
            .unwrap_or(Value::Null);
        // See eth_get_transaction_receipt above for the EVM-vs-native
        // `type` + `effectiveGasPrice` rationale.
        let is_evm = tx.is_evm_tx();
        let tx_type = if is_evm { "0x2" } else { "0x0" };
        let effective_gas_price = if is_evm {
            to_hex(sentrix_evm::gas::INITIAL_BASE_FEE)
        } else {
            "0x0".to_string()
        };
        receipts.push(json!({
            "transactionHash": format!("0x{}", tx.txid),
            "transactionIndex": to_hex(idx as u64),
            "blockNumber": to_hex(block.index),
            "blockHash": format!("0x{}", block.hash),
            "from": tx.from_address,
            "to": tx.to_address,
            "contractAddress": contract_address,
            "status": status,
            "gasUsed": to_hex(gas_used),
            "cumulativeGasUsed": to_hex(cumulative),
            "effectiveGasPrice": effective_gas_price,
            "type": tx_type,
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
    // Sentrix's on-chain unit is sentri (1 SRX = 1e8 sentri = 1e18 wei), so
    // values below 1e10 wei are sub-sentri dust. Truncating them silently
    // meant a caller sending 10_000_000_001 wei saw 1 sentri transferred
    // and 9 wei unaccounted — the 9 wei was neither burned, refunded, nor
    // credited. Reject non-divisible amounts so the mismatch surfaces at
    // the boundary instead of becoming phantom loss.
    if !value_wei.is_multiple_of(10_000_000_000u128) {
        return Err((
            -32602,
            "tx value is not a whole number of sentri (must be divisible by 1e10 wei)".into(),
        ));
    }
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

/// Real gas estimation via EVM dry-run (replaces the pre-2026-04-22 flat
/// 21_000 / 100_000 heuristic). Returns `receipt.gas_used` from an actual
/// read-only `execute_call`. For reverting transactions, returns `-32000`
/// with the revert reason — matches Geth semantics where a reverting tx
/// has no meaningful gas estimate.
async fn eth_estimate_gas(params: &Value, state: &SharedState) -> DispatchResult {
    // params[0] must be a call object. Keep the existing input-validation
    // rule from the 2026-04-22 hardening (PR #205) — reject non-object.
    let Some(call_obj) = params.get(0).filter(|v| v.is_object()) else {
        return Err((-32602, "expected call object as first param".into()));
    };
    match run_evm_dry_run(call_obj, state).await {
        Ok(receipt) => {
            // Match Geth: reverting tx has no meaningful gas estimate.
            // Surface the revert with -32000 so wallets show the error
            // instead of accepting a misleading "success" gas number.
            if !receipt.success {
                let reason = if receipt.output.is_empty() {
                    "execution reverted".to_string()
                } else {
                    format!("execution reverted: 0x{}", hex::encode(&receipt.output))
                };
                return Err((-32000, reason));
            }
            // EIP-150 buffer: revm's dry-run uses gas_limit=TX_GAS_LIMIT_CAP (16M),
            // so sub-calls always get plenty of forwarded gas (63/64 rule). When the
            // wallet then submits the tx at gas_limit=receipt.gas_used, every sub-call
            // gets MUCH less forwarded gas — typically ~5000 short of what cold-account
            // value-transfer CALLs need. CoinBlastCurve.buy() reproduced this exactly:
            // dry-run returned 319_355, real apply with the same limit OOG'd inside
            // _safeSendSRX(feeRecipient, fee).call{value:fee}("") and reverted with
            // TransferFailed(). Geth applies a 1.30× multiplier; we use 1.25× +
            // 30_000 floor — generous enough to cover cold-access overhead in any
            // reasonable contract while not waste-billing simple transfers.
            let buffered = receipt
                .gas_used
                .saturating_add(receipt.gas_used / 4)
                .max(receipt.gas_used.saturating_add(30_000));
            Ok(json!(to_hex(buffered)))
        }
        Err((code, msg)) => Err((code, msg)),
    }
}

/// Build TxEnv + InMemoryDB from a call object and run an EVM dry-run under
/// `execute_call` (read-only — no state mutation). Shared by `eth_call` and
/// `eth_estimateGas`: eth_call projects `receipt.output`, eth_estimateGas
/// projects `receipt.gas_used`.
///
/// Factored out 2026-04-22 so `eth_estimateGas` can stop returning the flat
/// 21_000 / 100_000 heuristic and return real gas from actual execution.
async fn run_evm_dry_run(
    call_obj: &Value,
    state: &SharedState,
) -> Result<sentrix_evm::executor::TxReceipt, (i32, String)> {
    if !state.read().await.is_evm_active() {
        return Err((-32000, "EVM not active yet".into()));
    }

    // Sentrix's AccountDB stores addresses lowercase (per
    // `address_to_sentrix` which uses `hex::encode(...)`). EVM tooling
    // commonly sends checksummed mixed-case addresses (EIP-55). A naive
    // `accounts.get(to_str)` lookup with a checksummed address misses
    // every contract → eth_call returns "0x" silently. Normalize to
    // lowercase before any AccountDB lookup. Live-discovered 2026-04-28
    // when canonical contracts (deployed lowercase per EVM CREATE
    // semantics) returned empty `name()` / `getBlockNumber()` /
    // `tokensOf()` over JSON-RPC.
    let from_str_owned = call_obj["from"]
        .as_str()
        .unwrap_or("0x0000000000000000000000000000000000000000")
        .to_ascii_lowercase();
    let to_str_owned = call_obj["to"].as_str().unwrap_or("").to_ascii_lowercase();
    let from_str: &str = &from_str_owned;
    let to_str: &str = &to_str_owned;
    let data_hex = call_obj["data"]
        .as_str()
        .unwrap_or("0x")
        .trim_start_matches("0x");
    let data_bytes = hex::decode(data_hex).unwrap_or_default();
    // P1: cap at TX_GAS_LIMIT_CAP (EIP-7825, 16_777_216). Without the cap
    // a client can request `u64::MAX` gas and force the EVM to run until
    // it naturally OOGs, which at current INITIAL_BASE_FEE is a free
    // long-running compute request against the validator — asymmetric DoS.
    //
    // Cap MUST be `TX_GAS_LIMIT_CAP`, not `BLOCK_GAS_LIMIT`. revm with
    // SpecId >= Osaka rejects `gas_limit > TX_GAS_LIMIT_CAP` (= 2^24)
    // with `TxGasLimitGreaterThanCap` even for read-only dry-runs, so
    // defaulting to `BLOCK_GAS_LIMIT` (30M) caused every `eth_call` and
    // every `eth_estimateGas` to fail. Live-discovered 2026-04-28 after
    // PR #389 deploy when `cast call WSRX.name()` returned `0x` for every
    // canonical contract. View calls fit inside 16M comfortably (most
    // use < 100K gas).
    let gas_limit = call_obj["gas"]
        .as_str()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(sentrix_evm::gas::TX_GAS_LIMIT_CAP)
        .min(sentrix_evm::gas::TX_GAS_LIMIT_CAP);

    let bc = state.read().await;
    use sentrix_evm::database::parse_sentrix_address;

    let chain_id = bc.chain_id;
    let from_addr = parse_sentrix_address(from_str).unwrap_or(alloy_primitives::Address::ZERO);
    let to_addr = parse_sentrix_address(to_str);

    let tx_kind = match to_addr {
        Some(addr) => revm::primitives::TxKind::Call(addr),
        None => revm::primitives::TxKind::Create,
    };

    // Thread `value` from the call object into TxEnv. Without it, every
    // dry-run simulates with msg.value=0 regardless of what the dApp
    // requested — payable functions that gate on `if (msg.value == 0)
    // revert ZeroValue();` (CoinBlastCurve.buy, WSRX.deposit, etc) always
    // revert during wagmi's pre-flight eth_estimateGas check, surfacing
    // as "RPC Request failed" in the user's wallet UI. Block-apply path
    // (block_executor::evm.rs) was fixed in v2.1.49; this dry-run path
    // was never updated. Found 2026-05-02 via CBLAST buy() debugging
    // post-EVM_VALUE_TRANSFER_HEIGHT activation.
    let tx_value: alloy_primitives::U256 = call_obj
        .get("value")
        .and_then(|v| v.as_str())
        .and_then(|s| alloy_primitives::U256::from_str_radix(
            s.trim_start_matches("0x"), 16
        ).ok())
        .unwrap_or(alloy_primitives::U256::ZERO);

    let tx = revm::context::TxEnv::builder()
        .caller(from_addr)
        .kind(tx_kind)
        .data(alloy_primitives::Bytes::from(data_bytes))
        .value(tx_value)
        .gas_limit(gas_limit)
        .gas_price(0)
        .nonce(bc.accounts.get_nonce(from_str))
        .chain_id(Some(chain_id))
        .build()
        .unwrap_or_default();

    let base_fee = sentrix_evm::gas::INITIAL_BASE_FEE;

    // Use SentrixEvmDb (revm::Database backed by AccountDB) instead of
    // InMemoryDB. The previous InMemoryDB pre-loaded only the target
    // contract's CODE — storage slots returned 0 for every read, so any
    // ERC-20 `name()` / `symbol()` / `balanceOf(addr)` / etc. returned
    // empty bytes. SentrixEvmDb's `Database` trait reads storage slots
    // on-demand from AccountDB's contract_storage map (populated by
    // `commit_state_to_account_db` on every CREATE/CALL). This makes
    // eth_call results match real on-chain state.
    let mut evm_db = sentrix_evm::database::SentrixEvmDb::from_account_db(&bc.accounts);
    // Override sender balance with a generous (but not absurd) amount in
    // wei so balance/gas checks during dry-run don't trip on a freshly
    // queried zero-balance EOA. Read-only callers don't have to be funded.
    use revm::Database;
    if let Ok(mut sender_info) = evm_db.basic(from_addr).map(|opt| opt.unwrap_or_default()) {
        if sender_info.balance.is_zero() {
            sender_info.balance = alloy_primitives::U256::from(1u64) << 96; // ~7.9e28 wei, plenty for any view call
        }
        evm_db.insert_account(from_addr, sender_info);
    }
    drop(bc);

    sentrix_evm::executor::execute_call_with_state(evm_db, tx, base_fee, chain_id)
        .map(|(receipt, _state)| receipt)
        .map_err(|e| (-32000, format!("EVM execution failed: {e}")))
}

async fn eth_call(params: &Value, state: &SharedState) -> DispatchResult {
    // Execute a read-only EVM call without state mutation.
    // params[0] = {from, to, data, value, gas}
    match run_evm_dry_run(&params[0], state).await {
        Ok(receipt) => {
            let output_hex = format!("0x{}", hex::encode(&receipt.output));
            Ok(json!(output_hex))
        }
        Err((code, msg)) => {
            if code == -32000 && msg.starts_with("EVM execution failed") {
                // Preserve pre-refactor behavior for eth_call specifically:
                // return "0x" on runtime execution error so dApps that don't
                // gracefully handle revert errors keep working. eth_estimateGas
                // surfaces the same error loudly because a reverting tx has no
                // meaningful gas estimate.
                tracing::warn!("{msg}");
                Ok(json!("0x"))
            } else {
                // Non-execution errors (EVM not active yet) still surface.
                Err((code, msg))
            }
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
    let address = match normalize_rpc_address(params[0].as_str().unwrap_or("")) {
        Ok(a) => a,
        Err(e) => return Err((-32602, e.into())),
    };
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
    let address = match normalize_rpc_address(params[0].as_str().unwrap_or("")) {
        Ok(a) => a,
        Err(e) => return Err((-32602, e.into())),
    };
    let slot = params[1].as_str().unwrap_or("0x0");
    // Storage slot must be valid hex (≤ 64 chars = 32 bytes). Rejecting
    // junk here keeps the downstream storage lookup from querying with
    // a malformed key that quietly returns zero.
    let slot_hex = slot.trim_start_matches("0x");
    if slot_hex.is_empty() || slot_hex.len() > 64 || !slot_hex.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err((-32602, "invalid storage slot (must be hex, ≤ 32 bytes)".into()));
    }
    let bc = state.read().await;
    if let Some(value) = bc.accounts.get_contract_storage(&address, slot_hex) {
        Ok(json!(format!("0x{}", hex::encode(value))))
    } else {
        Ok(json!(
            "0x0000000000000000000000000000000000000000000000000000000000000000"
        ))
    }
}
