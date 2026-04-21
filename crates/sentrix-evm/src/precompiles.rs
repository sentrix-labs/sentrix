// evm/precompiles.rs — back-compat re-export shim.
//
// Precompile address constants + `is_sentrix_precompile` moved to the
// dedicated `sentrix-precompiles` crate (2026-04-22 per CRATE_SPLIT_PLAN.md
// Tier 2 #9). This shim keeps existing `sentrix_evm::precompiles::*`
// import paths working. Follow-up PR will migrate call sites to
// `use sentrix_precompiles::*` directly and delete this file.

pub use sentrix_precompiles::{SLASHING_PRECOMPILE, STAKING_PRECOMPILE, is_sentrix_precompile};
