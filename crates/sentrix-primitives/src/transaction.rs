// transaction.rs - Sentrix

use crate::error::{SentrixError, SentrixResult};
use secp256k1::ecdsa::Signature;
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MIN_TX_FEE: u64 = 10_000; // 0.0001 SRX in sentri
pub const COINBASE_ADDRESS: &str = "COINBASE";
pub const TOKEN_OP_ADDRESS: &str = "0x0000000000000000000000000000000000000000";

/// V4 Step 3 reward-v2 escrow address. After `VOYAGER_REWARD_V2_HEIGHT`
/// activates, coinbase credits go here instead of directly to the
/// proposer's balance. `distribute_reward` updates in-registry
/// accumulators (`pending_rewards`, `delegator_rewards`) which are
/// receivables against this treasury. `StakingOp::ClaimRewards` drains
/// the claimer's accumulator by transferring `PROTOCOL_TREASURY →
/// claimer`.
///
/// No private key exists for this address — `tx.from_address ==
/// PROTOCOL_TREASURY` is rejected at signature-verify time (nothing
/// can sign as treasury). Treasury is drained only via the consensus-
/// level claim dispatch in `block_executor::apply_block_pass2`.
///
/// Supply invariant post-fork:
///   accounts[PROTOCOL_TREASURY] == sum(pending_rewards) + sum(delegator_rewards)
pub const PROTOCOL_TREASURY: &str = "0x0000000000000000000000000000000000000002";

// ── Token operation types (encoded in Transaction.data field) ──
//
// Wire format: JSON-encoded `{"op":"<variant_name_snake_case>", ...fields}`.
// Variants split into 3 family groups:
//   1. Fungible (SRC-20-equivalent): Deploy, Transfer, Burn, Mint, Approve
//   2. NFT (SRC-721-equivalent): Deploy/Mint/Transfer/Burn/Approve/SetApprovalForAll
//      — gated by NFT_TOKENOP_HEIGHT fork. Pre-fork: dispatch rejects.
//   3. Multi-token (SRC-1155-equivalent): DeployMulti/Mint/Transfer/Burn/BatchMint/BatchTransfer
//      — same fork gate.
//
// EVM ERC-20/721/1155 deployed via `eth_sendRawTransaction` is a parallel
// path; the two stacks DO NOT share contract addresses (native contract
// addresses are derived from tx.txid; EVM addresses follow CREATE/CREATE2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TokenOp {
    // ── SRC-20 (fungible) ────────────────────────────────────
    // max_supply=0 means unlimited; #[serde(default)] for backward compatibility with older transactions
    Deploy {
        name: String,
        symbol: String,
        decimals: u8,
        supply: u64,
        #[serde(default)]
        max_supply: u64,
    },
    Transfer {
        contract: String,
        to: String,
        amount: u64,
    },
    Burn {
        contract: String,
        amount: u64,
    },
    Mint {
        contract: String,
        to: String,
        amount: u64,
    },
    Approve {
        contract: String,
        spender: String,
        amount: u64,
    },

    // ── SRC-721 (NFT) ────────────────────────────────────────
    // Gated by NFT_TOKENOP_HEIGHT fork. Pre-fork: dispatch rejects.
    /// Deploy a SRC-721 NFT collection. `max_supply=0` means unlimited.
    /// `base_uri` is the metadata-URI prefix; per-token URI is
    /// `{base_uri}{token_id}`. Mint operations may override by passing
    /// `metadata_uri` explicitly.
    DeployNft {
        name: String,
        symbol: String,
        base_uri: String,
        max_supply: u64,
    },
    /// Mint a single SRC-721 NFT. Owner-only (deployer of the NFT contract).
    /// `metadata_uri` overrides the contract's base_uri convention if non-empty.
    MintNft {
        contract: String,
        to: String,
        token_id: u64,
        #[serde(default)]
        metadata_uri: String,
    },
    /// Transfer a SRC-721 NFT. Sender (`tx.from_address`) must be the
    /// current owner OR an approved operator.
    TransferNft {
        contract: String,
        from: String,
        to: String,
        token_id: u64,
    },
    /// Burn a SRC-721 NFT. Sender must be the current owner.
    BurnNft {
        contract: String,
        token_id: u64,
    },
    /// Approve a single token for transfer by `spender`. Approval cleared
    /// on next successful transfer.
    ApproveNft {
        contract: String,
        spender: String,
        token_id: u64,
    },
    /// Approve / revoke an operator for all tokens owned by `tx.from_address`
    /// in this contract. Mirrors ERC-721's `setApprovalForAll`.
    SetApprovalForAll {
        contract: String,
        operator: String,
        approved: bool,
    },

    // ── SRC-1155 (multi-token) ───────────────────────────────
    // Gated by NFT_TOKENOP_HEIGHT fork (same gate as SRC-721).
    /// Deploy a SRC-1155 multi-token contract. `base_uri` is the prefix for
    /// per-token URIs (`{base_uri}{token_id}`).
    DeployMulti {
        name: String,
        symbol: String,
        base_uri: String,
    },
    /// Mint `amount` units of `token_id` to `to`. Owner-only.
    /// `data` is opaque metadata passed through to receivers (matches ERC-1155).
    MintMulti {
        contract: String,
        to: String,
        token_id: u64,
        amount: u64,
        #[serde(default)]
        data: String,
    },
    /// Transfer `amount` units of `token_id`. Sender must be `from` OR an
    /// approved operator (via `SetApprovalForAll`).
    TransferMulti {
        contract: String,
        from: String,
        to: String,
        token_id: u64,
        amount: u64,
    },
    /// Burn `amount` units of `token_id` held by `from`. Sender must be
    /// `from` OR an approved operator.
    BurnMulti {
        contract: String,
        from: String,
        token_id: u64,
        amount: u64,
    },
    /// Batch-mint multiple `token_id` quantities. Lengths of `ids` and
    /// `amounts` must match. Owner-only.
    BatchMintMulti {
        contract: String,
        to: String,
        ids: Vec<u64>,
        amounts: Vec<u64>,
    },
    /// Batch-transfer multiple `token_id` quantities. Lengths of `ids` and
    /// `amounts` must match. Sender must be `from` OR an approved operator.
    BatchTransferMulti {
        contract: String,
        from: String,
        to: String,
        ids: Vec<u64>,
        amounts: Vec<u64>,
    },
}

