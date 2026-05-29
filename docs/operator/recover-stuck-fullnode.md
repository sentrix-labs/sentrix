# Recover A Stuck Fullnode

Use this runbook for a non-validator fullnode that is healthy at the process
level but not advancing height.

Never wipe data as the first option.

## 1. Check Current Height

If RPC is enabled:

```bash
curl http://localhost:8545/health
curl http://localhost:8545/chain/info | jq
```

Local binary:

```bash
SENTRIX_DATA_DIR=./data/fullnode \
SENTRIX_ENCRYPTED_DISK=true \
./target/release/sentrix chain info
```

Compare the height with a trusted peer or public RPC for the same network.

## 2. Check Logs

Systemd:

```bash
sudo journalctl -u <fullnode-service-name> -n 300 --no-pager
```

Docker:

```bash
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode logs --tail=300 sentrix-fullnode
```

Look for:

- repeated `Invalid block`;
- `bft_tx FULL`;
- `apply_watchdog`;
- repeated peer dial errors;
- storage or permission errors.

## 3. Check Networking And Peers

Confirm the fullnode is listening:

```bash
ss -ltnp | grep -E '(:8545|:30303)'
```

Confirm the `--peers` value is current. If it is not known:

```bash
TODO: confirm current bootstrap peers for the target network.
```

## 4. Verify Disk Space

```bash
df -h
du -sh ./data/fullnode/chain.db
```

Low disk or a full filesystem can make the node appear stuck.

## 5. Verify Config And Genesis

For testnet, confirm both are true:

```bash
echo "$SENTRIX_CHAIN_ID"
test -f genesis/testnet.toml
```

The running command should include:

```bash
--genesis genesis/testnet.toml
```

For mainnet, the command should omit `--genesis` unless maintainers explicitly
provided a replacement genesis.

## 6. Safe Recovery Path

Restart only the fullnode first:

```bash
sudo systemctl restart <fullnode-service-name>
```

or Docker:

```bash
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode restart sentrix-fullnode
```

Then re-check height:

```bash
curl http://localhost:8545/chain/info | jq
```

If the node catches up, keep it running and continue monitoring logs.

## 7. Back Up Before Deeper Repair

Stop the fullnode and back up data before any deeper operation:

```bash
sudo systemctl stop <fullnode-service-name>
tar -C . -czf "$HOME/sentrix-fullnode-data-$(date -u +%Y%m%dT%H%M%SZ).tar.gz" data/fullnode
```

Docker:

```bash
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode stop sentrix-fullnode
tar -C . -czf "$HOME/sentrix-fullnode-data-$(date -u +%Y%m%dT%H%M%SZ).tar.gz" data/fullnode
```

## 8. Unsafe Last Resort

Deleting `chain.db` or the mounted data directory forces a fresh resync and
destroys local chain history/cache.

Only do this intentionally after:

- a backup exists;
- maintainers confirm the node should resync from scratch;
- current bootstrap peers are known and reachable.

Command intentionally omitted. This runbook does not recommend wiping data as a
normal recovery step.
