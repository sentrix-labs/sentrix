# Validators

7 validators across 3 VPS, round-robin PoA.

## Current Set

| Slot | Name | Address prefix | VPS |
|------|------|---------------|-----|
| 0 | Sentrix Treasury | `0x0804...` | 2 |
| 1 | Sentrix Foundation | `0x753f...` | 1 |
| 2 | BlockForge Asia | `0x7be6...` | 2 |
| 3 | PacificStake | `0x7dcc...` | 2 |
| 4 | Sentrix Core | `0x87c9...` | 3 |
| 5 | Archipelago Network | `0xd211...` | 2 |
| 6 | Nusantara Node | `0xdd3c...` | 2 |

Sorted by address. Block producer = `height % 7`.

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

Each validator produces ~4,114 blocks/day (with 7 validators, 3s blocks). That's ~4,114 SRX/day from rewards alone, plus `floor(fee/2)` from each included transaction.

## Voyager

DPoS: open registration with 15K SRX stake, top 100 by stake score, epoch-based rotation, slashing. See [Voyager](../roadmap/PHASE2.md).
