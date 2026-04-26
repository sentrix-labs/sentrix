# Testnet Recovery

Steps to recover the Sentrix testnet when validators are stuck or chain has stalled.

## Symptoms

- Block height not advancing for >30 seconds
- BFT round numbers climbing rapidly (100+) without producing blocks
- Mempool full of stale/invalid transactions
- Validators healthy (health endpoint returns OK) but not producing blocks

## Quick Recovery: Simultaneous Restart

If the chain stalled due to BFT round desync (validators in different rounds and can't reach quorum):

```bash
# Stop all 4 validators AT THE SAME TIME
sudo systemctl stop sentrix-testnet-val1 sentrix-testnet-val2 sentrix-testnet-val3 sentrix-testnet-val4

# Wait for processes to fully exit
sleep 5

# Start all 4 AT THE SAME TIME
sudo systemctl start sentrix-testnet-val1 sentrix-testnet-val2 sentrix-testnet-val3 sentrix-testnet-val4

# Wait for BFT to converge (15-30 seconds)
sleep 20

# Verify height is advancing
for p in 9545 9546 9547 9548; do
  echo "port $p: $(curl -s http://localhost:$p/chain/info | python3 -c 'import sys,json;print(json.load(sys.stdin)["height"])')"
done
```

## Stuck Mempool Recovery

If the mempool has stale/bad-nonce transactions that prevent block inclusion:

```bash
# Stop affected validator
sudo systemctl stop sentrix-testnet-val1

# Clear mempool (requires MDBX exclusive access)
SENTRIX_DATA_DIR=/opt/sentrix-testnet/data SENTRIX_ENCRYPTED_DISK=true \
  sentrix mempool clear

# OR: copy chain.db from a healthy validator (preserves chain state, clears mempool)
sudo rm -rf /opt/sentrix-testnet/data/chain.db
sudo cp -r /opt/sentrix-testnet/data2/chain.db /opt/sentrix-testnet/data/chain.db
sudo chown -R sentriscloud:sentriscloud /opt/sentrix-testnet/data/chain.db

# Restart
sudo systemctl start sentrix-testnet-val1
```

**Do NOT copy `identity/` between validators** — each node must keep its unique identity keypair.

## State Root Mismatch

If validators diverge on state_root (different account states after an upgrade):

```bash
# Stop all 4
sudo systemctl stop sentrix-testnet-val{1..4}

# Reset trie on all 4 (rebuild from AccountDB)
for i in 1 2 3 4; do
  D=/opt/sentrix-testnet/data
  [ $i -gt 1 ] && D=/opt/sentrix-testnet/data$i
  SENTRIX_DATA_DIR=$D SENTRIX_ENCRYPTED_DISK=true sentrix chain reset-trie
done

# Start all 4
sudo systemctl start sentrix-testnet-val{1..4}
```

## Nuclear Reset (Full Chain Reset)

If all else fails, reinitialize the testnet from genesis:

```bash
# Source env file with admin key + validator configs
source /path/to/reset_testnet.env

# Run reset script (stops validators, clears data, re-initializes, starts)
bash /path/to/benchmark/reset_testnet.sh
```

This resets the chain to height 0 and re-registers all 4 validators. MetaMask users will need to reset their account (Settings → Advanced → Reset Account).

## Prevention

- Don't send transactions with wrong nonces to testnet validators
- Set appropriate rate limits (`SENTRIX_WRITE_RATE_LIMIT` env var)
- Monitor BFT round numbers in logs — rapid climbing (>10 rounds/minute) indicates desync
- After any multi-validator restart, always do a simultaneous stop/start
