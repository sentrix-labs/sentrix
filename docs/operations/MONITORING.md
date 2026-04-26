# Monitoring & Troubleshooting

> **Consensus context (post-Voyager activation 2026-04-25):** Both mainnet and testnet run **Voyager DPoS+BFT** (`voyager_activated=true`). PoA round-robin (Pioneer phase) was the bootstrap consensus until h=579047 mainnet / h=10 testnet. Health checks and recovery procedures below reflect Voyager semantics — `/sentrix_status` returns `consensus: "DPoS+BFT"`, blocks carry `BlockJustification` with BFT precommit signatures, validator selection is stake-weighted.

## Quick Checks

```bash
# Chain height + status (consensus_mode = "voyager", reports max_supply fork-aware)
curl -sf http://[NODE_IP]:8545/chain/info | jq

# Health endpoint
curl -sf http://[NODE_IP]:8545/health

# Sentrix status — explicit consensus mode + sync info
curl -sf http://[NODE_IP]:8545/sentrix_status | jq

# Compare heights across nodes
for ip in [NODE_1] [NODE_2] [NODE_3] [NODE_4]; do
  echo "$ip: $(curl -sf http://$ip:8545/chain/finalized-height | jq -c '{h: .latest_height, hash: (.finalized_hash[:16])}')"
done

# Validator status (stake-weighted active set, jail/tombstone state, pending rewards)
curl -sf http://[NODE_IP]:8545/staking/validators | jq '.validators[] | {addr: .address[:14], active: .is_active, jailed: .is_jailed, tombstoned: .is_tombstoned, missed: .blocks_missed, pending: .pending_rewards}'

# Mempool size
curl -sf http://[NODE_IP]:8545/mempool | jq '. | length'

# Disk usage
du -sh /opt/sentrix/data/chain.db/
```

## What to Watch

| Metric | Healthy |
|--------|---------|
| Chain height | Increasing every ~1s under nominal load |
| Consensus mode | `voyager` (post-2026-04-25 mainnet, post-2026-04-23 testnet) |
| Active validators | 4 (mainnet) / 4 (testnet) |
| Jailed validators | 0 — any non-zero indicates auto-jail divergence (see "Chain stalled" below) |
| BFT round (per height) | 0 most blocks; sustained round > 5 = BFT having trouble reaching supermajority |
| Mempool | < 10,000 |
| Service status | Active (running) |

## Common Issues

### Chain stalled

Height not moving for 30+ seconds.

**Voyager BFT supermajority loss:** Chain needs 3/4 stake-weighted precommits to finalize. Check `/staking/validators` on each node — if active counts differ across peers (e.g., Foundation+Beacon see active=3, Treasury+Core see active=4), it's **jail-state divergence**. See dedicated section below.

**Validator offline:** Check which validator's slot is expected for the current BFT round (`/sentrix_status` shows latest activity). Bring that validator back up. With 3 of 4 active, BFT can still finalize round-0; chain advances at degraded rate.

**Network partition:** Check libp2p connections (`journalctl -u sentrix-node | grep -E 'libp2p|peer'`). Check firewall (`ufw status`). Each validator should advertise its multiaddr via gossipsub `sentrix/validator-adverts/1` topic — verify it's broadcasting.

**Cold-start gate held:** Post-restart, validators wait for `peer_count >= active_set.len() - 1` (= ≥3 for 4-validator mesh) before entering BFT. If logs show "L2 cold-start gate: BFT activation blocked", verify systemd `--peers` arg lists ALL 3 other validators.

**State divergence at fork boundary:** All 4 validators should agree on block hashes at any specific height. If `/chain/blocks/<height>` returns different hashes across peers, recovery is chain.db rsync from canonical (see EMERGENCY_ROLLBACK.md § State Recovery).

### Jail-state divergence (Voyager-specific)

**Symptom:** Chain stalls. `/staking/validators` reports differ across peers — some see N validators active, others see N-1. P1 BFT safety gate trips on the lower-count peers ("active set < minimum 4 for BFT safety"). 2 of 4 attempting BFT but supermajority unreachable.

**Cause:** Per-validator slashing module's auto-jail counts missed proposals locally with timing-dependent increment. Sequential rolling restart causes the validator currently in its propose slot to miss the slot during its down-window. Whether peers count this miss inconsistently depends on observation timing → divergent local jail state.

**Recovery (~15 min):** Halt all 4 simultaneously, forensic backup divergent chain.db on each, tar-pipe canonical chain.db (one with majority signer-set view) → others, MD5 parity confirm, simultaneous start. See EMERGENCY_ROLLBACK.md § Worked Example #2 (h=633599 stall, 2026-04-26 evening).

**Prevention:** **NEVER use rolling sequential restart on mainnet** for env-var changes or restarts where consensus rules don't change between old/new state. Use halt-all + simultaneous-start. Same pattern was previously documented for testnet (2026-04-20 incident); 2026-04-26 evening confirmed it on mainnet.

### Block sync stuck

Validator received blocks but isn't applying them. Look for:

- `libp2p sync: block N failed: Invalid block: expected index N+1, got N` — duplicate-block-from-overlapping-GetBlocks-races. Pre-v2.1.37 caused cascade-bail. v2.1.37+ has the race-safe filter — should not stall but watch for `cumulative skipped (already-applied) crossed N` WARN log indicating the race is firing frequently.
- `expected_index N, got M` where M > N+1 — block gap. Need backfill from peer with full history.
- `Old validator not authorized` — validator set mismatch with chain history. Copy chain.db from synced node.

### Fork (different block hashes at same height)

`/chain/blocks/<height>` returns different `hash` across peers. Recovery: chain.db rsync from canonical (see EMERGENCY_ROLLBACK.md § State Recovery for full procedure). 2026-04-26 morning incident (h=604547, 4-way state divergence) is a worked example — RCA at `incidents/2026-04-26-libp2p-sync-cascade-bail-stall.md` (founder-private).

### Node won't start

**"Address already in use":** `lsof -i :8545` or `ss -tlnp | grep 8545`

**"Binary locked" (Windows):** `taskkill //F //IM sentrix.exe`

**"Permission denied":** `chmod +x /opt/sentrix/sentrix`

### Sync stuck

- Check if peer is actually ahead
- Look for "rejected block" in logs (chain divergence)
- "Old validator not authorized" = validator set doesn't match chain history. Copy chain.db from a synced node.

### DB corruption

MDBX is crash-safe, so this is rare. If it happens:

```bash
sudo systemctl stop sentrix-node
cp -r /opt/sentrix/data /opt/sentrix/data.backup
# Option 1: copy chain.db from healthy node
# Option 2: delete chain.db and resync from peers
sudo systemctl start sentrix-node
```

### Trie issues

```bash
sentrix chain reset-trie  # rebuild from current accounts
```

## Logs

```bash
sudo journalctl -u sentrix-node -f        # follow
sudo journalctl -u sentrix-node -n 100     # last 100 lines
```

```bash
RUST_LOG=info                              # default
RUST_LOG=debug                             # more detail
RUST_LOG=sentrix=debug,libp2p=warn         # per-module
```
