// net.rs — `net_*` JSON-RPC namespace.
//
// Spec: https://ethereum.org/en/developers/docs/apis/json-rpc/#net_version
//
// Pulled out of the monolithic `mod.rs` during the backlog #11 phase 2
// refactor. The handler signature matches the other namespace modules
// (eth / web3 / sentrix) so the top-level dispatcher in `mod.rs` can
// route by prefix.

use crate::routes::SharedState;
use serde_json::{Value, json};

use super::DispatchResult;

pub(super) async fn dispatch(method: &str, _params: &Value, state: &SharedState) -> DispatchResult {
    match method {
        "net_version" => {
            let bc = state.read().await;
            Ok(json!(bc.chain_id.to_string()))
        }
        "net_listening" => Ok(json!(true)),
        // `net_peerCount` — wagmi / ethers occasionally probe this. We
        // don't thread the libp2p Swarm handle through SharedState yet
        // (would be a deeper refactor), so return active validator count
        // minus self as a best-effort proxy — honest enough for the
        // wallets that look at this since none branch on the exact value.
        "net_peerCount" => {
            let bc = state.read().await;
            let n = bc.authority.active_count().saturating_sub(1);
            Ok(json!(format!("0x{:x}", n)))
        }
        _ => Err((-32601, format!("method not found: {}", method))),
    }
}
