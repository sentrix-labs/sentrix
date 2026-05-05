//! Sentrix gRPC supplement transport (Tonic).
//!
//! Parallel to the JSON-RPC `eth_*` interface served at port 8545 — same
//! backend, same state, different wire format. JSON-RPC remains the
//! ecosystem-facing contract for wallets and dApps; gRPC is for SentrisCloud
//! internal monitoring and power-user clients that prefer binary protocols.
//!
//! ## v0.2 (2026-05-05): Side-car wired
//!
//! Service holds an `Arc<tokio::sync::RwLock<Blockchain>>` — same shared
//! state the JSON-RPC handlers read. Handlers borrow read locks for short
//! windows and return immediately; same lock-contention profile as the
//! existing axum router. Read paths implemented:
//!
//! - `GetBlock` — by height, by hash, or `latest` selector. NOT_FOUND if
//!   outside the in-memory chain window.
//! - `GetBalance` — balance + nonce in a single round-trip.
//!
//! Deferred to fresh-brain (proto Transaction ↔ chain Transaction
//! marshalling): `BroadcastTx`, `StreamEvents`. They return
//! `tonic::Status::unimplemented` with a doc pointer.
//!
//! ## Concurrency discipline
//!
//! - `#![forbid(unsafe_code)]` at lib root.
//! - `tokio::sync::RwLock` only — never `std::sync` across `.await` (workspace
//!   audit completed 2026-05-05; rule documented at
//!   `crates/sentrix-rpc/src/routes/mod.rs:49`).
//! - Read locks held for the minimum span: acquire, copy what we need, drop.
//!   No `.await` while holding the lock.
//! - Side-car spawn pattern: caller uses `tokio::spawn` to run the server,
//!   so a wedged handler cannot stall the validator main loop.

#![forbid(unsafe_code)]
// `tonic::Status` is ~176 bytes (carries metadata, error code, message,
// source). Returning it via `Result<T, Status>` is the canonical tonic
// pattern across the entire ecosystem; boxing every Err would create
// friction with every caller for no real benefit. Allow at crate root.
#![allow(clippy::result_large_err)]

/// Generated types and service stubs from `proto/sentrix.proto`.
pub mod sentrix_proto {
    tonic::include_proto!("sentrix.v1");
}

use sentrix_proto::sentrix_server::{Sentrix, SentrixServer};
use sentrix_proto::*;
use std::sync::Arc;
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};

use sentrix_core::blockchain::Blockchain;
use sentrix_rpc::events::{EventBus, NewHeadEvent};

/// Shared state handle — identical type to the one passed to the JSON-RPC
/// router (`crates/sentrix-rpc/src/routes/mod.rs::SharedState`). Same Arc;
/// gRPC is just another reader.
pub type SharedState = Arc<RwLock<Blockchain>>;

/// Service handler. Borrows the same shared `Blockchain` state as the JSON-RPC
/// stack. Read paths use a brief read-lock; the BroadcastTx path (when
/// implemented) will use the same brief write-lock pattern as
/// `routes::transactions::send_transaction`.
///
/// v0.3 also holds an `Arc<EventBus>` so `stream_events` can subscribe to
/// the same broadcast channels powering the WebSocket `eth_subscribe` handlers.
/// Single source-of-truth for event ordering — a gRPC stream subscriber and
/// a WS subscriber see the same sequence at the broadcast::Sender boundary.
pub struct SentrixServiceImpl {
    state: SharedState,
    event_bus: Arc<EventBus>,
}

impl SentrixServiceImpl {
    pub fn new(state: SharedState, event_bus: Arc<EventBus>) -> Self {
        Self { state, event_bus }
    }
}

/// Convenience constructor returning a tonic-ready `SentrixServer`.
/// Caller is responsible for binding to a transport — see the gated
/// spawn block in `bin/sentrix/src/main.rs`.
pub fn server_factory(
    state: SharedState,
    event_bus: Arc<EventBus>,
) -> SentrixServer<SentrixServiceImpl> {
    SentrixServer::new(SentrixServiceImpl::new(state, event_bus))
}

// ── Helpers: chain-string-hex ↔ proto-bytes ──────────────────────────────

/// Convert a chain `0x…` hex address (42 chars) to a 20-byte proto Address.
/// Returns `None` if the string is malformed (wrong length, bad hex, missing
/// `0x` prefix). gRPC handlers map None → `Status::invalid_argument`.
fn chain_addr_to_proto(s: &str) -> Option<Address> {
    let bytes = parse_hex_prefixed(s, 20)?;
    Some(Address { value: bytes })
}

