// wallet/mod.rs — Re-export from sentrix-wallet crate for backward compatibility.
//
// All wallet types now live in the sentrix-wallet crate. These re-exports
// ensure existing `use crate::wallet::*` imports work unchanged.

pub use sentrix_wallet::keystore;
#[allow(clippy::module_inception)]
pub use sentrix_wallet::wallet;

// Also re-export at module level for convenience
pub use sentrix_wallet::{Keystore, Wallet};
