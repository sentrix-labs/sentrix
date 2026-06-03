//! Fork-height accessors — every consensus-fork activation height the
//! chain reads at runtime, plus the corresponding compile-time defaults.
//!
//! Each reader returns its chain-level compile-time default unless the
//! matching env var is set, which then takes precedence. Most defaults
//! remain `u64::MAX` (un-activated on mainnet); the ones that have
//! already activated on mainnet carry the real activation height as
//! their default so a fresh validator (or any rebuild from main)
//! computes the same state_root past the fork. Testnet has deterministic
//! defaults for mature forks already activated through the docker env
//! files. Mismatched activation heights across validators = state
//! divergence — change defaults only after the fork has actually crossed
//! on that chain.
//!
//! Extracted from `crate::blockchain` so this concern lives in one
//! file (was scattered across ~200 lines mid-blockchain.rs). The
//! `pub use` in `blockchain.rs` keeps the existing import paths
//! working bit-identically: `crate::blockchain::get_*_height` still
//! resolves, no caller had to change.

// ── Compile-time defaults — see module doc for activated-vs-disabled. ──

const MAINNET_CHAIN_ID: u64 = 7119;
const TESTNET_CHAIN_ID: u64 = 7120;

/// Voyager DPoS fork activation. Pre-fork: PoA round-robin. Post-fork:
/// stake-weighted BFT consensus.
const VOYAGER_DPOS_HEIGHT_DEFAULT: u64 = u64::MAX;
const VOYAGER_DPOS_HEIGHT_TESTNET_DEFAULT: u64 = 10;

/// EVM fork activation. Pre-fork: native-only tx flow. Post-fork:
/// payable EVM tx via revm.
const VOYAGER_EVM_HEIGHT_DEFAULT: u64 = u64::MAX;
const VOYAGER_EVM_HEIGHT_TESTNET_DEFAULT: u64 = 752;

/// V4 reward-v2 fork: coinbase → `PROTOCOL_TREASURY`, ClaimRewards
/// dispatch becomes consensus-valid.
const VOYAGER_REWARD_V2_HEIGHT_DEFAULT: u64 = u64::MAX;
const VOYAGER_REWARD_V2_HEIGHT_TESTNET_DEFAULT: u64 = 100;

/// Tokenomics v2 fork: 126M halving (BTC-parity 4-year) + 315M cap.
/// Replaces v1 emission schedule (42M halving + 210M cap).
const TOKENOMICS_V2_HEIGHT_DEFAULT: u64 = u64::MAX;
const TOKENOMICS_V2_HEIGHT_TESTNET_DEFAULT: u64 = 381_651;

/// BFT-gate-relax fork: `active >= ⌈2/3 × total⌉` threshold. Replaces
/// the legacy `active >= MIN_BFT_VALIDATORS` constant. See
/// `audits/jail-cascade-root-cause-analysis.md`.
const BFT_GATE_RELAX_HEIGHT_DEFAULT: u64 = u64::MAX;
const BFT_GATE_RELAX_HEIGHT_TESTNET_DEFAULT: u64 = 551_500;

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
///
/// Mainnet stays disabled (u64::MAX) until a planned activation window —
/// 2026-05-30 testnet stall recovery left val3 jailed below MIN_SELF_STAKE,
/// motivating early testnet activation. Mainnet operators should pick an
/// activation height during a scheduled halt-all + simul-start window, then
/// update this constant in a separate PR before redeploy.
const ADD_SELF_STAKE_HEIGHT_DEFAULT: u64 = u64::MAX;
/// Testnet activates AddSelfStake at h=5,800,000 — chosen 2026-05-30 while
/// testnet was at h=5,783,883 (~16K blocks / ~4.5h @ 1s/blk buffer). Closes
/// the unjail recovery path for val3 (jailed at 14,985 SRX self-stake after
/// the 0.1% liveness slash; cannot unjail without topping back up to
/// MIN_SELF_STAKE = 15,000 SRX via AddSelfStake).
const ADD_SELF_STAKE_HEIGHT_TESTNET_DEFAULT: u64 = 5_800_000;

