#!/usr/bin/env bash
# emergency-deploy.sh — BREAK-GLASS deploy path for Sentrix chain.
#
# ░░░ USE ONLY WHEN CI/CD IS UNAVAILABLE OR TOO SLOW ░░░
#
# Triggers:
#   * GitHub Actions degraded / down > 10 minutes
#   * Security exploit actively draining funds
#   * Mainnet chain halted > 5 minutes
#   * Crash-loop recovery where CI deploy can't complete
#
# Not an excuse to skip CI for convenience. Every emergency deploy:
#   1. Bypasses the `cargo test` and `cargo clippy` gates.
#   2. Produces no GitHub Actions audit trail.
#   3. Ships a binary that may differ from main branch HEAD.
#
# After using this script, operator MUST push the branch to GitHub so
# that main catches up — otherwise the next regular CI deploy will
# "revert" the emergency fix.
#
# Usage:
#   ./scripts/emergency-deploy.sh <testnet|mainnet>
#
# Example:
#   ./scripts/emergency-deploy.sh mainnet
#
# Environment:
#   SENTRIX_SKIP_TESTS=1    skip the minimal cargo-check sanity pass
#                           (default: run it — ~10s extra, catches
#                           syntax errors pre-deploy)
#   SENTRIX_ROLLBACK=<path> use a specific pre-archived binary instead
#                           of building (for instant rollback)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── Colors ──────────────────────────────────────────────────
red()    { printf '\033[31m%s\033[0m' "$*"; }
green()  { printf '\033[32m%s\033[0m' "$*"; }
yellow() { printf '\033[33m%s\033[0m' "$*"; }
blue()   { printf '\033[34m%s\033[0m' "$*"; }

# ── Args ────────────────────────────────────────────────────
TARGET="${1:-}"
case "$TARGET" in
    mainnet|testnet) ;;
    *)
        echo "Usage: $0 <mainnet|testnet>"
        echo ""
        echo "  mainnet — deploys to Foundation node (Foundation) + Treasury node (Treasury) + Core node (Core)"
        echo "  testnet — deploys to Core node (4 validators: sentrix-testnet-val{1..4})"
        exit 2
        ;;
esac

# ── VPS fleet mapping (wg1 private addresses) ───────────────
# SSH key required: ~/.ssh/satya_master (set in ~/.ssh/config for github.com;
# for VPS wg1 access we pass -i explicitly).
SSH_KEY="$HOME/.ssh/satya_master"
SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10"

# Fleet layout is read from env so the script does not hardcode
# operator-specific SSH user + wg1 IPs (pre-commit secret scanner
# rejects literal user@addr strings). Typical values live in
# `~/.config/sentrix/fleet.env` (git-ignored) sourced below.
#
# Expected env vars (build host operator):
#   FOUNDATION_USER, FOUNDATION_WG, FOUNDATION_SERVICE, FOUNDATION_PORT
#   TREASURY_USER, TREASURY_WG, TREASURY_SERVICE, TREASURY_PORT
#   CORE_USER, CORE_WG, CORE_SERVICE, CORE_PORT          (mainnet core)
#   TESTNET_USER, TESTNET_WG, TESTNET_VAL{1..4}_PORT            (testnet)
FLEET_ENV="${SENTRIX_FLEET_ENV:-$HOME/.config/sentrix/fleet.env}"
if [[ -f "$FLEET_ENV" ]]; then
    # shellcheck source=/dev/null
    source "$FLEET_ENV"
else
    echo "  $(red "Fleet env file not found: $FLEET_ENV")"
    echo "  Create it with FOUNDATION_USER/FOUNDATION_WG/FOUNDATION_SERVICE/FOUNDATION_PORT etc."
    echo "  See scripts/emergency-deploy.sh header for required vars."
    exit 2
fi

# Build host maps from env. Format: "USER@HOST:SERVICE:API_PORT".
declare -A MAINNET_HOSTS=(
    [foundation]="${FOUNDATION_USER}@${FOUNDATION_WG}:${FOUNDATION_SERVICE}:${FOUNDATION_PORT}"
    [treasury]="${TREASURY_USER}@${TREASURY_WG}:${TREASURY_SERVICE}:${TREASURY_PORT}"
    [core]="${CORE_USER}@${CORE_WG}:${CORE_SERVICE}:${CORE_PORT}"
)
declare -A TESTNET_HOSTS=(
    [testnet_val1]="${TESTNET_USER}@${TESTNET_WG}:sentrix-testnet-val1:${TESTNET_VAL1_PORT}"
    [testnet_val2]="${TESTNET_USER}@${TESTNET_WG}:sentrix-testnet-val2:${TESTNET_VAL2_PORT}"
    [testnet_val3]="${TESTNET_USER}@${TESTNET_WG}:sentrix-testnet-val3:${TESTNET_VAL3_PORT}"
    [testnet_val4]="${TESTNET_USER}@${TESTNET_WG}:sentrix-testnet-val4:${TESTNET_VAL4_PORT}"
)

