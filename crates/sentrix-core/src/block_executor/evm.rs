// Per-tx EVM execution. The block-apply hot path delegates here every
// time a Pass-2 transaction's data field starts with "EVM:" (the wire
// format eth_sendRawTransaction uses to smuggle alloy-decoded RLP
// envelopes through Sentrix's native Tx shape). We re-decode the
// envelope, run it through revm, and write back balance/storage/code
// deltas. Failures don't roll back the whole block — they mark the
// individual tx receipt as status=0x0 instead, mirroring how every
// EVM-compatible chain handles a single-tx revert.

use crate::blockchain::Blockchain;
use sentrix_primitives::error::SentrixResult;

impl Blockchain {
    /// Execute an EVM transaction (from eth_sendRawTransaction) within a block.
    /// Decodes the original RLP tx from the signature field, runs it through revm,
    /// applies state changes (contract creation, storage updates, balance transfers).
    pub(super) fn execute_evm_tx_in_block(
        &mut self,
        tx: &sentrix_primitives::transaction::Transaction,
        block_height: u64,
        block_hash_hex: &str,
        tx_index: u32,
    ) -> SentrixResult<()> {
        // Parse "EVM:gas_limit:hex_data" from data field
        let parts: Vec<&str> = tx.data.splitn(3, ':').collect();
        if parts.len() != 3 || parts[0] != "EVM" {
            return Ok(()); // not an EVM tx, skip
        }
        let gas_limit: u64 = parts[1].parse().unwrap_or(30_000_000);
        let calldata = hex::decode(parts[2]).unwrap_or_default();

        // Decode raw Ethereum tx from signature field for re-validation
        let raw_bytes = match hex::decode(&tx.signature) {
            Ok(b) => b,
            Err(_) => return Ok(()), // malformed, skip silently
        };

        use alloy_consensus::TxEnvelope;
        use alloy_consensus::Transaction as AlloyTx; // brings .value() into scope
        use alloy_consensus::transaction::SignerRecoverable;
        use alloy_eips::eip2718::Decodable2718;

        let envelope: TxEnvelope = match TxEnvelope::decode_2718(&mut raw_bytes.as_slice()) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };
        let _sender = envelope.recover_signer().ok();

        // Pull native-value out of the envelope. Pre-fix this was dropped on
        // the floor — TxEnv defaulted to U256::ZERO so revm never moved any
        // SRX between EOAs even when the user signed a `value > 0` tx.
        // Symptom: `WSRX.deposit{value: 1ether}()` and pure `cast send
        // --value Nether <addr>` both reported status=1 + recipient balance
        // unchanged. Native fee debit happens in the Pass-1 path
        // (`charge_fee_only`) so we only need value-of-tx here, not fee.
        // See `audits/2026-05-01-evm-value-transfer-bug.md`.
        let tx_value: alloy_primitives::U256 = envelope.value();

        // Build EVM tx
        use alloy_primitives::{B256, U256};
        use revm::context::TxEnv;
        use revm::primitives::TxKind;
        use revm::state::Bytecode;
        use sentrix_evm::database::{SentrixEvmDb, parse_sentrix_address};
        use sentrix_evm::executor::execute_tx_with_state;
        use sentrix_evm::gas::INITIAL_BASE_FEE;
        use sentrix_evm::writeback::commit_state_to_account_db;

        let from_addr =
            parse_sentrix_address(&tx.from_address).unwrap_or(alloy_primitives::Address::ZERO);
        let to_addr_str = if tx.to_address == sentrix_primitives::transaction::TOKEN_OP_ADDRESS {
            None
        } else {
            parse_sentrix_address(&tx.to_address)
        };
        let tx_kind = match to_addr_str {
            Some(addr) => TxKind::Call(addr),
            None => TxKind::Create,
        };

        // Build EVM db from CURRENT AccountDB so the EVM sees real balances,
        // nonces, and contract code pointers — not a fresh InMemoryDB as pre-fix.
        // from_account_db handles sentri → wei scaling for all accounts.
        let mut evm_db = SentrixEvmDb::from_account_db(&self.accounts);
        let sender_nonce = self.accounts.get_nonce(&tx.from_address);

