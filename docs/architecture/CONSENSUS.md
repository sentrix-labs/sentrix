# Consensus

Sentrix runs PoA consensus. Validators take turns producing blocks in round-robin order, one block every second.

## Round-Robin

Validators sorted by address (lowercase). Producer picked by:

```rust
sorted_validators[block_height % validator_count]
```

Deterministic — every node computes the same result independently. No communication needed.

With 4 validators and 1s blocks, each one produces a block every 4 seconds (1s × 4 slots).

> **Operational note (2026-04-25):** mainnet currently runs forced
> Pioneer (`SENTRIX_FORCE_PIONEER_MODE=1` env override on every
> validator) after a Voyager DPoS+BFT activation attempt at
> h=557244 livelocked. The Voyager fork height is parked at
> `u64::MAX` until V2 BFT wiring (issue #292) lands. See
> [EMERGENCY_ROLLBACK](../operations/EMERGENCY_ROLLBACK.md).

## Block Production

When it's your turn (`block_producer.rs`):

1. Check `height % count` matches your slot
2. Build coinbase tx (1 SRX reward)
3. Clone mempool, take up to 5,000 txs sorted by fee (highest first)
4. Assemble block: index, prev hash, timestamp, txs, merkle root

Mempool is cloned not drained — if the block gets rejected, txs survive.

## Two-Pass Validation

Every received block goes through `add_block()`:

Pass 1 (read-only): Check structure, validator auth, all tx signatures/nonces/balances, merkle root. If anything fails → reject entire block. No state changes.

Pass 2 (commit): Credit coinbase, execute transfers, distribute fees (ceil/2 burn, floor/2 validator), run token ops, update trie, persist to MDBX.

All-or-nothing. No partial state changes.

## Validator Management

Adding a validator needs:
- Admin auth (string comparison with admin address)
- Valid secp256k1 pubkey that derives to the claimed address
- `Wallet::derive_address(pubkey) == address` checked

`MIN_ACTIVE_VALIDATORS = 1` (since v2.1.11, PR #234) — chain can
proceed with a single active validator if the rest are jailed or
inactive. `MIN_BFT_VALIDATORS = 4` is the BFT-quorum floor for
Voyager activation.

Every add/remove/toggle/rename gets logged in the admin audit trail.

### Changing Validator Set (Critical)

This is the one thing that can brick your chain:

```
1. Stop ALL nodes
2. Run validator add/remove on EVERY data directory
3. Start ALL nodes
```

Doing this while nodes are running = scheduling mismatch = chain stall. Don't.

## Timestamp Rules

```
block.timestamp >= previous.timestamp    (monotonic)
block.timestamp <= now + 15s             (not too far ahead)
```

## Known Limitations

- No fork choice. First block at a height wins. Network partitions can cause permanent splits. Fine for 4 controlled validators, needs fixing before scaling.
- No block skip. If expected validator is offline, chain waits. No timeout.
- No block signatures. Block has validator address but isn't signed. Auth is via round-robin schedule check. Signing needed for Voyager.

## Voyager

Replaces PoA with DPoS: open registration (15K SRX stake), top 100 by stake, BFT finality (2/3+ votes), slashing. See [Voyager](../roadmap/PHASE2.md).
