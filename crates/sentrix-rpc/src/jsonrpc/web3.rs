// web3.rs — `web3_*` JSON-RPC namespace.
//
// Spec: https://ethereum.org/en/developers/docs/apis/json-rpc/#web3_clientversion
//
// Pulled out of the monolithic `mod.rs` during the backlog #11 phase 2
// refactor. `web3_sha3` (keccak256 utility) added 2026-05-05 after a
// transport audit found it advertised by the namespace but absent.

use crate::routes::SharedState;
use serde_json::{Value, json};
use sha3::{Digest, Keccak256};

use super::DispatchResult;

pub(super) async fn dispatch(
    method: &str,
    params: &Value,
    _state: &SharedState,
) -> DispatchResult {
    match method {
        "web3_clientVersion" => Ok(json!(format!("Sentrix/{}/Rust", env!("CARGO_PKG_VERSION")))),
        // `web3_sha3` — keccak256 hash of a hex-encoded byte string. The
        // wagmi / ethers utility tier occasionally calls this for client-
        // side hashing parity. Spec: input is a `0x`-prefixed hex string,
        // output is the keccak256 hash, also `0x`-prefixed.
        "web3_sha3" => {
            let input = params
                .get(0)
                .and_then(|v| v.as_str())
                .ok_or((-32602, "web3_sha3: expected hex string param".to_string()))?;
            let hex = input.strip_prefix("0x").unwrap_or(input);
            let bytes = hex::decode(hex)
                .map_err(|e| (-32602, format!("web3_sha3: invalid hex: {}", e)))?;
            let mut hasher = Keccak256::new();
            hasher.update(&bytes);
            let digest = hasher.finalize();
            Ok(json!(format!("0x{}", hex::encode(digest))))
        }
        _ => Err((-32601, format!("method not found: {}", method))),
    }
}
