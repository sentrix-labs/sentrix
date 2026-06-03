//! The collection model and all token state transitions.
//!
//! A [`NftCollection`] owns its tokens, balances, and approvals. Every
//! state-changing method returns an [`NftEvent`] describing what happened, so
//! the integrating layer (`sentrix-core`) can emit it without re-deriving the
//! effect.
//!
//! Determinism: state lives in `HashMap`s. In-memory order is per-process and
//! is never hashed directly; when this state is committed to the state trie
//! (future work in `sentrix-core`), maps are iterated in sorted order.

use crate::error::{NftError, NftResult};
use crate::events::NftEvent;
use crate::token::NftToken;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A native NFT collection (the Proof Asset issuer unit).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NftCollection {
    /// Collection id/address (deterministic; see [`crate::NftRegistry`]).
    pub id: String,
    /// Address that created the collection.
    pub creator: String,
    /// Address with administrative rights (mint, freeze, metadata). Defaults
    /// to `creator` at creation.
    pub admin: String,
    /// Human-readable name.
    pub name: String,
    /// Ticker symbol.
    pub symbol: String,
    /// Optional free-text description (empty = none).
    pub description: String,
    /// Metadata-URI prefix; per-token URI is `{base_uri}{token_id}` unless the
    /// token carries an explicit override.
    pub base_uri: String,
    /// Optional external URL for the collection (empty = none).
    pub external_url: String,
    /// Maximum number of tokens that may ever be minted. `None` = unlimited.
    /// Counted against ever-minted, so burning never frees a slot.
    pub max_supply: Option<u64>,
    /// Count of currently-live (non-burned) tokens.
    pub total_supply: u64,
    /// Monotonic count of tokens ever minted. Backs `max_supply` and the
    /// no-reuse guarantee. Never decreases.
    pub total_minted: u64,
    /// Default transferability applied to mints that don't override it.
    pub default_transferable: bool,
    /// Whether token metadata URIs may be updated after mint.
    pub metadata_mutable: bool,
    /// Whether the whole collection is frozen (blocks all transfers).
    pub frozen: bool,
    /// token_id → token. Includes burned tombstones (never removed) so ids are
    /// never reused.
    pub tokens: HashMap<u64, NftToken>,
    /// owner → count of live tokens held.
    pub balances: HashMap<String, u64>,
    /// token_id → approved spender (single-token approval).
    pub token_approvals: HashMap<u64, String>,
    /// owner → (operator → approved) for operator-for-all.
    pub operator_approvals: HashMap<String, HashMap<String, bool>>,
}

impl NftCollection {
    /// Create a new collection. `admin` defaults to `creator`.
    pub fn new(
        id: String,
        creator: String,
        name: String,
        symbol: String,
        base_uri: String,
        max_supply: Option<u64>,
        default_transferable: bool,
        metadata_mutable: bool,
    ) -> Self {
        Self {
            id,
            admin: creator.clone(),
            creator,
            name,
            symbol,
            description: String::new(),
            base_uri,
            external_url: String::new(),
            max_supply,
            total_supply: 0,
            total_minted: 0,
            default_transferable,
            metadata_mutable,
            frozen: false,
            tokens: HashMap::new(),
            balances: HashMap::new(),
            token_approvals: HashMap::new(),
            operator_approvals: HashMap::new(),
        }
    }

    // ── Reads ────────────────────────────────────────────

    /// Current owner of a live token, or `None` if it doesn't exist or is burned.
    pub fn owner_of(&self, token_id: u64) -> Option<&String> {
        match self.tokens.get(&token_id) {
            Some(t) if !t.burned => Some(&t.owner),
            _ => None,
        }
    }

    /// Number of live tokens `address` holds (0 if none).
    pub fn balance_of(&self, address: &str) -> u64 {
        self.balances.get(address).copied().unwrap_or(0)
    }

    /// Single approved spender for a token, or `None`.
    pub fn get_approved(&self, token_id: u64) -> Option<&String> {
        self.token_approvals.get(&token_id)
    }

    /// Whether `operator` may act on all of `owner`'s tokens.
    pub fn is_approved_for_all(&self, owner: &str, operator: &str) -> bool {
        self.operator_approvals
            .get(owner)
            .and_then(|m| m.get(operator))
            .copied()
            .unwrap_or(false)
    }

