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
use sentrix_primitives::events::TokenOpEvent;
use sentrix_primitives::transaction::TokenOp;

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

/// Apply one SRC-721 NFT `TokenOp` against `registry`, returning the domain
/// events it produced. This is the single dispatch point shared by all three
/// block-execution layers (read-only validate, Pass-1 dry-run, Pass-2 apply):
/// the caller passes `&mut self.nft_registry` to mutate real state, or a
/// `&mut clone` to dry-run.
///
/// `sender` is the authenticated transaction sender (`tx.from_address`) — it
/// is the ONLY authorization principal. NFT payloads never carry a `caller`
/// field, and where a payload does carry an address (`TransferNft.from`) it is
/// treated as untrusted claimed data that the domain checks against the real
/// owner; authority always comes from `sender`. `seed` is `tx.txid`, matching
/// the SRC-20 deterministic-address precedent (no wall-clock, no randomness).
///
/// Deterministic: the function only reads `op`, `sender`, `seed`, and the
/// registry state, so two nodes applying the same block produce identical
/// results and identical collection ids.
///
/// Scope (this PR): only the merged SRC-721 variants are wired. SRC-1155 and
/// the metadata/freeze admin ops have no TokenOp variant yet and return an
/// error here. Soulbound collections cannot yet be *deployed* via `DeployNft`
/// (the wire format carries no transferability selector) — apply-path
/// soulbound *enforcement* is fully wired and tested. See PR notes.
pub fn apply_nft_token_op(
    registry: &mut NftRegistry,
    op: &TokenOp,
    sender: &str,
    seed: &str,
) -> Result<Vec<NftEvent>, SentrixError> {
    fn collection_mut<'a>(
        reg: &'a mut NftRegistry,
        contract: &str,
    ) -> Result<&'a mut NftCollection, SentrixError> {
        reg.get_collection_mut(contract)
            .ok_or_else(|| SentrixError::NotFound(format!("nft collection {contract}")))
    }

    let event = match op {
        TokenOp::DeployNft {
            name,
            symbol,
            base_uri,
            max_supply,
        } => {
            // SRC-20 precedent: max_supply == 0 means unlimited → None.
            let max = if *max_supply == 0 {
                None
            } else {
                Some(*max_supply)
            };
            // The merged DeployNft wire format carries no soulbound /
            // metadata-mutability selector, so native collections default to
            // transferable + mutable metadata. Deploying soulbound proof
            // assets via tx needs a DeployNft field — deferred (see report).
            let (_id, ev) = registry
                .deploy_collection(sender, name, symbol, base_uri, max, true, true, seed)
                .map_err(nft_err_to_sentrix)?;
            ev
        }
        TokenOp::MintNft {
            contract,
            to,
            token_id,
            metadata_uri,
        } => collection_mut(registry, contract)?
            // transferable = None → use the collection default.
            .mint(sender, to, *token_id, metadata_uri, None)
            .map_err(nft_err_to_sentrix)?,
        TokenOp::TransferNft {
            contract,
            from,
            to,
            token_id,
        } => collection_mut(registry, contract)?
            // `from` is untrusted claimed data; the domain checks it against
            // the real owner. Authority is `sender` (caller).
            .transfer(sender, from, to, *token_id)
            .map_err(nft_err_to_sentrix)?,
        TokenOp::BurnNft { contract, token_id } => collection_mut(registry, contract)?
            // Domain clears the token's approval as part of the burn.
            .burn(sender, *token_id)
            .map_err(nft_err_to_sentrix)?,
        TokenOp::ApproveNft {
            contract,
            spender,
            token_id,
        } => collection_mut(registry, contract)?
            .approve(sender, spender, *token_id)
            .map_err(nft_err_to_sentrix)?,
        TokenOp::SetApprovalForAll {
            contract,
            operator,
            approved,
        } => collection_mut(registry, contract)?
            // owner == sender: a caller may only manage operators for tokens
            // it owns. The domain re-enforces caller == owner, so a payload
            // can never set approvals on behalf of a different owner.
            .set_approval_for_all(sender, sender, operator, *approved)
            .map_err(nft_err_to_sentrix)?,
        _ => {
            return Err(SentrixError::InvalidTransaction(
                "unsupported NFT TokenOp in apply path \
                 (SRC-1155 / admin metadata ops deferred)"
                    .into(),
            ));
        }
    };
    Ok(vec![event])
}

