// node.rs — Sentrix shared P2P type definitions.
//
// History: this file used to host a raw-TCP `Node` plus `Message` enum and
// peer-management machinery. Production switched to libp2p (see
// `libp2p_node.rs`) with PR #82; the legacy TCP path was retained for
// shared types only. The legacy `Node` impl had the same race-induced
// cascade-bail in its BlocksResponse loop as `libp2p_node.rs` did pre-v2.1.37
// (2026-04-26 mainnet stall RCA). Rather than carry parallel dead code with
// a known bug, the TCP path is excised entirely. Only the types still
// referenced by the rest of the workspace remain here.

use sentrix_core::blockchain::Blockchain;
use sentrix_primitives::block::Block;
use sentrix_primitives::transaction::Transaction;
use std::sync::Arc;
use tokio::sync::RwLock;

pub const DEFAULT_PORT: u16 = 30303;

pub type SharedBlockchain = Arc<RwLock<Blockchain>>;

#[derive(Debug)]
pub enum NodeEvent {
    NewBlock(Block),
    NewTransaction(Transaction),
    PeerConnected(String),
    PeerDisconnected(String),
    SyncNeeded { peer_addr: String, peer_height: u64 },
    BftProposal(sentrix_bft::messages::Proposal),
    BftPrevote(sentrix_bft::messages::Prevote),
    BftPrecommit(sentrix_bft::messages::Precommit),
    BftRoundStatus(sentrix_bft::messages::RoundStatus),
}