    /// Resolved metadata URI: explicit override if set, else the
    /// `{base_uri}{token_id}` convention. Empty for nonexistent/burned tokens.
    pub fn token_uri(&self, token_id: u64) -> String {
        match self.tokens.get(&token_id) {
            Some(t) if !t.burned => {
                if t.uri.is_empty() {
                    format!("{}{}", self.base_uri, token_id)
                } else {
                    t.uri.clone()
                }
            }
            _ => String::new(),
        }
    }

    /// Current live supply.
    pub fn total_supply(&self) -> u64 {
        self.total_supply
    }

    /// Configured maximum supply (`None` = unlimited).
    pub fn max_supply(&self) -> Option<u64> {
        self.max_supply
    }

    /// Borrow a token (including burned tombstones) for inspection.
    pub fn get_token(&self, token_id: u64) -> Option<&NftToken> {
        self.tokens.get(&token_id)
    }

    /// Authorization for transfer: owner, single-token approvee, or operator.
    /// Does NOT consider transferability — soulbound is checked separately and
    /// first, so approval can never bypass it.
    fn transfer_authorized(&self, caller: &str, owner: &str, token_id: u64) -> bool {
        caller == owner
            || self.token_approvals.get(&token_id).map(String::as_str) == Some(caller)
            || self.is_approved_for_all(owner, caller)
    }

    // ── Writes ───────────────────────────────────────────

    /// Mint `token_id` to `to`. Admin-only. `token_id` must never have been
    /// minted before (strict no-reuse). `transferable` overrides the
    /// collection default; pass `None` to use the default.
    pub fn mint(
        &mut self,
        caller: &str,
        to: &str,
        token_id: u64,
        uri: &str,
        transferable: Option<bool>,
    ) -> NftResult<NftEvent> {
        if caller != self.admin {
            return Err(NftError::Unauthorized(
                "only the collection admin can mint".into(),
            ));
        }
        if to.is_empty() {
            return Err(NftError::InvalidParams("empty recipient".into()));
        }
        // Strict no-reuse: any id ever minted (live OR burned tombstone) is taken.
        if self.tokens.contains_key(&token_id) {
            return Err(NftError::TokenAlreadyExists(token_id));
        }
        if let Some(max) = self.max_supply
            && self.total_minted >= max
        {
            return Err(NftError::MaxSupplyReached {
                minted: self.total_minted,
                max,
            });
        }

        let transferable = transferable.unwrap_or(self.default_transferable);
        self.tokens.insert(
            token_id,
            NftToken::new(
                self.id.clone(),
                token_id,
                to.to_string(),
                uri.to_string(),
                transferable,
            ),
        );
        let bal = self.balances.entry(to.to_string()).or_insert(0);
        *bal = bal
            .checked_add(1)
            .ok_or_else(|| NftError::Overflow("balance".into()))?;
        self.total_supply = self
            .total_supply
            .checked_add(1)
            .ok_or_else(|| NftError::Overflow("total_supply".into()))?;
        self.total_minted = self
            .total_minted
            .checked_add(1)
            .ok_or_else(|| NftError::Overflow("total_minted".into()))?;

        Ok(NftEvent::TokenMinted {
            collection_id: self.id.clone(),
            token_id,
            to: to.to_string(),
            transferable,
        })
    }

