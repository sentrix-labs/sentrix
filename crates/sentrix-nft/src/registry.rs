//! The collection registry and deterministic collection-id derivation.
//!
//! Collection ids/addresses are derived as `SRC721_<sha256(creator|seed)[..20]>`.
//! The `seed` is supplied by the caller (the transaction id at the integration
//! site) so every node derives the same address when applying the same block —
//! no wall-clock time, no randomness.

use crate::collection::NftCollection;
use crate::error::{NftError, NftResult};
use crate::events::NftEvent;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

/// Derive a deterministic collection address from creator + seed.
///
/// Uses length-prefixed encoding (`len(creator) ‖ creator ‖ len(seed) ‖ seed`,
/// lengths as `u64` big-endian) rather than a separator string. A separator
/// like `"{creator}|{seed}"` is ambiguous — `("a|b", "c")` and `("a", "b|c")`
/// would hash identically and collide on the same collection id. Length
/// prefixing makes the preimage unambiguous.
pub fn compute_collection_id(creator: &str, seed: &str) -> String {
    let mut h = Sha256::new();
    h.update((creator.len() as u64).to_be_bytes());
    h.update(creator.as_bytes());
    h.update((seed.len() as u64).to_be_bytes());
    h.update(seed.as_bytes());
    let hash = h.finalize();
    format!("SRC721_{}", hex::encode(&hash[..20]))
}

/// Holds every deployed native NFT collection.
///
/// The `collections` map is private: collections are only ever created through
/// [`NftRegistry::deploy_collection`] (which validates params and assigns the
/// deterministic id), so external code cannot insert an unvalidated collection
/// under an arbitrary id or delete one. serde still persists the field.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NftRegistry {
    collections: HashMap<String, NftCollection>,
}

impl NftRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Deploy a new collection. Returns the derived id and the
    /// `CollectionCreated` event. Errors on bad params or id collision.
    ///
    /// `max_supply` is optional; `Some(0)` is rejected (use `None` for
    /// unlimited).
    #[allow(clippy::too_many_arguments)]
    pub fn deploy_collection(
        &mut self,
        creator: &str,
        name: &str,
        symbol: &str,
        base_uri: &str,
        max_supply: Option<u64>,
        default_transferable: bool,
        metadata_mutable: bool,
        seed: &str,
    ) -> NftResult<(String, NftEvent)> {
        if creator.is_empty() {
            return Err(NftError::InvalidParams("empty creator".into()));
        }
        if name.is_empty() || name.len() > 64 {
            return Err(NftError::InvalidParams("name must be 1–64 bytes".into()));
        }
        if symbol.is_empty()
            || symbol.len() > 10
            || !symbol.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(NftError::InvalidParams(
                "symbol must be 1–10 ASCII alphanumeric chars".into(),
            ));
        }
        if max_supply == Some(0) {
            return Err(NftError::InvalidParams(
                "max_supply cannot be 0 (use None for unlimited)".into(),
            ));
        }

        let id = compute_collection_id(creator, seed);
        if self.collections.contains_key(&id) {
            return Err(NftError::CollectionExists(id));
        }
        let collection = NftCollection::new(
            id.clone(),
            creator.to_string(),
            name.to_string(),
            symbol.to_string(),
            base_uri.to_string(),
            max_supply,
            default_transferable,
            metadata_mutable,
        );
        self.collections.insert(id.clone(), collection);
        let event = NftEvent::CollectionCreated {
            collection_id: id.clone(),
            creator: creator.to_string(),
            name: name.to_string(),
            symbol: symbol.to_string(),
        };
        Ok((id, event))
    }

    /// Borrow a collection by id.
    pub fn get_collection(&self, id: &str) -> Option<&NftCollection> {
        self.collections.get(id)
    }

    /// Mutably borrow a collection by id (for applying token operations).
    pub fn get_collection_mut(&mut self, id: &str) -> Option<&mut NftCollection> {
        self.collections.get_mut(id)
    }

    /// Whether a collection with this id exists.
    pub fn collection_exists(&self, id: &str) -> bool {
        self.collections.contains_key(id)
    }

    /// Number of deployed collections.
    pub fn collection_count(&self) -> usize {
        self.collections.len()
    }

    /// Canonical, deterministic hash of the whole registry.
    ///
    /// Collections are folded in sorted-id order, each via
    /// [`NftCollection::canonical_hash`], so the digest is independent of
    /// `HashMap` iteration order. An empty registry hashes to a stable value
    /// distinct from any populated one. Backs native-module state commitment.
    pub fn canonical_hash(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut ids: Vec<&String> = self.collections.keys().collect();
        ids.sort_unstable();
        let mut h = Sha256::new();
        h.update((ids.len() as u64).to_be_bytes());
        for id in ids {
            h.update((id.len() as u64).to_be_bytes());
            h.update(id.as_bytes());
            h.update(self.collections[id].canonical_hash());
        }
        h.finalize().into()
    }
}
