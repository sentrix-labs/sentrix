#!/usr/bin/env bash
# fast-deploy.sh — Primary deploy path for Sentrix chain.
#
# Flow:
#   1. Preflight gates (cargo test + clippy + build) on build host
#   2. Push current branch to GitHub for audit trail (CI will
#      re-run tests as a second check, but will NOT re-deploy —
#      the `deploy` job in ci.yml is disabled when fast-deploy is
#      the primary path)
#   3. Upload binary to all 4 mainnet validators (Foundation, Treasury,
#      Core, Beacon) via wg1 SCP, archive previous, rolling restart
#      with health check
#   4. Post-deploy: verify chain height advances
#
# Takes ~3–5 minutes vs ~10–12 for the old CI-deploys-everything
# flow, because the build happens once on a warm cargo cache
# instead of being duplicated on a cold GitHub runner.
#
# Usage:
#   ./scripts/fast-deploy.sh <mainnet|testnet> [--skip-push]
#
# Environment:
#   SENTRIX_FAST_SKIP_TESTS=1   skip cargo test (use sparingly —
#                                loses the pre-deploy regression gate)
#   SENTRIX_ROLLBACK=<path>      reuse an archived binary instead of
#                                building (instant rollback)
#
# Per-host bin_dir is loaded from fleet.env via FOUNDATION_BIN_DIR /
# TREASURY_BIN_DIR / CORE_BIN_DIR / BEACON_BIN_DIR. Defaults match the
# 2026-04-25 mainnet layout: Foundation+Treasury at /opt/sentrix, Core
# at /opt/core, Beacon at /opt/beacon. Override only if the layout
# changes (e.g. multi-host consolidation).
#
# Differences vs emergency-deploy.sh:
#   fast-deploy             | emergency-deploy
#   ------------------------+-------------------------
#   default (normal dev)    | break-glass only
#   runs test+clippy        | skips gates
#   pushes to GitHub        | operator pushes manually after
#   light confirmation      | strict confirmation phrase

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

red()    { printf '\033[31m%s\033[0m' "$*"; }
green()  { printf '\033[32m%s\033[0m' "$*"; }
yellow() { printf '\033[33m%s\033[0m' "$*"; }
blue()   { printf '\033[34m%s\033[0m' "$*"; }

TARGET="${1:-}"
SKIP_PUSH=""
for arg in "$@"; do
    [ "$arg" = "--skip-push" ] && SKIP_PUSH=1
done
case "$TARGET" in
    mainnet|testnet) ;;
    *)
        echo "Usage: $0 <mainnet|testnet> [--skip-push]"
        exit 2
        ;;
esac

SSH_KEY="$HOME/.ssh/satya_master"
SSH_OPTS="-i $SSH_KEY -o StrictHostKeyChecking=accept-new -o ConnectTimeout=10"

FLEET_ENV="${SENTRIX_FLEET_ENV:-$HOME/.config/sentrix/fleet.env}"
if [[ ! -f "$FLEET_ENV" ]]; then
    echo "  $(red "Fleet env file not found: $FLEET_ENV")"
    echo "  See scripts/emergency-deploy.sh header for required vars."
    exit 2
fi
# shellcheck source=/dev/null
source "$FLEET_ENV"