/// EVM value-transfer activation. Pre-fork: Pass-2 EVM apply path runs
/// every tx with `TxEnv.value = U256::ZERO` regardless of envelope
/// value (matches v2.1.48 behaviour). Post-fork: revm sees the real
/// envelope value and moves SRX between EOAs / forwards into payable
/// contract calls.
///
/// **Activated on mainnet at h=1,748,900 on 2026-05-13** (closes #580
/// after the 5-step audit in `audits/2026-05-01-evm-value-transfer-divergence.md`).
/// The original 2026-05-01 split-brain halts were resolved by the
/// state_root v2 + extended-touch + strict-justification forks shipped
/// in v2.1.85+. Verified end-to-end via WSRX deposit at h=1,749,380.
/// Constant must match the runtime fork — fresh validators that compute
/// state_root past 1,748,900 with a different default would diverge.
const EVM_VALUE_TRANSFER_HEIGHT_DEFAULT: u64 = 1_748_900;
const EVM_VALUE_TRANSFER_HEIGHT_TESTNET_DEFAULT: u64 = 3_409_717;

/// Audit H3 (2026-05-06): EVM gas-fix fork. Pre-fork the write-path
/// EVM tx flow let revm internally deduct `gas_used × INITIAL_BASE_FEE`
/// from sender's wei balance, and the writeback's wei→sentri floor-div
/// silently dropped that delta (not burned, not credited to
/// validator) — `total_supply()` leaked. Post-fork sets
/// `cfg.disable_base_fee = true` on the write path too so revm skips
/// gas accounting; Pass-1 native `tx.fee` (10K sentri flat) is the
/// entire fee.
///
/// **Activated on mainnet at h=1,748,900 on 2026-05-13**, paired with
/// `EVM_VALUE_TRANSFER_HEIGHT_DEFAULT`. Constraint: GAS_FIX ≤ VALUE_TRANSFER —
/// equal-height activation is the safe form.
const EVM_GAS_FIX_HEIGHT_DEFAULT: u64 = 1_748_900;
const EVM_GAS_FIX_HEIGHT_TESTNET_DEFAULT: u64 = 3_787_000;

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

/// Bug A fix — off-trie consensus state migration to state trie
/// (SIP-6, drafted 2026-05-31 post testnet cascade 2026-05-30).
///
/// Pre-fork: `pending_rewards`, `total_minted`, `liveness`
/// (`blocks_signed`/`blocks_missed`/`jail_*`), and `epoch_state` are
/// stored on `StakeRegistry`'s in-memory `HashMap<String,
/// ValidatorStake>` and slashing/epoch engines. Their mutations on the
/// apply path land AFTER state_root commitment, so silent drift across
/// validators is undetectable until a consuming tx (ClaimRewards,
/// Unjail, AddSelfStake) re-reads the diverged value and forks the
/// cluster. Surfaced on 2026-05-30 testnet: val3 ClaimRewards triggered
/// 3-way state fork at h=5,817,131 → cascade.
///
/// Post-fork: those four consensus state pieces are written through
/// the state trie inside the apply path (before state_root is taken),
/// so any drift across validators surfaces immediately as a state_root
/// mismatch instead of silently accumulating. See
/// [SIP-6](https://github.com/sentrix-labs/SIPs/pull/3) for the
/// migration design (F2 — proper architectural fix; F3 B3b-style
/// epoch-boundary reconciliation and F1 preemptive consuming-ops
/// gate were considered and rejected).
///
/// Mainnet: stays `u64::MAX` until canonical-override design lands
/// (the activation hazard from STATE_ROOT_V2 first-activation fork
/// 2026-05-06 applies here too — first post-fork block writes any
/// pre-existing drift directly into state_root and forks).
///
/// Testnet: activation pinned at 6_026_000 (~46 min after the
/// 2026-06-01 v2.2.24 deploy window), preceded by halt-all + cp
/// val2/chain.db → val1+val4 + simul-start to harmonize the
/// pre-fork off-trie state across the active set. Mechanism
/// modelled on the 2026-05-30 testnet val3 recovery — proven to
/// land all vals on identical in-memory state on restart.
const STATE_IN_TRIE_HEIGHT_DEFAULT: u64 = u64::MAX;
const STATE_IN_TRIE_HEIGHT_TESTNET_DEFAULT: u64 = 6_026_000;