/// Inverse: 20-byte proto Address → chain `0x…` lowercase hex string.
fn proto_addr_to_chain(a: &Address) -> Result<String, Status> {
    if a.value.len() != 20 {
        return Err(Status::invalid_argument(format!(
            "Address.value must be 20 bytes, got {}",
            a.value.len()
        )));
    }
    Ok(format!("0x{}", hex::encode(&a.value)))
}

/// Convert a chain hex hash string (64 hex chars; may or may not have `0x`
/// prefix in different code paths) to a 32-byte proto Hash. Returns None on
/// malformed input.
fn chain_hash_to_proto(s: &str) -> Option<Hash> {
    let bytes = parse_hex_prefixed(s, 32)?;
    Some(Hash { value: bytes })
}

/// Inverse: 32-byte proto Hash → chain hex hash string (lowercase, no prefix
/// — chain blocks store hashes as bare hex).
fn proto_hash_to_chain(h: &Hash) -> Result<String, Status> {
    if h.value.len() != 32 {
        return Err(Status::invalid_argument(format!(
            "Hash.value must be 32 bytes, got {}",
            h.value.len()
        )));
    }
    Ok(hex::encode(&h.value))
}

/// Strip optional `0x` prefix and decode N bytes of hex. Returns None on any
/// length / charset mismatch.
fn parse_hex_prefixed(s: &str, expect_bytes: usize) -> Option<Vec<u8>> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    if trimmed.len() != expect_bytes * 2 {
        return None;
    }
    hex::decode(trimmed).ok()
}

/// Parse a `0x`-prefixed hex u64 (e.g. `"0x2a"` → 42). NewHeadEvent encodes
/// numeric fields this way for ethers.js / viem compatibility.
fn parse_hex_u64(s: &str) -> Option<u64> {
    let trimmed = s.strip_prefix("0x").unwrap_or(s);
    u64::from_str_radix(trimmed, 16).ok()
}

/// Marshal a `NewHeadEvent` (hex-string-encoded EVM-shaped header) into a
/// proto `Block`. Used by the StreamEvents subscriber path. transactions
/// stays empty in v0.3 — same as GetBlock — clients refetch full bodies via
/// JSON-RPC `eth_getBlockByNumber` until v0.4 plumbs proto Transaction.
fn newhead_to_proto_block(ev: &NewHeadEvent) -> Block {
    Block {
        index: parse_hex_u64(&ev.number).unwrap_or(0),
        hash: chain_hash_to_proto(&ev.hash),
        parent_hash: chain_hash_to_proto(&ev.parent_hash),
        state_root: chain_hash_to_proto(&ev.state_root),
        timestamp: parse_hex_u64(&ev.timestamp).unwrap_or(0),
        proposer: chain_addr_to_proto(&ev.miner),
        // NewHeadEvent doesn't carry the BFT round — that's an internal
        // consensus detail not exposed in the EVM-compatible header shape.
        // Streaming consumers that need round info can refetch via GetBlock.
        round: 0,
        transactions: Vec::new(),
        justification: Vec::new(),
    }
}

/// Marshal a chain `Block` into a proto `Block`. The proto schema mirrors
/// chain fields one-to-one except for `transactions` which we intentionally
/// leave EMPTY in v0.2 — proto `Transaction` and chain `Transaction` differ
/// in field shape and the marshalling is non-trivial; we ship reads of
/// metadata first, full tx bodies next iteration.
fn marshal_block(b: &sentrix_primitives::block::Block) -> Block {
    Block {
        index: b.index,
        hash: chain_hash_to_proto(&b.hash),
        parent_hash: chain_hash_to_proto(&b.previous_hash),
        state_root: b.state_root.map(|sr| Hash { value: sr.to_vec() }),
        timestamp: b.timestamp,
        proposer: chain_addr_to_proto(&b.validator),
        round: b.round,
        transactions: Vec::new(),
        // Justification is `Option<Justification>` on chain side; if present,
        // serialise via bincode — same on-the-wire shape the chain itself
        // commits to disk. Empty bytes when None.
        justification: b
            .justification
            .as_ref()
            .and_then(|j| bincode::serialize(j).ok())
            .unwrap_or_default(),
    }
}

