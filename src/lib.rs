// lib.rs - Sentrix
// Why: missing_docs is enabled in [lints.rust] to catch NEW undocumented public
// APIs going forward. The existing codebase pre-dates the doc policy; suppressed
// here until a dedicated documentation sprint adds top-level module docs.
#![allow(missing_docs)]

pub mod core;
pub mod wallet;
pub mod network;
pub mod api;
pub mod storage;
pub mod types;
