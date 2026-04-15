# Monitoring & Troubleshooting

## Quick Checks

```bash
# Chain height + status
curl -sf http://[NODE_IP]:8545/chain/info | jq

# Health endpoint
curl -sf http://[NODE_IP]:8545/health

# Compare heights across nodes
for ip in [NODE_1] [NODE_2] [NODE_3]; do
  echo "$ip: $(curl -sf http://$ip:8545/chain/info | jq .height)"
done

# Validator status
curl -sf http://[NODE_IP]:8545/validators | jq '.[] | {name, active, blocks_produced}'

# Mempool size
curl -sf http://[NODE_IP]:8545/mempool | jq '. | length'

# Disk usage
du -sh /opt/sentrix/data/chain.db/
```

## What to Watch

| Metric | Healthy |
|--------|---------|
| Chain height | Increasing every ~3s |
| Active validators | 7 |
| Mempool | < 10,000 |
| Service status | Active (running) |

## Common Issues

### Chain stalled

Height not moving for 30+ seconds.

Validator offline: Check which slot is expected (`height % 7`). Bring that validator back up.

Network partition: Check logs for peer connections (`journalctl -u sentrix-node | grep peer`). Check firewall (`ufw status`).

Validator set mismatch: Different nodes have different validator sets. Stop all → make all data dirs identical → restart all.

Fork: Compare block hashes at same height. If different, stop the forked node → copy chain.db from healthy node → restart.

### Node won't start

**"Address already in use":** `lsof -i :8545` or `ss -tlnp | grep 8545`

**"Binary locked" (Windows):** `taskkill //F //IM sentrix.exe`

**"Permission denied":** `chmod +x /opt/sentrix/sentrix`

### Sync stuck

- Check if peer is actually ahead
- Look for "rejected block" in logs (chain divergence)
- "Old validator not authorized" = validator set doesn't match chain history. Copy chain.db from a synced node.

### DB corruption

sled is crash-safe, so this is rare. If it happens:

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