impl TokenOp {
    /// Returns true if this variant requires the `NFT_TOKENOP_HEIGHT`
    /// fork to be active (SRC-721 + SRC-1155 family). Pre-fork dispatch
    /// must reject these variants. Used by Pass-1 validation.
    pub fn is_nft_family(&self) -> bool {
        matches!(
            self,
            TokenOp::DeployNft { .. }
                | TokenOp::MintNft { .. }
                | TokenOp::TransferNft { .. }
                | TokenOp::BurnNft { .. }
                | TokenOp::ApproveNft { .. }
                | TokenOp::SetApprovalForAll { .. }
                | TokenOp::DeployMulti { .. }
                | TokenOp::MintMulti { .. }
                | TokenOp::TransferMulti { .. }
                | TokenOp::BurnMulti { .. }
                | TokenOp::BatchMintMulti { .. }
                | TokenOp::BatchTransferMulti { .. }
        )
    }
}

// ── Staking operation types (Voyager Phase 2a) ──────────────
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum StakingOp {
    RegisterValidator {
        self_stake: u64,
        commission_rate: u16,
        public_key: String,
    },
    Delegate {
        validator: String,
        amount: u64,
    },
    Undelegate {
        validator: String,
        amount: u64,
    },
    Redelegate {
        from_validator: String,
        to_validator: String,
        amount: u64,
    },
    Unjail,
    /// V4 Step 3: delegator claims accumulated rewards.
    /// No parameters — sender address (tx.from) is the delegator.
    /// Apply-block flow drains `stake_registry.delegator_rewards[sender]`
    /// into the delegator's SRX balance. Dispatch wire still pending —
    /// the StakingOp enum has multiple variants defined (Delegate,
    /// Undelegate, Redelegate, Unjail, SubmitEvidence, ClaimRewards)
    /// but no apply-block dispatch implementation yet. Shipping the
    /// variant now so the wire format is stable ahead of dispatch.
    ClaimRewards,
    SubmitEvidence {
        height: u64,
        block_hash_a: String,
        block_hash_b: String,
        signature_a: String,
        signature_b: String,
    },
    /// Phase A: Consensus-computed jail evidence (data plumbing only —
    /// no dispatch wired yet). Activated post `JAIL_CONSENSUS_HEIGHT` fork
    /// (separate from BFT_GATE_RELAX_HEIGHT). At epoch boundary, the
    /// proposer scans the last LIVENESS_WINDOW blocks' justification.precommits,
    /// computes per-validator (signed_count, missed_count), includes the
    /// JailEvidence list in the boundary block as this StakingOp variant.
    /// Other validators Pass-1-validate (recompute independently from same
    /// blocks; reject block if cited evidence doesn't match).
    /// See `audits/consensus-computed-jail-design.md`.
    JailEvidenceBundle {
        epoch: u64,
        epoch_start_block: u64,
        epoch_end_block: u64,
        evidence: Vec<JailEvidence>,
    },
    /// Add real SRX to an existing validator's self_stake without
    /// phantom-mint. Only the validator itself (`tx.from_address ==
    /// validator address`) may execute. The outer `accounts.transfer`
    /// in apply-Pass-2 routes `tx.amount` from validator → treasury
    /// as the escrow move; dispatch then increments `self_stake` by
    /// the same amount. Supply-invariant preserving: SRX moves from
    /// circulating balance into bonded stake, no mint.
    ///
    /// Designed as the proper recovery path for slashed validators
    /// whose `self_stake < MIN_SELF_STAKE`, replacing the break-glass
    /// `force-unjail` (which mints phantom SRX). After AddSelfStake
    /// brings self_stake back ≥ MIN_SELF_STAKE, the standard
    /// `validator unjail` admin op (or the future `StakingOp::Unjail`
    /// tx-form once dispatched) clears the jail flag without supply
    /// damage.
    ///
    /// Fork-gated by `ADD_SELF_STAKE_HEIGHT` (env-controlled, default
    /// `u64::MAX` = disabled). Wire format stable on enum-add; gate
    /// check happens in dispatch.
    AddSelfStake { amount: u64 },
}

