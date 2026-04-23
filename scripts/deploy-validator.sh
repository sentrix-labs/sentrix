#!/usr/bin/env bash
# deploy-validator.sh — generic single-validator deploy tool.
#
# Works for ANY operator running ANY number of Sentrix validators on
# ANY mix of hosts. Unlike `fast-deploy.sh` (which is the Satya-fleet
# orchestrator — hardcoded to the 3-mainnet + 4-testnet VPS1/2/3/4
# topology that ships the reference mainnet) this script is a reusable
# primitive: it takes one target validator, uploads a binary, rolls
# the service, and reports health.
#
# Typical use by a third-party validator operator (not Satya):
#
#   ./scripts/deploy-validator.sh \
#     --ssh-key  ~/.ssh/my_operator_key \
#     --host     operator@validator1.example.com \
#     --service  sentrix-node \
#     --bin-dir  /opt/sentrix \
#     --rpc-url  http://127.0.0.1:8545 \
#     --binary   ./target/release/sentrix
#
# Typical use internally (fleet wrapper calling per-host):
#
#   for H in "${HOSTS[@]}"; do
#       ./scripts/deploy-validator.sh --host "$H" --service sentrix-node \
#         --bin-dir /opt/sentrix --rpc-url http://127.0.0.1:8545 \
#         --binary ./target/release/sentrix
#   done
#
# The script is intentionally ops-topology-agnostic:
#   - No VPS1/VPS2/VPS3 labels
#   - No mainnet/testnet split (operator picks bin-dir + service name)
#   - No fleet env file — every parameter is explicit
#   - No cross-host assumptions (no "health-gate testnet before mainnet"
#     logic that only makes sense in Satya's mixed topology)
#
# For health-gating, rolling restart across a fleet, and preflight
# cargo test / clippy, see `fast-deploy.sh` — it wraps this script
# for Satya's specific fleet layout.

set -euo pipefail

# ── Argument parsing ────────────────────────────────────────
SSH_KEY=""
HOST=""
SERVICE=""
BIN_DIR=""
RPC_URL=""
BINARY=""
WAIT_SEC="30"
HEALTH_PATH="/health"
RELEASES_KEEP="3"

usage() {
    cat >&2 <<EOF
Usage: $0 [options]

Required:
  --ssh-key <path>    SSH private key used for the scp + ssh ops.
  --host <user@addr>  Target host (scp'ed to; systemd-controlled there).
  --service <name>    systemd unit name (e.g. sentrix-node).
  --bin-dir <path>    Directory on the host where 'sentrix' binary lives
                      (e.g. /opt/sentrix or /opt/sentrix-testnet).
  --rpc-url <url>     Local-to-host RPC URL for health check
                      (e.g. http://127.0.0.1:8545 — the script greps
                      /health and /chain/info).
  --binary <path>     Local release binary to upload.

Optional:
  --wait-sec <n>      Seconds to sleep after restart before health
                      check. Default 30.
  --health-path <p>   Path appended to --rpc-url. Default /health.
  --releases-keep <n> Previous binaries kept under <bin-dir>/releases/.
                      Default 3.
  -h, --help          Show this.
EOF
    exit 2
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ssh-key)       SSH_KEY="$2"; shift 2;;
        --host)          HOST="$2"; shift 2;;
        --service)       SERVICE="$2"; shift 2;;
        --bin-dir)       BIN_DIR="$2"; shift 2;;
        --rpc-url)       RPC_URL="$2"; shift 2;;
        --binary)        BINARY="$2"; shift 2;;
        --wait-sec)      WAIT_SEC="$2"; shift 2;;
        --health-path)   HEALTH_PATH="$2"; shift 2;;
        --releases-keep) RELEASES_KEEP="$2"; shift 2;;
        -h|--help)       usage;;
        *)               echo "unknown arg: $1" >&2; usage;;
    esac
done

# Require all core args.
for var in SSH_KEY HOST SERVICE BIN_DIR RPC_URL BINARY; do
    if [[ -z "${!var}" ]]; then
        echo "missing required arg: --${var,,} (--$(echo $var | tr _ - | tr '[:upper:]' '[:lower:]'))" >&2
        usage
    fi
done

[[ -f "$SSH_KEY" ]] || { echo "ssh key not readable: $SSH_KEY" >&2; exit 2; }
[[ -f "$BINARY" ]] || { echo "binary not found: $BINARY" >&2; exit 2; }

# ── Helpers ────────────────────────────────────────────────
SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10"

red()   { printf '\033[31m%s\033[0m' "$*"; }
green() { printf '\033[32m%s\033[0m' "$*"; }
blue()  { printf '\033[34m%s\033[0m' "$*"; }

# ── Phase 1: upload ────────────────────────────────────────
echo "  $(blue '=>') Upload $(basename "$BINARY") → $HOST:/tmp/sentrix_new"
scp $SSH_OPTS "$BINARY" "$HOST:/tmp/sentrix_new" >/dev/null

# ── Phase 2: archive + swap ────────────────────────────────
echo "  $(blue '=>') Archive previous binary + install new"
ssh $SSH_OPTS "$HOST" "
set -e
sudo mkdir -p '$BIN_DIR/releases'
if [[ -f '$BIN_DIR/sentrix' ]]; then
    PREV_VER=\$(/opt/sentrix/sentrix --version 2>/dev/null | awk '{print \$2}' || echo 'unknown')
    TS=\$(date -u +%Y%m%dT%H%M%SZ)
    sudo cp '$BIN_DIR/sentrix' \"$BIN_DIR/releases/sentrix-v\${PREV_VER}-\${TS}\"
fi
cd '$BIN_DIR/releases' && ls -t | tail -n +$(($RELEASES_KEEP + 1)) | xargs -r sudo rm -f
sudo mv /tmp/sentrix_new '$BIN_DIR/sentrix'
sudo chmod +x '$BIN_DIR/sentrix'
"

# ── Phase 3: restart + health check ────────────────────────
echo "  $(blue '=>') systemctl restart $SERVICE"
ssh $SSH_OPTS "$HOST" "sudo systemctl restart '$SERVICE'"

echo "  $(blue '=>') Sleeping ${WAIT_SEC}s for warm-up"
sleep "$WAIT_SEC"

echo "  $(blue '=>') Health check ${RPC_URL}${HEALTH_PATH}"
if ssh $SSH_OPTS "$HOST" "curl -sf --max-time 10 '${RPC_URL}${HEALTH_PATH}' >/dev/null"; then
    echo "    $(green 'OK') $HOST: $SERVICE is healthy"
else
    echo "    $(red 'FAIL') $HOST: $SERVICE health check failed; investigate with:"
    echo "      ssh -i $SSH_KEY $HOST 'sudo journalctl -u $SERVICE -n 50 --no-pager'"
    exit 1
fi

echo "  $(green 'deploy-validator DONE') — $HOST : $SERVICE"
