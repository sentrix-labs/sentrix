# Run a Testnet Validator

This guide uses the installer that already exists in this repository:
`scripts/install-validator.sh`.

The installer creates a systemd service, builds the `sentrix` binary from this
repo, generates an encrypted validator keystore, and starts the node with the
testnet genesis file.

## Prerequisites

- Linux on `x86_64` or `aarch64`.
- Debian or Ubuntu with `apt-get`.
- At least 8 GiB RAM. The installer refuses lower memory.
- At least 60 GiB free disk on `/`. The installer refuses lower disk.
- `sudo` access.
- A safe place to store the validator keystore password.

Do not commit or paste validator keystores, private keys, `.env` files, or
wallet passwords.

## Install And Build

From a checked-out copy of this repository:

```bash
./scripts/install-validator.sh --network testnet --name sentrix-testnet-validator
```

The installer:

- installs required packages with `apt-get`;
- installs or updates Rust if needed;
- clones or updates `https://github.com/sentrix-labs/sentrix.git`;
- runs `cargo build --release -p sentrix-node`;
- installs the binary as `/opt/sentrix/sentrix` by default;
- copies `genesis/testnet.toml` to `/opt/sentrix/genesis-testnet.toml`;
- generates a validator keystore under `/opt/sentrix/data/wallets`;
- writes `/etc/sentrix/<name>.env`;
- writes `/etc/systemd/system/<name>.service`;
- starts the systemd service.

To use a pinned branch, tag, or local fork, use the installer's existing flags:

```bash
./scripts/install-validator.sh \
  --network testnet \
  --name sentrix-testnet-validator \
  --repo https://github.com/sentrix-labs/sentrix.git \
  --ref main
```

## Validator Key Setup

The installer runs:

```bash
sentrix wallet generate --password "<password>"
```

It stores the keystore under:

```text
/opt/sentrix/data/wallets/
```

It also writes a non-secret identity sidecar with the validator address and
public key. Back up the keystore and password separately.

## Start, Stop, Restart

Use the service name passed with `--name`:

```bash
sudo systemctl status sentrix-testnet-validator
sudo systemctl restart sentrix-testnet-validator
sudo systemctl stop sentrix-testnet-validator
sudo systemctl start sentrix-testnet-validator
```

Stopping or restarting the service does not delete chain data.

## Logs

```bash
sudo journalctl -u sentrix-testnet-validator -f
sudo journalctl -u sentrix-testnet-validator -n 200 --no-pager
```

## Height And Sync Checks

The node exposes HTTP API endpoints when the service is running:

```bash
curl http://localhost:8545/health
curl http://localhost:8545/chain/info | jq
```

Local binary checks:

```bash
SENTRIX_DATA_DIR=/opt/sentrix/data \
SENTRIX_ENCRYPTED_DISK=true \
/opt/sentrix/sentrix chain info
```

## Backups

Back up before upgrades or recovery work:

```bash
sudo systemctl stop sentrix-testnet-validator
sudo tar -C /opt/sentrix -czf "$HOME/sentrix-testnet-data-$(date -u +%Y%m%dT%H%M%SZ).tar.gz" data
sudo systemctl start sentrix-testnet-validator
```

DO NOT delete `/opt/sentrix/data`, `/opt/sentrix/data/chain.db`, or wallet
keystores unless you intentionally want a fresh resync and you have backed up
everything needed.

## TODO

- TODO: confirm current public testnet bootstrap peers before publishing a
  copy-paste `--peers` value for third-party operators.