if [[ "$TARGET" == "mainnet" ]]; then
    declare -n HOSTS=MAINNET_HOSTS
else
    declare -n HOSTS=TESTNET_HOSTS
fi

# ── Pre-flight confirmation ─────────────────────────────────
echo
red "  ╔════════════════════════════════════════════════════════════════╗"; echo
red "  ║  EMERGENCY DEPLOY — $TARGET                                       "; echo
red "  ║  CI/CD is being BYPASSED. Test gate is being BYPASSED.         ║"; echo
red "  ╚════════════════════════════════════════════════════════════════╝"; echo
echo
GIT_BRANCH=$(git rev-parse --abbrev-ref HEAD)
GIT_COMMIT=$(git rev-parse --short HEAD)
GIT_DIRTY=""
if ! git diff --quiet || ! git diff --cached --quiet; then
    GIT_DIRTY=" $(red 'DIRTY')"
fi
echo "  Git branch: $GIT_BRANCH"
echo "  Commit:     $GIT_COMMIT$GIT_DIRTY"
echo "  Target:     $TARGET"
echo "  Hosts:"
for h in "${!HOSTS[@]}"; do
    IFS=':' read -r userhost service port <<< "${HOSTS[$h]}"
    echo "    $h  ($userhost, service=$service, api=:$port)"
done
echo
printf "  Type '%s' to continue: " "$(yellow 'I know CI is bypassed')"
read -r confirmation
if [[ "$confirmation" != "I know CI is bypassed" ]]; then
    echo "  $(red 'Aborted.')"
    exit 1
fi
echo

# ── Build (skippable via SENTRIX_ROLLBACK) ──────────────────
if [[ -n "${SENTRIX_ROLLBACK:-}" ]]; then
    BINARY="$SENTRIX_ROLLBACK"
    if [[ ! -x "$BINARY" ]]; then
        echo "  $(red "ROLLBACK binary not executable: $BINARY")"
        exit 3
    fi
    echo "  $(blue '=>') Using rollback binary: $BINARY"
else
    BINARY="$REPO_ROOT/target/release/sentrix"
    if [[ "${SENTRIX_SKIP_TESTS:-0}" != "1" ]]; then
        echo "  $(blue '=>') Minimal sanity check (cargo check, ~10s)..."
        cargo check --workspace --release --quiet 2>&1 | tail -3
    else
        yellow "  !! cargo check skipped (SENTRIX_SKIP_TESTS=1)"; echo
    fi
    echo "  $(blue '=>') Building release binary on build host..."
    cargo build --workspace --release --quiet 2>&1 | tail -3
    if [[ ! -x "$BINARY" ]]; then
        echo "  $(red "Build produced no binary at $BINARY")"
        exit 4
    fi
fi
BINARY_SIZE=$(stat -c%s "$BINARY" 2>/dev/null || stat -f%z "$BINARY")
BINARY_HASH=$(sha256sum "$BINARY" | cut -c1-16)
echo "  Binary: $BINARY ($BINARY_SIZE bytes, sha256=$BINARY_HASH...)"
echo

# Build the set of unique "user@host" endpoints — on testnet, all
# four services live on the same VPS, so we must upload + swap the
# binary once per HOST rather than once per service. Without this
# de-dup the second iteration finds /tmp/sentrix_new already moved
# out by the first and aborts.
declare -A UNIQUE_USERHOSTS=()
for h in "${!HOSTS[@]}"; do
    IFS=':' read -r userhost _ _ <<< "${HOSTS[$h]}"
    UNIQUE_USERHOSTS["$userhost"]=1
done

# ── Phase 1: upload binary to every unique userhost ─────────
echo "  $(blue '=>') Phase 1: uploading binary to all hosts..."
for userhost in "${!UNIQUE_USERHOSTS[@]}"; do
    printf "    %-32s " "$userhost"
    if scp $SSH_OPTS "$BINARY" "$userhost:/tmp/sentrix_new" 2>&1 | tail -1; then
        echo "$(green 'OK')"
    else
        echo "$(red 'FAIL — upload failed, aborting before restart')"
        exit 5
    fi
