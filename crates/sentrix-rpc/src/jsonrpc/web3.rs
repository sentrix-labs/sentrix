// web3.rs — `web3_*` JSON-RPC namespace.
//
// Spec: https://ethereum.org/en/developers/docs/apis/json-rpc/#web3_clientversion
//
// Pulled out of the monolithic `mod.rs` during the backlog #11 phase 2
// refactor. Currently a single method; more may arrive later (e.g.
// `web3_sha3`).

use crate::routes::SharedState;
use serde_json::{Value, json};

use super::DispatchResult;

pub(super) async fn dispatch(
    method: &str,
    _params: &Value,
    _state: &SharedState,
) -> DispatchResult {
    match method {
        "web3_clientVersion" => Ok(json!(format!("Sentrix/{}/Rust", env!("CARGO_PKG_VERSION")))),
        _ => Err((-32601, format!("method not found: {}", method))),
    }
}
