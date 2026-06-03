//! The per-token model.
//!
//! A token carries its own transferability and freeze flags so the Proof
//! Asset layer can mint soulbound (non-transferable) badges alongside normal
//! transferable assets in the same collection. Burned tokens are kept as
//! tombstones (`burned = true`) so their ids can never be reused — this keeps
//! the reputation/audit history append-only.

use serde::{Deserialize, Serialize};

/// A single native NFT.
///
/// `owner` holds the last owner even after burn (for audit); read ownership
/// through [`crate::NftCollection::owner_of`], which returns `None` for burned
/// tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NftToken {
    /// Collection this token belongs to.
    pub collection_id: String,
    /// Token id (deterministic integer; never reused after burn).
    pub token_id: u64,
    /// Current owner (or last owner if `burned`).
    pub owner: String,
    /// Explicit metadata URI override. Empty means "derive from the
    /// collection's `base_uri`" (`{base_uri}{token_id}`).
    pub uri: String,
    /// Optional hash of the URI target (integrity pin). Metadata bytes
    /// themselves are never stored on-chain.
    pub uri_hash: Option<String>,
    /// Optional hash of the off-chain metadata document.
    pub metadata_hash: Option<String>,
    /// Whether this token may be transferred. `false` = soulbound.
    pub transferable: bool,
    /// Whether this token is individually frozen (transfers temporarily halted).
    pub frozen: bool,
    /// Whether this token has been burned (tombstone; id permanently retired).
    pub burned: bool,
}

impl NftToken {
    /// Construct a fresh, live token.
    pub fn new(
        collection_id: String,
        token_id: u64,
        owner: String,
        uri: String,
        transferable: bool,
    ) -> Self {
        Self {
            collection_id,
            token_id,
            owner,
            uri,
            uri_hash: None,
            metadata_hash: None,
            transferable,
            frozen: false,
            burned: false,
        }
    }
}