    /// Transfer a token. Order of checks matters: soulbound is rejected before
    /// authorization so an approved spender cannot move a soulbound token.
    pub fn transfer(
        &mut self,
        caller: &str,
        from: &str,
        to: &str,
        token_id: u64,
    ) -> NftResult<NftEvent> {
        if from.is_empty() || to.is_empty() {
            return Err(NftError::InvalidParams("empty address".into()));
        }
        if from == to {
            return Err(NftError::InvalidParams("cannot transfer to self".into()));
        }
        // Existence (burned tokens read as not-found).
        let token = self.tokens.get(&token_id).filter(|t| !t.burned);
        let token = token.ok_or(NftError::TokenNotFound(token_id))?;
        // Soulbound — checked first; no approval bypasses it.
        if !token.transferable {
            return Err(NftError::NotTransferable(token_id));
        }
        if token.frozen {
            return Err(NftError::TokenFrozen(token_id));
        }
        if self.frozen {
            return Err(NftError::CollectionFrozen);
        }
        if token.owner != from {
            return Err(NftError::NotOwner {
                token_id,
                claimed: from.to_string(),
            });
        }
        if !self.transfer_authorized(caller, from, token_id) {
            return Err(NftError::Unauthorized(format!(
                "{} cannot transfer token_id {}",
                caller, token_id
            )));
        }

        // Apply.
        let from_bal = self.balances.entry(from.to_string()).or_insert(0);
        *from_bal = from_bal
            .checked_sub(1)
            .ok_or_else(|| NftError::Overflow("transfer underflow".into()))?;
        let to_bal = self.balances.entry(to.to_string()).or_insert(0);
        *to_bal = to_bal
            .checked_add(1)
            .ok_or_else(|| NftError::Overflow("transfer overflow".into()))?;
        // Owner update + approval clear.
        if let Some(t) = self.tokens.get_mut(&token_id) {
            t.owner = to.to_string();
        }
        self.token_approvals.remove(&token_id);

        Ok(NftEvent::TokenTransferred {
            collection_id: self.id.clone(),
            token_id,
            from: from.to_string(),
            to: to.to_string(),
        })
    }

    /// Approve a single `spender` for `token_id`. Caller must be the owner or
    /// an operator-for-all. Use [`Self::clear_approval`] to revoke.
    pub fn approve(&mut self, caller: &str, spender: &str, token_id: u64) -> NftResult<NftEvent> {
        if spender.is_empty() {
            return Err(NftError::InvalidParams(
                "empty spender (use clear_approval)".into(),
            ));
        }
        let owner = self.live_owner(token_id)?;
        if caller != owner && !self.is_approved_for_all(&owner, caller) {
            return Err(NftError::Unauthorized(format!(
                "{} cannot approve token_id {}",
                caller, token_id
            )));
        }
        self.token_approvals.insert(token_id, spender.to_string());
        Ok(NftEvent::TokenApproved {
            collection_id: self.id.clone(),
            token_id,
            owner,
            approved: spender.to_string(),
        })
    }

    /// Clear the single-token approval for `token_id`. Caller must be the owner
    /// or an operator-for-all.
    pub fn clear_approval(&mut self, caller: &str, token_id: u64) -> NftResult<NftEvent> {
        let owner = self.live_owner(token_id)?;
        if caller != owner && !self.is_approved_for_all(&owner, caller) {
            return Err(NftError::Unauthorized(format!(
                "{} cannot clear approval on token_id {}",
                caller, token_id
            )));
        }
        self.token_approvals.remove(&token_id);
        Ok(NftEvent::TokenApproved {
            collection_id: self.id.clone(),
            token_id,
            owner,
            approved: String::new(),
        })
    }

    /// Grant or revoke `operator` over all of `owner`'s tokens.
    pub fn set_approval_for_all(
        &mut self,
        owner: &str,
        operator: &str,
        approved: bool,
    ) -> NftResult<NftEvent> {
        if owner.is_empty() || operator.is_empty() {
            return Err(NftError::InvalidParams("empty address".into()));
        }
        if owner == operator {
            return Err(NftError::InvalidParams(
                "cannot set operator for self".into(),
            ));
        }
        self.operator_approvals
            .entry(owner.to_string())
            .or_default()
            .insert(operator.to_string(), approved);
        Ok(NftEvent::ApprovalForAll {
            collection_id: self.id.clone(),
            owner: owner.to_string(),
            operator: operator.to_string(),
            approved,
        })
    }

    /// Burn a token. Caller must be the owner or the collection admin. The id
    /// is retired permanently (tombstone) and can never be reminted.
    pub fn burn(&mut self, caller: &str, token_id: u64) -> NftResult<NftEvent> {
        let owner = self.live_owner(token_id)?;
        if caller != owner && caller != self.admin {
            return Err(NftError::Unauthorized(format!(
                "only owner or admin can burn token_id {}",
                token_id
            )));
        }
        let bal = self.balances.entry(owner.clone()).or_insert(0);
        *bal = bal
            .checked_sub(1)
            .ok_or_else(|| NftError::Overflow("burn underflow".into()))?;
        self.total_supply = self
            .total_supply
            .checked_sub(1)
            .ok_or_else(|| NftError::Overflow("total_supply underflow".into()))?;
        self.token_approvals.remove(&token_id);
        // Tombstone: keep the entry so the id is never reusable; record audit.
        if let Some(t) = self.tokens.get_mut(&token_id) {
            t.burned = true;
            t.frozen = false;
        }
        Ok(NftEvent::TokenBurned {
            collection_id: self.id.clone(),
            token_id,
            owner,
        })
    }

