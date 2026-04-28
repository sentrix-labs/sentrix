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
use sentrix_primitives::events::{
    EventEmitter, JailEvent as PrimJailEvent, LogData, StakingOpEvent as PrimStakingOpEvent,
    TokenOpEvent as PrimTokenOpEvent,
};
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

/// Filterable EVM log event emitted on `eth_subscribe(logs)`. Mirrors
/// Ethereum's standard `eth_subscription` log payload — address,
/// topics, data, plus block + tx context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEvent {
    pub address: String, // 0x + 40 hex
    pub topics: Vec<String>, // 0x + 64 hex each
    pub data: String, // 0x + hex
    #[serde(rename = "blockNumber")]
    pub block_number: String, // hex u64
    #[serde(rename = "blockHash")]
    pub block_hash: String,
    #[serde(rename = "transactionHash")]
    pub transaction_hash: String,
    #[serde(rename = "transactionIndex")]
    pub transaction_index: String, // hex u32
    #[serde(rename = "logIndex")]
    pub log_index: String, // hex u32
    pub removed: bool,
}

impl LogEvent {
    pub fn from_log_data(log: &LogData) -> Self {
        let with_0x = |s: &str| {
            if s.starts_with("0x") {
                s.to_string()
            } else {
                format!("0x{}", s)
            }
        };
        Self {
            address: format!("0x{}", hex::encode(log.address)),
            topics: log
                .topics
                .iter()
                .map(|t| format!("0x{}", hex::encode(t)))
                .collect(),
            data: format!("0x{}", hex::encode(&log.data)),
            block_number: format!("0x{:x}", log.block_height),
            block_hash: with_0x(&log.block_hash),
            transaction_hash: with_0x(&log.tx_hash),
            transaction_index: format!("0x{:x}", log.tx_index),
            log_index: format!("0x{:x}", log.log_index),
            removed: false,
        }
    }
}

/// Pending-transaction event emitted on `eth_subscribe(newPendingTransactions)`.
/// Standard Ethereum payload is just the txid string; we keep that exact
/// shape so dApp tooling consumes without special-casing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingTxEvent {
    pub txid: String,
}

/// Sentrix-native: emitted on `sentrix_subscribe(finalized)`. Reports
/// BFT finalization with justification signer count. Distinct from
/// `newHeads` because Sentrix has instant BFT finality — every new
/// block is finalized at the same moment it's produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalizedEvent {
    pub height: u64,
    pub hash: String,
    #[serde(rename = "justificationSigners")]
    pub justification_signers: usize,
}

/// Sentrix-native: emitted on `sentrix_subscribe(validatorSet)`.
/// Fires at epoch boundary when the validator set rotates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorSetEvent {
    pub epoch: u64,
    pub validators: Vec<String>,
}

/// Sentrix-native: emitted on `sentrix_subscribe(tokenOps)`. Fires
/// after every successfully-applied native TokenOp (SRC-20/721/1155).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenOpEvent {
    pub op: String,
    pub contract: String,
    pub from: String,
    pub to: String,
    pub amount: u64,
    pub txid: String,
    #[serde(rename = "blockHeight")]
    pub block_height: u64,
}

/// Sentrix-native: emitted on `sentrix_subscribe(stakingOps)`. Fires
/// after every successfully-applied StakingOp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StakingOpEvent {
    pub op: String,
    pub validator: String,
    pub delegator: String,
    pub amount: u64,
    pub txid: String,
    #[serde(rename = "blockHeight")]
    pub block_height: u64,
}

/// Sentrix-native: emitted on `sentrix_subscribe(jail)`. Fires only
/// post-fork (`JAIL_CONSENSUS_HEIGHT` active) when JailEvidenceBundle
/// dispatch produces a jail decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JailEvent {
    pub validator: String,
    pub epoch: u64,
    #[serde(rename = "missedBlocks")]
    pub missed_blocks: u64,
    #[serde(rename = "blockHeight")]
    pub block_height: u64,
}

/// Concrete event bus. Held as `Arc<EventBus>` and shared between
/// the consensus path (which calls `emit_*` methods) and the
/// WebSocket subscription handlers (which call `<channel>.subscribe()`
/// to obtain a Receiver).
#[derive(Debug, Clone)]
pub struct EventBus {
    pub new_heads: broadcast::Sender<NewHeadEvent>,
    pub logs: broadcast::Sender<LogEvent>,
    pub pending_txs: broadcast::Sender<PendingTxEvent>,
    pub finalized: broadcast::Sender<FinalizedEvent>,
    pub validator_set: broadcast::Sender<ValidatorSetEvent>,
    pub token_ops: broadcast::Sender<TokenOpEvent>,
    pub staking_ops: broadcast::Sender<StakingOpEvent>,
    pub jail: broadcast::Sender<JailEvent>,
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
            logs: broadcast::channel(capacity).0,
            pending_txs: broadcast::channel(capacity).0,
            finalized: broadcast::channel(capacity).0,
            validator_set: broadcast::channel(capacity).0,
            token_ops: broadcast::channel(capacity).0,
            staking_ops: broadcast::channel(capacity).0,
            jail: broadcast::channel(capacity).0,
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

    fn emit_log(&self, log: &LogData) {
        let _ = self.logs.send(LogEvent::from_log_data(log));
    }

    fn emit_pending_tx(&self, txid: &str) {
        let _ = self.pending_txs.send(PendingTxEvent {
            txid: if txid.starts_with("0x") {
                txid.to_string()
            } else {
                format!("0x{}", txid)
            },
        });
    }

    fn emit_finalized(&self, height: u64, hash: &str, justification_signers: usize) {
        let with_0x = if hash.starts_with("0x") {
            hash.to_string()
        } else {
            format!("0x{}", hash)
        };
        let _ = self.finalized.send(FinalizedEvent {
            height,
            hash: with_0x,
            justification_signers,
        });
    }

    fn emit_validator_set(&self, epoch: u64, validators: &[String]) {
        let _ = self.validator_set.send(ValidatorSetEvent {
            epoch,
            validators: validators.to_vec(),
        });
    }

    fn emit_token_op(&self, ev: &PrimTokenOpEvent) {
        let _ = self.token_ops.send(TokenOpEvent {
            op: ev.op.clone(),
            contract: ev.contract.clone(),
            from: ev.from.clone(),
            to: ev.to.clone(),
            amount: ev.amount,
            txid: ev.txid.clone(),
            block_height: ev.block_height,
        });
    }

    fn emit_staking_op(&self, ev: &PrimStakingOpEvent) {
        let _ = self.staking_ops.send(StakingOpEvent {
            op: ev.op.clone(),
            validator: ev.validator.clone(),
            delegator: ev.delegator.clone(),
            amount: ev.amount,
            txid: ev.txid.clone(),
            block_height: ev.block_height,
        });
    }

    fn emit_jail(&self, ev: &PrimJailEvent) {
        let _ = self.jail.send(JailEvent {
            validator: ev.validator.clone(),
            epoch: ev.epoch,
            missed_blocks: ev.missed_blocks,
            block_height: ev.block_height,
        });
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