# Per-host entry format: "user@host:service:port:bin_dir"
# bin_dir defaults to /opt/sentrix for hosts that don't override it.
# Sentrix mainnet is co-tenant-free per validator-isolation policy:
# Foundation (VPS1) + Treasury (VPS2) live at /opt/sentrix; Core (VPS3)
# at /opt/core; Beacon (VPS5) at /opt/beacon. Each role's bin_dir is
# loaded from fleet.env (FOUNDATION_BIN_DIR, etc.) with a /opt/sentrix
# fallback for backward compatibility with legacy fleet.env files.
declare -A MAINNET_HOSTS=(
    [foundation]="${FOUNDATION_USER}@${FOUNDATION_WG}:${FOUNDATION_SERVICE}:${FOUNDATION_PORT}:${FOUNDATION_BIN_DIR:-/opt/sentrix}"
    [treasury]="${TREASURY_USER}@${TREASURY_WG}:${TREASURY_SERVICE}:${TREASURY_PORT}:${TREASURY_BIN_DIR:-/opt/sentrix}"
    [core]="${CORE_USER}@${CORE_WG}:${CORE_SERVICE}:${CORE_PORT}:${CORE_BIN_DIR:-/opt/core}"
    [beacon]="${BEACON_USER}@${BEACON_WG}:${BEACON_SERVICE}:${BEACON_PORT}:${BEACON_BIN_DIR:-/opt/beacon}"
)
declare -A TESTNET_HOSTS=(
    [testnet_val1]="${TESTNET_USER}@${TESTNET_WG}:sentrix-testnet-val1:${TESTNET_VAL1_PORT}:/opt/sentrix-testnet"
    [testnet_val2]="${TESTNET_USER}@${TESTNET_WG}:sentrix-testnet-val2:${TESTNET_VAL2_PORT}:/opt/sentrix-testnet"
    [testnet_val3]="${TESTNET_USER}@${TESTNET_WG}:sentrix-testnet-val3:${TESTNET_VAL3_PORT}:/opt/sentrix-testnet"
    [testnet_val4]="${TESTNET_USER}@${TESTNET_WG}:sentrix-testnet-val4:${TESTNET_VAL4_PORT}:/opt/sentrix-testnet"
)

if [[ "$TARGET" == "mainnet" ]]; then
    declare -n HOSTS=MAINNET_HOSTS
else
    declare -n HOSTS=TESTNET_HOSTS
fi

# ── Banner ──────────────────────────────────────────────────
echo
blue "  ╔════════════════════════════════════════════════════════════════╗"; echo
blue "  ║  fast-deploy — $TARGET                                             "; echo
blue "  ╚════════════════════════════════════════════════════════════════╝"; echo
GIT_BRANCH=$(git rev-parse --abbrev-ref HEAD)
GIT_COMMIT=$(git rev-parse --short HEAD)
GIT_DIRTY=""
if ! git diff --quiet || ! git diff --cached --quiet; then
    GIT_DIRTY=" $(red 'DIRTY')"
fi
echo "  Branch:     $GIT_BRANCH"
echo "  Commit:     $GIT_COMMIT$GIT_DIRTY"
echo "  Target:     $TARGET"
echo

# Mainnet asks for explicit confirmation; testnet runs silently.
if [[ "$TARGET" == "mainnet" ]]; then
    printf "  Deploy %s to mainnet? [y/N] " "$GIT_COMMIT"
    read -r confirm
    if [[ "$confirm" != "y" && "$confirm" != "Y" ]]; then
        echo "  $(red 'Aborted.')"
        exit 1
    fi
fi

# ── Preflight gates ─────────────────────────────────────────
if [[ "${SENTRIX_FAST_SKIP_TESTS:-0}" != "1" ]]; then
    echo "  $(blue '=>') Preflight: cargo test --workspace --release ..."
    cargo test --workspace --release --quiet 2>&1 | tail -3
    echo "  $(blue '=>') Preflight: cargo clippy --workspace --tests --release -- -D warnings ..."
    cargo clippy --workspace --tests --release -- -D warnings 2>&1 | tail -2
else
    yellow "  !! preflight skipped via SENTRIX_FAST_SKIP_TESTS=1"; echo
fi

# ── Build ───────────────────────────────────────────────────
# Build in a Debian bullseye container (glibc 2.31) so the resulting
# binary runs on every target — Foundation node/Treasury node ship Ubuntu 22.04 (glibc 2.35)
# and Core node ships 24.04 (glibc 2.39). A native build host build would pin to
# glibc 2.39 and crash on Foundation node/Treasury node (happened on commit e49e01d).
# Cargo cache is mounted so only the first build is cold.
DOCKER_BUILD_IMAGE="${SENTRIX_BUILD_IMAGE:-rust:1.95-bullseye}"
DOCKER_CACHE="${SENTRIX_DOCKER_CACHE:-$HOME/.cache/sentrix-docker-build}"
if [[ -n "${SENTRIX_ROLLBACK:-}" ]]; then
    BINARY="$SENTRIX_ROLLBACK"
    echo "  $(blue '=>') Using rollback binary: $BINARY"
