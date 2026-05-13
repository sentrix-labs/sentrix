//! Generated tonic + prost types for the Sentrix Chain gRPC service.
//!
//! Single source of truth for the `sentrix.v1` schema. The chain itself
//! consumes these types server-side (`crates/sentrix-grpc`); SDK clients
//! (Rust, WASM, future polyglot) depend on this crate via crates.io to
//! avoid drift between server and clients.
//!
//! ## Example
//!
//! ```no_run
//! use sentrix_proto::sentrix_client::SentrixClient;
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let mut client = SentrixClient::connect("https://grpc.sentrixchain.com").await?;
//! let resp = client
//!     .get_block(sentrix_proto::GetBlockRequest {
//!         selector: Some(sentrix_proto::get_block_request::Selector::Latest(true)),
//!     })
//!     .await?;
//! println!("latest block: {:?}", resp.into_inner().block.unwrap().header);
//! # Ok(())
//! # }
//! ```
//!
//! ## Schema versioning
//!
//! The schema is namespaced `package sentrix.v1;`. Breaking field renames /
//! type changes will go to a `sentrix.v2` package, not a v1 in-place edit —
//! that gives client crates a deterministic boundary for major-version bumps.

#![forbid(unsafe_code)]

tonic::include_proto!("sentrix.v1");