/// Map an [`NftEvent`] onto the existing [`TokenOpEvent`] WS/SSE channel.
/// Lossy by design — it reuses the SRC-20 emitter rather than inventing a
/// native NFT event subsystem (deferred). `amount` carries the `token_id`
/// for token-level events (0 for collection-level).
pub fn nft_event_to_token_op_event(ev: &NftEvent, txid: &str, block_height: u64) -> TokenOpEvent {
    let (op, contract, from, to, amount) = match ev {
        NftEvent::CollectionCreated {
            collection_id,
            creator,
            ..
        } => (
            "deploy_nft",
            collection_id,
            creator.clone(),
            String::new(),
            0,
        ),
        NftEvent::TokenMinted {
            collection_id,
            token_id,
            to,
            ..
        } => (
            "mint_nft",
            collection_id,
            String::new(),
            to.clone(),
            *token_id,
        ),
        NftEvent::TokenTransferred {
            collection_id,
            token_id,
            from,
            to,
        } => (
            "transfer_nft",
            collection_id,
            from.clone(),
            to.clone(),
            *token_id,
        ),
        NftEvent::TokenBurned {
            collection_id,
            token_id,
            owner,
        } => (
            "burn_nft",
            collection_id,
            owner.clone(),
            String::new(),
            *token_id,
        ),
        NftEvent::TokenApproved {
            collection_id,
            token_id,
            owner,
            approved,
        } => (
            "approve_nft",
            collection_id,
            owner.clone(),
            approved.clone(),
            *token_id,
        ),
        NftEvent::ApprovalForAll {
            collection_id,
            owner,
            operator,
            approved,
        } => (
            "set_approval_for_all",
            collection_id,
            owner.clone(),
            operator.clone(),
            u64::from(*approved),
        ),
        NftEvent::CollectionUpdated { collection_id }
        | NftEvent::CollectionFrozen { collection_id }
        | NftEvent::CollectionMetadataLocked { collection_id } => (
            "nft_collection_update",
            collection_id,
            String::new(),
            String::new(),
            0,
        ),
        NftEvent::TokenUriUpdated {
            collection_id,
            token_id,
        }
        | NftEvent::TokenFrozen {
            collection_id,
            token_id,
        }
        | NftEvent::TokenUnfrozen {
            collection_id,
            token_id,
        } => (
            "nft_token_update",
            collection_id,
            String::new(),
            String::new(),
            *token_id,
        ),
    };
    TokenOpEvent {
        op: op.to_string(),
        contract: contract.clone(),
        from,
        to,
        amount,
        txid: txid.to_string(),
        block_height,
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

    // ── apply-path dispatch tests ────────────────────────────
    //
    // These exercise `apply_nft_token_op` directly — the exact function the
    // read-only validate, Pass-1 dry-run, and Pass-2 apply layers all call.
    // Testing it here covers the wired apply logic (sender authorization,
    // deterministic seed, soulbound enforcement, strict no-reuse, supply
    // accounting, typed-error mapping) deterministically and without the
    // env-gated full block harness. End-to-end `add_block` wiring (fork gate,
    // snapshot rollback, cross-node determinism) is covered separately in
    // `block_executor.rs`.

    const ADMIN: &str = "0xadmin";
    const ALICE: &str = "0xalice";
    const BOB: &str = "0xbob";

    fn deploy(reg: &mut NftRegistry, seed: &str) -> String {
        let op = TokenOp::DeployNft {
            name: "Proof".into(),
            symbol: "PRF".into(),
            base_uri: "ipfs://Q/".into(),
            max_supply: 0,
        };
        let evs = apply_nft_token_op(reg, &op, ADMIN, seed).expect("deploy");
        match &evs[0] {
            NftEvent::CollectionCreated { collection_id, .. } => collection_id.clone(),
            other => panic!("expected CollectionCreated, got {other:?}"),
        }
    }

    fn mint(
        reg: &mut NftRegistry,
        cid: &str,
        to: &str,
        token_id: u64,
        caller: &str,
    ) -> Result<Vec<NftEvent>, SentrixError> {
        let op = TokenOp::MintNft {
            contract: cid.into(),
            to: to.into(),
            token_id,
            metadata_uri: String::new(),
        };
        apply_nft_token_op(reg, &op, caller, "mintseed")
    }

    fn transfer(
        reg: &mut NftRegistry,
        cid: &str,
        from: &str,
        to: &str,
        id: u64,
        caller: &str,
    ) -> Result<Vec<NftEvent>, SentrixError> {
        let op = TokenOp::TransferNft {
            contract: cid.into(),
            from: from.into(),
            to: to.into(),
            token_id: id,
        };
        apply_nft_token_op(reg, &op, caller, "xferseed")
    }

    // 1. DeployNft through the apply dispatch.
    #[test]
    fn apply_deploy_creates_collection() {
        let mut reg = NftRegistry::new();
        let cid = deploy(&mut reg, "tx1");
        let c = reg.get_collection(&cid).expect("collection");
        assert_eq!(c.creator(), ADMIN);
        assert_eq!(c.admin(), ADMIN);
        assert_eq!(c.name(), "Proof");
        // max_supply 0 → unlimited (None), default transferable + mutable.
        assert_eq!(c.max_supply(), None);
        assert!(c.default_transferable());
    }

    // 2. MintNft through the apply dispatch.
    #[test]
    fn apply_mint_assigns_owner() {
        let mut reg = NftRegistry::new();
        let cid = deploy(&mut reg, "tx1");
        mint(&mut reg, &cid, ALICE, 1, ADMIN).expect("mint");
        assert_eq!(reg.get_collection(&cid).unwrap().owner_of(1), Some(ALICE));
    }

    // 3. TransferNft of a transferable token.
    #[test]
    fn apply_transfer_transferable_moves_owner() {
        let mut reg = NftRegistry::new();
        let cid = deploy(&mut reg, "tx1");
        mint(&mut reg, &cid, ALICE, 1, ADMIN).unwrap();
        transfer(&mut reg, &cid, ALICE, BOB, 1, ALICE).expect("transfer");
        assert_eq!(reg.get_collection(&cid).unwrap().owner_of(1), Some(BOB));
    }

    // 4. Soulbound token cannot transfer through the apply path. The merged
    //    DeployNft wire format can't request soulbound, so the collection is
    //    seeded soulbound via the domain ctor; the mint + transfer still go
    //    through the apply dispatch, proving apply-path enforcement.
    #[test]
    fn apply_transfer_soulbound_rejected() {
        let mut reg = NftRegistry::new();
        let (cid, _) = reg
            .deploy_collection(ADMIN, "Soul", "SBT", "u", None, false, true, "seedsb")
            .unwrap();
        mint(&mut reg, &cid, ALICE, 1, ADMIN).unwrap();
        let err = transfer(&mut reg, &cid, ALICE, BOB, 1, ALICE).unwrap_err();
        assert!(
            matches!(err, SentrixError::InvalidTransaction(ref m) if m.contains("not transferable") || m.contains("transferable")),
            "soulbound transfer must map to a typed rejection, got {err:?}"
        );
        // Ownership unchanged.
        assert_eq!(reg.get_collection(&cid).unwrap().owner_of(1), Some(ALICE));
    }

    // 5. Approved single-token spender can transfer a transferable NFT.
    #[test]
    fn apply_approved_spender_can_transfer() {
        let mut reg = NftRegistry::new();
        let cid = deploy(&mut reg, "tx1");
        mint(&mut reg, &cid, ALICE, 1, ADMIN).unwrap();
        // ALICE approves BOB for token 1.
        let approve = TokenOp::ApproveNft {
            contract: cid.clone(),
            spender: BOB.into(),
            token_id: 1,
        };
        apply_nft_token_op(&mut reg, &approve, ALICE, "aseed").unwrap();
        // BOB (the approved spender) moves it to himself.
        transfer(&mut reg, &cid, ALICE, BOB, 1, BOB).expect("approved transfer");
        assert_eq!(reg.get_collection(&cid).unwrap().owner_of(1), Some(BOB));
    }

    // 6. Operator (approval-for-all) can transfer any of the owner's tokens.
    #[test]
    fn apply_operator_can_transfer() {
        let mut reg = NftRegistry::new();
        let cid = deploy(&mut reg, "tx1");
        mint(&mut reg, &cid, ALICE, 1, ADMIN).unwrap();
        // ALICE sets BOB as operator for all her tokens (owner == sender).
        let op = TokenOp::SetApprovalForAll {
            contract: cid.clone(),
            operator: BOB.into(),
            approved: true,
        };
        apply_nft_token_op(&mut reg, &op, ALICE, "opseed").unwrap();
        transfer(&mut reg, &cid, ALICE, BOB, 1, BOB).expect("operator transfer");
        assert_eq!(reg.get_collection(&cid).unwrap().owner_of(1), Some(BOB));
    }

    // 7. A caller cannot set operator approval on behalf of a different owner:
    //    owner is forced to the sender, so the operator is set for the SENDER,
    //    never for the impersonated victim.
    #[test]
    fn apply_set_approval_for_all_owner_is_sender() {
        let mut reg = NftRegistry::new();
        let cid = deploy(&mut reg, "tx1");
        mint(&mut reg, &cid, ALICE, 1, ADMIN).unwrap();
        // BOB tries to make himself operator of ALICE's tokens. owner is
        // pinned to the sender (BOB), so this grants BOB→operate-on-BOB only.
        let op = TokenOp::SetApprovalForAll {
            contract: cid.clone(),
            operator: "0xcarol".into(),
            approved: true,
        };
        apply_nft_token_op(&mut reg, &op, BOB, "evilseed").expect("sets for BOB only");
        let c = reg.get_collection(&cid).unwrap();
        // Carol is NOT an operator over ALICE.
        assert!(!c.is_approved_for_all(ALICE, "0xcarol"));
        // BOB still cannot move ALICE's token.
        let err = transfer(&mut reg, &cid, ALICE, BOB, 1, BOB).unwrap_err();
        assert!(matches!(err, SentrixError::UnauthorizedValidator(_)));
    }

    // 8. BurnNft works and clears the token's single-token approval.
    #[test]
    fn apply_burn_works_and_clears_approval() {
        let mut reg = NftRegistry::new();
        let cid = deploy(&mut reg, "tx1");
        mint(&mut reg, &cid, ALICE, 1, ADMIN).unwrap();
        let approve = TokenOp::ApproveNft {
            contract: cid.clone(),
            spender: BOB.into(),
            token_id: 1,
        };
        apply_nft_token_op(&mut reg, &approve, ALICE, "aseed").unwrap();
        let burn = TokenOp::BurnNft {
            contract: cid.clone(),
            token_id: 1,
        };
        apply_nft_token_op(&mut reg, &burn, ALICE, "bseed").expect("burn");
        let c = reg.get_collection(&cid).unwrap();
        assert_eq!(c.owner_of(1), None, "burned token has no live owner");
        assert_eq!(c.get_approved(1), None, "burn cleared approval");
    }

    // 9. A burned token id can never be reminted (strict no-reuse).
    #[test]
    fn apply_burned_id_not_reusable() {
        let mut reg = NftRegistry::new();
        let cid = deploy(&mut reg, "tx1");
        mint(&mut reg, &cid, ALICE, 1, ADMIN).unwrap();
        let burn = TokenOp::BurnNft {
            contract: cid.clone(),
            token_id: 1,
        };
        apply_nft_token_op(&mut reg, &burn, ALICE, "bseed").unwrap();
        let err = mint(&mut reg, &cid, BOB, 1, ADMIN).unwrap_err();
        assert!(
            matches!(err, SentrixError::InvalidTransaction(ref m) if m.contains("already")),
            "reusing a burned id must be rejected, got {err:?}"
        );
    }

    // 10. max_supply counts ever-minted; a burn does NOT free a slot.
    #[test]
    fn apply_max_supply_counts_ever_minted() {
        let mut reg = NftRegistry::new();
        let (cid, _) = reg
            .deploy_collection(ADMIN, "Cap", "CAP", "u", Some(1), true, true, "capseed")
            .unwrap();
        mint(&mut reg, &cid, ALICE, 1, ADMIN).unwrap();
        // Burn the only token — supply drops but total_minted stays at the cap.
        let burn = TokenOp::BurnNft {
            contract: cid.clone(),
            token_id: 1,
        };
        apply_nft_token_op(&mut reg, &burn, ALICE, "bseed").unwrap();
        // A second mint must still be refused — the slot is not freed.
        let err = mint(&mut reg, &cid, BOB, 2, ADMIN).unwrap_err();
        assert!(
            matches!(err, SentrixError::InvalidTransaction(ref m) if m.contains("supply") || m.contains("max")),
            "max_supply must count ever-minted, got {err:?}"
        );
    }

    // 11. Metadata-lock / collection-metadata ops are NOT exposed by the
    //     wired SRC-721 TokenOp subset (no UpdateTokenUri / LockMetadata
    //     variant), so the apply path cannot reach them in this PR. The
    //     domain crate covers metadata-lock behavior in its own tests. Here
    //     we assert the apply path refuses an out-of-scope NFT variant rather
    //     than silently doing nothing.
    #[test]
    fn apply_unsupported_nft_variant_rejected() {
        let mut reg = NftRegistry::new();
        // SRC-1155 family is `is_nft_family()` but not wired here.
        let op = TokenOp::DeployMulti {
            name: "M".into(),
            symbol: "M".into(),
            base_uri: "u".into(),
        };
        let err = apply_nft_token_op(&mut reg, &op, ADMIN, "seed").unwrap_err();
        assert!(
            matches!(err, SentrixError::InvalidTransaction(ref m) if m.contains("unsupported"))
        );
    }

    // 12. Deterministic replay: applying the same ops to two fresh registries
    //     yields identical collection ids and identical resulting state.
    #[test]
    fn apply_deterministic_replay() {
        let run = || {
            let mut reg = NftRegistry::new();
            let cid = deploy(&mut reg, "same-txid");
            mint(&mut reg, &cid, ALICE, 7, ADMIN).unwrap();
            transfer(&mut reg, &cid, ALICE, BOB, 7, ALICE).unwrap();
            (cid, reg)
        };
        let (cid_a, reg_a) = run();
        let (cid_b, reg_b) = run();
        assert_eq!(cid_a, cid_b, "collection id must be deterministic");
        // Logical state equality. NftCollection derives PartialEq, whose
        // HashMap comparison is order-independent — the correct notion of
        // determinism here. (Serialized-byte equality is NOT a valid check:
        // HashMap iteration order is per-process; canonical ordering is the
        // deferred state_root work's job, not the in-memory registry's.)
        assert_eq!(
            reg_a.get_collection(&cid_a),
            reg_b.get_collection(&cid_b),
            "replayed collections must be logically identical"
        );
    }

    // 13. An unauthorized action maps to a typed SentrixError. Non-admin mint.
    #[test]
    fn apply_unauthorized_mint_typed_error() {
        let mut reg = NftRegistry::new();
        let cid = deploy(&mut reg, "tx1");
        let err = mint(&mut reg, &cid, ALICE, 1, BOB).unwrap_err(); // BOB != admin
        assert!(
            matches!(err, SentrixError::UnauthorizedValidator(_)),
            "non-admin mint must map to UnauthorizedValidator, got {err:?}"
        );
    }

    // Unknown collection → NotFound (not a panic).
    #[test]
    fn apply_unknown_collection_not_found() {
        let mut reg = NftRegistry::new();
        let err = mint(&mut reg, "SRC721_deadbeef", ALICE, 1, ADMIN).unwrap_err();
        assert!(matches!(err, SentrixError::NotFound(_)));
    }
}
