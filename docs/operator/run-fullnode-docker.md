# Run A Docker Fullnode

This Docker template runs a testnet fullnode only. It does not configure a
validator key and must not be used as a validator template.

Files:

- `docker-compose.fullnode.yml`
- `.env.fullnode.example`
- `Dockerfile`

The compose file builds locally from the repository Dockerfile. It does not
require a published container image.

## Data Safety

The fullnode stores persistent chain data in:

```text
./data/fullnode
```

DO NOT delete `./data/fullnode` or `./data/fullnode/chain.db` unless you
intentionally want to resync from scratch.

## Prepare Environment

```bash
cp .env.fullnode.example .env.fullnode
mkdir -p data/fullnode
```

Edit `.env.fullnode` and set `SENTRIX_PEERS` to current testnet bootstrap
peers if maintainers provide them.

If peers are not known:

```bash
TODO: confirm current testnet bootstrap peers.
```

## Start

```bash
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode up -d --build
```

The compose file:

- bind-mounts `./data/fullnode:/data`;
- mounts `./genesis/testnet.toml` read-only;
- starts `sentrix start --genesis /genesis/testnet.toml`;
- publishes RPC on `127.0.0.1:${SENTRIX_API_PORT}`;
- publishes P2P on `${SENTRIX_P2P_PORT}`;
- does not mount or pass validator keys.

## Logs

```bash
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode logs -f sentrix-fullnode
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode logs --tail=300 sentrix-fullnode
```

## Health And Sync

```bash
curl http://127.0.0.1:8545/health
curl http://127.0.0.1:8545/chain/info | jq
```

If you changed `SENTRIX_API_PORT`, use that port instead of `8545`.

Inside the container:

```bash
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode exec sentrix-fullnode sentrix chain info
```

## Stop And Restart

```bash
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode stop sentrix-fullnode
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode restart sentrix-fullnode
```

Stopping or restarting the container does not delete `./data/fullnode`.

## Backup

Stop the container before taking a file-level backup:

```bash
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode stop sentrix-fullnode
tar -C . -czf "$HOME/sentrix-fullnode-data-$(date -u +%Y%m%dT%H%M%SZ).tar.gz" data/fullnode
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode start sentrix-fullnode
```

## Upgrade Without Deleting Data

Pull or checkout the target code, then rebuild and restart:

```bash
git pull --ff-only
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode up -d --build
```

The bind-mounted `./data/fullnode` directory remains on the host.

After upgrade:

```bash
docker compose -f docker-compose.fullnode.yml --env-file .env.fullnode exec sentrix-fullnode sentrix --version
curl http://127.0.0.1:8545/chain/info | jq
```

## Troubleshooting

- Check `SENTRIX_PEERS`.
- Check disk space with `df -h` and `du -sh data/fullnode`.
- Check logs for `Invalid block`, `bft_tx FULL`, `apply_watchdog`, or peer
  dial errors.
- Confirm `genesis/testnet.toml` exists and `.env.fullnode` has
  `SENTRIX_CHAIN_ID=7120`.
