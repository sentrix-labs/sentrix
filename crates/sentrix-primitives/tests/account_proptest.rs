//! Property-based invariant tests for AccountDB.
//!
//! Background: the 2026-05-06 rust-workspace audit flagged H3 — a supply
//! leak in the EVM tx path where `commit_state_to_account_db` calls
//! `set_balance` to write a post-revm balance back, and the wei-side gas
//! deduction silently disappears from `total_supply()`. The mechanism is
//! generic: ANY caller that uses `set_balance` to drop an account
//! balance without crediting another account or calling `burn` violates
//! the supply-conservation invariant.
//!
//! These tests treat AccountDB as a state machine and assert two
//! invariants across random op sequences:
//!
//!   I1 (positive): `credit`, `charge_fee_only`, and `transfer` together
//!      preserve `total_supply() + total_burned + validator_pool` where
//!      `validator_pool` is the per-tx `floor(fee/2)` that the caller
//!      tracks for end-of-block `apply_block_reward`. The model ledger
//!      mirrors AccountDB.
//!
//!   I2 (negative — H3 hazard demonstration): `set_balance` reductions
//!      DO break the invariant unless the caller bookkeeps the delta
//!      elsewhere. We assert this directly so a future PR that tightens
//!      the AccountDB API (private `set_balance`, force `credit`/`burn`
//!      for all balance changes) is a regression-guarded refactor.
//!
//! Run with: `cargo test -p sentrix-primitives --test account_proptest`
//!
//! Audit reference: founder-private/audits/2026-05-06-rust-workspace-security-audit.md

use proptest::prelude::*;
use sentrix_primitives::account::AccountDB;

/// Address pool kept small so transfer ops have a non-trivial chance of
/// picking sender == recipient (which AccountDB tolerates) and so the
/// state space is dense enough for shrinking to find minimal counter-
/// examples on regression.
const ADDRESSES: &[&str] = &["alice", "bob", "carol", "dave", "eve"];

/// Operations the model supports. Mirrors AccountDB's bookkeeping
/// surface; deliberately excludes `set_balance` / `set_nonce` which the
/// negative test (`H3-style hazard`) covers separately.
#[derive(Debug, Clone)]
enum Op {
    /// Mint into `addr`. Models block-reward credit / genesis funding.
    Credit { addr: usize, amount: u64 },
    /// Pass-1 EVM fee deduction. Half is burned, half is implicitly
    /// validator-share (caller responsibility to credit at block-end;
    /// the model tracks it as `validator_pool`).
    ChargeFee { from: usize, fee: u64 },
    /// Native value transfer with fee. Same fee-split semantics as
    /// ChargeFee — `floor(fee/2)` to validator_pool, `ceil(fee/2)` burned.
    Transfer {
        from: usize,
        to: usize,
        amount: u64,
        fee: u64,
    },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    let addr = 0usize..ADDRESSES.len();
    // Small bounds keep test runs fast while still surfacing arithmetic
    // edge cases (rounding direction on odd fees, zero-amount transfers).
    let small_amount = 0u64..1_000_000_000u64; // 0..10 SRX
    let small_fee = 0u64..100_000u64; // 0..0.001 SRX

    prop_oneof![
        (addr.clone(), small_amount.clone()).prop_map(|(addr, amount)| Op::Credit { addr, amount }),
        (addr.clone(), small_fee.clone()).prop_map(|(from, fee)| Op::ChargeFee { from, fee }),
        (
            addr.clone(),
            addr.clone(),
            small_amount,
            small_fee,
        )
            .prop_map(|(from, to, amount, fee)| Op::Transfer {
                from,
                to,
                amount,
                fee,
            }),
    ]
}

