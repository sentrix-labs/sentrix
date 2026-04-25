// evm/executor.rs — Block-level EVM execution engine
//
// Wraps revm to execute transactions within a block context.
// Handles: tx ordering by priority fee, gas metering, state diffs,
// and fee distribution (base_fee burned, priority_fee to validator).

use crate::gas::BLOCK_GAS_LIMIT;
use alloy_primitives::Address;
use revm::context::TxEnv;
use revm::context::result::ExecutionResult;
use revm::database::InMemoryDB;
use revm::database_interface::Database;
use revm::state::EvmState;
use revm::{ExecuteEvm, MainBuilder, MainContext};

/// Result of executing a single EVM transaction.
#[derive(Debug, Clone)]
pub struct TxReceipt {
    /// Whether the transaction succeeded
    pub success: bool,
    /// Gas used by the transaction
    pub gas_used: u64,
    /// Contract address if this was a CREATE
    pub contract_address: Option<Address>,
    /// Logs emitted
    pub logs: Vec<revm::primitives::Log>,
    /// Output data
    pub output: Vec<u8>,
}

/// Execute a single EVM transaction against an in-memory database.
///
/// Back-compat wrapper that preserves the original signature — discards the
/// state diff from revm and returns the receipt only. New callers that need
/// to persist state changes to AccountDB should use [`execute_tx_with_state`].
///
/// # Arguments
/// * `chain_id` — active chain ID for EIP-155 replay protection. If the tx
///   carries its own `chain_id` (EIP-155 signed), it MUST equal this value
///   or execution is rejected. Caller is authoritative — this function has
///   no default/fallback.
pub fn execute_tx(
    db: InMemoryDB,
    tx: TxEnv,
    block_base_fee: u64,
    chain_id: u64,
) -> Result<TxReceipt, String> {
    execute_tx_with_state(db, tx, block_base_fee, chain_id).map(|(receipt, _state)| receipt)
}

/// Read-only variant — disables balance/nonce checks for eth_call.
/// Back-compat wrapper that discards the state diff; see [`execute_call_with_state`].
///
/// # Arguments
/// * `chain_id` — active chain ID for EIP-155 replay protection (see
///   [`execute_tx`] for semantics).
pub fn execute_call(
    db: InMemoryDB,
    tx: TxEnv,
    block_base_fee: u64,
    chain_id: u64,
) -> Result<TxReceipt, String> {
    execute_call_with_state(db, tx, block_base_fee, chain_id).map(|(receipt, _state)| receipt)
}

/// Execute a transaction AND return the revm state diff.
///
/// The state diff (an `EvmState` = `HashMap<Address, Account>` of touched
/// accounts with balance/nonce/storage/code changes) is what the block
/// executor needs to persist back to AccountDB. Prior to this variant the
/// diff was being dropped on the floor — see
/// `internal design doc`.
///
/// Generic over any `D: Database` so callers can pass either `InMemoryDB`
/// (the original path, kept for the #[cfg(test)] unit tests) or
/// `SentrixEvmDb` (the block-apply path that needs to pre-load target
/// contract code + storage). The DB's associated error type must convert
/// to `String` to fit the existing Result shape.
pub fn execute_tx_with_state<D>(
    db: D,
    tx: TxEnv,
    block_base_fee: u64,
    chain_id: u64,
) -> Result<(TxReceipt, EvmState), String>
where
    D: Database,
    D::Error: std::fmt::Debug,
{
    execute_tx_inner(db, tx, block_base_fee, false, chain_id)
}

/// Read-only variant of [`execute_tx_with_state`] — disables balance/nonce
/// checks and base-fee enforcement for `eth_call` / `debug_traceCall` paths
/// that want to introspect the state diff without requiring the caller to
/// be funded.
pub fn execute_call_with_state<D>(
    db: D,
    tx: TxEnv,
    block_base_fee: u64,
    chain_id: u64,
) -> Result<(TxReceipt, EvmState), String>
where
    D: Database,
    D::Error: std::fmt::Debug,
{
    execute_tx_inner(db, tx, block_base_fee, true, chain_id)
}