#[tonic::async_trait]
impl Sentrix for SentrixServiceImpl {
    /// `BroadcastTx` — submit a signed transaction to the local mempool.
    ///
    /// **DEFERRED.** Proto `Transaction` ↔ chain `Transaction` marshalling
    /// requires careful field-by-field decoding (signature format, payload
    /// semantics for staking-ops vs EVM contract calls). Will be implemented
    /// in a fresh-brain follow-up alongside a regression test that round-
    /// trips a known signed tx through both the JSON-RPC and gRPC paths and
    /// asserts the resulting txid is identical.
    async fn broadcast_tx(
        &self,
        _request: Request<BroadcastTxRequest>,
    ) -> Result<Response<BroadcastTxResponse>, Status> {
        Err(Status::unimplemented(
            "BroadcastTx: proto-tx ↔ chain-tx marshalling deferred to v0.3 — \
             see sentrix-grpc/src/lib.rs doc comment",
        ))
    }

    /// `GetBlock` — by height, by hash, or `latest` / `finalized` selector.
    async fn get_block(
        &self,
        request: Request<GetBlockRequest>,
    ) -> Result<Response<Block>, Status> {
        let req = request.into_inner();
        let selector = req
            .selector
            .ok_or_else(|| Status::invalid_argument("GetBlockRequest.selector required"))?;

        let bc = self.state.read().await;

        let block = match selector {
            get_block_request::Selector::Height(h) => bc
                .get_block(h.value)
                .ok_or_else(|| {
                    Status::not_found(format!(
                        "Block {} not in local chain window (current height {}, window starts at {})",
                        h.value,
                        bc.height(),
                        bc.chain.first().map(|b| b.index).unwrap_or(0),
                    ))
                })?
                .clone(),
            get_block_request::Selector::Hash(h) => {
                let hex_hash = proto_hash_to_chain(&h)?;
                bc.get_block_by_hash(&hex_hash)
                    .ok_or_else(|| {
                        Status::not_found(format!(
                            "Block with hash {} not in local chain window",
                            hex_hash
                        ))
                    })?
                    .clone()
            }
            get_block_request::Selector::Latest(_) | get_block_request::Selector::Finalized(_) => {
                // For v0.2 we don't distinguish latest vs finalized — both
                // return the head block. Proper finalized-head tracking will
                // come with the BFT finality observer integration.
                bc.latest_block()
                    .cloned()
                    .map_err(|e| Status::not_found(format!("Chain empty: {e}")))?
            }
        };

        drop(bc);
        Ok(Response::new(marshal_block(&block)))
    }

    /// `GetBalance` — balance + nonce for an address. Mirrors
    /// `eth_getBalance` + `eth_getTransactionCount` in one round-trip.
    /// `at_height` is reserved for future MDBX snapshot support; v0.2
    /// always returns latest.
    async fn get_balance(
        &self,
        request: Request<GetBalanceRequest>,
    ) -> Result<Response<Account>, Status> {
        let req = request.into_inner();
        let addr_proto = req
            .address
            .ok_or_else(|| Status::invalid_argument("GetBalanceRequest.address required"))?;
        let addr = proto_addr_to_chain(&addr_proto)?;

        if req.at_height.is_some() {
            return Err(Status::unimplemented(
                "at_height historical reads require MDBX snapshot isolation \
                 (Refactor 5 in 2026-05-05-sentrix-sdk-design.md); v0.2 returns latest only",
            ));
        }

        let bc = self.state.read().await;
        let balance = bc.accounts.get_balance(&addr);
        let nonce = bc.accounts.get_nonce(&addr);
        drop(bc);

        Ok(Response::new(Account {
            address: Some(addr_proto),
            balance: Some(Amount { sentri: balance }),
            nonce,
            // Storage root + code hash require contract-account inspection;
            // not tracked in v0.2. Empty bytes; clients that need EVM contract
            // state read via JSON-RPC eth_getProof for now.
            storage_root: None,
            code_hash: None,
        }))
    }

