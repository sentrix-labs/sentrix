//! Fork-height accessors — every consensus-fork activation height the
//! chain reads at runtime, plus the corresponding compile-time defaults.
//!
//! All readers default to `u64::MAX` (disabled). Operators activate a
//! fork by setting the matching env var to a real height in every
//! validator's process environment and halt-all + simultaneous-start
//! the cluster — a mismatch produces a state divergence.
//!
//! Extracted from `crate::blockchain` so this concern lives in one
//! file (was scattered across ~200 lines mid-blockchain.rs). The
//! `pub use` in `blockchain.rs` keeps the existing import paths
//! working bit-identically: `crate::blockchain::get_*_height` still
//! resolves, no caller had to change.

// ── Compile-time defaults (all u64::MAX = disabled, mainnet-safe) ──────

/// Voyager DPoS fork activation. Pre-fork: PoA round-robin. Post-fork:
/// stake-weighted BFT consensus.
const VOYAGER_DPOS_HEIGHT_DEFAULT: u64 = u64::MAX;

/// EVM fork activation. Pre-fork: native-only tx flow. Post-fork:
/// payable EVM tx via revm.
const VOYAGER_EVM_HEIGHT_DEFAULT: u64 = u64::MAX;

/// V4 reward-v2 fork: coinbase → `PROTOCOL_TREASURY`, ClaimRewards
/// dispatch becomes consensus-valid.
const VOYAGER_REWARD_V2_HEIGHT_DEFAULT: u64 = u64::MAX;

/// Tokenomics v2 fork: 126M halving (BTC-parity 4-year) + 315M cap.
/// Replaces v1 emission schedule (42M halving + 210M cap).
const TOKENOMICS_V2_HEIGHT_DEFAULT: u64 = u64::MAX;

/// BFT-gate-relax fork: `active >= ⌈2/3 × total⌉` threshold. Replaces
/// the legacy `active >= MIN_BFT_VALIDATORS` constant. See
/// `audits/jail-cascade-root-cause-analysis.md`.
const BFT_GATE_RELAX_HEIGHT_DEFAULT: u64 = u64::MAX;

/// Phase B consensus-jail dispatch activation. **Known halt risk** —
/// see [`warn_if_jail_consensus_armed`] for the warning that fires at
/// startup when this is non-default.
const JAIL_CONSENSUS_HEIGHT_DEFAULT: u64 = u64::MAX;

/// NFT TokenOp dispatch (SRC-721 + SRC-1155). Wire format stable
/// from this PR; activation gated.
const NFT_TOKENOP_HEIGHT_DEFAULT: u64 = u64::MAX;

/// `StakingOp::AddSelfStake` dispatch. Lets a validator's wallet bond
/// real SRX into its own `self_stake` without the phantom-mint that
/// `force-unjail` produces. Designed as the proper recovery path for
/// slashed validators whose `self_stake < MIN_SELF_STAKE` (the
/// 2026-04-27 self-stake-shortfall incident).
///
/// Post-fork: tx.amount transferred validator → treasury via the outer
/// apply-Pass-2 transfer; `self_stake` incremented in registry. Supply-
/// invariant preserving — no mint.
const ADD_SELF_STAKE_HEIGHT_DEFAULT: u64 = u64::MAX;

/// EVM value-transfer activation. Pre-fork: Pass-2 EVM apply path runs
/// every tx with `TxEnv.value = U256::ZERO` regardless of envelope
/// value (matches v2.1.48 behaviour). Post-fork: revm sees the real
/// envelope value and moves SRX between EOAs / forwards into payable
/// contract calls.
///
/// Why gated: shipped flat in v2.1.49, recurred the eager-write
/// divergence pattern that v2.1.48's FinalizeBlock guard was meant to
/// close. Three 2v2 split-brain halts on 2026-05-01 (h≈1180k / 1191k /
/// 1192k) all followed the same shape — validator-pair A finalizes one
/// hash, validator-pair B the other. Until RCA lands
/// (`audits/2026-05-01-evm-value-transfer-divergence.md`), default
/// disabled mirrors v2.1.48 behaviour.
const EVM_VALUE_TRANSFER_HEIGHT_DEFAULT: u64 = u64::MAX;

