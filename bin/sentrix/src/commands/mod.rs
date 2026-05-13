//! CLI subcommand implementations.
//!
//! `main.rs` had grown to ~4.8k LOC partly because every `cmd_*`
//! function lived inline beside the clap-derived `Cli` enum and the
//! async runtime bootstrap. The actual node start path (`cmd_start`) is
//! still in `main.rs` — it owns the BFT engine, libp2p swarm, and
//! validator key load, which the orchestration layer needs to drive
//! directly. Everything else moves here, one logical group per file.

pub mod chain;
pub mod misc;
pub mod state;
pub mod validator;
pub mod wallet;
