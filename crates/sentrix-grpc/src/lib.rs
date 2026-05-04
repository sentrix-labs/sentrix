//! Sentrix gRPC supplement transport (Tonic).
//!
//! Parallel to the JSON-RPC `eth_*` interface served at port 8545 — same
//! backend, same state, different wire format. JSON-RPC remains the
//! ecosystem-facing contract for wallets and dApps; gRPC is for SentrisCloud
//! internal monitoring and power-user clients that prefer binary protocols.
//!
//! ## Status (2026-05-05 v0.1)
//!
//! Skeleton crate. Service handlers return [`tonic::Status::unimplemented`]
//! until `bin/sentrix/src/main.rs` is updated to spawn a Tonic server next
//! to the existing axum HTTP server and pass it the shared `Blockchain`
//! state. That integration step is fork-gate-free (read-only handlers use
//! the same `Arc<RwLock<Blockchain>>` as the JSON-RPC handlers; the
//! `BroadcastTx` handler calls into the same `add_to_mempool` path) but
//! requires care to avoid touching the chain binary mid-marathon — see
//! the design doc at `founder-private/audits/2026-05-05-grpc-service-proto-draft.md`
//! for the sequenced rollout plan.
//!
//! ## Concurrency
//!
//! Handlers are `async`. The bridge to the validator's existing channels
//! uses `tokio::sync::mpsc` and `tokio::sync::broadcast` (see the
//! `StreamEvents` plan). No `std::sync::Mutex` or `std::sync::RwLock` —
//! the chain workspace audit completed 2026-05-05 enforces this discipline
//! across all production code, and this crate is `#![forbid(unsafe_code)]`
//! at the root.

#![forbid(unsafe_code)]

/// Generated types and service stubs from `proto/sentrix.proto`.
pub mod sentrix_proto {
    tonic::include_proto!("sentrix.v1");
}

use sentrix_proto::sentrix_server::{Sentrix, SentrixServer};
use sentrix_proto::*;
use tonic::{Request, Response, Status};

/// Service handler. v0.1 holds no state — every method returns
/// `Status::unimplemented`. The next iteration (post-marathon, fresh-brain)
/// will add a `state: Arc<RwLock<Blockchain>>` field plumbed from
/// `bin/sentrix/src/main.rs`.
#[derive(Default)]
pub struct SentrixService {
    // Future:
    //   shared_state: Arc<tokio::sync::RwLock<sentrix_core::blockchain::Blockchain>>,
    //   event_bus: Arc<sentrix_rpc::events::EventBus>,
    //
    // Why tokio::sync (not std::sync): handlers are async fn that .await on
    // the lock. std::sync::RwLock would block the tokio worker thread —
    // the same wedge class fixed in chain v2.1.65/67/68 by switching the
    // BFT message channels to try_send. The discipline is now codebase-wide
    // (see the comment at crates/sentrix-rpc/src/routes/mod.rs:49 and the
    // audit memo at founder-private/audits/2026-05-05-sentrix-sdk-design.md).
}

#[tonic::async_trait]
impl Sentrix for SentrixService {
    async fn broadcast_tx(
        &self,
        _request: Request<BroadcastTxRequest>,
    ) -> Result<Response<BroadcastTxResponse>, Status> {
        Err(Status::unimplemented(
            "BroadcastTx not yet wired to mempool — see design doc \
             founder-private/audits/2026-05-05-grpc-service-proto-draft.md",
        ))
    }

    async fn get_block(
        &self,
        _request: Request<GetBlockRequest>,
    ) -> Result<Response<Block>, Status> {
        Err(Status::unimplemented(
            "GetBlock not yet wired to chain state",
        ))
    }

    async fn get_balance(
        &self,
        _request: Request<GetBalanceRequest>,
    ) -> Result<Response<Account>, Status> {
        Err(Status::unimplemented(
            "GetBalance not yet wired to chain state",
        ))
    }

    /// Server-streaming type. v0.1 returns an empty pinned stream that
    /// immediately yields `Status::unimplemented`. Real impl will be a
    /// `tokio::sync::broadcast::Receiver<ChainEvent>` adapted via
    /// `tokio_stream::wrappers::BroadcastStream`, mirroring the existing
    /// JSON-RPC WebSocket pattern at `crates/sentrix-rpc/src/ws/mod.rs`
    /// (which also handles `RecvError::Lagged` by emitting a synthetic
    /// `StreamLagged` sentinel).
    type StreamEventsStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<ChainEvent, Status>> + Send>>;

    async fn stream_events(
        &self,
        _request: Request<StreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        Err(Status::unimplemented(
            "StreamEvents not yet wired to event bus",
        ))
    }
}

/// Build a `SentrixServer` instance ready to be served on a tonic transport.
///
/// Caller is responsible for binding to a transport (typically
/// `tonic::transport::Server::builder().add_service(server_factory()).serve(addr)`).
/// This crate INTENTIONALLY does not auto-bind — the bind decision lives
/// in `bin/sentrix/src/main.rs` so the operator can gate it behind
/// `SENTRIX_GRPC_ENABLED=1` and choose the address per host.
pub fn server_factory() -> SentrixServer<SentrixService> {
    SentrixServer::new(SentrixService::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_factory_compiles() {
        let _ = server_factory();
    }

    #[tokio::test]
    async fn handlers_return_unimplemented() {
        let svc = SentrixService::default();
        let req = Request::new(GetBlockRequest { selector: None });
        let res = svc.get_block(req).await;
        assert!(res.is_err());
        assert_eq!(res.unwrap_err().code(), tonic::Code::Unimplemented);
    }
}