/// Audit H3 (2026-05-06): EVM gas-fix fork. Pre-fork the write-path
/// EVM tx flow let revm internally deduct `gas_used × INITIAL_BASE_FEE`
/// from sender's wei balance, and the writeback's wei→sentri floor-div
/// silently dropped that delta (not burned, not credited to
/// validator) — `total_supply()` leaked. Post-fork sets
/// `cfg.disable_base_fee = true` on the write path too so revm skips
/// gas accounting; Pass-1 native `tx.fee` (10K sentri flat) is the
/// entire fee.
const EVM_GAS_FIX_HEIGHT_DEFAULT: u64 = u64::MAX;

/// state_root v2 drift fix (2026-05-07, post-halt #5 RCA). Pre-fork
/// `update_trie_for_block` derives `touched_addrs` from `tx.from` /
/// `tx.to` / `block.validator` + `PROTOCOL_TREASURY` only. That misses
/// every address mutated by the EVM apply path that isn't named in
/// the outer Sentrix tx — CREATE'd contract addresses, internal-CALL
/// recipients, contract storage from internal SSTOREs. AccountDB
/// tracks them, the trie doesn't. Halt at h=1,650,000 on 2026-05-07
/// localised this exactly.
///
/// Post-fork the `touched_addrs` list is augmented with every address
/// in `accounts.touched_in_block`.
const EXTENDED_TOUCH_LIST_HEIGHT_DEFAULT: u64 = u64::MAX;

/// Strict-justification fork (2026-05-07, post-halt #9 RCA). Pre-fork
/// the peer-justification gate verified only stake-weight arithmetic —
/// it summed the `stake_weight` field on each `SignedPrecommit` (sender-
/// supplied number) and checked that against the receiver's local
/// `total_active_stake` threshold. The signatures themselves were
/// never recovered or matched against the claimed validator.
///
/// Post-fork: every precommit signature is recovered with
/// `Precommit::signing_payload_for_height` + `recover_signer` and
/// matched against the claimed validator address. Verified-stake is
/// summed using the receiver's OWN `stake_registry.get_validator(...)`,
/// not the sender's `stake_weight` field.
const STRICT_JUSTIFICATION_HEIGHT_DEFAULT: u64 = u64::MAX;

// ── Runtime readers (env → u64, default to compile-time default) ──────