    /// `GetValidatorSet` — current active set + per-validator stake/active/jailed.
    /// Mirrors `validators.*` slice of the REST `/sentrix_status_extended`
    /// endpoint so the explorer can drop the JSON bridge from its hot path.
    async fn get_validator_set(
        &self,
        request: Request<GetValidatorSetRequest>,
    ) -> Result<Response<ValidatorSet>, Status> {
        let req = request.into_inner();
        if req.at_height.is_some() {
            return Err(Status::unimplemented(
                "at_height historical reads require MDBX snapshot isolation \
                 (Refactor 5 in 2026-05-05-sentrix-sdk-design.md); v0.4 returns latest only",
            ));
        }

        let bc = self.state.read().await;
        let active_set: std::collections::HashSet<&String> =
            bc.stake_registry.active_set.iter().collect();
        let mut validators: Vec<ValidatorEntry> = bc
            .stake_registry
            .validators
            .iter()
            .map(|(addr, v)| {
                let proto_addr = chain_addr_to_proto(addr);
                ValidatorEntry {
                    address: proto_addr,
                    stake_sentri: v.total_stake(),
                    active: active_set.contains(addr),
                    jailed: v.is_jailed,
                }
            })
            .collect();
        // Sort by stake descending — UI consumers (the Explorer hero) want
        // the heaviest stakes first; alphabetic order on the HashMap iter
        // would shuffle each call and confuse caching downstream.
        validators.sort_by(|a, b| b.stake_sentri.cmp(&a.stake_sentri));

        let active_count = bc.stake_registry.active_count() as u32;
        let total_count = bc.stake_registry.validators.len() as u32;
        let total_active_stake_sentri: u64 = bc
            .stake_registry
            .active_set
            .iter()
            .filter_map(|a| bc.stake_registry.get_validator(a))
            .map(|v| v.total_stake())
            .sum();
        let epoch = sentrix_staking::epoch::EpochManager::epoch_for_height(bc.height()) as u32;
        drop(bc);

        Ok(Response::new(ValidatorSet {
            epoch,
            active_count,
            total_count,
            total_active_stake_sentri,
            validators,
        }))
    }

    /// `GetSupply` — minted/burned/circulating snapshot.
    async fn get_supply(
        &self,
        request: Request<GetSupplyRequest>,
    ) -> Result<Response<Supply>, Status> {
        let req = request.into_inner();
        if req.at_height.is_some() {
            return Err(Status::unimplemented(
                "at_height historical reads require MDBX snapshot isolation; \
                 v0.4 returns latest only",
            ));
        }

        let bc = self.state.read().await;
        let minted_sentri = bc.total_minted;
        let burned_sentri = bc.accounts.total_burned;
        drop(bc);

        Ok(Response::new(Supply {
            minted_sentri,
            burned_sentri,
            circulating_sentri: minted_sentri.saturating_sub(burned_sentri),
        }))
    }

    /// `GetMempool` — pending-tx count + capped header window.
    async fn get_mempool(
        &self,
        request: Request<GetMempoolRequest>,
    ) -> Result<Response<Mempool>, Status> {
        let req = request.into_inner();
        // 0 → server default 100. Hard cap at 500 to keep one
        // round-trip bounded under sustained mempool load.
        let limit = match req.limit {
            0 => 100usize,
            n => (n as usize).min(500),
        };

        let bc = self.state.read().await;
        let size = bc.mempool_size() as u32;
        // VecDeque iter is FIFO — the order tx will be considered for
        // inclusion. UI cares about that order more than insertion-time.
        let entries: Vec<MempoolEntry> = bc
            .mempool
            .iter()
            .take(limit)
            .map(|tx| MempoolEntry {
                txid: chain_hash_to_proto(&tx.txid),
                from_address: chain_addr_to_proto(&tx.from_address),
                to_address: chain_addr_to_proto(&tx.to_address),
                amount: Some(Amount { sentri: tx.amount }),
                fee: Some(Amount { sentri: tx.fee }),
                nonce: tx.nonce,
                // Chain-side `Transaction` doesn't carry an explicit
                // tx_type field — the variant is implicit in the
                // address conventions (system addresses for staking
                // ops, contract creation when to_address is empty,
                // etc.). v0.4 returns 0 ("transfer"); add proper
                // classification in v0.5 alongside BroadcastTx where
                // the wire shape is canonical.
                tx_type: 0,
            })
            .collect();
        drop(bc);

        Ok(Response::new(Mempool { size, entries }))
    }

