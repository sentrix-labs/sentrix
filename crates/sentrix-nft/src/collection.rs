//! The collection model and all token state transitions.
//!
//! A [`NftCollection`] owns its tokens, balances, and approvals. Every
//! state-changing method returns an [`NftEvent`] describing what happened, so
//! the integrating layer (`sentrix-core`) can emit it without re-deriving the
//! effect.
//!
//! Invariant-carrying fields are private; read them through the getters. This
//! prevents external code from mutating supply counters, balances, ownership,
//! or approvals out from under the invariants the methods enforce.
//!
//! Determinism: state lives in `HashMap`s. In-memory order is per-process and
//! is never hashed directly; when this state is committed to the state trie
//! (future work in `sentrix-core`), maps are iterated in sorted order.
//!
//! Atomicity: every state-changing method performs all validation and checked
//! arithmetic into locals first, and only commits mutations once every fallible
//! step has succeeded — so a rejected operation never leaves partial state.

use crate::error::{NftError, NftResult};
use crate::events::NftEvent;
use crate::token::NftToken;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A native NFT collection (the Proof Asset issuer unit).
///
/// Fields are private; construct via [`NftCollection::new`] and read via the
/// getters. Mutate only through the methods, which enforce authorization,
/// supply, soulbound, freeze, and metadata-lock invariants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NftCollection {
    id: String,
    creator: String,
    admin: String,
    name: String,
    symbol: String,
    description: String,
    base_uri: String,
    external_url: String,
    max_supply: Option<u64>,
    total_supply: u64,
    total_minted: u64,
    default_transferable: bool,
    metadata_mutable: bool,
    frozen: bool,
    tokens: HashMap<u64, NftToken>,
    balances: HashMap<String, u64>,
    token_approvals: HashMap<u64, String>,
    operator_approvals: HashMap<String, HashMap<String, bool>>,
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

    // ── Getters (read-only) ──────────────────────────────

    /// Collection id/address.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Address that created the collection.
    pub fn creator(&self) -> &str {
        &self.creator
    }
    /// Address with administrative rights.
    pub fn admin(&self) -> &str {
        &self.admin
    }
    /// Human-readable name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Ticker symbol.
    pub fn symbol(&self) -> &str {
        &self.symbol
    }
    /// Free-text description (empty = none).
    pub fn description(&self) -> &str {
        &self.description
    }
    /// Metadata-URI prefix.
    pub fn base_uri(&self) -> &str {
        &self.base_uri
    }
    /// External URL (empty = none).
    pub fn external_url(&self) -> &str {
        &self.external_url
    }
    /// Configured maximum supply (`None` = unlimited).
    pub fn max_supply(&self) -> Option<u64> {
        self.max_supply
    }
    /// Current live (non-burned) supply.
    pub fn total_supply(&self) -> u64 {
        self.total_supply
    }
    /// Monotonic count of tokens ever minted (backs `max_supply` + no-reuse).
    pub fn total_minted(&self) -> u64 {
        self.total_minted
    }
    /// Default transferability applied to mints that don't override it.
    pub fn default_transferable(&self) -> bool {
        self.default_transferable
    }
    /// Whether token metadata URIs may still be updated.
    pub fn metadata_mutable(&self) -> bool {
        self.metadata_mutable
    }
    /// Whether the whole collection is frozen.
    pub fn frozen(&self) -> bool {
        self.frozen
    }

    /// Borrow a token (including burned tombstones) for inspection.
    pub fn token(&self, token_id: u64) -> Option<&NftToken> {
        self.tokens.get(&token_id)
    }

    /// Current owner of a live token, or `None` if it doesn't exist or is burned.
    pub fn owner_of(&self, token_id: u64) -> Option<&str> {
        match self.tokens.get(&token_id) {
            Some(t) if !t.burned() => Some(t.owner()),
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
            Some(t) if !t.burned() => {
                if t.uri().is_empty() {
                    format!("{}{}", self.base_uri, token_id)
                } else {
                    t.uri().to_string()
                }
            }
            _ => String::new(),
        }
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
        // ── validate + compute (no mutation yet) ──
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
        let new_balance = self
            .balance_of(to)
            .checked_add(1)
            .ok_or_else(|| NftError::Overflow("balance".into()))?;
        let new_total_supply = self
            .total_supply
            .checked_add(1)
            .ok_or_else(|| NftError::Overflow("total_supply".into()))?;
        let new_total_minted = self
            .total_minted
            .checked_add(1)
            .ok_or_else(|| NftError::Overflow("total_minted".into()))?;

        // ── commit (all infallible) ──
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
        self.balances.insert(to.to_string(), new_balance);
        self.total_supply = new_total_supply;
        self.total_minted = new_total_minted;

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
        // ── validate + compute (no mutation yet) ──
        if from.is_empty() || to.is_empty() {
            return Err(NftError::InvalidParams("empty address".into()));
        }
        if from == to {
            return Err(NftError::InvalidParams("cannot transfer to self".into()));
        }
        let token = self
            .tokens
            .get(&token_id)
            .filter(|t| !t.burned())
            .ok_or(NftError::TokenNotFound(token_id))?;
        // Soulbound — checked first; no approval bypasses it.
        if !token.transferable() {
            return Err(NftError::NotTransferable(token_id));
        }
        if token.frozen() {
            return Err(NftError::TokenFrozen(token_id));
        }
        if self.frozen {
            return Err(NftError::CollectionFrozen);
        }
        if token.owner() != from {
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
        let new_from = self
            .balance_of(from)
            .checked_sub(1)
            .ok_or_else(|| NftError::Overflow("transfer underflow".into()))?;
        let new_to = self
            .balance_of(to)
            .checked_add(1)
            .ok_or_else(|| NftError::Overflow("transfer overflow".into()))?;

        // ── commit (all infallible) ──
        self.balances.insert(from.to_string(), new_from);
        self.balances.insert(to.to_string(), new_to);
        if let Some(t) = self.tokens.get_mut(&token_id) {
            t.set_owner(to.to_string());
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

    /// Grant or revoke `operator` over all of `owner`'s tokens. `caller` must be
    /// `owner` — a caller can only manage operators for their own tokens.
    pub fn set_approval_for_all(
        &mut self,
        caller: &str,
        owner: &str,
        operator: &str,
        approved: bool,
    ) -> NftResult<NftEvent> {
        if owner.is_empty() || operator.is_empty() {
            return Err(NftError::InvalidParams("empty address".into()));
        }
        if caller != owner {
            return Err(NftError::Unauthorized(
                "caller may only set operator approvals for itself".into(),
            ));
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
        // ── validate + compute (no mutation yet) ──
        let owner = self.live_owner(token_id)?;
        if caller != owner && caller != self.admin {
            return Err(NftError::Unauthorized(format!(
                "only owner or admin can burn token_id {}",
                token_id
            )));
        }
        let new_balance = self
            .balance_of(&owner)
            .checked_sub(1)
            .ok_or_else(|| NftError::Overflow("burn underflow".into()))?;
        let new_total_supply = self
            .total_supply
            .checked_sub(1)
            .ok_or_else(|| NftError::Overflow("total_supply underflow".into()))?;

        // ── commit (all infallible) ──
        self.balances.insert(owner.clone(), new_balance);
        self.total_supply = new_total_supply;
        self.token_approvals.remove(&token_id);
        // Tombstone: keep the entry so the id is never reusable; record audit.
        if let Some(t) = self.tokens.get_mut(&token_id) {
            t.mark_burned();
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
        // A live token frozen individually also blocks its URI update.
        if self.tokens.get(&token_id).is_some_and(|t| t.frozen()) {
            return Err(NftError::TokenFrozen(token_id));
        }
        if let Some(t) = self.tokens.get_mut(&token_id) {
            t.set_uri(new_uri.to_string());
        }
        Ok(NftEvent::TokenUriUpdated {
            collection_id: self.id.clone(),
            token_id,
        })
    }

    /// Update collection-level metadata. Admin-only; fails if the collection is
    /// frozen or its metadata is locked.
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
        if !self.metadata_mutable {
            return Err(NftError::MetadataLocked);
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

    /// Lock collection metadata (collection + token URIs become immutable).
    /// Admin-only.
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
            t.set_frozen(frozen);
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

    /// Canonical, deterministic hash of this collection's full state.
    ///
    /// Every field and every map is folded in a fixed order with map keys
    /// sorted, so the result is independent of `HashMap` iteration order and
    /// identical across nodes/processes for the same logical state. This is
    /// the building block for native-module state commitment (fingerprint
    /// today; trie/state_root commitment is the fork-gated follow-up). The
    /// domain owns its canonical form so encapsulation stays intact — this is
    /// read-only and adds no mutable access to internals.
    pub fn canonical_hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        // Scalar / string fields, length-prefixed where ambiguous.
        for s in [
            self.id.as_str(),
            self.creator.as_str(),
            self.admin.as_str(),
            self.name.as_str(),
            self.symbol.as_str(),
            self.description.as_str(),
            self.base_uri.as_str(),
            self.external_url.as_str(),
        ] {
            h.update((s.len() as u64).to_be_bytes());
            h.update(s.as_bytes());
        }
        // Option<u64> max_supply: tag byte then value.
        match self.max_supply {
            Some(m) => {
                h.update([1u8]);
                h.update(m.to_be_bytes());
            }
            None => h.update([0u8]),
        }
        h.update(self.total_supply.to_be_bytes());
        h.update(self.total_minted.to_be_bytes());
        h.update([
            self.default_transferable as u8,
            self.metadata_mutable as u8,
            self.frozen as u8,
        ]);

        // tokens — sorted by token_id.
        let mut token_ids: Vec<&u64> = self.tokens.keys().collect();
        token_ids.sort_unstable();
        h.update((token_ids.len() as u64).to_be_bytes());
        for tid in token_ids {
            let t = &self.tokens[tid];
            h.update(tid.to_be_bytes());
            for s in [t.owner(), t.uri()] {
                h.update((s.len() as u64).to_be_bytes());
                h.update(s.as_bytes());
            }
            for opt in [t.uri_hash.as_deref(), t.metadata_hash.as_deref()] {
                match opt {
                    Some(s) => {
                        h.update([1u8]);
                        h.update((s.len() as u64).to_be_bytes());
                        h.update(s.as_bytes());
                    }
                    None => h.update([0u8]),
                }
            }
            h.update([t.transferable() as u8, t.frozen() as u8, t.burned() as u8]);
        }

        // balances — sorted by address.
        let mut bal: Vec<(&String, &u64)> = self.balances.iter().collect();
        bal.sort_unstable_by(|a, b| a.0.cmp(b.0));
        h.update((bal.len() as u64).to_be_bytes());
        for (addr, n) in bal {
            h.update((addr.len() as u64).to_be_bytes());
            h.update(addr.as_bytes());
            h.update(n.to_be_bytes());
        }

        // token_approvals — sorted by token_id.
        let mut appr: Vec<(&u64, &String)> = self.token_approvals.iter().collect();
        appr.sort_unstable_by(|a, b| a.0.cmp(b.0));
        h.update((appr.len() as u64).to_be_bytes());
        for (tid, spender) in appr {
            h.update(tid.to_be_bytes());
            h.update((spender.len() as u64).to_be_bytes());
            h.update(spender.as_bytes());
        }

        // operator_approvals — sorted by (owner, operator).
        let mut ops: Vec<(&String, &String, &bool)> = self
            .operator_approvals
            .iter()
            .flat_map(|(owner, m)| m.iter().map(move |(op, v)| (owner, op, v)))
            .collect();
        ops.sort_unstable_by(|a, b| a.0.cmp(b.0).then(a.1.cmp(b.1)));
        h.update((ops.len() as u64).to_be_bytes());
        for (owner, op, v) in ops {
            h.update((owner.len() as u64).to_be_bytes());
            h.update(owner.as_bytes());
            h.update((op.len() as u64).to_be_bytes());
            h.update(op.as_bytes());
            h.update([*v as u8]);
        }
        h.finalize().into()
    }

    /// Resolve the owner of a live token or error. Burned/absent → `TokenNotFound`.
    fn live_owner(&self, token_id: u64) -> NftResult<String> {
        match self.tokens.get(&token_id) {
            Some(t) if !t.burned() => Ok(t.owner().to_string()),
            _ => Err(NftError::TokenNotFound(token_id)),
        }
    }
}
