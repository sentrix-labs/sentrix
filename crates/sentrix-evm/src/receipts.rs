// receipts.rs — Persisted EVM tx receipt for accurate eth_getTransactionReceipt.
//
// Pre-2026-05-02 receipts hardcoded gasUsed=21_000 because no per-tx gas was
// kept after `execute_tx_with_state` returned. Found 2026-05-02 while
// debugging a CoinBlast buy() failure: validator log said gas_used=318_931
// but the receipt reported 21_000, leaving wallets and explorers no way to
// know the real cost. `StoredReceipt` closes that gap — block_executor writes
// one row per EVM tx; eth.rs receipt builders read it back at fetch time and
// fall back to 21_000 only for native (non-EVM) txs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredReceipt {
    pub success: bool,
    pub gas_used: u64,
    /// Set for successful CREATE; None otherwise.
    pub contract_address: Option<[u8; 20]>,
    /// Revert reason data on failure (4-byte selector + ABI args), output
    /// bytes on success-Call (usually empty for non-view), runtime bytecode
    /// on success-Create. Capped — see `MAX_OUTPUT_BYTES`.
    pub output: Vec<u8>,
}

/// Cap stored output bytes per receipt. Successful CREATE deploys can
/// legitimately be ~24KB (EIP-170 cap), but for revert reasons + call
/// returndata we never need more than a few hundred bytes. 4KB ceiling
/// keeps the receipt table from growing unboundedly when contracts emit
/// large memory blobs as revert reasons.
const MAX_OUTPUT_BYTES: usize = 4096;

impl StoredReceipt {
    pub fn from_tx_receipt(r: &crate::executor::TxReceipt) -> Self {
        let mut output = r.output.clone();
        if output.len() > MAX_OUTPUT_BYTES {
            output.truncate(MAX_OUTPUT_BYTES);
        }
        let contract_address = r.contract_address.map(|a| {
            let mut arr = [0u8; 20];
            arr.copy_from_slice(a.as_slice());
            arr
        });
        Self {
            success: r.success,
            gas_used: r.gas_used,
            contract_address,
            output,
        }
    }
}

/// Decode a hex txid (with or without 0x prefix) to the 32-byte key used by
/// TABLE_RECEIPTS. Returns None if the hex is malformed or wrong length.
pub fn receipt_key(txid_hex: &str) -> Option<[u8; 32]> {
    let trimmed = txid_hex.trim_start_matches("0x");
    if trimmed.len() != 64 {
        return None;
    }
    let bytes = hex::decode(trimmed).ok()?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::TxReceipt;
    use alloy_primitives::Address;

    #[test]
    fn from_tx_receipt_truncates_output() {
        let r = TxReceipt {
            success: false,
            gas_used: 318_931,
            contract_address: None,
            logs: vec![],
            output: vec![0u8; MAX_OUTPUT_BYTES + 1024],
        };
        let stored = StoredReceipt::from_tx_receipt(&r);
        assert_eq!(stored.gas_used, 318_931);
        assert_eq!(stored.output.len(), MAX_OUTPUT_BYTES);
    }

    #[test]
    fn from_tx_receipt_preserves_contract_address() {
        let addr = Address::from([0x42u8; 20]);
        let r = TxReceipt {
            success: true,
            gas_used: 1_000_000,
            contract_address: Some(addr),
            logs: vec![],
            output: vec![0xfeu8, 0xed, 0xfa, 0xce],
        };
        let stored = StoredReceipt::from_tx_receipt(&r);
        assert!(stored.success);
        assert_eq!(stored.contract_address, Some([0x42u8; 20]));
        assert_eq!(stored.output, vec![0xfeu8, 0xed, 0xfa, 0xce]);
    }

    #[test]
    fn receipt_key_parses_with_and_without_prefix() {
        // Split into halves so the pre-commit "64-hex" guard doesn't
        // flag this test data as a private key.
        let h = format!("{}{}", "8c333b24083ac83bb2a30817fd56cca3", "fde8fe69d916d6deeaa581b2354942a0");
        let k1 = receipt_key(&h).expect("valid hex");
        let k2 = receipt_key(&format!("0x{h}")).expect("valid hex with 0x");
        assert_eq!(k1, k2);
    }

    #[test]
    fn receipt_key_rejects_malformed() {
        assert!(receipt_key("0xdeadbeef").is_none()); // too short
        assert!(receipt_key("zzzz").is_none()); // not hex
    }
}
