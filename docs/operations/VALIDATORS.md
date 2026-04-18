# Validators

3 validators across 3 VPS, round-robin PoA (v2.0.0).

## Current Set

| Slot | Name | Address prefix | VPS | Service |
|------|------|---------------|-----|---------|
| 0 | Sentrix Treasury | `0x0804...` | VPS2 | sentrix-val5 |
| 1 | Sentrix Foundation | `0x753f...` | VPS1 | sentrix-node |
| 2 | Sentrix Core | `0x87c9...` | VPS3 | sentrix-core |

Sorted by address. Block producer = `height % 3`.

(Nusantara, BlockForge Asia, PacificStake, Archipelago — decommissioned during v2.0.0 reset; services stopped, NOT on chain.)

## Adding a Validator

Needs admin auth + valid secp256k1 pubkey that derives to the address.

```bash
# CLI
sentrix validator add --address 0x... --public-key 04... --name "Name"

# API
curl -X POST http://[NODE_IP]:8545/validators \
  -H "Content-Type: application/json" \
  -H "X-API-Key: <key>" \
  -d '{"address":"0x...","public_key":"04...","name":"Name","caller":"<admin>"}'
```

## Changing Validator Set (Read This)

This is the one procedure that can brick the chain. Round-robin depends on all nodes having the exact same validator set.

```
1. Stop ALL nodes on ALL machines
2. Run add/remove on EVERY data directory
3. Start ALL nodes
```

If you add a validator to some nodes but not others, they'll disagree on whose turn it is → chain stalls permanently.

## Other Commands

```bash
sentrix validator list
sentrix validator toggle --address 0x... --active false
sentrix validator rename --address 0x... --name "New Name"
sentrix validator remove --address 0x...
```

Min 3 active validators enforced — can't go below that.

## Audit Trail

Every operation logged. View with:

```bash
curl -H "X-API-Key: <key>" http://[NODE_IP]:8545/admin/log
```

## Economics

Each validator produces ~28,800 blocks/day (3 validators × 1s blocks ÷ 3 slots). That's ~28,800 SRX/day per validator from rewards alone, plus `floor(fee/2)` from each included transaction.

## Voyager

DPoS: open registration with 15K SRX stake, top 100 by stake score, epoch-based rotation, slashing. See [Voyager](../roadmap/PHASE2.md).
