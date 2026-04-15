// evm/gas.rs — EIP-1559 base fee calculation for Sentrix EVM
//
// Implements dynamic gas pricing: base_fee adjusts per block based on
// utilization relative to target. Base fee is burned, priority fee goes
// to the block producer.

/// Initial base fee in sentri (0.0001 SRX)
pub const INITIAL_BASE_FEE: u64 = 10_000;

/// Target gas per block (half of limit — blocks aim for 50% full)
pub const GAS_TARGET: u64 = 15_000_000;

/// Maximum gas per block
pub const BLOCK_GAS_LIMIT: u64 = 30_000_000;

/// Denominator for base fee change (max 12.5% per block)
pub const BASE_FEE_CHANGE_DENOMINATOR: u64 = 8;

/// Minimum base fee (never drops below 1 sentri)
pub const MIN_BASE_FEE: u64 = 1;

/// Calculate the next block's base fee based on the parent block's usage.
///
/// If parent used more gas than target → base fee increases.
/// If parent used less gas than target → base fee decreases.
/// Change is capped at 1/8 (12.5%) per block.
pub fn next_base_fee(parent_base_fee: u64, parent_gas_used: u64) -> u64 {
    if parent_gas_used == GAS_TARGET {
        return parent_base_fee;
    }

    if parent_gas_used > GAS_TARGET {
        // Increase: delta = parent_base_fee * (gas_used - target) / target / denominator
        let excess = parent_gas_used.saturating_sub(GAS_TARGET);
        let delta = parent_base_fee
            .saturating_mul(excess)
            / GAS_TARGET
            / BASE_FEE_CHANGE_DENOMINATOR;
        // At least 1 sentri increase to ensure convergence
        let delta = delta.max(1);
        parent_base_fee.saturating_add(delta)
    } else {
        // Decrease: delta = parent_base_fee * (target - gas_used) / target / denominator
        let deficit = GAS_TARGET.saturating_sub(parent_gas_used);
        let delta = parent_base_fee
            .saturating_mul(deficit)
            / GAS_TARGET
            / BASE_FEE_CHANGE_DENOMINATOR;
        parent_base_fee.saturating_sub(delta).max(MIN_BASE_FEE)
    }
}

/// Calculate total fee for a transaction.
/// Returns (base_fee_cost, priority_fee_cost).
///   - base_fee_cost is burned
///   - priority_fee_cost goes to the block producer
pub fn calculate_tx_fee(gas_used: u64, base_fee: u64, priority_fee: u64) -> (u64, u64) {
    let base_cost = gas_used.saturating_mul(base_fee);
    let priority_cost = gas_used.saturating_mul(priority_fee);
    (base_cost, priority_cost)
}

/// Check if a transaction's gas limit fits within the block gas limit.
pub fn fits_in_block(current_gas_used: u64, tx_gas_limit: u64) -> bool {
    current_gas_used.saturating_add(tx_gas_limit) <= BLOCK_GAS_LIMIT
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_fee_at_target() {
        // Exactly at target → no change
        assert_eq!(next_base_fee(10_000, GAS_TARGET), 10_000);
    }

    #[test]
    fn test_base_fee_above_target() {
        // Full block (30M) vs 15M target → increases by 12.5%
        let fee = next_base_fee(10_000, BLOCK_GAS_LIMIT);
        assert!(fee > 10_000);
        // At 2x target: delta = 10000 * 15M / 15M / 8 = 1250
        assert_eq!(fee, 11_250);
    }

    #[test]
    fn test_base_fee_below_target() {
        // Empty block (0 gas) → decreases by 12.5%
        let fee = next_base_fee(10_000, 0);
        assert!(fee < 10_000);
        // delta = 10000 * 15M / 15M / 8 = 1250
        assert_eq!(fee, 8_750);
    }

    #[test]
    fn test_base_fee_minimum() {
        // Can't go below MIN_BASE_FEE
        let fee = next_base_fee(1, 0);
        assert_eq!(fee, MIN_BASE_FEE);
    }

    #[test]
    fn test_base_fee_overflow_protection() {
        let fee = next_base_fee(u64::MAX, BLOCK_GAS_LIMIT);
        // Should not overflow
        assert!(fee > 0);
    }

    #[test]
    fn test_calculate_tx_fee() {
        let (base, priority) = calculate_tx_fee(21_000, 10_000, 1_000);
        assert_eq!(base, 210_000_000); // 21000 * 10000
        assert_eq!(priority, 21_000_000); // 21000 * 1000
    }

    #[test]
    fn test_fits_in_block() {
        assert!(fits_in_block(0, BLOCK_GAS_LIMIT));
        assert!(fits_in_block(BLOCK_GAS_LIMIT - 1, 1));
        assert!(!fits_in_block(BLOCK_GAS_LIMIT, 1));
        assert!(!fits_in_block(1, BLOCK_GAS_LIMIT));
    }

    #[test]
    fn test_gradual_convergence() {
        // Simulate 10 blocks at full capacity — fee should keep rising
        let mut fee = INITIAL_BASE_FEE;
        for _ in 0..10 {
            let new_fee = next_base_fee(fee, BLOCK_GAS_LIMIT);
            assert!(new_fee > fee);
            fee = new_fee;
        }
        // After 10 full blocks, fee should be significantly higher
        assert!(fee > INITIAL_BASE_FEE * 2);
    }

    #[test]
    fn test_gradual_decrease() {
        // Simulate 10 empty blocks — fee should keep dropping
        let mut fee = 100_000u64;
        for _ in 0..10 {
            let new_fee = next_base_fee(fee, 0);
            assert!(new_fee < fee);
            fee = new_fee;
        }
        assert!(fee < 50_000);
    }
}