/// Phase A data type: per-validator missed-block evidence for an epoch.
/// Self-validating: peers recompute by scanning chain for the same window
/// and comparing signed_count and missed_count. justification_hashes lets
/// peers selectively verify missed-block claims.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JailEvidence {
    pub validator: String,
    pub signed_count: u64,
    pub missed_count: u64,
    /// Hashes of blocks where validator was in active_set but missing
    /// from precommits. Subset (full list optional for size; min 1 for
    /// proof-of-evidence).
    pub justification_hashes: Vec<String>,
}

impl StakingOp {
    pub fn encode(&self) -> SentrixResult<String> {
        serde_json::to_string(self).map_err(|e| SentrixError::InvalidTransaction(e.to_string()))
    }

    pub fn decode(data: &str) -> Option<Self> {
        serde_json::from_str(data).ok()
    }

    pub fn is_staking_op(data: &str) -> bool {
        Self::decode(data).is_some()
    }
}

pub const STAKING_ADDRESS: &str = "0x0000000000000000000000000000000000000100";

impl TokenOp {
    pub fn encode(&self) -> SentrixResult<String> {
        serde_json::to_string(self).map_err(|e| SentrixError::InvalidTransaction(e.to_string()))
    }

    pub fn decode(data: &str) -> Option<Self> {
        serde_json::from_str(data).ok()
    }

    pub fn is_token_op(data: &str) -> bool {
        // Use the full decoder rather than naive string matching to correctly identify token op transactions
        Self::decode(data).is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub txid: String,
    pub from_address: String,
    pub to_address: String,
    pub amount: u64, // sentri
    pub fee: u64,    // sentri
    pub nonce: u64,
    pub data: String,
    pub timestamp: u64,     // unix timestamp seconds
    pub chain_id: u64,      // replay protection across chains
    pub signature: String,  // hex encoded
    pub public_key: String, // hex encoded
}

impl Transaction {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        from_address: String,
        to_address: String,
        amount: u64,
        fee: u64,
        nonce: u64,
        data: String,
        chain_id: u64,
        secret_key: &SecretKey,
        public_key: &PublicKey,
    ) -> SentrixResult<Self> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let public_key_hex = hex::encode(public_key.serialize_uncompressed());

        let mut tx = Self {
            txid: String::new(),
            from_address,
            to_address,
            amount,
            fee,
            nonce,
            data,
            timestamp,
            chain_id,
            signature: String::new(),
            public_key: public_key_hex,
        };

        let payload = tx.signing_payload();
        let secp = Secp256k1::signing_only();
        let msg = Self::payload_to_message(&payload)?;
        let sig = secp.sign_ecdsa(msg, secret_key);
        tx.signature = hex::encode(sig.serialize_compact());
        tx.txid = tx.compute_txid();