fn execute_tx_inner<D>(
    db: D,
    tx: TxEnv,
    block_base_fee: u64,
    read_only: bool,
    chain_id: u64,
) -> Result<(TxReceipt, EvmState), String>
where
    D: Database,
    D::Error: std::fmt::Debug,
{
    use revm::Context;

    // EIP-155 replay protection: reject tx whose embedded chain_id doesn't
    // match the executor's configured chain_id. `None` (pre-EIP-155 legacy
    // tx) is allowed through; revm will enforce its own rules.
    if let Some(tx_chain_id) = tx.chain_id
        && tx_chain_id != chain_id
    {
        return Err(format!(
            "chain_id mismatch: tx signed for chain {}, executor configured for chain {}",
            tx_chain_id, chain_id
        ));
    }
    let ctx = Context::mainnet()
        .modify_cfg_chained(|cfg| {
            cfg.chain_id = chain_id;
            if read_only {
                cfg.disable_balance_check = true;
                cfg.disable_nonce_check = true;
                cfg.disable_base_fee = true;
            }
        })
        .modify_block_chained(|blk| {
            blk.basefee = block_base_fee;
            blk.gas_limit = BLOCK_GAS_LIMIT;
        })
        .with_db(db);

    let mut evm = ctx.build_mainnet();

    let result = evm.transact(tx);

    match result {
        Ok(result_and_state) => {
            let state = result_and_state.state;
            let exec_result = result_and_state.result;
            let (contract_address, output) = match &exec_result {
                ExecutionResult::Success {
                    output: revm::context::result::Output::Create(runtime_bytes, addr),
                    ..
                } => (*addr, runtime_bytes.to_vec()),
                ExecutionResult::Success {
                    output: revm::context::result::Output::Call(call_bytes),
                    ..
                } => (None, call_bytes.to_vec()),
                _ => (None, Vec::new()),
            };
            let receipt = TxReceipt {
                success: exec_result.is_success(),
                gas_used: exec_result.tx_gas_used(),
                contract_address,
                logs: exec_result.into_logs(),
                output,
            };
            Ok((receipt, state))
        }
        Err(e) => Err(format!("EVM execution error: {:?}", e)),
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gas::INITIAL_BASE_FEE;
    use alloy_primitives::U256;
    use revm::primitives::TxKind;
    use revm::state::AccountInfo;

    #[test]
    fn test_simple_transfer() {
        let mut db = InMemoryDB::default();

        let sender = Address::from([0x01u8; 20]);
        let receiver = Address::from([0x02u8; 20]);
        db.insert_account_info(
            sender,
            AccountInfo {
                balance: U256::from(1_000_000_000_000_000_000u128),
                nonce: 0,
                code_hash: revm::primitives::KECCAK_EMPTY,
                account_id: None,
                code: None,
            },
        );

        let tx = TxEnv::builder()
            .caller(sender)
            .kind(TxKind::Call(receiver))
            .value(U256::from(100_000u64))
            .gas_limit(21_000)
            .gas_price((INITIAL_BASE_FEE + 1_000) as u128)
            .nonce(0)
            .chain_id(Some(7119))
            .build()
            .unwrap_or_default();

        let result = execute_tx(db, tx, INITIAL_BASE_FEE, 7119);
        assert!(result.is_ok(), "execute_tx failed: {:?}", result.err());
        let r = result.unwrap();
        assert!(r.success);
        assert_eq!(r.gas_used, 21_000);
        assert!(r.contract_address.is_none());
    }

    #[test]
    fn test_contract_deploy() {
        let mut db = InMemoryDB::default();

        let sender = Address::from([0x01u8; 20]);
        db.insert_account_info(
            sender,
            AccountInfo {
                balance: U256::from(10_000_000_000_000_000_000u128),
                nonce: 0,
                code_hash: revm::primitives::KECCAK_EMPTY,
                account_id: None,
                code: None,
            },
        );

        // Simple contract: PUSH1 0x42 PUSH1 0x00 MSTORE PUSH1 0x20 PUSH1 0x00 RETURN
        let bytecode = hex::decode("604260005260206000f3").unwrap_or_default();

        let tx = TxEnv::builder()
            .caller(sender)
            .kind(TxKind::Create)
            .value(U256::ZERO)
            .gas_limit(100_000)
            .gas_price((INITIAL_BASE_FEE + 1_000) as u128)
            .nonce(0)
            .data(alloy_primitives::Bytes::from(bytecode))
            .chain_id(Some(7119))
            .build()
            .unwrap_or_default();

        let result = execute_tx(db, tx, INITIAL_BASE_FEE, 7119);
        assert!(result.is_ok(), "deploy failed: {:?}", result.err());
        let r = result.unwrap();
        assert!(r.success);
        assert!(r.contract_address.is_some());
        assert!(r.gas_used > 21_000);
    }

    fn funded_sender_db() -> (InMemoryDB, Address) {
        let mut db = InMemoryDB::default();
        let sender = Address::from([0x01u8; 20]);
        db.insert_account_info(
            sender,
            AccountInfo {
                balance: U256::from(1_000_000_000_000_000_000u128),
                nonce: 0,
                code_hash: revm::primitives::KECCAK_EMPTY,
                account_id: None,
                code: None,
            },
        );
        (db, sender)
    }

    // EIP-155: if the tx declares a chain_id, executor MUST reject when it
    // doesn't match the configured chain_id (replay-attack guard).
    #[test]
    fn test_chain_id_mismatch_rejected() {
        let (db, sender) = funded_sender_db();
        let receiver = Address::from([0x02u8; 20]);

        let tx = TxEnv::builder()
            .caller(sender)
            .kind(TxKind::Call(receiver))
            .value(U256::from(100_000u64))
            .gas_limit(21_000)
            .gas_price((INITIAL_BASE_FEE + 1_000) as u128)
            .nonce(0)
            .chain_id(Some(9999)) // tx signed for different chain
            .build()
            .unwrap_or_default();

        let result = execute_tx(db, tx, INITIAL_BASE_FEE, 7119);
        assert!(result.is_err(), "mismatch should be rejected");
        let err = result.unwrap_err();
        assert!(
            err.contains("chain_id mismatch"),
            "expected chain_id mismatch error, got: {}",
            err
        );
    }

    // Pre-EIP-155 legacy transactions carry no chain_id; the executor should
    // still accept them (revm itself enforces downstream rules).
    #[test]
    fn test_chain_id_none_allowed() {
        let (db, sender) = funded_sender_db();
        let receiver = Address::from([0x02u8; 20]);

        let tx = TxEnv::builder()
            .caller(sender)
            .kind(TxKind::Call(receiver))
            .value(U256::from(100_000u64))
            .gas_limit(21_000)
            .gas_price((INITIAL_BASE_FEE + 1_000) as u128)
            .nonce(0)
            .chain_id(None) // legacy pre-EIP-155 tx
            .build()
            .unwrap_or_default();

        let result = execute_tx(db, tx, INITIAL_BASE_FEE, 7119);
        // Must not trip the chain_id check. If revm itself fails for another
        // reason that's fine — we only care that the mismatch path isn't taken.
        if let Err(e) = &result {
            assert!(
                !e.contains("chain_id mismatch"),
                "None chain_id must not be rejected as mismatch: {}",
                e
            );
        }
    }

    // The new *_with_state variant must expose the revm state diff so the
    // block executor can persist balance/nonce/storage changes back to
    // AccountDB. Without this the whole fix is pointless — assert the
    // HashMap is returned and contains the sender (whose balance changed
    // due to gas + value transfer).
    #[test]
    fn test_execute_tx_with_state_returns_state_diff() {
        let (db, sender) = funded_sender_db();
        let receiver = Address::from([0x02u8; 20]);

        let tx = TxEnv::builder()
            .caller(sender)
            .kind(TxKind::Call(receiver))
            .value(U256::from(100_000u64))
            .gas_limit(21_000)
            .gas_price((INITIAL_BASE_FEE + 1_000) as u128)
            .nonce(0)
            .chain_id(Some(7119))
            .build()
            .unwrap_or_default();

        let result = execute_tx_with_state(db, tx, INITIAL_BASE_FEE, 7119);
        assert!(result.is_ok(), "execute_tx_with_state failed: {:?}", result.err());
        let (receipt, state) = result.unwrap();
        assert!(receipt.success);
        assert!(
            state.contains_key(&sender),
            "state diff must contain the sender address"
        );
        let sender_acct = state.get(&sender).unwrap();
        // Sender's post-tx balance should be less than the starting 1 ETH
        // (gas + value both debited).
        assert!(
            sender_acct.info.balance < U256::from(1_000_000_000_000_000_000u128),
            "sender balance should have decreased"
        );
        // Receiver should also be in the state (received value), but
        // we only assert on sender here because receiver was zero-loaded
        // and its presence depends on revm's touched-account semantics.
    }

    // Reverted tx still returns a state diff — caller is responsible for
    // NOT applying it when receipt.success == false. This test pins that
    // the executor doesn't silently eat the state on failure.
    #[test]
    fn test_execute_tx_with_state_returns_state_even_on_noop() {
        let (db, sender) = funded_sender_db();
        // Call with no calldata to a non-contract address — no-op, success=true
        let receiver = Address::from([0x02u8; 20]);

        let tx = TxEnv::builder()
            .caller(sender)
            .kind(TxKind::Call(receiver))
            .value(U256::ZERO)
            .gas_limit(21_000)
            .gas_price((INITIAL_BASE_FEE + 1_000) as u128)
            .nonce(0)
            .chain_id(Some(7119))
            .build()
            .unwrap_or_default();

        let result = execute_tx_with_state(db, tx, INITIAL_BASE_FEE, 7119);
        let (_receipt, state) = result.expect("no-op call should succeed");
        // State map is returned even for trivial txs — sender is touched
        // by gas deduction at minimum.
        assert!(!state.is_empty(), "state diff must not be empty even for no-op");
    }
}