/// Native-module state-root commitment (SRC-20 ContractRegistry + NFT
/// NftRegistry into the trie). Default `u64::MAX` on BOTH mainnet AND
/// testnet — deliberately NOT reusing `STATE_IN_TRIE_HEIGHT` (already
/// active on testnet at 6,026,000): reusing it would retroactively change
/// testnet's state_root and fork the chain. This is a fresh, independent
/// gate that stays disabled everywhere until an explicit activation
/// height is set via env, preceded by halt-all + state-root-alignment
/// pre-flight + simul-start so every validator commits identical native
/// state at the activation block.
const NATIVE_STATE_IN_TRIE_HEIGHT_DEFAULT: u64 = u64::MAX;

// ── Runtime readers (env → u64, default to compile-time default) ──────

fn configured_chain_id() -> u64 {
    std::env::var("SENTRIX_CHAIN_ID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(MAINNET_CHAIN_ID)
}

fn chain_default(mainnet_default: u64, testnet_default: u64) -> u64 {
    match configured_chain_id() {
        TESTNET_CHAIN_ID => testnet_default,
        _ => mainnet_default,
    }
}

fn read_height(env_var: &str, default: u64) -> u64 {
    std::env::var(env_var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Read Voyager fork height from env, default u64::MAX (mainnet safe).
/// Testnet sets `VOYAGER_FORK_HEIGHT=<height>` in systemd service.
pub fn get_voyager_fork_height() -> u64 {
    read_height(
        "VOYAGER_FORK_HEIGHT",
        chain_default(
            VOYAGER_DPOS_HEIGHT_DEFAULT,
            VOYAGER_DPOS_HEIGHT_TESTNET_DEFAULT,
        ),
    )
}

/// Read EVM fork height from env, default u64::MAX (disabled).
/// Testnet: set `VOYAGER_EVM_HEIGHT=<height>` in systemd service.
pub fn get_evm_fork_height() -> u64 {
    read_height(
        "VOYAGER_EVM_HEIGHT",
        chain_default(
            VOYAGER_EVM_HEIGHT_DEFAULT,
            VOYAGER_EVM_HEIGHT_TESTNET_DEFAULT,
        ),
    )
}

/// V4 Step 3: read reward-v2 hard-fork height from env, default
/// `u64::MAX` (disabled — keeps current accumulator-only behaviour).
/// Post-fork: coinbase → `PROTOCOL_TREASURY`, ClaimRewards dispatch
/// becomes consensus-valid.
pub fn get_reward_v2_fork_height() -> u64 {
    read_height(
        "VOYAGER_REWARD_V2_HEIGHT",
        chain_default(
            VOYAGER_REWARD_V2_HEIGHT_DEFAULT,
            VOYAGER_REWARD_V2_HEIGHT_TESTNET_DEFAULT,
        ),
    )
}

/// Tokenomics v2: read fork height from env, default `u64::MAX`
/// (disabled — keeps v1 emission schedule: 42M halving + 210M cap).
/// Post-fork: 126M halving (BTC-parity 4-year) + 315M cap.
pub fn get_tokenomics_v2_height() -> u64 {
    read_height(
        "TOKENOMICS_V2_HEIGHT",
        chain_default(
            TOKENOMICS_V2_HEIGHT_DEFAULT,
            TOKENOMICS_V2_HEIGHT_TESTNET_DEFAULT,
        ),
    )
}

/// BFT-gate-relax: read fork height from env, default `u64::MAX`
/// (disabled — keeps current `active >= MIN_BFT_VALIDATORS` gate).
/// Post-fork: `active >= ⌈2/3 × total⌉` (= 3 for 4-validator network).
/// Fork is optional — operators set `BFT_GATE_RELAX_HEIGHT=<height>`
/// when they want to enable jail-cascade liveness margin. See
/// `audits/jail-cascade-root-cause-analysis.md`.
pub fn get_bft_gate_relax_height() -> u64 {
    read_height(
        "BFT_GATE_RELAX_HEIGHT",
        chain_default(
            BFT_GATE_RELAX_HEIGHT_DEFAULT,
            BFT_GATE_RELAX_HEIGHT_TESTNET_DEFAULT,
        ),
    )
}

/// Phase B: read `JAIL_CONSENSUS_HEIGHT` from env. Default `u64::MAX`
/// (disabled). Activates consensus-computed jail dispatch when set.
pub fn get_jail_consensus_height() -> u64 {
    read_height("JAIL_CONSENSUS_HEIGHT", JAIL_CONSENSUS_HEIGHT_DEFAULT)
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
    read_height("NFT_TOKENOP_HEIGHT", NFT_TOKENOP_HEIGHT_DEFAULT)
}

/// Read AddSelfStake fork height. Mainnet default is `u64::MAX` (disabled
/// pending a planned activation window); testnet default is 5,800,000
/// (set 2026-05-30 to recover val3). Operators can still override either
/// chain via `ADD_SELF_STAKE_HEIGHT=<height>` env, but every peer in the
/// active set must agree on the value or consensus will fork at the
/// activation boundary.
pub fn get_add_self_stake_height() -> u64 {
    read_height(
        "ADD_SELF_STAKE_HEIGHT",
        chain_default(
            ADD_SELF_STAKE_HEIGHT_DEFAULT,
            ADD_SELF_STAKE_HEIGHT_TESTNET_DEFAULT,
        ),
    )
}

/// Read EVM value-transfer fork height from env. Default 1,748,900 —
/// matches mainnet activation 2026-05-13 so a fresh validator built
/// from main computes consistent state_root past the fork. Post-fork
/// the Pass-2 EVM apply path threads envelope value into `TxEnv.value`.
/// Testnet sets `EVM_VALUE_TRANSFER_HEIGHT=3409717` in env to override.
pub fn get_evm_value_transfer_height() -> u64 {
    read_height(
        "EVM_VALUE_TRANSFER_HEIGHT",
        chain_default(
            EVM_VALUE_TRANSFER_HEIGHT_DEFAULT,
            EVM_VALUE_TRANSFER_HEIGHT_TESTNET_DEFAULT,
        ),
    )
}

/// Audit H3 EVM gas-fix fork height (2026-05-06). See
/// [`EVM_GAS_FIX_HEIGHT_DEFAULT`] for context. Default 1,748,900 —
/// matches mainnet activation 2026-05-13 (paired with VALUE_TRANSFER).
/// Testnet sets `EVM_GAS_FIX_HEIGHT=3787000` in env to override.
pub fn get_evm_gas_fix_height() -> u64 {
    read_height(
        "EVM_GAS_FIX_HEIGHT",
        chain_default(
            EVM_GAS_FIX_HEIGHT_DEFAULT,
            EVM_GAS_FIX_HEIGHT_TESTNET_DEFAULT,
        ),
    )
}

/// state_root v2 drift fix fork height (2026-05-07). See
/// [`EXTENDED_TOUCH_LIST_HEIGHT_DEFAULT`] for context. Default
/// `u64::MAX` keeps the legacy behaviour unchanged so v2.1.81 chain
/// history stays bit-identical.
pub fn get_extended_touch_list_height() -> u64 {
    read_height(
        "EXTENDED_TOUCH_LIST_HEIGHT",
        EXTENDED_TOUCH_LIST_HEIGHT_DEFAULT,
    )
}

/// Strict-justification fork height (2026-05-07). See
/// [`STRICT_JUSTIFICATION_HEIGHT_DEFAULT`] for context. Default
/// `u64::MAX` preserves legacy behaviour bit-identically.
pub fn get_strict_justification_height() -> u64 {
    read_height(
        "STRICT_JUSTIFICATION_HEIGHT",
        STRICT_JUSTIFICATION_HEIGHT_DEFAULT,
    )
}

/// State-in-trie fork height — SIP-6 Bug A migration. See
/// [`STATE_IN_TRIE_HEIGHT_DEFAULT`] for context + design link.
/// Default `u64::MAX` keeps the legacy off-trie write path
/// bit-identical; once an activation height is pinned via env,
/// the apply path routes the 4 consensus state pieces through the
/// state trie before state_root commitment.
pub fn get_state_in_trie_height() -> u64 {
    read_height(
        "STATE_IN_TRIE_HEIGHT",
        chain_default(
            STATE_IN_TRIE_HEIGHT_DEFAULT,
            STATE_IN_TRIE_HEIGHT_TESTNET_DEFAULT,
        ),
    )
}

/// Native-module state-root commitment fork height. Default `u64::MAX`
/// on both nets (see [`NATIVE_STATE_IN_TRIE_HEIGHT_DEFAULT`]). Once
/// pinned via env, the apply path commits the SRC-20 + NFT registry
/// canonical hashes into the trie before state_root.
pub fn get_native_state_in_trie_height() -> u64 {
    read_height(
        "NATIVE_STATE_IN_TRIE_HEIGHT",
        NATIVE_STATE_IN_TRIE_HEIGHT_DEFAULT,
    )
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

/// SIP-6 Bug A fix — true once `STATE_IN_TRIE_HEIGHT` activates.
/// Post-fork: the apply path commits `pending_rewards`,
/// `total_minted`, validator liveness, and `epoch_state` into the
/// state trie before state_root is taken — drift across validators
/// surfaces immediately as state_root mismatch instead of silent
/// off-trie divergence. Pre-fork the legacy in-memory write path is
/// preserved bit-identically.
pub fn is_state_in_trie_height(height: u64) -> bool {
    let fork = get_state_in_trie_height();
    fork != u64::MAX && height >= fork
}

/// Native-module state-root commitment — true once
/// `NATIVE_STATE_IN_TRIE_HEIGHT` activates. Post-fork: the apply path
/// commits the SRC-20 `ContractRegistry` and NFT `NftRegistry` canonical
/// hashes into the state trie before state_root, so divergence in native
/// token/NFT state surfaces as a state_root mismatch instead of relying
/// on deterministic re-execution + the (uncommitted) blob. Pre-fork the
/// state_root is computed exactly as before (native state stays off-trie).
pub fn is_native_state_in_trie_height(height: u64) -> bool {
    let fork = get_native_state_in_trie_height();
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
            std::env::remove_var("SENTRIX_CHAIN_ID");
            std::env::remove_var("VOYAGER_FORK_HEIGHT");
            assert_eq!(get_voyager_fork_height(), u64::MAX);

            std::env::set_var("VOYAGER_FORK_HEIGHT", "12345");
            assert_eq!(get_voyager_fork_height(), 12345);

            // Garbage value falls back to default — no panic.
            std::env::set_var("VOYAGER_FORK_HEIGHT", "not_a_number");
            assert_eq!(get_voyager_fork_height(), u64::MAX);

            std::env::remove_var("VOYAGER_FORK_HEIGHT");
            assert_eq!(get_voyager_fork_height(), u64::MAX);
            std::env::remove_var("SENTRIX_CHAIN_ID");
        }
    }

    #[test]
    fn mature_testnet_forks_have_code_level_defaults() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("SENTRIX_CHAIN_ID", TESTNET_CHAIN_ID.to_string());
            for var in [
                "VOYAGER_FORK_HEIGHT",
                "VOYAGER_EVM_HEIGHT",
                "VOYAGER_REWARD_V2_HEIGHT",
                "TOKENOMICS_V2_HEIGHT",
                "BFT_GATE_RELAX_HEIGHT",
                "EVM_VALUE_TRANSFER_HEIGHT",
                "EVM_GAS_FIX_HEIGHT",
            ] {
                std::env::remove_var(var);
            }

            assert_eq!(get_voyager_fork_height(), 10);
            assert_eq!(get_evm_fork_height(), 752);
            assert_eq!(get_reward_v2_fork_height(), 100);
            assert_eq!(get_tokenomics_v2_height(), 381_651);
            assert_eq!(get_bft_gate_relax_height(), 551_500);
            assert_eq!(get_evm_value_transfer_height(), 3_409_717);
            assert_eq!(get_evm_gas_fix_height(), 3_787_000);

            assert!(!is_voyager_height(9));
            assert!(is_voyager_height(10));
            assert!(!is_evm_gas_fix_height(3_786_999));
            assert!(is_evm_gas_fix_height(3_787_000));

            // AddSelfStake activates on testnet at 5,800,000 (see
            // ADD_SELF_STAKE_HEIGHT_TESTNET_DEFAULT — scheduled
            // 2026-05-30 to recover val3). Pin both the height and
            // the gate-boundary semantics.
            std::env::remove_var("ADD_SELF_STAKE_HEIGHT");
            assert_eq!(get_add_self_stake_height(), 5_800_000);
            assert!(!is_add_self_stake_height(5_799_999));
            assert!(is_add_self_stake_height(5_800_000));
            assert!(is_add_self_stake_height(6_000_000));

            // SIP-6 STATE_IN_TRIE activates on testnet at 6,026,000
            // (STATE_IN_TRIE_HEIGHT_TESTNET_DEFAULT — pinned 2026-06-01
            // for the v2.2.24 deploy + chain.db harmonize window).
            // Mainnet stays u64::MAX until canonical-override design.
            // Pin the height + gate-boundary semantics here so a
            // future edit of the testnet const can't silently drift
            // the activation block.
            std::env::remove_var("STATE_IN_TRIE_HEIGHT");
            assert_eq!(get_state_in_trie_height(), 6_026_000);
            assert!(!is_state_in_trie_height(6_025_999));
            assert!(is_state_in_trie_height(6_026_000));
            assert!(is_state_in_trie_height(6_026_001));

            std::env::remove_var("SENTRIX_CHAIN_ID");
        }
    }

    #[test]
    fn risky_forks_stay_disabled_by_default_on_testnet() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("SENTRIX_CHAIN_ID", TESTNET_CHAIN_ID.to_string());
            for var in [
                "JAIL_CONSENSUS_HEIGHT",
                "NFT_TOKENOP_HEIGHT",
                "EXTENDED_TOUCH_LIST_HEIGHT",
                "STRICT_JUSTIFICATION_HEIGHT",
            ] {
                std::env::remove_var(var);
            }

            // AddSelfStake moved out of this test in v2.2.22 — it's now
            // an enabled testnet fork (see mature_testnet_forks_have_code_
            // _level_defaults). Mainnet still defaults disabled.
            assert_eq!(get_jail_consensus_height(), u64::MAX);
            assert_eq!(get_nft_tokenop_height(), u64::MAX);
            assert_eq!(get_extended_touch_list_height(), u64::MAX);
            assert_eq!(get_strict_justification_height(), u64::MAX);

            assert!(!is_jail_consensus_height(0));
            assert!(!is_nft_tokenop_height(0));
            assert!(!is_extended_touch_list_height(0));
            assert!(!is_strict_justification_height(0));

            std::env::remove_var("SENTRIX_CHAIN_ID");
        }
    }

    #[test]
    fn mainnet_defaults_preserve_v2_2_11_values_without_env() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("SENTRIX_CHAIN_ID", MAINNET_CHAIN_ID.to_string());
            for var in [
                "VOYAGER_FORK_HEIGHT",
                "VOYAGER_EVM_HEIGHT",
                "VOYAGER_REWARD_V2_HEIGHT",
                "TOKENOMICS_V2_HEIGHT",
                "BFT_GATE_RELAX_HEIGHT",
                "EVM_VALUE_TRANSFER_HEIGHT",
                "EVM_GAS_FIX_HEIGHT",
                "ADD_SELF_STAKE_HEIGHT",
            ] {
                std::env::remove_var(var);
            }

            assert_eq!(get_voyager_fork_height(), u64::MAX);
            assert_eq!(get_evm_fork_height(), u64::MAX);
            assert_eq!(get_reward_v2_fork_height(), u64::MAX);
            assert_eq!(get_tokenomics_v2_height(), u64::MAX);
            assert_eq!(get_bft_gate_relax_height(), u64::MAX);
            assert_eq!(get_evm_value_transfer_height(), 1_748_900);
            assert_eq!(get_evm_gas_fix_height(), 1_748_900);
            // AddSelfStake stays disabled on mainnet until a planned
            // activation window (see ADD_SELF_STAKE_HEIGHT_DEFAULT doc).
            assert_eq!(get_add_self_stake_height(), u64::MAX);
            assert!(!is_add_self_stake_height(0));
            assert!(!is_add_self_stake_height(u64::MAX - 1));

            // SIP-6 STATE_IN_TRIE stays disabled on mainnet until a
            // canonical-override design lands for the first-activation
            // drift hazard (see STATE_IN_TRIE_HEIGHT_DEFAULT doc).
            std::env::remove_var("STATE_IN_TRIE_HEIGHT");
            assert_eq!(get_state_in_trie_height(), u64::MAX);
            assert!(!is_state_in_trie_height(0));
            assert!(!is_state_in_trie_height(u64::MAX - 1));

            std::env::remove_var("SENTRIX_CHAIN_ID");
        }
    }

    #[test]
    fn unknown_chain_ids_use_mainnet_safe_defaults() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("SENTRIX_CHAIN_ID", "999999");
            for var in [
                "VOYAGER_FORK_HEIGHT",
                "VOYAGER_EVM_HEIGHT",
                "VOYAGER_REWARD_V2_HEIGHT",
                "TOKENOMICS_V2_HEIGHT",
                "BFT_GATE_RELAX_HEIGHT",
                "EVM_VALUE_TRANSFER_HEIGHT",
                "EVM_GAS_FIX_HEIGHT",
                "STATE_IN_TRIE_HEIGHT",
            ] {
                std::env::remove_var(var);
            }

            assert_eq!(get_voyager_fork_height(), u64::MAX);
            assert_eq!(get_evm_fork_height(), u64::MAX);
            assert_eq!(get_reward_v2_fork_height(), u64::MAX);
            assert_eq!(get_tokenomics_v2_height(), u64::MAX);
            assert_eq!(get_bft_gate_relax_height(), u64::MAX);
            assert_eq!(get_evm_value_transfer_height(), 1_748_900);
            assert_eq!(get_evm_gas_fix_height(), 1_748_900);
            assert_eq!(get_state_in_trie_height(), u64::MAX);

            std::env::remove_var("SENTRIX_CHAIN_ID");
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

    /// NATIVE_STATE_IN_TRIE must default to disabled (`u64::MAX`) on BOTH
    /// mainnet and testnet — unlike SIP-6 STATE_IN_TRIE, which is already
    /// armed on testnet. Reusing that gate would have forked testnet; this
    /// fresh gate stays closed everywhere until explicitly pinned.
    #[test]
    fn native_state_in_trie_disabled_by_default_both_nets() {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("NATIVE_STATE_IN_TRIE_HEIGHT");

            std::env::set_var("SENTRIX_CHAIN_ID", MAINNET_CHAIN_ID.to_string());
            assert_eq!(get_native_state_in_trie_height(), u64::MAX);
            assert!(!is_native_state_in_trie_height(0));
            assert!(!is_native_state_in_trie_height(u64::MAX - 1));

            std::env::set_var("SENTRIX_CHAIN_ID", TESTNET_CHAIN_ID.to_string());
            assert_eq!(
                get_native_state_in_trie_height(),
                u64::MAX,
                "must stay disabled on testnet too (STATE_IN_TRIE is already armed there)"
            );
            assert!(!is_native_state_in_trie_height(6_026_000));

            std::env::remove_var("SENTRIX_CHAIN_ID");
        }
    }

    /// Once pinned, the gate flips open exactly at the activation height.
    #[test]
    fn native_state_in_trie_activates_at_pinned_height() {
        let _guard = env_test_lock();
        unsafe {
            std::env::set_var("NATIVE_STATE_IN_TRIE_HEIGHT", "1000");
            assert_eq!(get_native_state_in_trie_height(), 1000);
            assert!(!is_native_state_in_trie_height(999));
            assert!(is_native_state_in_trie_height(1000));
            assert!(is_native_state_in_trie_height(1001));
            std::env::remove_var("NATIVE_STATE_IN_TRIE_HEIGHT");
        }
    }
}
