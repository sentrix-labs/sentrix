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

use crate::events::{
    EventBus, FinalizedEvent, JailEvent, LogEvent, NewHeadEvent, PendingTxEvent, StakingOpEvent,
    TokenOpEvent, ValidatorSetEvent,
};
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

/// Cap concurrent WebSocket connections per source IP — defense
/// against a single client exhausting the validator's file descriptor
/// pool. 10 is generous: a wallet typically opens 1, an explorer
/// frontend 2-3, a power user with multiple browser tabs 5+. Exceed
/// → 503 at the upgrade response.
pub const MAX_CONNECTIONS_PER_IP: usize = 10;

/// Per-IP connection counter — shared across all WS upgrades. Wraps a
/// HashMap behind a `tokio::sync::Mutex` so the upgrade handler can
/// atomically check + increment, and the connection-close path can
/// decrement.
#[derive(Clone, Default)]
pub struct WsIpLimiter(pub Arc<Mutex<HashMap<std::net::IpAddr, usize>>>);

impl WsIpLimiter {
    /// Increment the counter for `ip`. Returns `Ok(guard)` on success;
    /// `Err(())` if the IP is at the cap. The guard's Drop decrements.
    pub async fn try_acquire(&self, ip: std::net::IpAddr) -> Result<WsIpGuard, ()> {
        let mut map = self.0.lock().await;
        let count = map.entry(ip).or_insert(0);
        if *count >= MAX_CONNECTIONS_PER_IP {
            return Err(());
        }
        *count += 1;
        Ok(WsIpGuard {
            inner: self.0.clone(),
            ip,
        })
    }
}

/// RAII guard that decrements the per-IP WS connection counter when
/// dropped. Owned by the per-connection async task; goes out of scope
/// when the connection closes (normal or error path), guaranteeing
/// the counter releases without an explicit unlock call.
pub struct WsIpGuard {
    inner: Arc<Mutex<HashMap<std::net::IpAddr, usize>>>,
    ip: std::net::IpAddr,
}

impl Drop for WsIpGuard {
    fn drop(&mut self) {
        // tokio::Mutex blocks_in_place isn't available in arbitrary
        // contexts; spawn the decrement on the runtime instead so Drop
        // stays infallible. This races against new acquires for the
        // same IP — acceptable because the worst case is a brief moment
        // where the counter is one higher than reality, never wedged.
        let inner = self.inner.clone();
        let ip = self.ip;
        tokio::spawn(async move {
            let mut map = inner.lock().await;
            if let Some(count) = map.get_mut(&ip) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    map.remove(&ip);
                }
            }
        });
    }
}

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

/// Combined router state for the WebSocket route. Holds the blockchain
/// state (for HTTP fall-through dispatch), the event bus (for streaming
/// subscriptions), and the per-IP connection limiter.
#[derive(Clone)]
pub struct WsState {
    pub state: SharedState,
    pub bus: Arc<EventBus>,
    pub ip_limiter: WsIpLimiter,
}