    /// Update a token's metadata URI. Caller must be the admin or the current
    /// owner. Fails if the collection is frozen, metadata is locked, or the
    /// token is frozen.
    pub fn update_token_uri(
        &mut self,
        caller: &str,
        token_id: u64,
        new_uri: &str,
    ) -> NftResult<NftEvent> {
        if self.frozen {
            return Err(NftError::CollectionFrozen);
        }
        if !self.metadata_mutable {
            return Err(NftError::MetadataLocked);
        }
        let owner = self.live_owner(token_id)?;
        if caller != self.admin && caller != owner {
            return Err(NftError::Unauthorized(format!(
                "{} cannot update token_id {} uri",
                caller, token_id
            )));
        }
        // Safe: live_owner proved a live token exists.
        if let Some(t) = self.tokens.get_mut(&token_id) {
            if t.frozen {
                return Err(NftError::TokenFrozen(token_id));
            }
            t.uri = new_uri.to_string();
        }
        Ok(NftEvent::TokenUriUpdated {
            collection_id: self.id.clone(),
            token_id,
        })
    }

    /// Update collection-level metadata. Admin-only; fails if frozen.
    pub fn update_collection_metadata(
        &mut self,
        caller: &str,
        description: Option<String>,
        base_uri: Option<String>,
        external_url: Option<String>,
    ) -> NftResult<NftEvent> {
        if caller != self.admin {
            return Err(NftError::Unauthorized(
                "only admin can update metadata".into(),
            ));
        }
        if self.frozen {
            return Err(NftError::CollectionFrozen);
        }
        if let Some(d) = description {
            self.description = d;
        }
        if let Some(b) = base_uri {
            self.base_uri = b;
        }
        if let Some(e) = external_url {
            self.external_url = e;
        }
        Ok(NftEvent::CollectionUpdated {
            collection_id: self.id.clone(),
        })
    }

    /// Freeze the whole collection (blocks all transfers). Admin-only.
    pub fn freeze_collection(&mut self, caller: &str) -> NftResult<NftEvent> {
        if caller != self.admin {
            return Err(NftError::Unauthorized("only admin can freeze".into()));
        }
        self.frozen = true;
        Ok(NftEvent::CollectionFrozen {
            collection_id: self.id.clone(),
        })
    }

    /// Lock collection metadata (token URIs become immutable). Admin-only.
    pub fn lock_metadata(&mut self, caller: &str) -> NftResult<NftEvent> {
        if caller != self.admin {
            return Err(NftError::Unauthorized(
                "only admin can lock metadata".into(),
            ));
        }
        self.metadata_mutable = false;
        Ok(NftEvent::CollectionMetadataLocked {
            collection_id: self.id.clone(),
        })
    }

    /// Freeze or unfreeze a single token. Admin-only.
    pub fn set_token_frozen(
        &mut self,
        caller: &str,
        token_id: u64,
        frozen: bool,
    ) -> NftResult<NftEvent> {
        if caller != self.admin {
            return Err(NftError::Unauthorized(
                "only admin can freeze tokens".into(),
            ));
        }
        // Must be a live token.
        self.live_owner(token_id)?;
        if let Some(t) = self.tokens.get_mut(&token_id) {
            t.frozen = frozen;
        }
        let collection_id = self.id.clone();
        Ok(if frozen {
            NftEvent::TokenFrozen {
                collection_id,
                token_id,
            }
        } else {
            NftEvent::TokenUnfrozen {
                collection_id,
                token_id,
            }
        })
    }

    /// Resolve the owner of a live token or error. Burned/absent → `TokenNotFound`.
    fn live_owner(&self, token_id: u64) -> NftResult<String> {
        match self.tokens.get(&token_id) {
            Some(t) if !t.burned => Ok(t.owner.clone()),
            _ => Err(NftError::TokenNotFound(token_id)),
        }
    }
}
