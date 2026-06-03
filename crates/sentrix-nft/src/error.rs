//! Typed errors for the Sentrix Native NFT domain.
//!
//! Pure domain errors — no I/O, no storage, no consensus. `sentrix-core`
//! converts these into its own `SentrixError` at the integration boundary
//! via a `From` impl on its side.

use thiserror::Error;

/// Result alias for NFT domain operations.
pub type NftResult<T> = Result<T, NftError>;

/// Every way an NFT domain operation can fail.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NftError {
    /// Caller is not permitted to perform the action.
    #[error("unauthorized: {0}")]
    Unauthorized(String),

    /// A token with this id already exists (live or burned tombstone).
    /// Burned ids are never reusable — strict no-reuse for a clean audit trail.
    #[error("token_id {0} already exists (ids are never reused after burn)")]
    TokenAlreadyExists(u64),

    /// No live token with this id.
    #[error("token_id {0} not found")]
    TokenNotFound(u64),

    /// The claimed `from` is not the current owner of the token.
    #[error("token_id {token_id}: {claimed} is not the owner")]
    NotOwner {
        /// Token whose ownership was asserted.
        token_id: u64,
        /// The address the caller claimed owned it.
        claimed: String,
    },

    /// Token is soulbound (non-transferable). No approval can bypass this.
    #[error("token_id {0} is soulbound (non-transferable)")]
    NotTransferable(u64),

    /// Token is individually frozen.
    #[error("token_id {0} is frozen")]
    TokenFrozen(u64),

    /// The whole collection is frozen.
    #[error("collection is frozen")]
    CollectionFrozen,

    /// Collection metadata is locked (immutable).
    #[error("collection metadata is locked")]
    MetadataLocked,

    /// Minting would exceed the collection's max supply (counts ever-minted,
    /// so burning does not free a slot).
    #[error("max supply reached: {minted} minted, max {max}")]
    MaxSupplyReached {
        /// Total ever minted in this collection.
        minted: u64,
        /// Configured maximum.
        max: u64,
    },

    /// A parameter failed a basic validity check.
    #[error("invalid params: {0}")]
    InvalidParams(String),

    /// A collection with this id/address already exists.
    #[error("collection {0} already exists")]
    CollectionExists(String),

    /// No collection with this id/address.
    #[error("collection {0} not found")]
    CollectionNotFound(String),

    /// A counter overflowed (supply or balance).
    #[error("arithmetic overflow: {0}")]
    Overflow(String),
}
