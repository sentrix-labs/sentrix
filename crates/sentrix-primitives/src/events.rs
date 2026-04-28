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

/// Minimal log payload — passed across the trait boundary so
/// `sentrix-primitives` doesn't need to depend on `sentrix-evm`.
/// The concrete bus converts `StoredLog` (in sentrix-evm) into this
/// shape at the emit site.
#[derive(Debug, Clone)]
pub struct LogData {
    pub block_height: u64,
    pub block_hash: String,
    pub tx_hash: String,
    pub tx_index: u32,
    pub log_index: u32,
    pub address: [u8; 20],
    pub topics: Vec<[u8; 32]>,
    pub data: Vec<u8>,
}

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
///
/// All emit_* methods MUST be non-blocking + infallible. Block
/// production must NEVER depend on subscriber liveness — a failed
/// broadcast (no receivers) is silently dropped.
///
/// All methods except `emit_new_head` have empty default impls so
/// implementors can ship channels incrementally without breaking
/// older callers. Phase 1 (newHeads) shipped 2026-04-28; Phase 2
/// (logs + pending_tx) + Phase 3 (sentrix_*) follow.
pub trait EventEmitter: Send + Sync + std::fmt::Debug {
    /// Called after every successfully-applied block (post chain.push).
    /// Subscribers to `eth_subscribe(newHeads)` get a notification
    /// with the block header.
    fn emit_new_head(&self, block: &Block);

    /// Called once per EVM log emitted within a block. Subscribers
    /// to `eth_subscribe(logs)` filter by address + topics and
    /// forward matching logs.
    fn emit_log(&self, _log: &LogData) {}

    /// Called when a tx is admitted to the mempool. Subscribers to
    /// `eth_subscribe(newPendingTransactions)` get the txid string.
    /// Note: this fires on admission only, NOT on rejection.
    fn emit_pending_tx(&self, _txid: &str) {}

    /// Sentrix-native: called after every BFT-finalized block.
    /// Equivalent to `emit_new_head` on the protocol-native side
    /// but exposes finalization-specific fields (justification
    /// signer count) that aren't in the EVM-compat header.
    fn emit_finalized(&self, _height: u64, _hash: &str, _justification_signers: usize) {}

    /// Sentrix-native: called at epoch boundary when the validator
    /// set rotates. Subscribers see the new active set.
    fn emit_validator_set(&self, _epoch: u64, _validators: &[String]) {}
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
