//! # Sentrix Native NFT — Proof Asset layer
//!
//! Pure domain logic for Sentrix's *native* NFT standard (SRC-721-style). This
//! is a **Proof Asset / reputation layer**, not a JPEG/PFP marketplace:
//!
//! - validator proof, builder proof, testnet-participation proof, contributor
//!   proof, bug-hunter proof, genesis-supporter proof, community reputation,
//!   and optional **soulbound** (non-transferable) assets.
//!
//! What this crate is and isn't:
//!
//! - It contains **only** NFT domain logic: collections, tokens, ownership,
//!   approvals, soulbound/freeze rules, metadata URIs, deterministic ids, and
//!   events. No storage, no trie, no consensus, no RPC, no marketplace, no
//!   bridge.
//! - Metadata and media live **off-chain**; only URIs (and optional integrity
//!   hashes) are kept. Image bytes are never stored in chain state.
//! - EVM ERC-721/ERC-1155 remain a **separate**, parallel path for public
//!   developers; this native rail does not replace them.
//!
//! Determinism: ids are derived from `sha256(creator|seed)` — no wall-clock
//! time, no randomness. Burned token ids are **never reused** (tombstones),
//! keeping the reputation history append-only and auditable.
//!
//! ## Future storage key design (NOT implemented here)
//!
//! Storage/MDBX integration is future work in `sentrix-core`. The intended key
//! scheme, for reference only:
//!
//! ```text
//! nft:collection:{collection_id}
//! nft:token:{collection_id}:{token_id}
//! nft:owner:{collection_id}:{token_id}
//! nft:owner_index:{owner}:{collection_id}:{token_id}
//! nft:approval:{collection_id}:{token_id}
//! nft:operator:{owner}:{operator}
//! nft:supply:{collection_id}
//! ```
//!
//! A `NftStorage` trait is intentionally deferred until a real storage consumer
//! exists — adding it now would be an unused abstraction.
//!
//! ## Examples
//!
//! Create a soulbound Builder Badge collection and mint a badge:
//!
//! ```
//! use sentrix_nft::NftRegistry;
//!
//! let mut reg = NftRegistry::new();
//! // default_transferable = false → badges are soulbound by default.
//! let (id, _ev) = reg
//!     .deploy_collection("0xfoundation", "Builder Badge", "BUILD", "ipfs://badges/", None, false, true, "tx-001")
//!     .unwrap();
//!
//! let col = reg.get_collection_mut(&id).unwrap();
//! // admin (= creator) mints a soulbound badge to a builder.
//! col.mint("0xfoundation", "0xbuilder", 1, "", None).unwrap();
//! assert_eq!(col.owner_of(1), Some("0xbuilder"));
//! ```
//!
//! Soulbound transfer fails; a transferable token moves normally:
//!
//! ```
//! use sentrix_nft::NftRegistry;
//!
//! let mut reg = NftRegistry::new();
//! let (id, _) = reg
//!     .deploy_collection("0xadmin", "Mixed", "MIX", "ipfs://", None, false, true, "seed")
//!     .unwrap();
//! let col = reg.get_collection_mut(&id).unwrap();
//!
//! // soulbound (uses the collection default false) → cannot move
//! col.mint("0xadmin", "0xalice", 1, "", None).unwrap();
//! assert!(col.transfer("0xalice", "0xalice", "0xbob", 1).is_err());
//!
//! // explicitly transferable → moves
//! col.mint("0xadmin", "0xalice", 2, "", Some(true)).unwrap();
//! col.transfer("0xalice", "0xalice", "0xbob", 2).unwrap();
//! assert_eq!(col.owner_of(2), Some("0xbob"));
//! ```

pub mod collection;
pub mod error;
pub mod events;
pub mod registry;
pub mod token;

pub use collection::NftCollection;
pub use error::{NftError, NftResult};
pub use events::NftEvent;
pub use registry::{NftRegistry, compute_collection_id};
pub use token::NftToken;

#[cfg(test)]
mod tests {
    use super::*;