else
    echo "  $(blue '=>') Building release binary on build host (docker: $DOCKER_BUILD_IMAGE) ..."
    mkdir -p "$DOCKER_CACHE/cargo-registry" "$DOCKER_CACHE/cargo-git" "$DOCKER_CACHE/target"
    docker run --rm \
        -v "$REPO_ROOT":/work \
        -v "$DOCKER_CACHE/cargo-registry":/usr/local/cargo/registry \
        -v "$DOCKER_CACHE/cargo-git":/usr/local/cargo/git \
        -v "$DOCKER_CACHE/target":/work/target \
        -w /work \
        -e CARGO_TERM_COLOR=never \
        "$DOCKER_BUILD_IMAGE" \
        bash -c "apt-get update -qq && apt-get install -y -qq pkg-config libssl-dev clang >/dev/null && cargo build --workspace --release --quiet" 2>&1 | tail -3
    BINARY="$DOCKER_CACHE/target/release/sentrix"
fi
[[ -x "$BINARY" ]] || { echo "  $(red "No binary at $BINARY")"; exit 4; }
BINARY_HASH=$(sha256sum "$BINARY" | cut -c1-16)
BINARY_SIZE=$(stat -c%s "$BINARY" 2>/dev/null || stat -f%z "$BINARY")
echo "  Binary: $BINARY_SIZE bytes, sha256=$BINARY_HASH..."
echo

# ── Push to GitHub (parallel to deploy) ─────────────────────
if [[ -z "$SKIP_PUSH" ]] && [[ -z "${SENTRIX_ROLLBACK:-}" ]]; then
    echo "  $(blue '=>') git push origin $GIT_BRANCH ..."
    git push -u origin "$GIT_BRANCH" 2>&1 | tail -2 || {
        yellow "  !! git push failed — continue with deploy but fix push after"; echo
    }
fi

# ── Deploy ─────────────────────────────────────────────────
# Build a map of userhost -> bin_dir. Foundation + Treasury share a
# wg1 IP/user but live at the same bin_dir. Core + Beacon each have
# their own bin_dir. Co-tenant Foundation+Treasury on the same host
# would collide on /tmp/sentrix_new; the Phase 1 upload-then-Phase 2
# install split tolerates that because Phase 2 mv'es to a host-specific
# bin_dir. Validator-isolation policy means this collision can't happen
# on real mainnet, but the script must be correct for testnet docker
# layouts too.
declare -A USERHOST_BINDIRS=()
for h in "${!HOSTS[@]}"; do
    IFS=':' read -r userhost _ _ bin_dir <<< "${HOSTS[$h]}"
    USERHOST_BINDIRS["$userhost|$bin_dir"]=1
done

echo "  $(blue '=>') Phase 1: upload binary to unique hosts ..."
declare -A UPLOADED=()
for key in "${!USERHOST_BINDIRS[@]}"; do
    userhost="${key%%|*}"
    [[ -n "${UPLOADED[$userhost]:-}" ]] && continue
    UPLOADED["$userhost"]=1
    printf "    %-32s " "$userhost"
    if scp $SSH_OPTS "$BINARY" "$userhost:/tmp/sentrix_new" >/dev/null 2>&1; then
        echo "$(green 'OK')"
    else
        echo "$(red 'FAIL — upload failed, aborting')"
        exit 5
    fi
done
echo