/// Axum WebSocket upgrade handler. Per-IP connection limit enforced
/// BEFORE the upgrade response — over-limit clients see a 503 instead
/// of an established socket they'd just have to close.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    State(ws_state): State<WsState>,
) -> axum::response::Response {
    let ip = addr.ip();
    let guard = match ws_state.ip_limiter.try_acquire(ip).await {
        Ok(g) => g,
        Err(()) => {
            return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    ws.on_upgrade(move |socket| {
        // Move the guard into the handle_socket future so its Drop
        // fires when the connection ends. Without this, the counter
        // would leak.
        let _g = guard;
        handle_socket(socket, ws_state, _g)
    })
    .into_response()
}

/// Per-connection main loop. Reads JSON-RPC requests from the client,
/// routes subscriptions to spawn-based listener tasks, falls through
/// non-subscribe methods to the same dispatcher used by HTTP RPC.
async fn handle_socket(socket: WebSocket, ws_state: WsState, _ip_guard: WsIpGuard) {
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
        "logs" => {
            // Optional second param is the filter object: { address, topics }.
            // Same filter shape as eth_getLogs. We parse here once, the
            // listener task applies per-event without re-parsing.
            let filter = LogFilter::from_params(params.get(1));
            let rx = bus.logs.subscribe();
            spawn_logs_listener(rx, sender.clone(), sub_id.clone(), filter)
        }
        "newPendingTransactions" => {
            let rx = bus.pending_txs.subscribe();
            spawn_pending_txs_listener(rx, sender.clone(), sub_id.clone())
        }
        // Sentrix-native channels (sentrix_subscribe equivalent — exposed
        // via the same eth_subscribe entry point so dApp tooling works
        // without learning a new method name).
        "sentrix_finalized" => {
            let rx = bus.finalized.subscribe();
            spawn_finalized_listener(rx, sender.clone(), sub_id.clone())
        }
        "sentrix_validatorSet" => {
            let rx = bus.validator_set.subscribe();
            spawn_validator_set_listener(rx, sender.clone(), sub_id.clone())
        }
        "sentrix_tokenOps" => {
            let rx = bus.token_ops.subscribe();
            spawn_token_ops_listener(rx, sender.clone(), sub_id.clone())
        }
        "sentrix_stakingOps" => {
            let rx = bus.staking_ops.subscribe();
            spawn_staking_ops_listener(rx, sender.clone(), sub_id.clone())
        }
        "sentrix_jail" => {
            let rx = bus.jail.subscribe();
            spawn_jail_listener(rx, sender.clone(), sub_id.clone())
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

/// Filter for `eth_subscribe(logs)` — mirrors the eth_getLogs filter
/// shape (address: single or array, topics: positional array of
/// single-hash-or-array). Empty filter means "all logs".
#[derive(Debug, Clone, Default)]
struct LogFilter {
    /// Hex addresses lowercased without the 0x prefix. Empty = match any.
    addresses: Vec<[u8; 20]>,
    /// Per-position topic match. None = wildcard at that position.
    /// `Some(vec![hash])` = match that single hash. `Some(vec![h1, h2])`
    /// = match either. Position 0..3.
    topics: Vec<Option<Vec<[u8; 32]>>>,
}

impl LogFilter {
    fn from_params(filter_value: Option<&Value>) -> Self {
        let Some(filter) = filter_value else {
            return Self::default();
        };
        let addresses = match filter.get("address") {
            Some(Value::String(s)) => parse_addr(s).into_iter().collect(),
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .filter_map(parse_addr)
                .collect(),
            _ => vec![],
        };
        let topics = match filter.get("topics") {
            Some(Value::Array(arr)) => arr
                .iter()
                .map(|slot| match slot {
                    Value::Null => None,
                    Value::String(s) => parse_topic(s).map(|h| vec![h]),
                    Value::Array(set) => {
                        let hashes: Vec<[u8; 32]> = set
                            .iter()
                            .filter_map(|v| v.as_str())
                            .filter_map(parse_topic)
                            .collect();
                        if hashes.is_empty() { None } else { Some(hashes) }
                    }
                    _ => None,
                })
                .collect(),
            _ => vec![],
        };
        Self { addresses, topics }
    }

    fn matches(&self, log: &LogEvent) -> bool {
        if !self.addresses.is_empty() {
            // log.address is "0x" + 40 hex; parse to bytes.
            let log_addr = match parse_addr(&log.address) {
                Some(a) => a,
                None => return false,
            };
            if !self.addresses.iter().any(|a| a == &log_addr) {
                return false;
            }
        }
        for (i, slot) in self.topics.iter().enumerate() {
            let Some(set) = slot else { continue };
            let topic = match log.topics.get(i).and_then(|t| parse_topic(t)) {
                Some(t) => t,
                None => return false,
            };
            if !set.iter().any(|s| s == &topic) {
                return false;
            }
        }
        true
    }
}

fn parse_addr(s: &str) -> Option<[u8; 20]> {
    let s = s.trim_start_matches("0x").to_ascii_lowercase();
    let bytes = hex::decode(&s).ok()?;
    if bytes.len() != 20 {
        return None;
    }
    let mut out = [0u8; 20];
    out.copy_from_slice(&bytes);
    Some(out)
}

fn parse_topic(s: &str) -> Option<[u8; 32]> {
    let s = s.trim_start_matches("0x").to_ascii_lowercase();
    let bytes = hex::decode(&s).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Some(out)
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

/// Phase 2: per-subscription listener for `eth_subscribe(logs)`. Applies the
/// LogFilter per-event before forwarding so unsubscribed addresses /
/// non-matching topics never reach the wire. Exits on Lagged or Closed
/// with the same semantics as `spawn_new_heads_listener`.
fn spawn_logs_listener(
    mut rx: broadcast::Receiver<LogEvent>,
    sender: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    sub_id: String,
    filter: LogFilter,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if !filter.matches(&event) {
                        continue;
                    }
                    let payload = wrap_subscription(&sub_id, json!(event));
                    let mut s = sender.lock().await;
                    if s.send(Message::Text(payload.to_string().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
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
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Phase 2: per-subscription listener for `eth_subscribe(newPendingTransactions)`.
/// Standard Ethereum payload is just the txid string; emits that exact
/// shape so dApp tooling consumes without special-casing.
fn spawn_pending_txs_listener(
    mut rx: broadcast::Receiver<PendingTxEvent>,
    sender: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    sub_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(event) => {
                    let payload = wrap_subscription(&sub_id, json!(event.txid));
                    let mut s = sender.lock().await;
                    if s.send(Message::Text(payload.to_string().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
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
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

/// Phase 3: Sentrix-native — `sentrix_finalized` listener.
/// On Lagged or Closed, exits the task (client must reconnect to resume).
fn spawn_finalized_listener(
    mut rx: broadcast::Receiver<FinalizedEvent>,
    sender: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    sub_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let payload = wrap_subscription(&sub_id, json!(event));
            let mut s = sender.lock().await;
            if s.send(Message::Text(payload.to_string().into())).await.is_err() {
                break;
            }
        }
    })
}

/// Phase 3: Sentrix-native — `sentrix_validatorSet` listener. Fires at
/// epoch boundary when the active set rotates. Wire-up of emit_validator_set
/// in the staking layer pending — the channel is ready; subscribers will
/// start receiving once the staking epoch-advance path calls
/// `emit_validator_set`.
fn spawn_validator_set_listener(
    mut rx: broadcast::Receiver<ValidatorSetEvent>,
    sender: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    sub_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let payload = wrap_subscription(&sub_id, json!(event));
            let mut s = sender.lock().await;
            if s.send(Message::Text(payload.to_string().into())).await.is_err() {
                break;
            }
        }
    })
}

/// Phase 3: Sentrix-native — `sentrix_tokenOps` listener. Forwards
/// every successfully-applied native TokenOp event.
fn spawn_token_ops_listener(
    mut rx: broadcast::Receiver<TokenOpEvent>,
    sender: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    sub_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let payload = wrap_subscription(&sub_id, json!(event));
            let mut s = sender.lock().await;
            if s.send(Message::Text(payload.to_string().into())).await.is_err() {
                break;
            }
        }
    })
}

/// Phase 3: Sentrix-native — `sentrix_stakingOps` listener.
fn spawn_staking_ops_listener(
    mut rx: broadcast::Receiver<StakingOpEvent>,
    sender: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    sub_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let payload = wrap_subscription(&sub_id, json!(event));
            let mut s = sender.lock().await;
            if s.send(Message::Text(payload.to_string().into())).await.is_err() {
                break;
            }
        }
    })
}

/// Phase 3: Sentrix-native — `sentrix_jail` listener. Silent until
/// `JAIL_CONSENSUS_HEIGHT` activates and JailEvidenceBundle dispatch
/// produces real jail decisions.
fn spawn_jail_listener(
    mut rx: broadcast::Receiver<JailEvent>,
    sender: Arc<Mutex<futures_util::stream::SplitSink<WebSocket, Message>>>,
    sub_id: String,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let payload = wrap_subscription(&sub_id, json!(event));
            let mut s = sender.lock().await;
            if s.send(Message::Text(payload.to_string().into())).await.is_err() {
                break;
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
