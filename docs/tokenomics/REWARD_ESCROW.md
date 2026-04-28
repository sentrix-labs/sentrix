---
sidebar_position: 5
title: Reward Escrow & Sentinel Addresses
---

# Reward escrow & protocol sentinels

Three addresses on Sentrix Chain hold balances but have **no private key** — they're protocol-reserved sentinels, mutated only by consensus-level operations. If you spot them in the explorer's top accounts and wonder where the funds came from, this is the explainer.

| Address | Role | Live mainnet balance (2026-04-28) |
|---|---|---|
| `0x0000000000000000000000000000000000000002` | **PROTOCOL_TREASURY** — V4 reward escrow | ~903K SRX |
| `0x0000000000000000000000000000000000000000` | **TOKEN_OP_ADDRESS** — TokenOp routing marker + EVM CREATE marker | ~0 SRX |
| `0x0000000000000000000000000000000000000100` | **STAKING_ADDRESS** — reserved (not currently used as `to`) | 0 SRX |

## Protocol Treasury (`0x0000…0002`) — the most important one

Active since the V4 reward-v2 fork at `VOYAGER_REWARD_V2_HEIGHT=590100` (2026-04-25). Pre-fork: every block's 1 SRX coinbase reward minted directly into the proposer's balance. Post-fork: coinbase mints into `PROTOCOL_TREASURY`. The stake registry tracks per-validator `pending_rewards` and per-delegator `delegator_rewards` as accumulators. Validators + delegators drain their share via `StakingOp::ClaimRewards`, which transfers from the treasury → claimer's balance.

**Therefore:** `balance(PROTOCOL_TREASURY)` ≈ sum of unclaimed rewards owed by the protocol to participants. **NOT a foundation-controlled treasury.** No keystore can move funds out — only the consensus-level `ClaimRewards` dispatch (gated by stake registry accumulator entries) can mutate this balance.

### Code paths

- [`crates/sentrix-primitives/src/transaction.rs`](https://github.com/sentrix-labs/sentrix/blob/main/crates/sentrix-primitives/src/transaction.rs) — `pub const PROTOCOL_TREASURY = "0x0000…0002"`
- [`crates/sentrix-core/src/block_executor.rs:789-793`](https://github.com/sentrix-labs/sentrix/blob/main/crates/sentrix-core/src/block_executor.rs#L789) — coinbase routing: `if Self::is_reward_v2_height(block.index) { coinbase_recipient = PROTOCOL_TREASURY }`
- [`crates/sentrix-staking/src/staking.rs::distribute_reward`](https://github.com/sentrix-labs/sentrix/blob/main/crates/sentrix-staking/src/staking.rs) — multi-signer pro-rata accumulator update
- `block_executor.rs::StakingOp::ClaimRewards` — drain path

### Daily flow

```
Block produced
  ↓
Coinbase 1 SRX → 0x0000…0002 (PROTOCOL_TREASURY)
  ↓
StakeRegistry tracks per-validator pending_rewards + per-delegator delegator_rewards (accumulators)
  ↓
Validator/delegator submits StakingOp::ClaimRewards
  ↓
Transfer from 0x0000…0002 → claimer's balance (drains the accumulator)
```

86,400 blocks/day × 1 SRX = ~86,400 SRX/day minted into treasury. Drain rate = sum of all daily ClaimRewards. Steady-state: drains roughly match mints; balance reflects "lazy claimers" who haven't bothered to drain accumulators yet.

## Token Op sentinel (`0x0000…0000`)

Marker address used in two distinct cases:

- **Native TokenOp routing** — `Transaction.to_address` of any `TokenOp::Deploy` / `Transfer` / `Burn` is set to this sentinel. The actual token-state mutation happens in the consensus dispatch logic (not as a regular balance transfer). Block executor recognizes the marker and routes to `ContractRegistry`.
- **EVM CREATE marker** — when an EVM tx has `to_address = 0x0` (= sentinel), block executor knows it's a contract deployment and routes to revm's `TxKind::Create`.

Balance under normal operation: 0 SRX. Token-burn ops have it as the destination but the burned supply is accounted in `total_burned`, not in this address's balance.

## Staking sentinel (`0x0000…0100`)

Reserved for staking-op routing convention. Not currently used as a tx `to` field — staking ops route via `PROTOCOL_TREASURY` in the V4 reward-v2 era. Reserved so dApps and tooling don't accidentally claim it as a regular EOA address.

## Audit verification

```bash
# Live balance via eth_getBalance
curl -s -X POST https://rpc.sentrixchain.com -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getBalance","params":["0x0000000000000000000000000000000000000002","latest"],"id":1}'

# Or via /accounts/top REST
curl -s "https://rpc.sentrixchain.com/accounts/top?limit=10" | jq '.accounts[] | select(.address=="0x0000000000000000000000000000000000000002")'

# In the explorer: https://scan.sentrixchain.com/address/0x0000000000000000000000000000000000000002
```

The explorer (`scan.sentrixchain.com`) labels these addresses as "Protocol Treasury (Reward Escrow)", "Sentrix Token Op (sentinel)", "Sentrix Staking (sentinel)" so they don't appear as unlabeled hex in the top-accounts view.

## See also

- [Tokenomics Overview](./OVERVIEW) — full SRX economic model
- [Staking](./STAKING) — Voyager DPoS reward distribution
- [Claim Rewards](../operations/CLAIM_REWARDS) — operator runbook for draining the escrow