        // For Call txs, pre-load the target contract's bytecode + existing
        // storage slots. Without this, revm sees an empty-code address and
        // executes nothing useful. CREATE txs don't need pre-load — code
        // comes from tx data and gets deployed by revm itself.
        if let TxKind::Call(target_addr) = tx_kind {
            let target_str = format!("0x{}", hex::encode(target_addr.as_slice()));
            if let Some(target_account) = self.accounts.accounts.get(&target_str)
                && target_account.code_hash != sentrix_primitives::EMPTY_CODE_HASH
            {
                let code_hash_hex = hex::encode(target_account.code_hash);
                if let Some(bytecode) = self.accounts.get_contract_code(&code_hash_hex) {
                    let code = Bytecode::new_raw(alloy_primitives::Bytes::from(bytecode.clone()));
                    evm_db.insert_code(B256::from(target_account.code_hash), code);
                }
                // Pre-load storage slots whose key starts with "{target_addr}:".
                // Sorted by slot_hex so the insertion order is deterministic
                // across validator processes (HashMap iteration otherwise is
                // non-deterministic; same pattern as init_trie backfill).
                let prefix = format!("{}:", target_str);
                let mut slots: Vec<(String, Vec<u8>)> = self
                    .accounts
                    .contract_storage
                    .iter()
                    .filter(|(k, _)| k.starts_with(&prefix))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                slots.sort_by(|a, b| a.0.cmp(&b.0));
                for (key, value) in slots {
                    if let Some(slot_hex) = key.strip_prefix(&prefix)
                        && let Ok(slot) = U256::from_str_radix(slot_hex, 16)
                    {
                        // Left-pad short values with leading zeros to 32 bytes
                        // (big-endian high bits on the left). Note: normally
                        // values ARE already 32 bytes because we always store
                        // them that way in writeback, but defensive for any
                        // legacy entries or state-imports with short blobs.
                        let mut val_bytes = [0u8; 32];
                        let n = value.len().min(32);
                        val_bytes[32 - n..].copy_from_slice(&value[..n]);
                        let val = U256::from_be_slice(&val_bytes);
                        evm_db.insert_storage(target_addr, slot, val);
                    }
                }
            }
        }

        let evm_tx = TxEnv::builder()
            .caller(from_addr)
            .kind(tx_kind)
            .data(alloy_primitives::Bytes::from(calldata))
            .value(tx_value)
            .gas_limit(gas_limit)
            .gas_price(INITIAL_BASE_FEE as u128)
            // EVM CREATE/CALL nonce: revm checks `tx.nonce == state.nonce`
            // and bumps state.nonce internally. Native pass no longer
            // pre-bumps nonce for EVM txs (charge_fee_only above), so
            // `sender_nonce` here equals what the EVM tx claimed in its
            // RLP. Pre-fix this used `sender_nonce.saturating_sub(1)` to
            // compensate for native double-bump; that was a band-aid that
            // broke for fresh-wallet CREATE (nonce 0 saturating to 0,
            // state already at 1 → NonceTooLow). See
            // `audits/evm-create-nonce-bug-2026-04-27.md`.
            .nonce(sender_nonce)
            .chain_id(Some(tx.chain_id))
            .build()
            .unwrap_or_default();