/// Read Voyager fork height from env, default u64::MAX (mainnet safe).
/// Testnet sets `VOYAGER_FORK_HEIGHT=<height>` in systemd service.
pub fn get_voyager_fork_height() -> u64 {
    std::env::var("VOYAGER_FORK_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(VOYAGER_DPOS_HEIGHT_DEFAULT)
}

/// Read EVM fork height from env, default u64::MAX (disabled).
/// Testnet: set `VOYAGER_EVM_HEIGHT=<height>` in systemd service.
pub fn get_evm_fork_height() -> u64 {
    std::env::var("VOYAGER_EVM_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(VOYAGER_EVM_HEIGHT_DEFAULT)
}

/// V4 Step 3: read reward-v2 hard-fork height from env, default
/// `u64::MAX` (disabled — keeps current accumulator-only behaviour).
/// Post-fork: coinbase → `PROTOCOL_TREASURY`, ClaimRewards dispatch
/// becomes consensus-valid.
pub fn get_reward_v2_fork_height() -> u64 {
    std::env::var("VOYAGER_REWARD_V2_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(VOYAGER_REWARD_V2_HEIGHT_DEFAULT)
}

/// Tokenomics v2: read fork height from env, default `u64::MAX`
/// (disabled — keeps v1 emission schedule: 42M halving + 210M cap).
/// Post-fork: 126M halving (BTC-parity 4-year) + 315M cap.
pub fn get_tokenomics_v2_height() -> u64 {
    std::env::var("TOKENOMICS_V2_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(TOKENOMICS_V2_HEIGHT_DEFAULT)
}

/// BFT-gate-relax: read fork height from env, default `u64::MAX`
/// (disabled — keeps current `active >= MIN_BFT_VALIDATORS` gate).
/// Post-fork: `active >= ⌈2/3 × total⌉` (= 3 for 4-validator network).
/// Fork is optional — operators set `BFT_GATE_RELAX_HEIGHT=<height>`
/// when they want to enable jail-cascade liveness margin. See
/// `audits/jail-cascade-root-cause-analysis.md`.
pub fn get_bft_gate_relax_height() -> u64 {
    std::env::var("BFT_GATE_RELAX_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(BFT_GATE_RELAX_HEIGHT_DEFAULT)
}

/// Phase B: read `JAIL_CONSENSUS_HEIGHT` from env. Default `u64::MAX`
/// (disabled). Activates consensus-computed jail dispatch when set.
pub fn get_jail_consensus_height() -> u64 {
    std::env::var("JAIL_CONSENSUS_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(JAIL_CONSENSUS_HEIGHT_DEFAULT)
}

/// Startup-time guardrail: if `JAIL_CONSENSUS_HEIGHT` is set to anything
/// other than `u64::MAX`, log a loud warning explaining what's at stake.
///
/// Why this exists: the consensus-jail dispatch has a known
/// LivenessTracker non-determinism bug that has halted mainnet twice
/// (h=892799 on 2026-04-29, h=979199 on 2026-04-30). Each incident the
/// recovery loop ends with "set `JAIL_CONSENSUS_HEIGHT=u64::MAX` in
/// env" — but env files are easy to forget about and easy to revert by
/// accident. This warning fires loudly at every startup so the operator
/// can catch a misconfig BEFORE the next epoch boundary triggers the
/// divergence.
///
/// Removing this warning is tied to landing the actual canonical-only
/// LivenessTracker fix (see `audits/AUDIT_REPORT_2026_04_29.md`
/// Phase 3). Until then, any value other than `u64::MAX` is operator
/// risk.
pub fn warn_if_jail_consensus_armed() {
    let height = get_jail_consensus_height();
    if height < u64::MAX {
        tracing::warn!(
            "⚠️  JAIL_CONSENSUS_HEIGHT={} (NOT u64::MAX) — consensus-jail \
             dispatch ARMED. Known to halt mainnet via LivenessTracker \
             non-determinism (incidents: h=892799 / 2026-04-29, h=979199 / \
             2026-04-30). Set env to 18446744073709551615 unless you have \
             explicitly verified the canonical-only LivenessTracker fix \
             has shipped. See internal Sentrix Labs audit (2026-04-29).",
            height
        );
        // Eprintln backup so the warning is also visible without RUST_LOG
        // wired to WARN — defensive against ops accidentally running with
        // info-only logging.
        eprintln!(
            "⚠️  JAIL_CONSENSUS_HEIGHT={} — see warn log above. This is a \
             KNOWN HALT CONDITION. Confirm intent before continuing.",
            height
        );
    }
}

/// Read NFT TokenOp fork height from env, default `u64::MAX` (disabled).
/// Post-fork: SRC-721 + SRC-1155 dispatch active. Operators activate via
/// halt-all + simultaneous-start with `NFT_TOKENOP_HEIGHT=<height>`.
pub fn get_nft_tokenop_height() -> u64 {
    std::env::var("NFT_TOKENOP_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(NFT_TOKENOP_HEIGHT_DEFAULT)
}

/// Read AddSelfStake fork height from env, default `u64::MAX`
/// (disabled). Post-fork: `StakingOp::AddSelfStake` dispatch active —
/// validators can top up their own `self_stake` with real SRX.
/// Operators activate via halt-all + simultaneous-start with
/// `ADD_SELF_STAKE_HEIGHT=<height>`.
pub fn get_add_self_stake_height() -> u64 {
    std::env::var("ADD_SELF_STAKE_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(ADD_SELF_STAKE_HEIGHT_DEFAULT)
}

/// Read EVM value-transfer fork height from env, default `u64::MAX`
/// (disabled — matches v2.1.48 EVM behaviour). Post-fork: Pass-2 EVM
/// apply path threads envelope value into `TxEnv.value` so revm
/// performs real SRX transfers between EOAs and into payable contract
/// calls.
pub fn get_evm_value_transfer_height() -> u64 {
    std::env::var("EVM_VALUE_TRANSFER_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(EVM_VALUE_TRANSFER_HEIGHT_DEFAULT)
}

/// Audit H3 EVM gas-fix fork height (2026-05-06). See
/// [`EVM_GAS_FIX_HEIGHT_DEFAULT`] for context. Default `u64::MAX`
/// keeps the supply-leak behaviour unchanged on chains that haven't
/// activated.
pub fn get_evm_gas_fix_height() -> u64 {
    std::env::var("EVM_GAS_FIX_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(EVM_GAS_FIX_HEIGHT_DEFAULT)
}

/// state_root v2 drift fix fork height (2026-05-07). See
/// [`EXTENDED_TOUCH_LIST_HEIGHT_DEFAULT`] for context. Default
/// `u64::MAX` keeps the legacy behaviour unchanged so v2.1.81 chain
/// history stays bit-identical.
pub fn get_extended_touch_list_height() -> u64 {
    std::env::var("EXTENDED_TOUCH_LIST_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(EXTENDED_TOUCH_LIST_HEIGHT_DEFAULT)
}

/// Strict-justification fork height (2026-05-07). See
/// [`STRICT_JUSTIFICATION_HEIGHT_DEFAULT`] for context. Default
/// `u64::MAX` preserves legacy behaviour bit-identically.
pub fn get_strict_justification_height() -> u64 {
    std::env::var("STRICT_JUSTIFICATION_HEIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(STRICT_JUSTIFICATION_HEIGHT_DEFAULT)
}

// ── Height predicates ────────────────────────────────────
//
// Every fork-height predicate has the same shape:
//   `is_X_height(height) := fork != u64::MAX && height >= fork`
// `u64::MAX` is the unset sentinel — the operator hasn't pinned an
// activation height, so the gate stays closed (returns false) on
// every block. Once the env var is set to a real height, the gate
// flips open at that height.
//
// These used to live as `impl Blockchain { pub fn is_X_height(h) … }`
// inside `blockchain.rs`. Moved here so the const + accessor + gate
// for every fork sit in one module. `Blockchain` keeps thin
// delegating methods for API compat.

/// **Static / env-var only.** Returns true iff the operator set
/// `VOYAGER_FORK_HEIGHT` to a real value AND the height is past it.
/// Default `u64::MAX` makes this return false for all heights —
/// the mainnet-safe-default-pre-activation pattern.
///
/// **Use [`crate::Blockchain::voyager_mode_for`] in consensus paths** —
/// it ORs this check with the runtime persisted `voyager_activated`
/// flag, so post-activation chains don't depend on the env var being
/// set correctly. The 2026-04-26 mainnet stall happened because
/// `validate_block` called this static function: env var was at
/// default `u64::MAX`, function returned false, validate_block fell
/// through to Pioneer auth check, which rejected legitimate Voyager
/// skip-round blocks.
pub fn is_voyager_height(height: u64) -> bool {
    let fork = get_voyager_fork_height();
    fork != u64::MAX && height >= fork
}

/// V4 Step 3: is the given height at or after the reward-v2 fork?
/// Post-fork: coinbase routes to `PROTOCOL_TREASURY`, `ClaimRewards`
/// dispatch is consensus-valid.
pub fn is_reward_v2_height(height: u64) -> bool {
    let fork = get_reward_v2_fork_height();
    fork != u64::MAX && height >= fork
}

/// Tokenomics v2: is the given height at or after the fork?
/// Post-fork: 126M halving + 315M cap (BTC-parity 4-year emission).
pub fn is_tokenomics_v2_height(height: u64) -> bool {
    let fork = get_tokenomics_v2_height();
    fork != u64::MAX && height >= fork
}

/// Phase B (consensus-jail): is the given height at or after the fork?
/// Post-fork: `StakingOp::JailEvidenceBundle` dispatch is consensus-
/// valid; epoch-boundary proposer includes evidence; peers verify and
/// apply jail as on-chain state mutation. Pre-fork: legacy local
/// check_liveness.
pub fn is_jail_consensus_height(height: u64) -> bool {
    let fork = get_jail_consensus_height();
    fork != u64::MAX && height >= fork
}

/// Is the given height at or after the NFT TokenOp fork?
/// Post-fork: SRC-721 + SRC-1155 `TokenOp` variants dispatch.
/// Pre-fork: dispatch rejects (wire format stable, storage layer +
/// REST handlers gated until activation).
pub fn is_nft_tokenop_height(height: u64) -> bool {
    let fork = get_nft_tokenop_height();
    fork != u64::MAX && height >= fork
}

/// Is the given height at or after the `AddSelfStake` fork?
/// Post-fork: `StakingOp::AddSelfStake` dispatch is consensus-valid —
/// validators can bond real SRX into their own self_stake without
/// phantom-mint. Pre-fork: dispatch rejects (wire format stable from
/// the activation PR; gate keeps it dormant until operator rollout).
pub fn is_add_self_stake_height(height: u64) -> bool {
    let fork = get_add_self_stake_height();
    fork != u64::MAX && height >= fork
}

/// EVM value-transfer fork-gate. Pre-fork: Pass-2 EVM apply runs every
/// tx with `TxEnv.value = U256::ZERO` (v2.1.48 behaviour, divergence-
/// free). Post-fork: envelope value flows into revm — value-bearing
/// EVM txs move SRX between accounts. See
/// `EVM_VALUE_TRANSFER_HEIGHT_DEFAULT` for the regression context
/// this gate manages.
pub fn is_evm_value_transfer_height(height: u64) -> bool {
    let fork = get_evm_value_transfer_height();
    fork != u64::MAX && height >= fork
}

/// Audit H3 — true once `EVM_GAS_FIX_HEIGHT` activates. Post-fork the
/// write-path EVM tx skips revm's internal gas accounting
/// (`cfg.disable_base_fee = true`); Pass-1 flat `tx.fee` is the entire
/// fee, supply invariant restored.
pub fn is_evm_gas_fix_height(height: u64) -> bool {
    let fork = get_evm_gas_fix_height();
    fork != u64::MAX && height >= fork
}

/// state_root v2 drift fix — true once `EXTENDED_TOUCH_LIST_HEIGHT`
/// activates. Post-fork `update_trie_for_block` augments the
/// `tx.from`/`tx.to`/validator/+TREASURY touch list with every address
/// in `accounts.touched_in_block` (every AccountDB mutator records
/// there). Closes the EVM-CREATE'd contract + internal CALL trie-vs-
/// AccountDB divergence class.
pub fn is_extended_touch_list_height(height: u64) -> bool {
    let fork = get_extended_touch_list_height();
    fork != u64::MAX && height >= fork
}

/// Strict justification verification — true once
/// `STRICT_JUSTIFICATION_HEIGHT` activates. Post-fork every peer-
/// supplied block runs full crypto verification on its justification
/// precommits (recover signer, match against claimed validator, sum
/// verified stake from receiver's own registry). Closes the chain-
/// fork class identified by halt #9.
pub fn is_strict_justification_height(height: u64) -> bool {
    let fork = get_strict_justification_height();
    fork != u64::MAX && height >= fork
}

/// BFT-gate-relax: is the given height at or after the fork?
/// Post-fork: validator-loop's P1 BFT safety gate uses
/// `active >= ⌈2/3 × total⌉` instead of `active >= MIN_BFT_VALIDATORS
/// (=4)`. For a 4-validator network: gate becomes 3 instead of 4
/// (= 1-jail tolerance). See `audits/jail-cascade-root-cause-
/// analysis.md`.
pub fn is_bft_gate_relax_height(height: u64) -> bool {
    let fork = get_bft_gate_relax_height();
    fork != u64::MAX && height >= fork
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::env_test_lock;

    /// Each fork-height reader is `env::var` parse → fallback. We
    /// exercise the env-set + env-unset paths once for one
    /// representative reader and trust the rest: the implementations
    /// are mechanically identical (same env-var-to-default pattern),
    /// so a single round-trip covers the read pathway.
    #[test]
    fn voyager_fork_height_env_round_trip() {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("VOYAGER_FORK_HEIGHT");
            assert_eq!(get_voyager_fork_height(), u64::MAX);

            std::env::set_var("VOYAGER_FORK_HEIGHT", "12345");
            assert_eq!(get_voyager_fork_height(), 12345);

            // Garbage value falls back to default — no panic.
            std::env::set_var("VOYAGER_FORK_HEIGHT", "not_a_number");
            assert_eq!(get_voyager_fork_height(), u64::MAX);

            std::env::remove_var("VOYAGER_FORK_HEIGHT");
            assert_eq!(get_voyager_fork_height(), u64::MAX);
        }
    }

    /// `warn_if_jail_consensus_armed` is a side-effect-only function
    /// (tracing + eprintln). The behaviour we care about is "don't
    /// panic, don't error" across both armed and disarmed states.
    #[test]
    fn warn_if_jail_consensus_armed_doesnt_panic() {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
            warn_if_jail_consensus_armed(); // disarmed path

            std::env::set_var("JAIL_CONSENSUS_HEIGHT", "42");
            warn_if_jail_consensus_armed(); // armed path — should emit warning

            std::env::remove_var("JAIL_CONSENSUS_HEIGHT");
        }
    }
}
