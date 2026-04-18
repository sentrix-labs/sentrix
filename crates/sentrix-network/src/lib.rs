//! sentrix-network — P2P networking for Sentrix blockchain.
//!
//! Provides libp2p-based networking: gossipsub for block/tx propagation,
//! kademlia for peer discovery, request-response for sync + BFT messages.

#![allow(missing_docs)]

pub mod behaviour;
pub mod libp2p_node;
pub mod node;
pub mod sync;
pub mod transport;

pub use libp2p_node::{LibP2pNode, make_multiaddr};
pub use node::{NodeEvent, SharedBlockchain, DEFAULT_PORT};