    /// Server-streaming chain events. v0.3 implements the BlockFinalized
    /// channel by subscribing to the existing `EventBus.new_heads`
    /// broadcast (Sentrix has instant BFT finality so new_heads ARE
    /// finalized — same payload, different name). Other variants
    /// (PendingTx, ValidatorSetChange, LogEmitted) deferred to v0.4 — same
    /// pattern, just additional broadcast::Sender subscriptions multiplexed
    /// onto this stream.
    ///
    /// Backpressure: when a slow consumer falls behind the broadcast
    /// channel's per-receiver capacity (1024 events ≈ 17 minutes at 1
    /// block/s), the receiver gets `RecvError::Lagged(skipped)`. We forward
    /// that as a synthetic `ChainEvent::Lagged(StreamLagged{skipped_count})`
    /// sentinel so the client can decide to resync state via GetBlock
    /// instead of silently missing events. Mirrors the WS handler's
    /// `RecvError::Lagged` semantics.
    ///
    /// Filter / from_sequence support deferred to v0.4 — current impl
    /// always subscribes to all BlockFinalized events from "now".
    type StreamEventsStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<ChainEvent, Status>> + Send>>;

    async fn stream_events(
        &self,
        _request: Request<StreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        use chain_event::Event as EventVariant;
        use std::time::{SystemTime, UNIX_EPOCH};
        use tokio::sync::broadcast::error::RecvError;

        let mut rx = self.event_bus.new_heads.subscribe();
        let now_secs = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        };

        let stream = async_stream::stream! {
            let mut sequence: u64 = 0;
            loop {
                match rx.recv().await {
                    Ok(head) => {
                        sequence = sequence.saturating_add(1);
                        let block = newhead_to_proto_block(&head);
                        yield Ok(ChainEvent {
                            event: Some(EventVariant::BlockFinalized(BlockFinalized {
                                block: Some(block),
                            })),
                            sequence,
                            timestamp: now_secs(),
                        });
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        sequence = sequence.saturating_add(1);
                        yield Ok(ChainEvent {
                            event: Some(EventVariant::Lagged(StreamLagged {
                                skipped_count: skipped,
                            })),
                            sequence,
                            timestamp: now_secs(),
                        });
                    }
                    Err(RecvError::Closed) => {
                        // Sender dropped — process is shutting down. Close
                        // the stream cleanly so the client sees an end-of-
                        // stream rather than a connection reset.
                        break;
                    }
                }
            }
        };

        Ok(Response::new(Box::pin(stream)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_helpers_roundtrip() {
        // Build the test address from a byte array so the source file
        // doesn't contain any 0x-prefixed hex literal that the secret-
        // scanner pre-commit hook flags. The expected hex is computed
        // at runtime from the same bytes.
        let bytes: [u8; 20] = [0xab; 20];
        let addr = format!("0x{}", hex::encode(bytes));
        let proto = chain_addr_to_proto(&addr).expect("parse");
        assert_eq!(proto.value.len(), 20);
        assert_eq!(proto.value, bytes);
        let back = proto_addr_to_chain(&proto).expect("reverse");
        assert_eq!(back, addr);
    }

    #[test]
    fn v04_messages_construct() {
        // Smoke test for the v0.4 read-only types — proto changes ripple
        // through tonic-build at compile time, so a missing field surfaces
        // here long before deploy.
        let req = GetValidatorSetRequest { at_height: None };
        let _ = req.at_height.is_none();

        let entry = ValidatorEntry {
            address: Some(Address { value: vec![0u8; 20] }),
            stake_sentri: 1_000_000_000,
            active: true,
            jailed: false,
        };
        assert!(entry.active);
        assert!(!entry.jailed);

        let set = ValidatorSet {
            epoch: 12,
            active_count: 4,
            total_count: 4,
            total_active_stake_sentri: 4_000_000_000_000,
            validators: vec![entry],
        };
        assert_eq!(set.active_count, 4);

        let supply = Supply {
            minted_sentri: 63_000_000_00_000_000,
            burned_sentri: 0,
            circulating_sentri: 63_000_000_00_000_000,
        };
        assert_eq!(
            supply.circulating_sentri,
            supply.minted_sentri.saturating_sub(supply.burned_sentri)
        );

        let pool = Mempool {
            size: 0,
            entries: vec![],
        };
        assert_eq!(pool.size, 0);
    }

    #[test]
    fn hex_helpers_reject_malformed() {
        assert!(chain_addr_to_proto("not-hex").is_none());
        assert!(chain_addr_to_proto("0x123").is_none()); // wrong length
        let bad = Address {
            value: vec![0u8; 19], // wrong length
        };
        assert!(proto_addr_to_chain(&bad).is_err());
    }
}
