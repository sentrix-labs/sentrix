// error.rs — Re-export from sentrix-primitives for backward compatibility.
//
// All error types now live in the sentrix-primitives crate. This module
// re-exports them so existing `use crate::types::error::*` imports throughout
// the codebase continue to work without any changes.

pub use sentrix_primitives::error::*;
