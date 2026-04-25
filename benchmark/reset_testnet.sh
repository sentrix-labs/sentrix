#!/bin/bash
# reset_testnet.sh — Full testnet reset with 4 validators properly registered.
# Run on the testnet host (e.g. Core node as the service user).
#
# Configuration via env vars (NEVER hardcode in this file):
#   SENTRIX_ADMIN_KEY  — admin private key (raw hex, no 0x prefix)
#   SENTRIX_FOUNDER    — admin/founder address (0x + 40 hex)
#   TESTNET_VAL1_ADDR  — testnet validator 1 address
#   TESTNET_VAL1_PUB   — testnet validator 1 uncompressed public key (130 hex chars, 04 prefix)
#   TESTNET_VAL2_ADDR / TESTNET_VAL2_PUB
#   TESTNET_VAL3_ADDR / TESTNET_VAL3_PUB
#   TESTNET_VAL4_ADDR / TESTNET_VAL4_PUB
#   SENTRIX_DATA_PARENT — parent dir for data dirs (default /opt/sentrix-testnet)
#   SENTRIX_BIN         — path to sentrix binary (default /opt/sentrix/sentrix)
#   SENTRIX_USER        — service user (default sentriscloud)
#   SENTRIX_CHAIN_ID    — chain id (default 7120 = testnet)

set -e

# ── Required env-var checks ──────────────────────────────────
require() {
  if [ -z "${!1}" ]; then
    echo "ERROR: $1 env var required (no hardcoded secrets in this script)" >&2
    exit 1
  fi
}

require SENTRIX_ADMIN_KEY
require SENTRIX_FOUNDER
require TESTNET_VAL1_ADDR
require TESTNET_VAL1_PUB
require TESTNET_VAL2_ADDR
require TESTNET_VAL2_PUB
require TESTNET_VAL3_ADDR
require TESTNET_VAL3_PUB
require TESTNET_VAL4_ADDR
require TESTNET_VAL4_PUB

# ── Defaults ──────────────────────────────────────────────────
DATA_PARENT="${SENTRIX_DATA_PARENT:-/opt/sentrix-testnet}"
SENTRIX="${SENTRIX_BIN:-/opt/sentrix/sentrix}"
SVC_USER="${SENTRIX_USER:-sentriscloud}"
CHAIN_ID="${SENTRIX_CHAIN_ID:-7120}"

echo "=== Stopping testnet validators ==="
sudo systemctl stop sentrix-testnet-val1 sentrix-testnet-val2 sentrix-testnet-val3 sentrix-testnet-val4 2>/dev/null || true

echo "=== Clearing testnet state ==="
for i in 1 2 3 4; do
  D="$DATA_PARENT/data"
  [ "$i" -gt 1 ] && D="$DATA_PARENT/data$i"
  sudo rm -rf "$D/chain.db" "$D/identity"
done

# ── Per-data-dir init + validator registration ───────────────
for i in 1 2 3 4; do
  D="$DATA_PARENT/data"
  [ "$i" -gt 1 ] && D="$DATA_PARENT/data$i"

  echo
  echo "=== Setting up $D (val$i) ==="

  sudo -u "$SVC_USER" env \
    SENTRIX_DATA_DIR="$D" \
    SENTRIX_CHAIN_ID="$CHAIN_ID" \
    "$SENTRIX" init --admin "$SENTRIX_FOUNDER" 2>&1 | tail -1

  for v in 1 2 3 4; do
    case $v in
      1) VA="$TESTNET_VAL1_ADDR"; VP="$TESTNET_VAL1_PUB"; VN="testnet-val1" ;;
      2) VA="$TESTNET_VAL2_ADDR"; VP="$TESTNET_VAL2_PUB"; VN="testnet-val2" ;;
      3) VA="$TESTNET_VAL3_ADDR"; VP="$TESTNET_VAL3_PUB"; VN="testnet-val3" ;;
      4) VA="$TESTNET_VAL4_ADDR"; VP="$TESTNET_VAL4_PUB"; VN="testnet-val4" ;;
    esac
    sudo -u "$SVC_USER" env \
      SENTRIX_DATA_DIR="$D" \
      SENTRIX_CHAIN_ID="$CHAIN_ID" \
      SENTRIX_ADMIN_KEY="$SENTRIX_ADMIN_KEY" \
      "$SENTRIX" validator add "$VA" "$VN" "$VP" 2>&1 | tail -1
  done
done

echo
echo "=== Starting all validators simultaneously ==="
sudo systemctl daemon-reload
sudo systemctl start sentrix-testnet-val1 sentrix-testnet-val2 sentrix-testnet-val3 sentrix-testnet-val4
sleep 15

echo
echo "=== Health check ==="
for i in 1 2 3 4; do
  PORT=$((9544 + i))
  H=$(curl -sf "http://localhost:$PORT/chain/info" 2>/dev/null \
      | python3 -c "import sys,json; d=json.load(sys.stdin); print(f'height={d[\"height\"]} validators={d[\"active_validators\"]}')" 2>/dev/null \
      || echo "DOWN")
  echo "val$i (port $PORT): $H"
done
