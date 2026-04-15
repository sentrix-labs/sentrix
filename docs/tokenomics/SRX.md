# SRX

Native coin. Used for fees, validator rewards, and staking (Voyager).

## Units

```
1 SRX = 100,000,000 sentri
```

Everything internal is u64 in sentri. No floats.

## Supply

Hard cap: 210,000,000 SRX. Can't change without a fork.

| Source | SRX | % |
|--------|-----|---|
| Premine | 63,000,000 | 30% |
| Block rewards | ≤147,000,000 | 70% |

### Premine

| Wallet | SRX | Purpose |
|--------|-----|---------|
| Founder | 21,000,000 | Treasury, ops |
| Ecosystem | 21,000,000 | Grants, partnerships |
| Early Validator | 10,500,000 | Validator incentives |
| Reserve | 10,500,000 | Emergency |

### Actual Max Circulating

Mining rewards converge to 84M SRX total (geometric series). So: 63M + 84M = **147M SRX** actual max. The 210M cap is never reached.

## Halving

Block reward halves every 42M blocks (~4 years).

| Era | Blocks | Reward | Start |
|-----|--------|--------|-------|
| 0 | 0 – 41.9M | 1 SRX | 2026 |
| 1 | 42M – 83.9M | 0.5 SRX | ~2030 |
| 2 | 84M – 125.9M | 0.25 SRX | ~2034 |
| 3 | 126M – 167.9M | 0.125 SRX | ~2038 |

```rust
fn get_block_reward(height: u64) -> u64 {
    BLOCK_REWARD >> (height / HALVING_INTERVAL) // 1 SRX >> era
}
```

## Fees

Min fee: 0.0001 SRX (10,000 sentri).

```
ceil(fee/2) → burned permanently
floor(fee/2) → block validator
```

Odd sentri goes to burn. Eventually burns > rewards = deflation.

## Address Format

`0x` + 40 hex chars. Derived from `Keccak-256(secp256k1 pubkey)[12..32]`. Same as Ethereum — MetaMask/ethers.js work out of the box.

## Constants

```rust
CHAIN_ID           = 7119
MAX_SUPPLY         = 21_000_000_000_000_000  // 210M in sentri
BLOCK_REWARD       = 100_000_000             // 1 SRX
HALVING_INTERVAL   = 42_000_000
BLOCK_TIME_SECS    = 3
MIN_TX_FEE         = 10_000                  // 0.0001 SRX
CHAIN_WINDOW_SIZE  = 1000
STATE_ROOT_FORK_HEIGHT = 100_000
```