/// Independent ledger model. Tracks the same state as AccountDB plus an
/// implicit `validator_pool` for collected fees that haven't yet been
/// credited via `apply_block_reward`. AccountDB doesn't expose this pool
/// directly because `transfer` / `charge_fee_only` only update
/// `total_burned`; the validator share is reconciled at block-end.
#[derive(Default, Clone)]
struct ModelLedger {
    balances: std::collections::HashMap<String, u64>,
    total_burned: u64,
    /// `floor(fee/2)` collected per fee-op. Stays in escrow until the
    /// caller calls `apply_block_reward(validator, reward, fee_share)`
    /// at end of block. Conserved-supply invariant assumes this pool
    /// IS still part of the system's total tokens.
    validator_pool: u64,
    /// Cumulative tokens introduced via `credit` (new mint) — block
    /// rewards, genesis funding, etc. Conservation invariant:
    ///     supply + burned + validator_pool == initial_seed + total_minted
    total_minted: u64,
}

impl ModelLedger {
    fn balance(&self, addr: &str) -> u64 {
        self.balances.get(addr).copied().unwrap_or(0)
    }

    fn credit(&mut self, addr: &str, amount: u64) {
        let entry = self.balances.entry(addr.to_string()).or_insert(0);
        *entry = entry.saturating_add(amount);
    }

    /// Returns true if op was applicable (sufficient balance / no overflow).
    /// Matches AccountDB's Err-on-insufficient-balance semantics — when
    /// AccountDB would Err, the model returns false and skips the change.
    fn apply(&mut self, op: &Op) -> bool {
        match op {
            Op::Credit { addr, amount } => {
                let key = ADDRESSES[*addr];
                let new = self.balance(key).checked_add(*amount);
                match new {
                    Some(v) => {
                        self.balances.insert(key.to_string(), v);
                        self.total_minted = self.total_minted.saturating_add(*amount);
                        true
                    }
                    None => false, // overflow — AccountDB Errs, model skips
                }
            }
            Op::ChargeFee { from, fee } => {
                let key = ADDRESSES[*from];
                if self.balance(key) < *fee {
                    return false;
                }
                let entry = self.balances.entry(key.to_string()).or_insert(0);
                *entry -= fee;
                let burn = fee.div_ceil(2);
                self.total_burned = self.total_burned.saturating_add(burn);
                self.validator_pool = self.validator_pool.saturating_add(fee - burn);
                true
            }
            Op::Transfer {
                from,
                to,
                amount,
                fee,
            } => {
                let from_key = ADDRESSES[*from];
                let to_key = ADDRESSES[*to];
                let total = match amount.checked_add(*fee) {
                    Some(t) => t,
                    None => return false,
                };
                if self.balance(from_key) < total {
                    return false;
                }
                // Match AccountDB::credit — it fails on overflow rather
                // than wrapping. If recipient overflow would happen,
                // AccountDB returns Err and rolls the sender deduction
                // back via early-return BEFORE the mutation. Model
                // mirrors by checking the recipient first.
                if self.balance(to_key).checked_add(*amount).is_none() {
                    return false;
                }
                let from_entry = self.balances.entry(from_key.to_string()).or_insert(0);
                *from_entry -= total;
                let to_entry = self.balances.entry(to_key.to_string()).or_insert(0);
                *to_entry = to_entry.saturating_add(*amount);
                let burn = fee.div_ceil(2);
                self.total_burned = self.total_burned.saturating_add(burn);
                self.validator_pool = self.validator_pool.saturating_add(fee - burn);
                true
            }
        }
    }

    fn account_db_supply(&self) -> u64 {
        self.balances.values().sum()
    }
}

