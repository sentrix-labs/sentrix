//! sentrix-rpc — REST API, JSON-RPC, and block explorer for Sentrix blockchain.

#![allow(missing_docs)]

pub mod events;
pub mod explorer;
pub mod explorer_api;
pub mod jsonrpc;
pub mod routes;
pub mod ws;

pub use events::{EventBus, NewHeadEvent};
pub use routes::{SharedState, create_router};
