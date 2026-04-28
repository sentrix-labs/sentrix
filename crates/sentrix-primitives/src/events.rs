//! Event emitter trait — abstraction for chain-event subscribers.
//!
//! Defined in `sentrix-primitives` so `sentrix-core` can hold an
//! `Option<Arc<dyn EventEmitter>>` without depending on tokio or any
//! async runtime. The concrete implementation (`EventBus`) lives in
//! `sentrix-rpc` where the WebSocket subscription machinery owns the
//! tokio broadcast channels.
//!
//! Pattern: dependency inversion — core defines the trait, RPC layer
//! implements it. Core code calls trait methods after every consensus-
//! finalized block; the RPC layer routes those calls to broadcast
//! channels that WebSocket subscribers listen on.
//!
//! All methods are non-blocking + infallible (errors must NOT propagate
//! to consensus). A failed broadcast (e.g. no receivers) is silently
//! dropped — block production must never depend on subscriber liveness.

use crate::block::Block;
use std::sync::Arc;

/// Trait implemented by the WebSocket / SSE event-bus to receive
/// notifications from consensus on chain events. Held as
/// `Option<Arc<dyn EventEmitter>>` on `Blockchain`; default `None`
/// means events are not emitted (e.g. tests, lightweight CLI tools).
///
/// Implementors MUST be Send + Sync + Debug — Debug because
/// `Blockchain` derives `Debug` and we need the trait object to
/// participate in that derive. Send + Sync because the bus is
/// shared across async tasks. tokio::sync::broadcast::Sender
/// already satisfies all three.
pub trait EventEmitter: Send + Sync + std::fmt::Debug {
    /// Called after every successfully-applied block (post chain.push).
    /// Subscribers to `newHeads` get a notification with the block
    /// header. The full block is passed for flexibility — the bus
    /// decides which fields to project into the event payload.
    fn emit_new_head(&self, block: &Block);
}

/// No-op emitter used as the default — useful for tests and any
/// non-RPC binary that doesn't need event emission. Avoids the
/// Option-unwrap pattern at every call site in core.
#[derive(Debug)]
pub struct NoopEmitter;

impl EventEmitter for NoopEmitter {
    fn emit_new_head(&self, _block: &Block) {}
}

/// Convenience: shared-pointer alias for the trait object.
pub type SharedEmitter = Arc<dyn EventEmitter>;
