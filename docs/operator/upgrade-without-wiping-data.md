# Upgrade Without Wiping Data

This runbook upgrades the `sentrix` binary while preserving chain data.

DO NOT delete `chain.db` or the data directory unless you intentionally want a
fresh resync from scratch.

## 1. Identify Paths

Set these for your service:

```bash
SERVICE=sentrix-testnet-validator
INSTALL_DIR=/opt/sentrix
DATA_DIR=/opt/sentrix/data
NEW_BINARY=/path/to/new/sentrix
```

Use the actual service and install paths from your host.

## 2. Check Current State

```bash
sudo systemctl status "$SERVICE"
"$INSTALL_DIR/sentrix" --version
SENTRIX_DATA_DIR="$DATA_DIR" SENTRIX_ENCRYPTED_DISK=true "$INSTALL_DIR/sentrix" chain info
```

If RPC is enabled:

```bash
curl http://localhost:8545/health
curl http://localhost:8545/chain/info | jq
```

## 3. Back Up The Current Binary

```bash
stamp=$(date -u +%Y%m%dT%H%M%SZ)
sudo mkdir -p "$INSTALL_DIR/releases"
sudo cp -a "$INSTALL_DIR/sentrix" "$INSTALL_DIR/releases/sentrix-$("$INSTALL_DIR/sentrix" --version | awk '{print $2}')-$stamp"
```

## 4. Back Up Data

Stop the service first so the backup is not taken while MDBX is being written:

```bash
sudo systemctl stop "$SERVICE"
sudo tar -C "$INSTALL_DIR" -czf "$HOME/sentrix-data-$stamp.tar.gz" "$(basename "$DATA_DIR")"
```

For very large nodes, use your normal filesystem snapshot or backup tooling.
The key requirement is that the backup is taken while the node is stopped.

## 5. Replace Binary

```bash
sudo install -m 0755 "$NEW_BINARY" "$INSTALL_DIR/sentrix"
sudo chown -R "$(id -un):$(id -gn)" "$INSTALL_DIR"
"$INSTALL_DIR/sentrix" --version
```

If your install directory is intentionally root-owned, keep the ownership model
from your existing systemd unit instead of copying the `chown` line.

## 6. Restart And Verify

```bash
sudo systemctl start "$SERVICE"
sudo systemctl status "$SERVICE"
sudo journalctl -u "$SERVICE" -n 200 --no-pager
```

Check height:

```bash
curl http://localhost:8545/chain/info | jq
```

or:

```bash
SENTRIX_DATA_DIR="$DATA_DIR" SENTRIX_ENCRYPTED_DISK=true "$INSTALL_DIR/sentrix" chain info
```

## Rollback

If the new binary fails before applying new blocks, stop the service and restore
the previous binary from `$INSTALL_DIR/releases/`.

Do not roll back after a coordinated fork or migration unless maintainers
explicitly confirm it is safe.