    const ADMIN: &str = "0xadmin";
    const ALICE: &str = "0xalice";
    const BOB: &str = "0xbob";
    const CAROL: &str = "0xcarol";

    /// Transferable-by-default collection for the common cases.
    fn transferable_collection() -> NftCollection {
        NftCollection::new(
            "SRC721_test".into(),
            ADMIN.into(),
            "Test".into(),
            "TEST".into(),
            "https://meta/".into(),
            None, // unlimited
            true, // default_transferable
            true, // metadata_mutable
        )
    }

    /// Soulbound-by-default collection (Proof Asset style).
    fn soulbound_collection() -> NftCollection {
        NftCollection::new(
            "SRC721_badge".into(),
            ADMIN.into(),
            "Badge".into(),
            "BADGE".into(),
            "ipfs://b/".into(),
            None,
            false, // default_transferable = false → soulbound
            true,
        )
    }

    // ── collection / registry ────────────────────────────

    #[test]
    fn registry_deploy_deterministic_and_prefixed() {
        let mut a = NftRegistry::new();
        let mut b = NftRegistry::new();
        let (id_a, ev) = a
            .deploy_collection(ADMIN, "C", "C", "u", None, true, true, "tx1")
            .unwrap();
        let (id_b, _) = b
            .deploy_collection(ADMIN, "C", "C", "u", None, true, true, "tx1")
            .unwrap();
        assert_eq!(id_a, id_b);
        assert!(id_a.starts_with("SRC721_"));
        assert!(matches!(ev, NftEvent::CollectionCreated { .. }));
    }

    #[test]
    fn registry_rejects_duplicate_collection() {
        let mut r = NftRegistry::new();
        r.deploy_collection(ADMIN, "C", "C", "u", None, true, true, "tx1")
            .unwrap();
        assert!(
            r.deploy_collection(ADMIN, "C", "C", "u", None, true, true, "tx1")
                .is_err()
        );
        assert_eq!(r.collection_count(), 1);
    }

    #[test]
    fn registry_rejects_empty_name_symbol_and_zero_max_supply() {
        let mut r = NftRegistry::new();
        assert!(
            r.deploy_collection(ADMIN, "", "C", "u", None, true, true, "t")
                .is_err()
        );
        assert!(
            r.deploy_collection(ADMIN, "C", "", "u", None, true, true, "t")
                .is_err()
        );
        assert!(
            r.deploy_collection(ADMIN, "C", "bad sym", "u", None, true, true, "t")
                .is_err()
        );
        assert!(
            r.deploy_collection(ADMIN, "C", "C", "u", Some(0), true, true, "t")
                .is_err()
        );
    }

    #[test]
    fn registry_get_and_exists() {
        let mut r = NftRegistry::new();
        let (id, _) = r
            .deploy_collection(ADMIN, "C", "C", "u", None, true, true, "t")
            .unwrap();
        assert!(r.collection_exists(&id));
        assert!(!r.collection_exists("SRC721_nope"));
        assert!(r.get_collection(&id).is_some());
    }

    // ── mint ─────────────────────────────────────────────

    #[test]
    fn mint_sets_owner_balance_supply_and_event() {
        let mut c = transferable_collection();
        let ev = c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        assert_eq!(c.owner_of(1), Some(ALICE));
        assert_eq!(c.balance_of(ALICE), 1);
        assert_eq!(c.total_supply(), 1);
        assert!(matches!(ev, NftEvent::TokenMinted { token_id: 1, .. }));
    }

    #[test]
    fn mint_only_admin() {
        let mut c = transferable_collection();
        assert!(c.mint(ALICE, ALICE, 1, "", None).is_err());
        assert_eq!(c.total_supply(), 0);
    }

