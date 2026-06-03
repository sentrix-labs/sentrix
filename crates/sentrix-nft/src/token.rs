//! The per-token model.
//!
//! A token carries its own transferability and freeze flags so the Proof
//! Asset layer can mint soulbound (non-transferable) badges alongside normal
//! transferable assets in the same collection. Burned tokens are kept as
//! tombstones (`burned = true`) so their ids can never be reused — this keeps
//! the reputation/audit history append-only.
//!
//! Fields are private: the transferability / freeze / burned / owner
//! invariants are enforced by [`crate::NftCollection`]'s methods, so the type
//! must not allow external code to flip them directly. Read via the getters;
//! mutation is `pub(crate)` and only happens inside `collection.rs`.

use serde::{Deserialize, Serialize};

/// A single native NFT.
///
/// `owner` holds the last owner even after burn (for audit); read ownership
/// through [`crate::NftCollection::owner_of`], which returns `None` for burned
/// tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NftToken {
    collection_id: String,
    token_id: u64,
    owner: String,
    uri: String,
    /// Optional integrity hash of the URI target (never media bytes). No
    /// behavioral invariant, so it stays public — unlike owner/transferable/
    /// frozen/burned which gate state transitions and are private.
    pub uri_hash: Option<String>,
    /// Optional hash of the off-chain metadata document. No behavioral
    /// invariant; public like `uri_hash`.
    pub metadata_hash: Option<String>,
    transferable: bool,
    frozen: bool,
    burned: bool,
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

    // ── Getters ──────────────────────────────────────────

    /// Collection this token belongs to.
    pub fn collection_id(&self) -> &str {
        &self.collection_id
    }
    /// Token id.
    pub fn token_id(&self) -> u64 {
        self.token_id
    }
    /// Current owner (or last owner if burned).
    pub fn owner(&self) -> &str {
        &self.owner
    }
    /// Explicit metadata URI override (empty = derive from collection base_uri).
    pub fn uri(&self) -> &str {
        &self.uri
    }
    /// Whether this token may be transferred (`false` = soulbound).
    pub fn transferable(&self) -> bool {
        self.transferable
    }
    /// Whether this token is individually frozen.
    pub fn frozen(&self) -> bool {
        self.frozen
    }
    /// Whether this token has been burned (tombstone; id permanently retired).
    pub fn burned(&self) -> bool {
        self.burned
    }

    // ── Crate-internal mutation (only NftCollection methods) ──

    pub(crate) fn set_owner(&mut self, owner: String) {
        self.owner = owner;
    }
    pub(crate) fn set_uri(&mut self, uri: String) {
        self.uri = uri;
    }
    pub(crate) fn set_frozen(&mut self, frozen: bool) {
        self.frozen = frozen;
    }
    /// Mark the token burned and clear its freeze flag (tombstone).
    pub(crate) fn mark_burned(&mut self) {
        self.burned = true;
        self.frozen = false;
    }
}
