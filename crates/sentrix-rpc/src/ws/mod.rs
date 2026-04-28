//! WebSocket subscription endpoint at `/ws`.
//!
//! Implements `eth_subscribe` / `eth_unsubscribe` for real-time chain
//! events. dApps that need live updates (wallets watching tx
//! confirmations, DEX UIs streaming prices, explorers showing live
//! blocks) connect once and subscribe to the channels they care about.
//!
//! ## Channels supported (this PR — Phase 1)
//!
//! - `newHeads` — fires on every consensus-finalized block. Payload
//!   is an Ethereum-compatible header (`number`, `hash`, `parentHash`,
//!   `timestamp`, `miner`, `stateRoot`, `transactionsRoot`, `gasLimit`,
//!   `gasUsed`, `difficulty`, `nonce`, `extraData`, `size`). Reuses
//!   the dApp tooling that already speaks Ethereum's `eth_subscribe`
//!   protocol.
//!
//! ## Channels stubbed (returns NotImplemented for now)
//!
//! - `logs` — EVM contract event subscriptions. Phase 2 work; needs
//!   the per-tx log extraction wired into the EventBus.
//! - `newPendingTransactions` — mempool admission events. Phase 2
//!   work; needs `add_to_mempool` to call into the bus.
//! - `syncing` — sync status changes. Always false on Sentrix today
//!   (we don't have an active "syncing" mode that lasts long enough
//!   to be observable), so stub returns false immediately.
//!
//! ## Non-subscription methods over WS
//!
//! Any non-subscribe method received over the WS connection is
//! delegated to the same dispatcher used for HTTP RPC. So `eth_call`,
//! `eth_getBalance`, `eth_blockNumber` etc. work over the same WS
//! connection that streams subscriptions — saves dApps having to
//! maintain two connections (HTTP for queries, WS for streams).
//!
//! ## Lifecycle
//!
//! Each subscription = one tokio::spawn task. eth_unsubscribe aborts
//! that task. Connection close aborts every task on that connection.
//! Slow consumer that lags >1024 events behind gets a `Lagged` error
//! emitted to the client and the task aborts itself; the client must
//! reconnect to resubscribe.

use crate::events::{EventBus, NewHeadEvent};
use crate::jsonrpc::{JsonRpcRequest, JsonRpcResponse, dispatch_request};
use crate::routes::SharedState;
use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::{Mutex, broadcast};

/// Cap concurrent subscriptions per connection — guards against an
/// abusive client trying to allocate thousands of tokio tasks via
/// repeated `eth_subscribe` calls. 100 is generous (a real dApp uses
/// 1-5).
pub const MAX_SUBS_PER_CONNECTION: usize = 100;

/// `eth_subscription` envelope that wraps every subscription event
/// payload. Matches Ethereum's standard shape so existing dApp tooling
/// (ethers.js, viem, web3.js) parses it without special-casing.
///
/// ```json
/// {
///   "jsonrpc": "2.0",
///   "method": "eth_subscription",
///   "params": {
///     "subscription": "<sub-id>",
///     "result": { ...payload... }
///   }
/// }
/// ```
#[derive(Debug, Serialize)]
struct SubscriptionMessage<'a> {
    jsonrpc: &'static str,
    method: &'static str,
    params: SubscriptionPayload<'a>,
}

#[derive(Debug, Serialize)]
struct SubscriptionPayload<'a> {
    subscription: &'a str,
    result: Value,
}

fn wrap_subscription(sub_id: &str, result: Value) -> Value {
    json!(SubscriptionMessage {
        jsonrpc: "2.0",
        method: "eth_subscription",
        params: SubscriptionPayload { subscription: sub_id, result },
    })
}

/// Combined router state for the WebSocket route. Holds both the
/// blockchain state (for HTTP fall-through dispatch) and the event bus
/// (for streaming subscriptions).
#[derive(Clone)]
pub struct WsState {
    pub state: SharedState,
    pub bus: Arc<EventBus>,
}

/// Axum WebSocket upgrade handler. Routes new connections to
/// `handle_socket` after the HTTP-to-WS upgrade dance.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(ws_state): State<WsState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, ws_state))
}

/// Per-connection main loop. Reads JSON-RPC requests from the client,
/// routes subscriptions to spawn-based listener tasks, falls through
/// non-subscribe methods to the same dispatcher used by HTTP RPC.
async fn handle_socket(socket: WebSocket, ws_state: WsState) {
    let (sender, mut receiver) = socket.split();
    // Wrap the sender in a Mutex so multiple subscription tasks can
    // serialize writes safely. Without this, two tasks calling send
    // concurrently would interleave bytes mid-frame.
    let sender = Arc::new(Mutex::new(sender));
    let subscriptions: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    // Monotonic per-connection counter for subscription IDs. Format
    // mirrors what geth/erigon emit (`0x` + 16 hex chars) so existing
    // dApp tooling treats them as opaque opaque tokens consistently.
    let next_sub_id = Arc::new(AtomicU64::new(1));

    while let Some(msg_result) = receiver.next().await {
        let msg = match msg_result {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => break,
            Ok(_) => continue, // Ping/Pong/Binary — ignore
            Err(e) => {
                tracing::warn!("ws: receive error: {}", e);
                break;
            }
        };

        let req: JsonRpcRequest = match serde_json::from_str(&msg) {
            Ok(r) => r,
            Err(_) => {
                send_error(&sender, None, -32700, "Parse error").await;
                continue;
            }
        };

        let id = req.id.clone();
        let method = req.method.clone();

        match method.as_str() {
            "eth_subscribe" => {
                handle_subscribe(
                    req,
                    &subscriptions,
                    &next_sub_id,
                    &sender,
                    &ws_state.bus,
                    id,
                )
                .await;
            }
            "eth_unsubscribe" => {
                handle_unsubscribe(req, &subscriptions, &sender, id).await;
            }
            _ => {
                // Fall-through: regular JSON-RPC method. Dispatch via the
                // same path HTTP uses so every method reachable over HTTP
                // is reachable over WS for free.
                let resp = dispatch_request(&ws_state.state, req).await;
                send_response(&sender, resp).await;
            }
        }
    }

    // Connection closed — abort every spawned subscription task.
    let mut subs = subscriptions.lock().await;
    for (_, handle) in subs.drain() {
        handle.abort();
    }
}

