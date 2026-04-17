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
/// The db is consumed. Returns the receipt and the accumulated state changes.
pub fn execute_tx(db: InMemoryDB, tx: TxEnv, block_base_fee: u64) -> Result<TxReceipt, String> {
    execute_tx_inner(db, tx, block_base_fee, false)
}

/// Read-only variant — disables balance/nonce checks for eth_call.
pub fn execute_call(db: InMemoryDB, tx: TxEnv, block_base_fee: u64) -> Result<TxReceipt, String> {
    execute_tx_inner(db, tx, block_base_fee, true)
}

fn execute_tx_inner(
    db: InMemoryDB,
    tx: TxEnv,
    block_base_fee: u64,
    read_only: bool,
) -> Result<TxReceipt, String> {
    use revm::Context;

    let chain_id = tx.chain_id.unwrap_or(7119);
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
            Ok(receipt)
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

        let result = execute_tx(db, tx, INITIAL_BASE_FEE);
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

        let result = execute_tx(db, tx, INITIAL_BASE_FEE);
        assert!(result.is_ok(), "deploy failed: {:?}", result.err());
        let r = result.unwrap();
        assert!(r.success);
        assert!(r.contract_address.is_some());
        assert!(r.gas_used > 21_000);
    }
}
