# Claim Rewards

Validators (and post-fork their delegators) accumulate `pending_rewards` in the `PROTOCOL_TREASURY` escrow on every block they help produce. To move those funds into a spendable balance, submit a `ClaimRewards` staking-op transaction.

## Status

Active on mainnet since h=590100 (2026-04-25). Default behaviour for v2.1.30+ binaries when `VOYAGER_REWARD_V2_HEIGHT` env var is set on the validator.

## Mechanism

```
Pre-V4 fork (`h<590100`):
  block produced → coinbase 1 SRX → validator balance (direct credit)

Post-V4 fork (h≥590100):
  block produced → coinbase 1 SRX → PROTOCOL_TREASURY
                                     ↓ (escrow, queryable)
                                     ↓
                  ClaimRewards tx ← validator/delegator
                                     ↓ (apply-time)
                  PROTOCOL_TREASURY → claimer balance
                  pending_rewards reset to 0
```

`PROTOCOL_TREASURY` = `0x0000000000000000000000000000000000000002`.

## When to claim

There is no deadline — accumulated rewards stay in escrow indefinitely. But:

- Slashing reduces `pending_rewards` before claim: misbehavior only takes from yet-to-be-claimed share
- Holding period reset: claiming triggers fresh accumulation cycle
- Operational liquidity: claimed SRX is spendable; escrowed SRX is not

Practical pattern: claim weekly or monthly, depending on accrual rate vs gas economics.

## Query pending rewards

```bash
curl -sf https://rpc.sentrixchain.com/staking/validators | jq '.validators[] | {address, pending_rewards, total_stake}'
```

Output (per validator):
```json
{
  "address": "0x753f2f68829fbe76a0132295624f48b27ce2e2d9",
  "pending_rewards": 375500000000,
  "total_stake": 1500000000000
}
```

`pending_rewards` is in sentri (1 SRX = 100,000,000 sentri).

## Submit a claim

The reference `tools/claim-rewards/` binary accepts the validator's secret key on stdin and submits a properly-encoded `StakingOp::ClaimRewards` transaction:

```bash
# Build it (one-time, on a host with the workspace + Rust toolchain)
cd /path/to/sentrix/tools/claim-rewards
cargo build --release
# Binary: ./target/release/claim-rewards

# Submit (interactive — privkey stays in process memory; never logged)
echo "<64-hex-privkey>" | ./target/release/claim-rewards \
  --rpc       https://rpc.sentrixchain.com \
  --chain-id  7119

# Add --dry-run to build + sign without POSTing (for verification)
```

Mainnet chain ID is `7119`; testnet is `7120`.

## Tx shape

Under the hood, `ClaimRewards` is a `StakingOp`-encoded transaction:

| Field | Value |
|---|---|
| `from_address` | The claimer (validator or delegator address) |
| `to_address` | `PROTOCOL_TREASURY` (`0x0000…0002`) |
| `amount` | `0` — the apply-time treasury credit handles fund movement; `tx.amount` is unused |
| `fee` | `MIN_TX_FEE` (10,000 sentri = 0.0001 SRX) |
| `data` | `{"op":"claim_rewards"}` (JSON-encoded `StakingOp::ClaimRewards`) |
| `chain_id` | 7119 (mainnet) or 7120 (testnet) |
| `signature` | secp256k1 ECDSA over canonical signing payload |

## After the claim

Within 1-2 blocks (roughly 1-2 seconds at 1s block time):

1. Mempool admits the tx
2. Proposer includes it in the next block
3. Block apply runs `ClaimRewards` dispatch:
   - Validator's `pending_rewards` and any delegator share is summed into `total_claim`
   - `accounts.transfer(PROTOCOL_TREASURY → claimer, total_claim, fee=0)`
   - Validator's `pending_rewards` reset to 0
4. Subsequent blocks accumulate fresh `pending_rewards`

Verify post-claim:

```bash
# Pending should be 0
curl -sf https://rpc.sentrixchain.com/staking/validators | \
  jq '.validators[] | select(.address=="0x<your-addr>") | .pending_rewards'

# Balance should have grown by claimed amount minus MIN_TX_FEE
curl -sf -X POST https://rpc.sentrixchain.com/rpc \
  -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getBalance","params":["0x<your-addr>","latest"],"id":1}'
```

## Failure modes

| Symptom | Cause | Fix |
|---|---|---|
| `validator … not found in active set` | Address derived from the privkey isn't a registered validator | Verify privkey corresponds to the validator address |
| `nothing to claim — pending_rewards = 0` | Already claimed, or no blocks produced yet | Wait for accumulation; no action |
| `insufficient balance for MIN_TX_FEE` | Claimer balance < 10,000 sentri | Top up balance manually; the fee is taken from balance, not pending_rewards |
| `wrong chain_id` (HTTP 400) | `--chain-id` mismatch with target network | Use 7119 for mainnet, 7120 for testnet |
| `nonce too low` | A previous tx from same address is still in mempool | Wait for inclusion or check tx status |

## Security

- The `claim-rewards` binary reads privkey from stdin only — never from CLI args or env vars (those would leak to process listings or shell history).
- Pipe directly from a wallet decryptor; never write privkey to a file.
- Run on a host with strict access controls (validator host or operator workstation behind 2FA).
- Privkey stays in process memory during signing only; not logged, not written to disk.
