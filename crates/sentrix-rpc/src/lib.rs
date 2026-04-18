//! sentrix-rpc — REST API, JSON-RPC, and block explorer for Sentrix blockchain.

#![allow(missing_docs)]

pub mod explorer;
pub mod jsonrpc;
pub mod routes;

pub use routes::{SharedState, create_router};
