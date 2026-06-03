//! Pure NFT event types.
//!
//! Each state-changing operation returns one [`NftEvent`]. These are plain
//! data — `sentrix-core` decides whether/how to emit them into block logs.
//! Nothing here touches the event bus, RPC, or storage.

use serde::{Deserialize, Serialize};

/// Domain events emitted by NFT state transitions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NftEvent {
    /// A new collection was created.
    CollectionCreated {
        /// Collection id/address.
        collection_id: String,
        /// Address that created (and by default administers) it.
        creator: String,
        /// Human-readable name.
        name: String,
        /// Ticker symbol.
        symbol: String,
    },
    /// Collection-level metadata (description / base_uri / external_url) changed.
    CollectionUpdated {
        /// Collection id/address.
        collection_id: String,
    },
    /// The collection was frozen (all transfers now fail).
    CollectionFrozen {
        /// Collection id/address.
        collection_id: String,
    },
    /// Collection metadata was locked (token URIs can no longer change).
    CollectionMetadataLocked {
        /// Collection id/address.
        collection_id: String,
    },
    /// A token was minted.
    TokenMinted {
        /// Collection id/address.
        collection_id: String,
        /// Minted token id.
        token_id: u64,
        /// Recipient / first owner.
        to: String,
        /// Whether the token can ever be transferred (`false` = soulbound).
        transferable: bool,
    },
    /// A token changed owner.
    TokenTransferred {
        /// Collection id/address.
        collection_id: String,
        /// Token id.
        token_id: u64,
        /// Previous owner.
        from: String,
        /// New owner.
        to: String,
    },
    /// A token was burned (tombstoned; its id is never reusable).
    TokenBurned {
        /// Collection id/address.
        collection_id: String,
        /// Token id.
        token_id: u64,
        /// Owner at burn time.
        owner: String,
    },
    /// A single-token approval was set or cleared (`approved` empty = cleared).
    TokenApproved {
        /// Collection id/address.
        collection_id: String,
        /// Token id.
        token_id: u64,
        /// Owner granting the approval.
        owner: String,
        /// Approved spender (empty string means the approval was cleared).
        approved: String,
    },
    /// An operator-for-all approval was set or revoked.
    ApprovalForAll {
        /// Collection id/address.
        collection_id: String,
        /// Token owner.
        owner: String,
        /// Operator.
        operator: String,
        /// Whether the operator is now approved.
        approved: bool,
    },
    /// A token's metadata URI was updated.
    TokenUriUpdated {
        /// Collection id/address.
        collection_id: String,
        /// Token id.
        token_id: u64,
    },
    /// A token was individually frozen.
    TokenFrozen {
        /// Collection id/address.
        collection_id: String,
        /// Token id.
        token_id: u64,
    },
    /// A token was individually unfrozen.
    TokenUnfrozen {
        /// Collection id/address.
        collection_id: String,
        /// Token id.
        token_id: u64,
    },
}
