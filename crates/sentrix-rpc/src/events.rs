//! `EventBus` — concrete implementation of the `EventEmitter` trait
//! defined in `sentrix-primitives::events`.
//!
//! Holds a `tokio::sync::broadcast::Sender` per channel. Subscribers
//! call `bus.new_heads.subscribe()` to get a `Receiver<NewHeadEvent>`.
//! When the consensus path emits via `emit_new_head`, every subscriber
//! receives a copy of the event (or `RecvError::Lagged` if their
//! buffer overflowed).
//!
//! Capacity is configured at construction. broadcast channel drops
//! the OLDEST event when full, returning `Lagged(skipped)` to slow
//! receivers — that's the correct semantic: a slow consumer should
//! not block fast ones, and a slow consumer that's lagged 1024+
//! events behind is no longer "real-time" anyway.

use sentrix_primitives::block::Block;
use sentrix_primitives::events::EventEmitter;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Default channel capacity. broadcast::channel(N) buffers up to N
/// events per receiver before lagging. 1024 is generous for newHeads
/// (1 block/s × 17 min of buffer) and forces clear failure mode for
/// slow consumers.
pub const DEFAULT_BUS_CAPACITY: usize = 1024;

/// Block-header summary emitted on `eth_subscribe(newHeads)`.
/// Mirrors Ethereum's `eth_subscription` payload shape so existing
/// dApp tooling (ethers.js, viem, web3.js) can consume without
/// special-casing Sentrix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewHeadEvent {
    pub number: String,        // hex-encoded u64
    pub hash: String,          // 0x-prefixed
    #[serde(rename = "parentHash")]
    pub parent_hash: String,   // 0x-prefixed
    pub timestamp: String,     // hex-encoded u64
    pub miner: String,         // validator address (lowercase, 0x-prefixed)
    #[serde(rename = "transactionsRoot")]
    pub transactions_root: String,
    #[serde(rename = "stateRoot")]
    pub state_root: String,
    #[serde(rename = "gasLimit")]
    pub gas_limit: String,
    #[serde(rename = "gasUsed")]
    pub gas_used: String,
    pub difficulty: String,
    pub nonce: String,
    #[serde(rename = "extraData")]
    pub extra_data: String,
    pub size: String,
}

impl NewHeadEvent {
    /// Project a `Block` into the EVM-compatible `eth_subscription`
    /// payload shape. Fields not represented in Sentrix's native
    /// model are filled with sensible Ethereum-default constants
    /// (difficulty=0x0, nonce=0x0…0, extraData=0x).
    pub fn from_block(block: &Block) -> Self {
        let to_hex = |n: u64| format!("0x{:x}", n);
        let with_0x = |s: &str| {
            if s.starts_with("0x") {
                s.to_string()
            } else {
                format!("0x{}", s)
            }
        };
        let state_root_hex = match block.state_root {
            Some(root) => format!("0x{}", hex::encode(root)),
            None => "0x0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
        };
        Self {
            number: to_hex(block.index),
            hash: with_0x(&block.hash),
            parent_hash: with_0x(&block.previous_hash),
            timestamp: to_hex(block.timestamp),
            miner: block.validator.clone(),
            transactions_root: with_0x(&block.merkle_root),
            state_root: state_root_hex,
            gas_limit: to_hex(30_000_000),
            gas_used: to_hex(0),
            difficulty: "0x0".to_string(),
            nonce: "0x0000000000000000".to_string(),
            extra_data: "0x".to_string(),
            size: to_hex(1000),
        }
    }
}

/// Concrete event bus. Held as `Arc<EventBus>` and shared between
/// the consensus path (which calls `emit_new_head`) and the
/// WebSocket subscription handlers (which call `new_heads.subscribe()`
/// to obtain a Receiver).
#[derive(Debug, Clone)]
pub struct EventBus {
    pub new_heads: broadcast::Sender<NewHeadEvent>,
}

impl EventBus {
    /// Construct a new bus with the default capacity per channel.
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_BUS_CAPACITY)
    }

    /// Construct with explicit capacity. Tests use small values to
    /// exercise the lagged-receiver path quickly.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            new_heads: broadcast::channel(capacity).0,
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventEmitter for EventBus {
    fn emit_new_head(&self, block: &Block) {
        // broadcast::send returns Err if there are no active receivers,
        // which is fine — we don't want consensus to depend on whether
        // a websocket client is connected. Drop the result.
        let _ = self.new_heads.send(NewHeadEvent::from_block(block));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sentrix_primitives::block::Block;

    fn make_block() -> Block {
        Block {
            index: 42,
            previous_hash: "abcd".to_string(),
            transactions: vec![],
            timestamp: 1234567890,
            merkle_root: "0".repeat(64),
            validator: "0xvalidator".to_string(),
            hash: "1234".to_string(),
            state_root: Some([0u8; 32]),
            round: 0,
            justification: None,
        }
    }

    #[test]
    fn new_head_projects_block_correctly() {
        let block = make_block();
        let event = NewHeadEvent::from_block(&block);
        assert_eq!(event.number, "0x2a"); // 42 in hex
        assert_eq!(event.hash, "0x1234");
        assert_eq!(event.parent_hash, "0xabcd");
        assert_eq!(event.timestamp, "0x499602d2"); // 1234567890
        assert_eq!(event.miner, "0xvalidator");
        assert_eq!(event.gas_limit, "0x1c9c380"); // 30M
        assert!(event.state_root.starts_with("0x"));
        assert_eq!(event.state_root.len(), 66); // 0x + 64 hex
    }

    #[tokio::test]
    async fn emit_new_head_reaches_subscriber() {
        let bus = EventBus::new();
        let mut rx = bus.new_heads.subscribe();
        let block = make_block();
        bus.emit_new_head(&block);
        let event = rx.recv().await.expect("event delivered");
        assert_eq!(event.number, "0x2a");
        assert_eq!(event.hash, "0x1234");
    }

    #[tokio::test]
    async fn emit_with_no_subscribers_is_silent() {
        // Ensure dropping send result doesn't propagate as panic.
        let bus = EventBus::new();
        let block = make_block();
        bus.emit_new_head(&block); // no receivers — must not panic
        bus.emit_new_head(&block);
    }

    #[tokio::test]
    async fn lagged_receiver_returns_lagged_error() {
        // Capacity 2 — emit 5 events, expect the receiver to get Lagged.
        let bus = EventBus::with_capacity(2);
        let mut rx = bus.new_heads.subscribe();
        let block = make_block();
        for _ in 0..5 {
            bus.emit_new_head(&block);
        }
        // First recv should signal Lagged because we overflowed the buffer.
        let res = rx.recv().await;
        assert!(matches!(res, Err(broadcast::error::RecvError::Lagged(_))));
    }
}