        match execute_tx_with_state(evm_db, evm_tx, INITIAL_BASE_FEE, self.chain_id) {
            Ok((receipt, state)) => {
                tracing::info!(
                    "EVM tx {}: success={} gas_used={} contract={:?}",
                    &tx.txid[..16.min(tx.txid.len())],
                    receipt.success,
                    receipt.gas_used,
                    receipt
                        .contract_address
                        .map(|a| format!("0x{}", hex::encode(a.as_slice()))),
                );
                if !receipt.success {
                    // A2: reverted EVM tx — record so eth_getTransactionReceipt
                    // returns status=0x0 instead of the default 0x1.
                    self.accounts.mark_evm_tx_failed(&tx.txid);
                }
                // Sprint 2: persist every log emitted by this tx. Key is
                // (height, tx_index, log_index) BE-packed so range scans
                // return logs in canonical Ethereum order.
                if let Some(storage) = self.mdbx_storage.as_ref() {
                    use sentrix_evm::{StoredLog, log_key};
                    let mut block_hash_bytes = [0u8; 32];
                    if let Ok(decoded) = hex::decode(block_hash_hex.trim_start_matches("0x")) {
                        let n = decoded.len().min(32);
                        block_hash_bytes[..n].copy_from_slice(&decoded[..n]);
                    }
                    let mut tx_hash_bytes = [0u8; 32];
                    if let Ok(decoded) = hex::decode(tx.txid.trim_start_matches("0x")) {
                        let n = decoded.len().min(32);
                        tx_hash_bytes[..n].copy_from_slice(&decoded[..n]);
                    }
                    for (log_idx, log) in receipt.logs.iter().enumerate() {
                        let stored = StoredLog::from_revm(
                            log,
                            block_height,
                            block_hash_bytes,
                            tx_hash_bytes,
                            tx_index,
                            log_idx as u32,
                        );
                        let key = log_key(block_height, tx_index, log_idx as u32);
                        let _ =
                            storage.put_bincode(sentrix_storage::tables::TABLE_LOGS, &key, &stored);

                        // Notify WebSocket subscribers — eth_subscribe(logs).
                        // Convert StoredLog to the trait-friendly LogData
                        // shape (sentrix-primitives doesn't depend on
                        // sentrix-evm so the trait can't take StoredLog
                        // directly). Non-blocking, infallible.
                        if let Some(emitter) = &self.event_emitter {
                            let log_data = sentrix_primitives::events::LogData {
                                block_height,
                                block_hash: block_hash_hex.to_string(),
                                tx_hash: tx.txid.clone(),
                                tx_index,
                                log_index: log_idx as u32,
                                address: stored.address,
                                topics: stored.topics.clone(),
                                data: stored.data.clone(),
                            };
                            emitter.emit_log(&log_data);
                        }
                    }
                }
                // Store contract RUNTIME code (not init code) if CREATE succeeded.
                // receipt.output contains the runtime bytecode returned by the constructor.
                //
                // P1 (EIP-170): cap deployed runtime bytecode at 24_576
                // bytes. Without this an attacker could ship a contract
                // whose runtime code expands state and trie pages
                // disproportionately per gas unit, amplifying the cost
                // of every subsequent block that touches the account.
                // We reject the CREATE by dropping the stored code and
                // marking the tx failed; the sender still paid gas but
                // no contract is registered.
                const EIP170_MAX_CODE_SIZE: usize = 24_576;
                if let Some(contract_addr) = receipt.contract_address
                    && !receipt.output.is_empty()
                {
                    if receipt.output.len() > EIP170_MAX_CODE_SIZE {
                        tracing::warn!(
                            "P1/EIP-170: contract {} runtime bytecode {} B > {} B limit; \
                             rejecting deploy (tx {})",
                            format!("0x{}", hex::encode(contract_addr.as_slice())),
                            receipt.output.len(),
                            EIP170_MAX_CODE_SIZE,
                            &tx.txid[..16.min(tx.txid.len())]
                        );
                        self.accounts.mark_evm_tx_failed(&tx.txid);
                    } else {
                        let addr_str = format!("0x{}", hex::encode(contract_addr.as_slice()));
                        use sha3::{Digest as _, Keccak256};
                        let code_hash: [u8; 32] = Keccak256::digest(&receipt.output).into();
                        let code_hash_hex = hex::encode(code_hash);
                        self.accounts
                            .store_contract_code(&code_hash_hex, receipt.output.clone());
                        self.accounts.set_contract(&addr_str, code_hash);
                    }
                }

                // ── EVM state writeback (closes Bug #1) ─────────────
                // On success, commit every touched account's balance,
                // nonce, storage slots, and (for CREATE) bytecode back
                // to AccountDB so the next tx / next block sees them.
                // Skip on revert — EVM spec requires state changes to
                // be discarded on revert; the gas fee was already
                // debited by the native Pass-1 .transfer() call above,
                // so there's nothing else to do.
                //
                // The EIP-170 guard above may flip the tx to failed
                // even for a successful revm receipt — in that case
                // we skip the writeback too. Check mark AFTER the
                // EIP-170 block ran so "was marked failed" is
                // authoritative.
                if receipt.success && !self.accounts.is_evm_tx_failed(&tx.txid) {
                    commit_state_to_account_db(&state, &mut self.accounts)?;
                }
            }
            Err(e) => {
                tracing::warn!("EVM tx {} failed: {}", &tx.txid[..16.min(tx.txid.len())], e);
                // A2: hard execution error — also mark as failed so the
                // tx receipt reports status=0x0.
                self.accounts.mark_evm_tx_failed(&tx.txid);
            }
        }
        Ok(())
    }
}