    #[test]
    fn mint_rejects_duplicate_token_id() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        assert!(c.mint(ADMIN, BOB, 1, "", None).is_err());
        assert_eq!(c.owner_of(1), Some(ALICE));
    }

    #[test]
    fn mint_respects_max_supply_against_ever_minted() {
        let mut c = NftCollection::new(
            "SRC721_cap".into(),
            ADMIN.into(),
            "Cap".into(),
            "CAP".into(),
            "u".into(),
            Some(2),
            true,
            true,
        );
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.mint(ADMIN, ALICE, 2, "", None).unwrap();
        assert!(c.mint(ADMIN, ALICE, 3, "", None).is_err());
        // burning does NOT free a cap slot (counts ever-minted)
        c.burn(ALICE, 1).unwrap();
        assert!(c.mint(ADMIN, ALICE, 9, "", None).is_err());
    }

    #[test]
    fn mint_soulbound_and_transferable() {
        let mut c = soulbound_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap(); // default soulbound
        assert!(!c.token(1).unwrap().transferable());
        c.mint(ADMIN, ALICE, 2, "", Some(true)).unwrap(); // override
        assert!(c.token(2).unwrap().transferable());
    }

    #[test]
    fn mint_uri_override_and_base_convention() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 7, "ipfs://custom", None).unwrap();
        assert_eq!(c.token_uri(7), "ipfs://custom");
        c.mint(ADMIN, ALICE, 8, "", None).unwrap();
        assert_eq!(c.token_uri(8), "https://meta/8");
    }

    // ── transfer ─────────────────────────────────────────

    #[test]
    fn owner_can_transfer_transferable() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        let ev = c.transfer(ALICE, ALICE, BOB, 1).unwrap();
        assert_eq!(c.owner_of(1), Some(BOB));
        assert_eq!(c.balance_of(ALICE), 0);
        assert_eq!(c.balance_of(BOB), 1);
        assert!(matches!(ev, NftEvent::TokenTransferred { .. }));
    }

    #[test]
    fn soulbound_transfer_fails_typed() {
        let mut c = soulbound_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        assert_eq!(
            c.transfer(ALICE, ALICE, BOB, 1),
            Err(NftError::NotTransferable(1))
        );
        assert_eq!(c.owner_of(1), Some(ALICE));
    }

    #[test]
    fn approval_does_not_bypass_soulbound() {
        let mut c = soulbound_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        // even if an operator is set, soulbound still can't move
        c.set_approval_for_all(ALICE, ALICE, BOB, true).unwrap();
        assert_eq!(
            c.transfer(BOB, ALICE, CAROL, 1),
            Err(NftError::NotTransferable(1))
        );
    }

    #[test]
    fn transfer_unauthorized_fails() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        assert!(c.transfer(BOB, ALICE, BOB, 1).is_err());
    }

    #[test]
    fn transfer_wrong_from_fails() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        assert!(matches!(
            c.transfer(ALICE, BOB, CAROL, 1),
            Err(NftError::NotOwner { token_id: 1, .. })
        ));
    }

    #[test]
    fn transfer_nonexistent_and_self_fail() {
        let mut c = transferable_collection();
        assert_eq!(
            c.transfer(ALICE, ALICE, BOB, 99),
            Err(NftError::TokenNotFound(99))
        );
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        assert!(c.transfer(ALICE, ALICE, ALICE, 1).is_err());
    }

    #[test]
    fn approved_spender_transfers_then_approval_clears() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.approve(ALICE, BOB, 1).unwrap();
        assert_eq!(c.get_approved(1), Some(&BOB.to_string()));
        c.transfer(BOB, ALICE, CAROL, 1).unwrap();
        assert_eq!(c.owner_of(1), Some(CAROL));
        assert_eq!(c.get_approved(1), None); // cleared by transfer
    }

    #[test]
    fn operator_can_transfer_and_be_revoked() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.mint(ADMIN, ALICE, 2, "", None).unwrap();
        c.set_approval_for_all(ALICE, ALICE, BOB, true).unwrap();
        c.transfer(BOB, ALICE, CAROL, 1).unwrap();
        c.set_approval_for_all(ALICE, ALICE, BOB, false).unwrap();
        assert!(c.transfer(BOB, ALICE, CAROL, 2).is_err());
    }

    #[test]
    fn frozen_token_and_collection_block_transfer() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.mint(ADMIN, ALICE, 2, "", None).unwrap();
        // token freeze
        c.set_token_frozen(ADMIN, 1, true).unwrap();
        assert_eq!(
            c.transfer(ALICE, ALICE, BOB, 1),
            Err(NftError::TokenFrozen(1))
        );
        c.set_token_frozen(ADMIN, 1, false).unwrap();
        c.transfer(ALICE, ALICE, BOB, 1).unwrap();
        // collection freeze
        c.freeze_collection(ADMIN).unwrap();
        assert_eq!(
            c.transfer(ALICE, ALICE, BOB, 2),
            Err(NftError::CollectionFrozen)
        );
    }

    // ── approval ─────────────────────────────────────────

    #[test]
    fn approve_requires_owner_or_operator() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        assert!(c.approve(BOB, BOB, 1).is_err());
        // operator may approve on owner's behalf
        c.set_approval_for_all(ALICE, ALICE, BOB, true).unwrap();
        c.approve(BOB, CAROL, 1).unwrap();
        assert_eq!(c.get_approved(1), Some(&CAROL.to_string()));
    }

    #[test]
    fn clear_approval_works() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.approve(ALICE, BOB, 1).unwrap();
        c.clear_approval(ALICE, 1).unwrap();
        assert_eq!(c.get_approved(1), None);
    }

    #[test]
    fn approval_cleared_after_burn() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.approve(ALICE, BOB, 1).unwrap();
        c.burn(ALICE, 1).unwrap();
        assert_eq!(c.get_approved(1), None);
    }

    #[test]
    fn set_approval_for_all_rejects_self() {
        let mut c = transferable_collection();
        assert!(c.set_approval_for_all(ALICE, ALICE, ALICE, true).is_err());
    }

    // ── burn (strict no-reuse) ───────────────────────────

    #[test]
    fn owner_burn_works_balance_and_supply_drop() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "ipfs://x", None).unwrap();
        let ev = c.burn(ALICE, 1).unwrap();
        assert_eq!(c.owner_of(1), None);
        assert_eq!(c.balance_of(ALICE), 0);
        assert_eq!(c.total_supply(), 0);
        assert_eq!(c.token_uri(1), "");
        assert!(matches!(ev, NftEvent::TokenBurned { token_id: 1, .. }));
    }

    #[test]
    fn admin_can_burn() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.burn(ADMIN, 1).unwrap();
        assert_eq!(c.owner_of(1), None);
    }

    #[test]
    fn unauthorized_burn_fails() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        assert!(c.burn(BOB, 1).is_err());
        assert_eq!(c.owner_of(1), Some(ALICE));
    }

    #[test]
    fn burn_nonexistent_and_double_burn_fail() {
        let mut c = transferable_collection();
        assert_eq!(c.burn(ALICE, 5), Err(NftError::TokenNotFound(5)));
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.burn(ALICE, 1).unwrap();
        assert_eq!(c.burn(ALICE, 1), Err(NftError::TokenNotFound(1)));
    }

    #[test]
    fn burned_token_id_strictly_not_reusable() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.burn(ALICE, 1).unwrap();
        // strict no-reuse: id 1 is retired forever
        assert_eq!(
            c.mint(ADMIN, BOB, 1, "", None),
            Err(NftError::TokenAlreadyExists(1))
        );
        // tombstone preserved for audit
        assert!(c.token(1).unwrap().burned());
    }

    #[test]
    fn transfer_burned_token_fails() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.burn(ALICE, 1).unwrap();
        assert_eq!(
            c.transfer(ALICE, ALICE, BOB, 1),
            Err(NftError::TokenNotFound(1))
        );
    }

    // ── metadata ─────────────────────────────────────────

    #[test]
    fn update_token_uri_by_owner_and_admin() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.update_token_uri(ALICE, 1, "ipfs://new").unwrap();
        assert_eq!(c.token_uri(1), "ipfs://new");
        c.update_token_uri(ADMIN, 1, "ipfs://admin").unwrap();
        assert_eq!(c.token_uri(1), "ipfs://admin");
    }

    #[test]
    fn update_token_uri_unauthorized_fails() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        assert!(c.update_token_uri(BOB, 1, "ipfs://x").is_err());
    }

    #[test]
    fn update_token_uri_fails_when_locked() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.lock_metadata(ADMIN).unwrap();
        assert_eq!(
            c.update_token_uri(ALICE, 1, "ipfs://x"),
            Err(NftError::MetadataLocked)
        );
    }

    #[test]
    fn update_collection_metadata_admin_only() {
        let mut c = transferable_collection();
        assert!(
            c.update_collection_metadata(BOB, Some("d".into()), None, None)
                .is_err()
        );
        c.update_collection_metadata(ADMIN, Some("desc".into()), Some("ipfs://b/".into()), None)
            .unwrap();
        assert_eq!(c.description(), "desc");
        assert_eq!(c.base_uri(), "ipfs://b/");
    }

    #[test]
    fn frozen_collection_blocks_metadata_update() {
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        c.freeze_collection(ADMIN).unwrap();
        assert_eq!(
            c.update_token_uri(ALICE, 1, "x"),
            Err(NftError::CollectionFrozen)
        );
        assert!(
            c.update_collection_metadata(ADMIN, Some("d".into()), None, None)
                .is_err()
        );
    }

    #[test]
    fn uri_hash_fields_serde_round_trip() {
        // Integrity hashes are optional URI references (never media bytes) and
        // survive serialization. Tested on NftToken directly — collection
        // internals are sealed.
        let mut t = NftToken::new("SRC721_c".into(), 1, ALICE.into(), "ipfs://x".into(), true);
        t.uri_hash = Some("deadbeef".into());
        t.metadata_hash = Some("cafebabe".into());
        let json = serde_json::to_string(&t).unwrap();
        let back: NftToken = serde_json::from_str(&json).unwrap();
        assert_eq!(back.uri_hash.as_deref(), Some("deadbeef"));
        assert_eq!(back.metadata_hash.as_deref(), Some("cafebabe"));
        assert!(!back.burned());
    }

    // ── CodeRabbit hardening regressions ─────────────────

    #[test]
    fn set_approval_for_all_non_owner_rejected() {
        // F1: a caller cannot set operator approvals on someone else's behalf.
        let mut c = transferable_collection();
        c.mint(ADMIN, ALICE, 1, "", None).unwrap();
        // BOB tries to make CAROL an operator over ALICE's tokens.
        assert_eq!(
            c.set_approval_for_all(BOB, ALICE, CAROL, true),
            Err(NftError::Unauthorized(
                "caller may only set operator approvals for itself".into()
            ))
        );
        assert!(!c.is_approved_for_all(ALICE, CAROL));
    }

    #[test]
    fn collection_id_no_separator_collision() {
        // F2: ambiguous-separator inputs must NOT collide.
        let a = compute_collection_id("alice|x", "1");
        let b = compute_collection_id("alice", "x|1");
        assert_ne!(a, b, "length-prefixing must disambiguate the preimage");
        // determinism preserved for identical inputs
        assert_eq!(
            compute_collection_id("alice", "x|1"),
            compute_collection_id("alice", "x|1")
        );
    }

    #[test]
    fn collection_metadata_update_fails_after_lock() {
        // F5: lock must block collection-level metadata, not just token URIs.
        let mut c = transferable_collection();
        // works before lock
        c.update_collection_metadata(ADMIN, Some("d1".into()), None, None)
            .unwrap();
        c.lock_metadata(ADMIN).unwrap();
        assert_eq!(
            c.update_collection_metadata(ADMIN, Some("d2".into()), None, None),
            Err(NftError::MetadataLocked)
        );
        // collection-level field unchanged
        assert_eq!(c.description(), "d1");
    }

    // ── events ───────────────────────────────────────────

    #[test]
    fn events_are_returned_for_each_op() {
        let mut r = NftRegistry::new();
        let (id, created) = r
            .deploy_collection(ADMIN, "C", "C", "u", None, true, true, "t")
            .unwrap();
        assert!(matches!(created, NftEvent::CollectionCreated { .. }));
        let c = r.get_collection_mut(&id).unwrap();
        assert!(matches!(
            c.mint(ADMIN, ALICE, 1, "", None).unwrap(),
            NftEvent::TokenMinted { .. }
        ));
        assert!(matches!(
            c.approve(ALICE, BOB, 1).unwrap(),
            NftEvent::TokenApproved { .. }
        ));
        assert!(matches!(
            c.transfer(BOB, ALICE, CAROL, 1).unwrap(),
            NftEvent::TokenTransferred { .. }
        ));
        assert!(matches!(
            c.burn(CAROL, 1).unwrap(),
            NftEvent::TokenBurned { .. }
        ));
    }

    // ── canonical_hash (native-module state commitment) ──────

    /// Building the same logical state via a different mint order must yield
    /// the same canonical hash — proves HashMap iteration order can't move it.
    #[test]
    fn canonical_hash_is_order_independent() {
        let build = |order: &[u64]| -> [u8; 32] {
            let mut r = NftRegistry::new();
            let (id, _) = r
                .deploy_collection(ADMIN, "C", "C", "u", None, true, true, "seed")
                .unwrap();
            let c = r.get_collection_mut(&id).unwrap();
            for &t in order {
                c.mint(ADMIN, ALICE, t, "", None).unwrap();
            }
            r.canonical_hash()
        };
        assert_eq!(build(&[1, 2, 3, 4, 5]), build(&[5, 4, 3, 2, 1]));
        assert_eq!(build(&[3, 1, 4, 5, 2]), build(&[1, 2, 3, 4, 5]));
    }

    /// Every state transition must move the canonical hash.
    #[test]
    fn canonical_hash_tracks_state_changes() {
        let mut r = NftRegistry::new();
        let empty = r.canonical_hash();

        let (id, _) = r
            .deploy_collection(ADMIN, "C", "C", "u", None, true, true, "seed")
            .unwrap();
        let after_deploy = r.canonical_hash();
        assert_ne!(empty, after_deploy, "deploy must change hash");

        r.get_collection_mut(&id)
            .unwrap()
            .mint(ADMIN, ALICE, 1, "", None)
            .unwrap();
        let after_mint = r.canonical_hash();
        assert_ne!(after_deploy, after_mint, "mint must change hash");

        r.get_collection_mut(&id)
            .unwrap()
            .approve(ALICE, BOB, 1)
            .unwrap();
        let after_approve = r.canonical_hash();
        assert_ne!(after_mint, after_approve, "approve must change hash");

        r.get_collection_mut(&id)
            .unwrap()
            .transfer(ALICE, ALICE, CAROL, 1)
            .unwrap();
        let after_transfer = r.canonical_hash();
        assert_ne!(after_approve, after_transfer, "transfer must change hash");

        r.get_collection_mut(&id).unwrap().burn(CAROL, 1).unwrap();
        assert_ne!(after_transfer, r.canonical_hash(), "burn must change hash");
    }

    /// Different owners ⇒ different hash; identical logical state ⇒ identical.
    #[test]
    fn canonical_hash_distinguishes_owners_and_is_deterministic() {
        let make = |owner: &str| -> [u8; 32] {
            let mut r = NftRegistry::new();
            let (id, _) = r
                .deploy_collection(ADMIN, "C", "C", "u", None, true, true, "seed")
                .unwrap();
            r.get_collection_mut(&id)
                .unwrap()
                .mint(ADMIN, owner, 1, "", None)
                .unwrap();
            r.canonical_hash()
        };
        assert_ne!(make(ALICE), make(BOB), "different owner ⇒ different hash");
        assert_eq!(make(ALICE), make(ALICE), "same state ⇒ same hash");
    }

    /// Empty registry hashes to a stable, non-zero, distinct value.
    #[test]
    fn canonical_hash_empty_is_stable() {
        assert_eq!(
            NftRegistry::new().canonical_hash(),
            NftRegistry::new().canonical_hash()
        );
        assert_ne!(NftRegistry::new().canonical_hash(), [0u8; 32]);
    }
}
