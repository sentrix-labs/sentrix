#!/usr/bin/env bash
# install-testnet-watchdog.sh — One-shot installer for the testnet
# livelock watchdog on Core node.
#
# Run locally on build host (or whoever has satya_master SSH). Copies the
# watchdog script to Core node, writes a systemd oneshot service + timer
# pair, enables the timer. Idempotent — safe to re-run.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WATCHDOG_SCRIPT="$SCRIPT_DIR/testnet-livelock-watchdog.sh"
CORE_HOST="${CORE_HOST:-}"
if [[ -z "$CORE_HOST" ]]; then
    echo "CORE_HOST not set — export CORE_HOST=<user>@<ip> before running" >&2
    exit 2
fi
SSH_KEY="${SSH_KEY:-$HOME/.ssh/satya_master}"
SSH="ssh -i $SSH_KEY -o StrictHostKeyChecking=accept-new $CORE_HOST"

[[ -f "$WATCHDOG_SCRIPT" ]] || { echo "missing $WATCHDOG_SCRIPT"; exit 1; }

echo "==> Uploading watchdog script to Core node"
scp -i "$SSH_KEY" "$WATCHDOG_SCRIPT" "$CORE_HOST:/tmp/testnet-livelock-watchdog.sh"

echo "==> Installing watchdog to /usr/local/bin"
$SSH 'sudo install -m 0755 /tmp/testnet-livelock-watchdog.sh /usr/local/bin/sentrix-testnet-livelock-watchdog && rm /tmp/testnet-livelock-watchdog.sh'

echo "==> Writing systemd unit + timer"
$SSH 'sudo tee /etc/systemd/system/sentrix-testnet-watchdog.service > /dev/null <<EOF
[Unit]
Description=Sentrix testnet livelock watchdog (backlog #1d workaround)
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/bin/sentrix-testnet-livelock-watchdog
# Watchdog itself should be cheap; generous timeout only in case
# systemctl restart fans out slowly.
TimeoutStartSec=120

[Install]
WantedBy=multi-user.target
EOF'

$SSH 'sudo tee /etc/systemd/system/sentrix-testnet-watchdog.timer > /dev/null <<EOF
[Unit]
Description=Run Sentrix testnet watchdog every minute

[Timer]
OnBootSec=60
OnUnitActiveSec=60
AccuracySec=5
Unit=sentrix-testnet-watchdog.service

[Install]
WantedBy=timers.target
EOF'

echo "==> Enabling + starting timer"
$SSH 'sudo systemctl daemon-reload && sudo systemctl enable --now sentrix-testnet-watchdog.timer'

echo "==> Verification"
$SSH 'sudo systemctl status sentrix-testnet-watchdog.timer --no-pager | head -10'
echo
echo "==> Tail logs with:"
echo "    ssh $CORE_HOST 'journalctl -t sentrix-livelock-watchdog -f'"