echo "  $(blue '=>') Phase 2: archive + replace binary on each (host, bin_dir) pair ..."
declare -A INSTALLED=()
for key in "${!USERHOST_BINDIRS[@]}"; do
    [[ -n "${INSTALLED[$key]:-}" ]] && continue
    INSTALLED["$key"]=1
    userhost="${key%%|*}"
    bin_dir="${key##*|}"
    printf "    %-32s " "$userhost ($bin_dir)"
    ssh $SSH_OPTS "$userhost" "
        set -e
        sudo mkdir -p $bin_dir/releases
        PREV_VER=\$($bin_dir/sentrix --version 2>/dev/null | awk '{print \$2}' || echo unknown)
        sudo cp $bin_dir/sentrix $bin_dir/releases/sentrix-v\${PREV_VER}-\$(date +%Y%m%d%H%M%S) 2>/dev/null || true
        cd $bin_dir/releases && ls -t | tail -n +4 | xargs -r sudo rm -f
        sudo cp /tmp/sentrix_new $bin_dir/sentrix
        sudo chmod +x $bin_dir/sentrix
    " >/dev/null 2>&1
    echo "$(green 'OK')"
done
# Final: clean up the staged /tmp/sentrix_new on each host
for userhost in "${!UPLOADED[@]}"; do
    ssh $SSH_OPTS "$userhost" "rm -f /tmp/sentrix_new" >/dev/null 2>&1 || true
done
echo

FIRST_HOST=$(echo "${!HOSTS[@]}" | awk '{print $1}')
IFS=':' read -r first_userhost _ first_port _ <<< "${HOSTS[$FIRST_HOST]}"
PRE_HEIGHT=$(ssh $SSH_OPTS "$first_userhost" "curl -sf --max-time 5 http://localhost:$first_port/chain/info 2>/dev/null" \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['height'])" 2>/dev/null || echo "?")
echo "  $(blue 'Pre-restart chain height:') $PRE_HEIGHT"
echo

# Health-check waits in a bounded loop instead of a fixed 30 s sleep
# because mainnet MDBX load on a 45 K-block chain legitimately takes
# longer than 30 s. The loop returns as soon as /health responds 200.
wait_healthy() {
    local userhost="$1" port="$2" max="${3:-120}" i=0
    until ssh $SSH_OPTS "$userhost" "curl -sf --max-time 3 http://localhost:$port/health >/dev/null 2>&1"; do
        i=$((i + 2))
        if [[ $i -ge $max ]]; then return 1; fi
        sleep 2
    done
    return 0
}

echo "  $(blue '=>') Phase 3: rolling restart (health check loop) ..."
for h in "${!HOSTS[@]}"; do
    IFS=':' read -r userhost service port bin_dir <<< "${HOSTS[$h]}"
    echo "    $(yellow 'Restarting') $h → $service"
    ssh $SSH_OPTS "$userhost" "sudo systemctl restart $service" >/dev/null 2>&1
    if wait_healthy "$userhost" "$port" 120; then
        echo "    $(green 'health OK')"
    else
        echo "    $(red 'HEALTH CHECK FAILED after 120 s — aborting further rollout')"
        echo "    $(red "Rollback: SENTRIX_ROLLBACK=$bin_dir/releases/<prev> ./scripts/fast-deploy.sh $TARGET")"
        exit 6
    fi
done
echo

sleep 5
POST_HEIGHT=$(ssh $SSH_OPTS "$first_userhost" "curl -sf --max-time 5 http://localhost:$first_port/chain/info 2>/dev/null" \
    | python3 -c "import sys,json;print(json.load(sys.stdin)['height'])" 2>/dev/null || echo "?")
echo "  $(blue 'Post-restart chain height:') $POST_HEIGHT"
if [[ "$PRE_HEIGHT" != "?" && "$POST_HEIGHT" != "?" && "$POST_HEIGHT" -gt "$PRE_HEIGHT" ]]; then
    echo "  $(green '✓ Chain advanced') (+$((POST_HEIGHT - PRE_HEIGHT)) blocks)"
else
    yellow "  ! Chain height not advancing yet — watch logs."; echo
fi
echo
green "  fast-deploy DONE — $GIT_COMMIT on $TARGET"; echo