done
echo

# ── Phase 2: archive + replace binary (once per unique host) ─
echo "  $(blue '=>') Phase 2: archiving previous binary + replacing on disk..."
for userhost in "${!UNIQUE_USERHOSTS[@]}"; do
    printf "    %-32s " "$userhost"
    ssh $SSH_OPTS "$userhost" "
        set -e
        sudo mkdir -p /opt/sentrix/releases
        PREV_VER=\$(/opt/sentrix/sentrix --version 2>/dev/null | awk '{print \$2}' || echo unknown)
        sudo cp /opt/sentrix/sentrix /opt/sentrix/releases/sentrix-v\${PREV_VER}-emergency-\$(date +%Y%m%d%H%M%S) 2>/dev/null || true
        cd /opt/sentrix/releases && ls -t | tail -n +4 | xargs -r sudo rm -f
        sudo mv /tmp/sentrix_new /opt/sentrix/sentrix
        sudo chmod +x /opt/sentrix/sentrix
    " 2>&1 | tail -1
    echo "$(green 'OK')"
done
echo

# ── Capture pre-restart height to detect regression ─────────
# Pull height from the FIRST host (arbitrary — they should match).
FIRST_HOST=$(echo "${!HOSTS[@]}" | awk '{print $1}')
IFS=':' read -r first_userhost first_service first_port <<< "${HOSTS[$FIRST_HOST]}"
PRE_HEIGHT=$(ssh $SSH_OPTS "$first_userhost" "curl -sf --max-time 5 http://localhost:$first_port/chain/info" 2>/dev/null \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['height'])" 2>/dev/null || echo "?")
echo "  $(blue 'Pre-restart chain height (from '"$FIRST_HOST"'):') $PRE_HEIGHT"
echo

# ── Phase 3: rolling restart with health check ──────────────
echo "  $(blue '=>') Phase 3: rolling restart (one node at a time, 30s health wait)..."
for h in "${!HOSTS[@]}"; do
    IFS=':' read -r userhost service port <<< "${HOSTS[$h]}"
    echo "    $(yellow 'Restarting') $h → $service"
    ssh $SSH_OPTS "$userhost" "sudo systemctl restart $service" 2>&1 | tail -1
    echo "    Waiting 30s for warm-up + health check..."
    sleep 30
    if ssh $SSH_OPTS "$userhost" "curl -sf --max-time 10 http://localhost:$port/health >/dev/null" 2>/dev/null; then
        echo "    $(green 'health OK')"
    else
        echo "    $(red 'HEALTH CHECK FAILED — aborting further rollout')"
        echo "    $(red 'You need to manually investigate '"$h"' before restarting the remaining nodes.')"
        echo "    $(red 'Rollback: SENTRIX_ROLLBACK=/opt/sentrix/releases/<prev> ./scripts/emergency-deploy.sh '"$TARGET")"
        exit 6
    fi
done
echo

# ── Post-deploy height check ────────────────────────────────
sleep 5
POST_HEIGHT=$(ssh $SSH_OPTS "$first_userhost" "curl -sf --max-time 5 http://localhost:$first_port/chain/info" 2>/dev/null \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['height'])" 2>/dev/null || echo "?")
echo "  $(blue 'Post-restart chain height:') $POST_HEIGHT"
if [[ "$PRE_HEIGHT" != "?" && "$POST_HEIGHT" != "?" ]]; then
    if [[ "$POST_HEIGHT" -gt "$PRE_HEIGHT" ]]; then
        echo "  $(green '✓ Chain advanced') (+$((POST_HEIGHT - PRE_HEIGHT)) blocks since pre-deploy)"
    else
        yellow "  ! Chain height not advancing yet — watch logs (sudo journalctl -u <service> -f)."; echo
    fi
fi
echo

# ── Done ────────────────────────────────────────────────────
green "  ╔════════════════════════════════════════════════════════════════╗"; echo
green "  ║  Emergency deploy COMPLETE                                      ║"; echo
green "  ╚════════════════════════════════════════════════════════════════╝"; echo
echo
echo "  $(yellow 'NEXT STEPS (required):')"
echo "    1. Push current branch to GitHub so main catches up:"
echo "         git push -u origin $GIT_BRANCH"
echo "         gh pr create && gh pr merge --squash"
echo "    2. Verify the CI/CD run that kicks off from the merge matches this"
echo "       binary (same sha256 prefix: $BINARY_HASH)"
echo "    3. Log this incident in internal docs"
echo