// I1 — across any sequence of `credit` / `charge_fee_only` / `transfer`,
// AccountDB's observed `total_supply()` and `total_burned` match a
// from-scratch model ledger applied to the same op sequence. Validates
// the bookkeeping discipline of every public mutation that's allowed
// to change supply.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]
    #[test]
    fn supply_invariant_holds_under_random_op_sequences(
        ops in prop::collection::vec(op_strategy(), 1..50),
        initial_funding in 1_000_000_000u64..10_000_000_000u64, // 10..100 SRX per address
    ) {
        let mut db = AccountDB::new();
        let mut model = ModelLedger::default();

        // Seed every address with `initial_funding` so most ops have a
        // non-trivial chance of succeeding instead of dying instantly on
        // InsufficientBalance.
        for addr in ADDRESSES {
            db.credit(addr, initial_funding).unwrap();
            model.credit(addr, initial_funding);
        }

        for op in &ops {
            match op {
                Op::Credit { addr, amount } => {
                    let key = ADDRESSES[*addr];
                    let real_ok = db.credit(key, *amount).is_ok();
                    let model_ok = model.apply(op);
                    prop_assert_eq!(real_ok, model_ok, "credit ok-status drift");
                }
                Op::ChargeFee { from, fee } => {
                    let key = ADDRESSES[*from];
                    let real_ok = db.charge_fee_only(key, *fee).is_ok();
                    let model_ok = model.apply(op);
                    prop_assert_eq!(real_ok, model_ok, "charge_fee_only ok-status drift");
                }
                Op::Transfer { from, to, amount, fee } => {
                    let from_key = ADDRESSES[*from];
                    let to_key = ADDRESSES[*to];
                    let real_ok = db.transfer(from_key, to_key, *amount, *fee).is_ok();
                    let model_ok = model.apply(op);
                    prop_assert_eq!(real_ok, model_ok, "transfer ok-status drift");
                }
            }

            // Per-step invariants. We re-check after every op so
            // shrinking pinpoints the offending op.
            for addr in ADDRESSES {
                prop_assert_eq!(
                    db.get_balance(addr),
                    model.balance(addr),
                    "balance drift on {}",
                    addr,
                );
            }
            prop_assert_eq!(
                db.total_supply(),
                model.account_db_supply(),
                "total_supply drift",
            );
            prop_assert_eq!(
                db.total_burned,
                model.total_burned,
                "total_burned drift",
            );

            // Conservation: total tokens in the system =
            //     supply (held in accounts) + total_burned + validator_pool
            // and grows by exactly the amount of any new `credit` mint.
            // Anything else (transfer, charge_fee_only) is zero-sum on the
            // (supply + burned + pool) total.
            let initial_seed = ADDRESSES.len() as u64 * initial_funding;
            let supply_now = db.total_supply();
            let conserved = supply_now
                .saturating_add(db.total_burned)
                .saturating_add(model.validator_pool);
            let expected = initial_seed.saturating_add(model.total_minted);
            prop_assert_eq!(
                conserved, expected,
                "supply conservation broken: supply={} burned={} validator_pool={} initial_seed={} minted_via_credit={}",
                supply_now, db.total_burned, model.validator_pool, initial_seed, model.total_minted,
            );
        }
    }
}

/// I2 — `set_balance` is the H3 hazard. Reducing a balance via
/// `set_balance` without bookkeeping the delta DOES break the supply
/// invariant. This test is a deliberate negative: it asserts the bug
/// EXISTS today so a future PR that fixes it (e.g. by making
/// `set_balance` private + introducing `credit_absolute` / `burn_to`
/// pairing) is forced to also remove or update this test. That's the
/// regression-guard pattern we want.
#[test]
fn set_balance_reduction_leaks_supply_h3_class() {
    let mut db = AccountDB::new();
    db.credit("alice", 1_000_000_000).unwrap();
    let initial_supply = db.total_supply();
    let initial_burned = db.total_burned;

    // Simulate the EVM writeback path: alice's wei balance drops by the
    // gas-deduction equivalent. set_balance writes the absolute new
    // value; no `burn` or `credit` paired with it.
    let post_evm_balance = db.get_balance("alice") - 100;
    db.set_balance("alice", post_evm_balance);

    let supply_drop = initial_supply - db.total_supply();
    let burned_delta = db.total_burned - initial_burned;

    assert_eq!(
        supply_drop, 100,
        "set_balance dropped supply by {} (expected 100)",
        supply_drop,
    );
    assert_eq!(
        burned_delta, 0,
        "set_balance must not bookkeep total_burned (it doesn't know intent)",
    );
    // The leak: 100 sentri vanished from the system. Until the API is
    // tightened, callers must pair set_balance with explicit burn() or
    // credit(other, delta). The H3 fix in sentrix-evm/writeback.rs has
    // to do exactly that pairing.
    assert!(
        supply_drop > burned_delta,
        "regression-guard: if set_balance ever stops leaking, this test \
         fails and you need to update or delete it (see audit H3 fix)",
    );
}