async fn handle_subscribe(
    req: JsonRpcRequest,
    subscriptions: &Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    next_sub_id: &Arc<AtomicU64>,
    sender: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    bus: &Arc<EventBus>,
    id: Option<Value>,
) {
    let params = req.params.unwrap_or(json!([]));
    let channel = match params.get(0).and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            send_error(sender, id, -32602, "missing channel parameter").await;
            return;
        }
    };

    {
        let subs = subscriptions.lock().await;
        if subs.len() >= MAX_SUBS_PER_CONNECTION {
            send_error(
                sender,
                id,
                -32005,
                &format!(
                    "subscription limit reached ({} per connection)",
                    MAX_SUBS_PER_CONNECTION
                ),
            )
            .await;
            return;
        }
    }

    let sub_id = format!(
        "0x{:016x}",
        next_sub_id.fetch_add(1, Ordering::Relaxed)
    );

    let handle = match channel.as_str() {
        "newHeads" => {
            let rx = bus.new_heads.subscribe();
            spawn_new_heads_listener(rx, sender.clone(), sub_id.clone())
        }
        "logs" | "newPendingTransactions" => {
            send_error(
                sender,
                id,
                -32601,
                &format!(
                    "channel '{channel}' not yet implemented (Phase 2 work; only 'newHeads' shipped in this PR)"
                ),
            )
            .await;
            return;
        }
        "syncing" => {
            // Sentrix doesn't have a long-lived "syncing" mode, but we
            // emit a single false on subscribe so dApps that block on
            // this don't hang. Then the task exits — never streams more.
            let sender_clone = sender.clone();
            let sub_id_clone = sub_id.clone();
            tokio::spawn(async move {
                let payload = wrap_subscription(&sub_id_clone, json!(false));
                let mut s = sender_clone.lock().await;
                let _ = s.send(Message::Text(payload.to_string().into())).await;
            })
        }
        _ => {
            send_error(
                sender,
                id,
                -32602,
                &format!("unknown subscription channel: {channel}"),
            )
            .await;
            return;
        }
    };

    subscriptions.lock().await.insert(sub_id.clone(), handle);
    send_response(
        sender,
        JsonRpcResponse::ok(id, json!(sub_id)),
    )
    .await;
}

async fn handle_unsubscribe(
    req: JsonRpcRequest,
    subscriptions: &Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    sender: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    id: Option<Value>,
) {
    let params = req.params.unwrap_or(json!([]));
    let sub_id = match params.get(0).and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            send_error(sender, id, -32602, "missing subscription id").await;
            return;
        }
    };

    let removed = subscriptions.lock().await.remove(sub_id);
    if let Some(handle) = &removed {
        handle.abort();
    }
    send_response(
        sender,
        JsonRpcResponse::ok(id, json!(removed.is_some())),
    )
    .await;
}

/// Spawn the per-subscription listener for `newHeads`. Loops on the
/// broadcast Receiver and forwards every event to the WS sender as
/// an `eth_subscription` message. Exits on Lagged or Closed.
fn spawn_new_heads_listener(
    mut rx: broadcast::Receiver<NewHeadEvent>,
    sender: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    sub_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let payload = wrap_subscription(&sub_id, json!(event));
                    let mut s = sender.lock().await;
                    if s.send(Message::Text(payload.to_string().into())).await.is_err() {
                        // Client disconnected — exit task; the per-conn
                        // subscriptions HashMap will be drained by the
                        // outer loop on close.
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    // Notify the client they fell behind, then exit. The
                    // client is expected to reconnect + resubscribe to
                    // recover. This is the canonical broadcast::Receiver
                    // semantic — slow consumers don't block fast ones.
                    let payload = json!({
                        "jsonrpc": "2.0",
                        "method": "eth_subscription",
                        "params": {
                            "subscription": sub_id,
                            "result": null,
                            "error": format!("subscription lagged ({skipped} events skipped); reconnect to resume"),
                        },
                    });
                    let mut s = sender.lock().await;
                    let _ = s.send(Message::Text(payload.to_string().into())).await;
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    // Bus dropped — server is shutting down.
                    break;
                }
            }
        }
    })
}

async fn send_response(
    sender: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    resp: JsonRpcResponse,
) {
    let body = match serde_json::to_string(&resp) {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut s = sender.lock().await;
    let _ = s.send(Message::Text(body.into())).await;
}

async fn send_error(
    sender: &Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    id: Option<Value>,
    code: i32,
    message: &str,
) {
    send_response(sender, JsonRpcResponse::err(id, code, message)).await;
}