        Ok(tx)
    }

    pub fn new_coinbase(
        to_address: String,
        amount: u64,
        block_index: u64,
        block_timestamp: u64,
    ) -> Self {
        let mut tx = Self {
            txid: String::new(),
            from_address: COINBASE_ADDRESS.to_string(),
            to_address,
            amount,
            fee: 0,
            nonce: 0,
            data: format!("block_{}", block_index),
            timestamp: block_timestamp,
            chain_id: 0,
            signature: String::new(),
            public_key: String::new(),
        };
        tx.txid = tx.compute_txid();
        tx
    }

    /// Phase D: build a system-emitted JailEvidenceBundle transaction. Sender
    /// is `PROTOCOL_TREASURY`; signature/public_key empty (auth via Pass-1
    /// recompute-and-compare in dispatch). The op payload is JSON-encoded
    /// `StakingOp::JailEvidenceBundle`. This tx is only valid in finalized
    /// blocks at epoch boundaries when post-fork.
    pub fn new_jail_evidence_bundle(
        op: StakingOp,
        block_index: u64,
        block_timestamp: u64,
    ) -> SentrixResult<Self> {
        // Defensive: only accept the JailEvidenceBundle variant
        if !matches!(op, StakingOp::JailEvidenceBundle { .. }) {
            return Err(SentrixError::InvalidTransaction(
                "new_jail_evidence_bundle: op must be StakingOp::JailEvidenceBundle".into(),
            ));
        }
        let data = serde_json::to_string(&op).map_err(|e| {
            SentrixError::InvalidTransaction(format!(
                "new_jail_evidence_bundle: serialize failed: {e}"
            ))
        })?;
        let mut tx = Self {
            txid: String::new(),
            from_address: PROTOCOL_TREASURY.to_string(),
            to_address: PROTOCOL_TREASURY.to_string(),
            amount: 0,
            fee: 0,
            nonce: block_index,
            data,
            timestamp: block_timestamp,
            chain_id: 0,
            signature: String::new(),
            public_key: String::new(),
        };
        tx.txid = tx.compute_txid();
        Ok(tx)
    }

    pub fn is_coinbase(&self) -> bool {
        self.from_address == COINBASE_ADDRESS
    }

    /// Returns true if this is an EVM transaction (originated from eth_sendRawTransaction).
    /// Format: data starts with "EVM:" and signature contains the original RLP-encoded tx.
    pub fn is_evm_tx(&self) -> bool {
        self.data.starts_with("EVM:")
    }

    /// Returns true if this tx carries a `StakingOp::JailEvidenceBundle` payload.
    /// Used by Phase D consensus-jail emission — system-emitted txs at epoch
    /// boundaries when post-fork. Auth via PROTOCOL_TREASURY sender + Pass-1
    /// recompute-and-compare in dispatch.
    pub fn is_jail_evidence_bundle_tx(&self) -> bool {
        // Cheap pre-check: StakingOp uses #[serde(tag = "op", rename_all =
        // "snake_case")], so the variant tag in JSON is "jail_evidence_bundle".
        if !self.data.contains("\"jail_evidence_bundle\"") {
            return false;
        }
        // Only accept if payload is well-formed StakingOp::JailEvidenceBundle
        matches!(
            serde_json::from_str::<StakingOp>(&self.data),
            Ok(StakingOp::JailEvidenceBundle { .. })
        )
    }

    /// Returns true if this is a Phase D system-emitted tx (PROTOCOL_TREASURY
    /// sender + JailEvidenceBundle payload). Used by validate / Pass-1 / Pass-2
    /// to skip standard nonce/balance/fee/signature checks; auth happens via
    /// consensus dispatch (recompute-and-compare in block_executor).
    pub fn is_system_tx(&self) -> bool {
        self.from_address == PROTOCOL_TREASURY && self.is_jail_evidence_bundle_tx()
    }

    // Canonical signing payload uses BTreeMap for deterministic key ordering across all nodes
    // and serde_json for proper escaping of special characters
    pub fn signing_payload(&self) -> String {
        let mut map = std::collections::BTreeMap::new();
        map.insert("amount", serde_json::Value::from(self.amount));
        map.insert("chain_id", serde_json::Value::from(self.chain_id));
        map.insert("data", serde_json::Value::from(self.data.as_str()));
        map.insert("fee", serde_json::Value::from(self.fee));
        map.insert("from", serde_json::Value::from(self.from_address.as_str()));
        map.insert("nonce", serde_json::Value::from(self.nonce));
        map.insert("timestamp", serde_json::Value::from(self.timestamp));
        map.insert("to", serde_json::Value::from(self.to_address.as_str()));
        // M-06: the previous `unwrap_or_else(|_| "{}")` silently replaced
        // a serialisation failure with an empty JSON object. That would
        // have made every such tx share the identical txid (hash of "{}")
        // and identical signing payload, which is a replay-protection
        // nightmare. The BTreeMap here is a fixed set of owned, serde-
        // clean values — `to_string` can only fail on OOM or programmer
        // error, and both warrant a loud crash rather than a
        // silently-wrong payload. `expect` is deliberately chosen over
        // `unwrap_or_default` because "" would be equally broken.
        #[allow(clippy::expect_used)]
        {
            serde_json::to_string(&map)
                .expect("signing_payload: BTreeMap of scalar fields must always serialise")
        }
    }

    pub fn compute_txid(&self) -> String {
        let payload = self.signing_payload();
        let mut hasher = Sha256::new();
        hasher.update(payload.as_bytes());
        hex::encode(hasher.finalize())
    }

    fn payload_to_message(payload: &str) -> SentrixResult<Message> {
        let mut hasher = Sha256::new();
        hasher.update(payload.as_bytes());
        let hash: [u8; 32] = hasher.finalize().into();
        Ok(Message::from_digest(hash))
    }

    pub fn verify(&self) -> SentrixResult<()> {
        if self.is_coinbase() {
            // Coinbase transactions have empty signature and public_key — no private key signs block rewards
            if !self.signature.is_empty() || !self.public_key.is_empty() {
                return Err(SentrixError::InvalidTransaction(
                    "coinbase transaction must not have signature or public_key".to_string(),
                ));
            }
            return Ok(());
        }

        // EVM transactions are pre-verified at the JSON-RPC layer via Ethereum
        // signature recovery. The original RLP-encoded tx is stored in `signature`
        // for re-verification at block validation time.
        if self.is_evm_tx() {
            return Ok(());
        }

        // Phase D: system-emitted JailEvidenceBundle txs (consensus-applied
        // jail decisions) bypass signature verification. Authorization model:
        // - tx.from_address == PROTOCOL_TREASURY (system actor)
        // - tx.data is JSON-encoded StakingOp::JailEvidenceBundle
        // - signature + public_key empty
        // - dispatch (block_executor) verifies evidence cryptographically by
        //   recomputing from local LivenessTracker — non-matching evidence
        //   rejects the block at Pass-1
        // Auth via consensus: this op type only valid in finalized blocks
        // (BFT supermajority signs block, all participants verified evidence).
        if self.from_address == PROTOCOL_TREASURY && self.is_jail_evidence_bundle_tx() {
            if !self.signature.is_empty() || !self.public_key.is_empty() {
                return Err(SentrixError::InvalidTransaction(
                    "JailEvidenceBundle system tx must not have signature or public_key".into(),
                ));
            }
            return Ok(());
        }

        let pub_key_bytes =
            hex::decode(&self.public_key).map_err(|_| SentrixError::InvalidSignature)?;
        let secp = Secp256k1::verification_only();
        let public_key =
            PublicKey::from_slice(&pub_key_bytes).map_err(|_| SentrixError::InvalidSignature)?;

        // Verify the public key cryptographically derives to from_address — prevents key substitution
        let derived_address = crate::address::derive_address(&public_key);
        if derived_address != self.from_address {
            return Err(SentrixError::InvalidTransaction(format!(
                "public key does not match from_address: expected {}, derived {}",
                self.from_address, derived_address
            )));
        }

        let sig_bytes = hex::decode(&self.signature).map_err(|_| SentrixError::InvalidSignature)?;
        let sig =
            Signature::from_compact(&sig_bytes).map_err(|_| SentrixError::InvalidSignature)?;

        let payload = self.signing_payload();
        let msg = Self::payload_to_message(&payload)?;
        secp.verify_ecdsa(msg, &sig, &public_key)
            .map_err(|_| SentrixError::InvalidSignature)?;

        Ok(())
    }

    pub fn validate(&self, expected_nonce: u64, expected_chain_id: u64) -> SentrixResult<()> {
        if self.is_coinbase() {
            return Ok(());
        }

        // Phase D: system-emitted txs (JailEvidenceBundle from PROTOCOL_TREASURY)
        // bypass standard validation. Their auth is consensus-driven: dispatch
        // verifies the evidence list against the local LivenessTracker recompute.
        if self.is_system_tx() {
            return Ok(());
        }

        if self.fee < MIN_TX_FEE {
            return Err(SentrixError::InvalidTransaction(format!(
                "fee {} below minimum {}",
                self.fee, MIN_TX_FEE
            )));
        }

        // amount=0 is allowed for token operations, EVM contract calls,
        // AND staking operations (data field carries op/calldata). The
        // staking-op exemption was missed when StakingOp dispatch landed —
        // surfaced 2026-04-26 when the first ClaimRewards tx was rejected
        // here despite being a valid op (data = `{"op":"claim_rewards"}`).
        // ClaimRewards specifically has tx.amount=0 because the apply-time
        // payout transfers from PROTOCOL_TREASURY → claimer; no on-tx
        // amount needed. Same exemption applies to other no-fund-movement
        // staking ops (Unjail, SubmitEvidence).
        if self.amount == 0
            && !TokenOp::is_token_op(&self.data)
            && !self.is_evm_tx()
            && !StakingOp::is_staking_op(&self.data)
        {
            return Err(SentrixError::InvalidTransaction(
                "amount must be > 0 (unless token/EVM/staking operation)".to_string(),
            ));
        }

        if self.nonce != expected_nonce {
            return Err(SentrixError::InvalidNonce {
                expected: expected_nonce,
                got: self.nonce,
            });
        }

        // Chain ID replay protection
        if self.chain_id != expected_chain_id {
            return Err(SentrixError::InvalidTransaction(format!(
                "wrong chain_id: expected {}, got {}",
                expected_chain_id, self.chain_id
            )));
        }

        self.verify()?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_CHAIN_ID: u64 = 7119;

    fn make_keypair() -> (SecretKey, PublicKey) {
        let secp = Secp256k1::new();
        let mut rng = secp256k1::rand::rng();
        secp.generate_keypair(&mut rng)
    }

    fn derive_addr(pk: &PublicKey) -> String {
        crate::address::derive_address(pk)
    }

    #[test]
    fn test_coinbase_transaction() {
        let tx =
            Transaction::new_coinbase("SRX_validator".to_string(), 100_000_000, 1, 1_712_620_800);
        assert!(tx.is_coinbase());
        assert_eq!(tx.amount, 100_000_000);
        assert!(!tx.txid.is_empty());
    }

    #[test]
    fn test_sign_and_verify() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);
        let tx = Transaction::new(
            from,
            "SRX_bob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            TEST_CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        assert!(tx.verify().is_ok());
        assert!(!tx.txid.is_empty());
        assert!(!tx.signature.is_empty());
    }

    #[test]
    fn test_validate_correct_nonce() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);
        let tx = Transaction::new(
            from,
            "SRX_bob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            TEST_CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        assert!(tx.validate(0, TEST_CHAIN_ID).is_ok());
    }

    #[test]
    fn test_validate_wrong_nonce() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);
        let tx = Transaction::new(
            from,
            "SRX_bob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            TEST_CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        assert!(tx.validate(1, TEST_CHAIN_ID).is_err());
    }

    #[test]
    fn test_validate_wrong_chain_id() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);
        let tx = Transaction::new(
            from,
            "SRX_bob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            TEST_CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        assert!(tx.validate(0, 9999).is_err()); // wrong chain
    }

    #[test]
    fn test_validate_fee_too_low() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);
        let tx = Transaction::new(
            from,
            "SRX_bob".to_string(),
            1_000_000,
            1,
            0,
            String::new(),
            TEST_CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        assert!(tx.validate(0, TEST_CHAIN_ID).is_err());
    }

    #[test]
    fn test_tampered_signature_fails() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);
        let mut tx = Transaction::new(
            from,
            "SRX_bob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            TEST_CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        tx.amount = 999_999_999;
        assert!(tx.verify().is_err());
    }

    #[test]
    fn test_c01_verify_rejects_mismatched_address() {
        let (sk, pk) = make_keypair();
        let real_address = derive_addr(&pk);

        // Create valid tx with correct from_address
        let mut tx = Transaction::new(
            real_address.clone(),
            "SRX_bob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            TEST_CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        assert!(tx.verify().is_ok());

        // Tamper from_address to a different address
        tx.from_address = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string();
        // Should fail: public key doesn't match from_address
        assert!(tx.verify().is_err());
    }

    #[test]
    fn test_h01_signing_payload_canonical_and_escaped() {
        let (sk, pk) = make_keypair();
        let from = derive_addr(&pk);

        // Test deterministic: same inputs → same payload
        let tx1 = Transaction::new(
            from.clone(),
            "0xbob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            TEST_CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        let _tx2 = Transaction::new(
            from.clone(),
            "0xbob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            String::new(),
            TEST_CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        // Timestamps differ, but let's verify the format is valid JSON
        let payload = tx1.signing_payload();
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["amount"], 1_000_000);
        assert_eq!(parsed["chain_id"], TEST_CHAIN_ID);
        assert_eq!(parsed["from"], from);

        // Test special chars in data field are properly escaped (not injected)
        let tx_xss = Transaction::new(
            from.clone(),
            "0xbob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            r#"<script>alert("xss")</script>"#.to_string(),
            TEST_CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        let payload_xss = tx_xss.signing_payload();
        // Must be valid JSON — serde_json escapes the quotes in data field
        let parsed_xss: serde_json::Value = serde_json::from_str(&payload_xss).unwrap();
        assert_eq!(parsed_xss["data"], r#"<script>alert("xss")</script>"#);

        // Test with quote injection attempt in data
        let tx_inject = Transaction::new(
            from.clone(),
            "0xbob".to_string(),
            1_000_000,
            MIN_TX_FEE,
            0,
            r#"","fee":0,"from":"attacker"#.to_string(),
            TEST_CHAIN_ID,
            &sk,
            &pk,
        )
        .unwrap();
        let payload_inject = tx_inject.signing_payload();
        // Must still be valid JSON with the injection attempt as a plain string value
        let parsed_inject: serde_json::Value = serde_json::from_str(&payload_inject).unwrap();
        assert!(parsed_inject["data"].as_str().unwrap().contains("attacker"));
        // The "from" field must still be the real address, not "attacker"
        assert_eq!(parsed_inject["from"], from);
    }

    // ── JailEvidenceBundle (Phase A — consensus-jail data plumbing) ──

    #[test]
    fn test_jail_evidence_bundle_encode_decode_roundtrip() {
        let bundle = StakingOp::JailEvidenceBundle {
            epoch: 42,
            epoch_start_block: 590100,
            epoch_end_block: 604499,
            evidence: vec![
                JailEvidence {
                    validator: "0x87c9976d4b2e360b9fbb87e4bd5442edce2a7511".into(),
                    signed_count: 3000,
                    missed_count: 11400,
                    justification_hashes: vec!["abc123".into(), "def456".into()],
                },
            ],
        };

        let encoded = bundle.encode().expect("encode");
        let decoded = StakingOp::decode(&encoded).expect("decode");
        match decoded {
            StakingOp::JailEvidenceBundle {
                epoch,
                epoch_start_block,
                epoch_end_block,
                evidence,
            } => {
                assert_eq!(epoch, 42);
                assert_eq!(epoch_start_block, 590100);
                assert_eq!(epoch_end_block, 604499);
                assert_eq!(evidence.len(), 1);
                assert_eq!(
                    evidence[0].validator,
                    "0x87c9976d4b2e360b9fbb87e4bd5442edce2a7511"
                );
                assert_eq!(evidence[0].signed_count, 3000);
                assert_eq!(evidence[0].missed_count, 11400);
                assert_eq!(evidence[0].justification_hashes.len(), 2);
            }
            other => panic!("expected JailEvidenceBundle, got {:?}", other),
        }
    }

    #[test]
    fn test_jail_evidence_serialization_uses_snake_case_op_tag() {
        let bundle = StakingOp::JailEvidenceBundle {
            epoch: 1,
            epoch_start_block: 0,
            epoch_end_block: 14400,
            evidence: vec![],
        };
        let encoded = bundle.encode().expect("encode");
        // Per #[serde(tag = "op", rename_all = "snake_case")] on StakingOp,
        // the variant tag should be "jail_evidence_bundle".
        assert!(
            encoded.contains("\"op\":\"jail_evidence_bundle\""),
            "expected snake_case op tag, got: {encoded}"
        );
    }

    #[test]
    fn test_jail_evidence_is_staking_op() {
        let bundle = StakingOp::JailEvidenceBundle {
            epoch: 1,
            epoch_start_block: 0,
            epoch_end_block: 14400,
            evidence: vec![],
        };
        let encoded = bundle.encode().expect("encode");
        assert!(StakingOp::is_staking_op(&encoded));
    }

    #[test]
    fn test_add_self_stake_encode_decode_roundtrip() {
        let op = StakingOp::AddSelfStake { amount: 1_500_000_000 };
        let encoded = op.encode().expect("encode");
        let decoded = StakingOp::decode(&encoded).expect("decode");
        match decoded {
            StakingOp::AddSelfStake { amount } => {
                assert_eq!(amount, 1_500_000_000);
            }
            other => panic!("expected AddSelfStake, got {other:?}"),
        }
    }

    #[test]
    fn test_add_self_stake_serialization_uses_snake_case_op_tag() {
        let op = StakingOp::AddSelfStake { amount: 42 };
        let encoded = op.encode().expect("encode");
        // Per #[serde(tag = "op", rename_all = "snake_case")] the variant
        // tag must be "add_self_stake" — wire format is consensus-load-bearing.
        assert!(
            encoded.contains("\"op\":\"add_self_stake\""),
            "expected snake_case op tag, got: {encoded}"
        );
        assert!(
            encoded.contains("\"amount\":42"),
            "expected amount field present, got: {encoded}"
        );
    }

    #[test]
    fn test_add_self_stake_is_staking_op() {
        let op = StakingOp::AddSelfStake { amount: 100 };
        let encoded = op.encode().expect("encode");
        assert!(StakingOp::is_staking_op(&encoded));
    }

    #[test]
    fn test_jail_evidence_struct_equality() {
        let a = JailEvidence {
            validator: "0xval1".into(),
            signed_count: 100,
            missed_count: 50,
            justification_hashes: vec!["h1".into()],
        };
        let b = a.clone();
        assert_eq!(a, b, "JailEvidence must implement PartialEq for testing");
    }

    // ── SRC-721 / SRC-1155 TokenOp wire-format tests ──────────────

    #[test]
    fn test_nft_deploy_round_trip() {
        let op = TokenOp::DeployNft {
            name: "Test NFT".into(),
            symbol: "TNFT".into(),
            base_uri: "ipfs://Qm/".into(),
            max_supply: 10_000,
        };
        let encoded = op.encode().expect("encode");
        assert!(encoded.contains("\"op\":\"deploy_nft\""));
        let decoded = TokenOp::decode(&encoded).expect("decode");
        assert!(matches!(decoded, TokenOp::DeployNft { .. }));
    }

    #[test]
    fn test_nft_mint_round_trip() {
        let op = TokenOp::MintNft {
            contract: "0xnft1".into(),
            to: "0xrecipient".into(),
            token_id: 42,
            metadata_uri: "ipfs://Qm/42.json".into(),
        };
        let encoded = op.encode().expect("encode");
        assert!(encoded.contains("\"op\":\"mint_nft\""));
        let decoded = TokenOp::decode(&encoded).expect("decode");
        assert!(matches!(decoded, TokenOp::MintNft { token_id: 42, .. }));
    }

    #[test]
    fn test_set_approval_for_all_round_trip() {
        let op = TokenOp::SetApprovalForAll {
            contract: "0xnft1".into(),
            operator: "0xop".into(),
            approved: true,
        };
        let encoded = op.encode().expect("encode");
        assert!(encoded.contains("\"op\":\"set_approval_for_all\""));
        let decoded = TokenOp::decode(&encoded).expect("decode");
        assert!(matches!(decoded, TokenOp::SetApprovalForAll { approved: true, .. }));
    }

    #[test]
    fn test_multi_batch_mint_round_trip() {
        let op = TokenOp::BatchMintMulti {
            contract: "0xmt1".into(),
            to: "0xrecipient".into(),
            ids: vec![1, 2, 3],
            amounts: vec![10, 20, 30],
        };
        let encoded = op.encode().expect("encode");
        assert!(encoded.contains("\"op\":\"batch_mint_multi\""));
        let decoded = TokenOp::decode(&encoded).expect("decode");
        assert!(matches!(decoded, TokenOp::BatchMintMulti { .. }));
    }

    #[test]
    fn test_is_nft_family_predicate() {
        // SRC-20 fungible variants → NOT NFT family
        assert!(!TokenOp::Deploy {
            name: "x".into(), symbol: "X".into(), decimals: 8, supply: 0, max_supply: 0
        }.is_nft_family());
        assert!(!TokenOp::Transfer {
            contract: "c".into(), to: "t".into(), amount: 1
        }.is_nft_family());

        // SRC-721 variants → NFT family
        assert!(TokenOp::DeployNft {
            name: "n".into(), symbol: "N".into(), base_uri: "u".into(), max_supply: 0
        }.is_nft_family());
        assert!(TokenOp::MintNft {
            contract: "c".into(), to: "t".into(), token_id: 1, metadata_uri: String::new()
        }.is_nft_family());
        assert!(TokenOp::SetApprovalForAll {
            contract: "c".into(), operator: "o".into(), approved: false
        }.is_nft_family());

        // SRC-1155 variants → NFT family
        assert!(TokenOp::DeployMulti {
            name: "n".into(), symbol: "N".into(), base_uri: "u".into()
        }.is_nft_family());
        assert!(TokenOp::BatchTransferMulti {
            contract: "c".into(), from: "f".into(), to: "t".into(),
            ids: vec![1], amounts: vec![1]
        }.is_nft_family());
    }

    #[test]
    fn test_nft_op_decodes_via_token_op_enum() {
        // Proves wire format is stable: a JSON-encoded NFT op decodes
        // through TokenOp::decode without requiring a separate enum.
        let json = r#"{"op":"transfer_nft","contract":"0xc","from":"0xf","to":"0xt","token_id":7}"#;
        let decoded = TokenOp::decode(json).expect("decode");
        match decoded {
            TokenOp::TransferNft { token_id, .. } => assert_eq!(token_id, 7),
            _ => panic!("expected TransferNft variant"),
        }
    }
}
