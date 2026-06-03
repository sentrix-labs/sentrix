// nft.rs - Sentrix — native NFT facade.
//
// The NFT domain logic lives in the standalone `sentrix-nft` crate (pure,
// no storage/consensus). This module re-exports it so existing
// `crate::nft::*` paths keep resolving, and bridges `sentrix_nft::NftError`
// into the workspace `SentrixError` so block-execution handlers (future
// work) can use `?` on NFT operations.
//
// Pure NFT logic was moved out of sentrix-core into sentrix-nft on
// 2026-06-03 so the Proof Asset layer is auditable in isolation and cannot
// reach into block/state/consensus internals.

pub use sentrix_nft::{
    NftCollection, NftError, NftEvent, NftRegistry, NftResult, NftToken, compute_collection_id,
};

use sentrix_primitives::error::SentrixError;

/// Map a pure NFT domain error onto the workspace error type at the
/// sentrix-core boundary. Authorization failures become
/// `UnauthorizedValidator`; arithmetic overflows become `Internal`;
/// everything else is an `InvalidTransaction` carrying the typed message.
///
/// A free function rather than a `From` impl because both `NftError` and
/// `SentrixError` are foreign to this crate (orphan rule). Future NFT
/// block-execution handlers convert with `.map_err(nft_err_to_sentrix)`.
pub fn nft_err_to_sentrix(e: NftError) -> SentrixError {
    match e {
        NftError::Unauthorized(_) => SentrixError::UnauthorizedValidator(e.to_string()),
        NftError::Overflow(_) => SentrixError::Internal(e.to_string()),
        other => SentrixError::InvalidTransaction(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nft_error_maps_to_sentrix_error() {
        assert!(matches!(
            nft_err_to_sentrix(NftError::Unauthorized("nope".into())),
            SentrixError::UnauthorizedValidator(_)
        ));
        assert!(matches!(
            nft_err_to_sentrix(NftError::Overflow("balance".into())),
            SentrixError::Internal(_)
        ));
        assert!(matches!(
            nft_err_to_sentrix(NftError::TokenNotFound(7)),
            SentrixError::InvalidTransaction(_)
        ));
    }

    #[test]
    fn facade_reexports_usable() {
        // The re-exported registry works through the facade path.
        let mut reg = NftRegistry::new();
        let (id, _) = reg
            .deploy_collection("0xa", "C", "C", "u", None, true, true, "tx")
            .unwrap();
        assert!(reg.collection_exists(&id));
    }
}
