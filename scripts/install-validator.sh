#!/usr/bin/env bash
# install-validator.sh — one-line Sentrix validator installer.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/sentrix-labs/sentrix/main/scripts/install-validator.sh | bash
#
# Or with options (download first, then run):
#   curl -fsSL https://raw.githubusercontent.com/sentrix-labs/sentrix/main/scripts/install-validator.sh -o install-validator.sh
#   chmod +x install-validator.sh
#   ./install-validator.sh --network testnet --name "my-validator"
#
# What it does (in order):
#   1. Pre-flight: distro / arch / RAM / swap / disk checks (refuses unsafe configs)
#   2. apt deps:  git, curl, build-essential, pkg-config, libssl-dev, jq
#   3. Rust 1.95+ via rustup (skipped if already installed)
#   4. Clone github.com/sentrix-labs/sentrix to ~/sentrix-src
#   5. Cargo build --release -p sentrix-node into /opt/sentrix/sentrix
#   6. Generate validator keystore (interactive password prompt)
#   7. Drop /etc/sentrix/<name>.env (mode 600) + /etc/systemd/system/<name>.service
#   8. systemctl enable --now, tail journalctl for 8s, print next-step block
#
# Idempotent — safe to re-run. Detects existing install and offers
# repair / rebuild instead of clobbering. Never touches keystores.
#
# Mainnet activation requires emailing validators@sentrixchain.com with
# your address + pubkey (the script prints them). The chain admin
# co-signs the on-chain RegisterValidator op + shares back the
# activation height. See docs.sentrixchain.com/operations/VALIDATOR_ONBOARDING.

set -euo pipefail

# ── Defaults (overridable via flags) ────────────────────────
NETWORK="${NETWORK:-mainnet}"
NAME="${NAME:-sentrix-validator}"
INSTALL_DIR="${INSTALL_DIR:-/opt/sentrix}"
SRC_DIR="${SRC_DIR:-$HOME/sentrix-src}"
REPO_URL="${REPO_URL:-https://github.com/sentrix-labs/sentrix.git}"
REPO_REF="${REPO_REF:-main}"
RUST_MIN_MAJOR=1
RUST_MIN_MINOR=95
SKIP_BUILD="${SKIP_BUILD:-0}"
ASSUME_YES="${ASSUME_YES:-0}"

# ── Argument parsing ────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --network)      NETWORK="$2"; shift 2 ;;
        --name)         NAME="$2"; shift 2 ;;
        --install-dir)  INSTALL_DIR="$2"; shift 2 ;;
        --src-dir)      SRC_DIR="$2"; shift 2 ;;
        --repo)         REPO_URL="$2"; shift 2 ;;
        --ref)          REPO_REF="$2"; shift 2 ;;
        --skip-build)   SKIP_BUILD=1; shift ;;
        --yes|-y)       ASSUME_YES=1; shift ;;
        --help|-h)
            sed -n '1,/^set -euo pipefail/p' "$0" | sed 's/^# \{0,1\}//' | head -n -1
            exit 0
            ;;
        *) echo "unknown flag: $1 (try --help)"; exit 2 ;;
    esac
done

case "$NETWORK" in
    mainnet|testnet) ;;
    *) echo "--network must be mainnet or testnet, got: $NETWORK"; exit 2 ;;
esac

# ── Pretty output ───────────────────────────────────────────
if [[ -t 1 ]]; then
    BOLD=$'\033[1m'; DIM=$'\033[2m'; RED=$'\033[31m'
    GREEN=$'\033[32m'; YELLOW=$'\033[33m'; BLUE=$'\033[34m'
    GOLD=$'\033[38;5;178m'; RESET=$'\033[0m'
else
    BOLD=""; DIM=""; RED=""; GREEN=""; YELLOW=""; BLUE=""; GOLD=""; RESET=""
fi

step()  { printf "${BLUE}==>${RESET} ${BOLD}%s${RESET}\n" "$*"; }
ok()    { printf "    ${GREEN}✓${RESET} %s\n" "$*"; }
warn()  { printf "    ${YELLOW}!${RESET} %s\n" "$*"; }
fail()  { printf "    ${RED}✗${RESET} %s\n" "$*" >&2; exit 1; }
info()  { printf "    ${DIM}%s${RESET}\n" "$*"; }

confirm() {
    local prompt="$1"
    if [[ "$ASSUME_YES" == "1" ]]; then return 0; fi
    read -r -p "    ${prompt} [y/N] " reply
    [[ "$reply" =~ ^[Yy]$ ]]
}

banner() {
    cat <<EOF
${GOLD}
   ███████╗███████╗███╗   ██╗████████╗██████╗ ██╗██╗  ██╗
   ██╔════╝██╔════╝████╗  ██║╚══██╔══╝██╔══██╗██║╚██╗██╔╝
   ███████╗█████╗  ██╔██╗ ██║   ██║   ██████╔╝██║ ╚███╔╝
   ╚════██║██╔══╝  ██║╚██╗██║   ██║   ██╔══██╗██║ ██╔██╗
   ███████║███████╗██║ ╚████║   ██║   ██║  ██║██║██╔╝ ██╗
   ╚══════╝╚══════╝╚═╝  ╚═══╝   ╚═╝   ╚═╝  ╚═╝╚═╝╚═╝  ╚═╝
${RESET}${DIM}              Layer-1 · Rust · Voyager DPoS+BFT${RESET}

   Validator installer · network=${BOLD}${NETWORK}${RESET} · name=${BOLD}${NAME}${RESET}

EOF
}

banner

# ── Step 1: pre-flight checks ───────────────────────────────
step "Pre-flight checks"

# OS / distro
if [[ "$(uname -s)" != "Linux" ]]; then
    fail "this installer targets Linux (got $(uname -s))"
fi
if [[ "$(uname -m)" != "x86_64" ]] && [[ "$(uname -m)" != "aarch64" ]]; then
    fail "unsupported architecture: $(uname -m) (need x86_64 or aarch64)"
fi
ok "kernel: $(uname -sr) on $(uname -m)"

if ! command -v apt-get >/dev/null 2>&1; then
    fail "apt-get not found — this installer expects Debian/Ubuntu. For other distros, follow docs/operations/VALIDATOR_ONBOARDING.md manually."
fi
DISTRO_ID=$(. /etc/os-release && echo "${ID:-unknown}")
DISTRO_VER=$(. /etc/os-release && echo "${VERSION_ID:-?}")
ok "distro: ${DISTRO_ID} ${DISTRO_VER}"

# Memory + swap
mem_gib=$(awk '/MemTotal/ {printf "%.0f", $2/1024/1024}' /proc/meminfo)
swap_gib=$(awk '/SwapTotal/ {printf "%.0f", $2/1024/1024}' /proc/meminfo)
if (( mem_gib < 8 )); then
    fail "RAM = ${mem_gib} GiB; need ≥ 8 GiB. chain.db is mmap'd; tight memory has historically produced page-cache thrash → silent halts."
fi
ok "RAM: ${mem_gib} GiB"

if (( swap_gib < 8 )); then
    warn "swap = ${swap_gib} GiB; recommend ≥ 8 GiB (persistent in /etc/fstab)"
    if confirm "Create an 8 GiB /swapfile-sentrix and persist it now?"; then
        sudo fallocate -l 8G /swapfile-sentrix
        sudo chmod 600 /swapfile-sentrix
        sudo mkswap /swapfile-sentrix >/dev/null
        sudo swapon /swapfile-sentrix
        echo '/swapfile-sentrix none swap sw 0 0' | sudo tee -a /etc/fstab >/dev/null
        ok "8 GiB swap created + persisted"
    else
        warn "continuing without swap bump — at-risk under sustained load"
    fi
else
    ok "swap: ${swap_gib} GiB"
fi

# Disk
free_gib=$(df -BG --output=avail / | tail -1 | tr -dc '0-9')
if (( free_gib < 60 )); then
    fail "free disk on / = ${free_gib} GiB; need ≥ 60 GiB. chain.db grows ~250 MB/day."
fi
ok "disk free: ${free_gib} GiB on /"

# ── Step 2: install apt deps ────────────────────────────────
step "Install apt dependencies"

DEPS=(git curl build-essential pkg-config libssl-dev clang jq ca-certificates protobuf-compiler)
MISSING=()
for pkg in "${DEPS[@]}"; do
    dpkg -s "$pkg" >/dev/null 2>&1 || MISSING+=("$pkg")
done

if (( ${#MISSING[@]} > 0 )); then
    info "missing: ${MISSING[*]}"
    sudo apt-get update -qq
    sudo apt-get install -y -qq "${MISSING[@]}"
    ok "installed: ${MISSING[*]}"
else
    ok "all deps present"
fi

# ── Step 3: Rust toolchain ──────────────────────────────────
step "Rust toolchain (need ≥ ${RUST_MIN_MAJOR}.${RUST_MIN_MINOR})"

# rustup may not be in PATH if just installed — source cargo env
if [[ -f "$HOME/.cargo/env" ]]; then
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

need_rust=1
# Use `rustc --version` rather than `command -v rustc` — operators can have
# `rustc` on PATH but a broken rustup state (no default toolchain set, stale
# settings.toml from a wiped install, etc.) where `rustc --version` fails.
# Set -e would abort the script silently; treat any failure as "need rustup
# install" instead.
if rustc_out=$(rustc --version 2>&1); then
    rustc_ver=$(echo "$rustc_out" | awk '{print $2}')
    rustc_major=$(echo "$rustc_ver" | cut -d. -f1)
    rustc_minor=$(echo "$rustc_ver" | cut -d. -f2)
    if [[ -n "$rustc_major" ]] && [[ -n "$rustc_minor" ]] && \
       { (( rustc_major > RUST_MIN_MAJOR )) || \
         (( rustc_major == RUST_MIN_MAJOR && rustc_minor >= RUST_MIN_MINOR )); }; then
        ok "rustc ${rustc_ver} (>= ${RUST_MIN_MAJOR}.${RUST_MIN_MINOR})"
        need_rust=0
    else
        warn "rustc ${rustc_ver} < ${RUST_MIN_MAJOR}.${RUST_MIN_MINOR}; will upgrade via rustup"
    fi
else
    info "rustc present but unusable (likely broken rustup state); will (re)install via rustup"
fi

if (( need_rust == 1 )); then
    if ! command -v rustup >/dev/null 2>&1; then
        info "installing rustup..."
        curl -fsSL https://sh.rustup.rs | sh -s -- -y --default-toolchain stable --profile minimal >/dev/null
        # shellcheck disable=SC1091
        source "$HOME/.cargo/env"
    fi
    # `rustup default stable` will install stable if absent, repair the
    # default-toolchain link if a stale settings.toml left it unset.
    rustup default stable >/dev/null
    rustup update stable >/dev/null
    ok "rustup default: $(rustc --version)"
fi

# ── Step 4: clone or pull source ────────────────────────────
step "Source tree at ${SRC_DIR}"

if [[ -d "$SRC_DIR/.git" ]]; then
    info "git fetch + checkout ${REPO_REF}"
    git -C "$SRC_DIR" fetch origin "$REPO_REF" --quiet
    git -C "$SRC_DIR" checkout "$REPO_REF" --quiet
    git -C "$SRC_DIR" pull --ff-only origin "$REPO_REF" --quiet
    ok "updated to $(git -C "$SRC_DIR" rev-parse --short HEAD)"
elif [[ -d "$SRC_DIR" ]] && [[ -n "$(ls -A "$SRC_DIR" 2>/dev/null)" ]]; then
    # Directory exists, has contents, but isn't a git checkout — likely a
    # half-finished previous install, or an operator pre-created the path
    # for a bind-mount cache. Refusing to clone-into-non-empty avoids a
    # cryptic "destination path … already exists" git error and a partial
    # state where the build lands in someone's unrelated tree.
    fail "$SRC_DIR exists but is not a git checkout. Remove it (or pick another --src-dir) and re-run."
else
    info "git clone ${REPO_URL}"
    sudo mkdir -p "$(dirname "$SRC_DIR")"
    git clone --branch "$REPO_REF" --depth 1 "$REPO_URL" "$SRC_DIR" --quiet
    ok "cloned at $(git -C "$SRC_DIR" rev-parse --short HEAD)"
fi

# ── Step 5: build binary ────────────────────────────────────
step "Build sentrix-node (release profile)"

if [[ "$SKIP_BUILD" == "1" ]]; then
    warn "skipping build (--skip-build)"
elif [[ -x "$INSTALL_DIR/sentrix" ]] && \
     [[ "$INSTALL_DIR/sentrix" -nt "$SRC_DIR/Cargo.lock" ]]; then
    ok "binary up-to-date at ${INSTALL_DIR}/sentrix (skipping rebuild)"
else
    info "this takes 6–15 min on first build, sub-minute on incrementals"
    (cd "$SRC_DIR" && cargo build --release -p sentrix-node 2>&1 | grep -vE '^\s*Compiling|^\s*Updating|^\s*Downloading' | tail -5)
    sudo mkdir -p "$INSTALL_DIR"
    sudo cp "$SRC_DIR/target/release/sentrix" "$INSTALL_DIR/sentrix"
    sudo chmod +x "$INSTALL_DIR/sentrix"
    ok "binary installed at ${INSTALL_DIR}/sentrix"
    "$INSTALL_DIR/sentrix" --version
fi

# Genesis config — mainnet uses the embedded canonical TOML (no flag),
# testnet needs --genesis pointing at the testnet config so the binary
# boots chain 7120 instead of 7119. Copy the source-tree genesis into
# the install dir so a future cleanup of $SRC_DIR doesn't break the
# unit's --genesis path.
GENESIS_PATH=""
if [[ "$NETWORK" == "testnet" ]]; then
    GENESIS_PATH="$INSTALL_DIR/genesis-testnet.toml"
    if [[ ! -f "$SRC_DIR/genesis/testnet.toml" ]]; then
        fail "missing $SRC_DIR/genesis/testnet.toml (incomplete clone?)"
    fi
    sudo cp "$SRC_DIR/genesis/testnet.toml" "$GENESIS_PATH"
    ok "testnet genesis copied → $GENESIS_PATH"
fi

# ── Step 6: keystore ────────────────────────────────────────
step "Validator keystore"

# `sentrix wallet generate` writes to ${SENTRIX_DATA_DIR}/wallets/<addr[2..10]>.json
# (8-char prefix, .json, no override flag). And `wallet info` doesn't print
# the public key — only generate does. So we set SENTRIX_DATA_DIR before
# generate, capture the printed Address + Public key + Keystore path, and
# stash address/pubkey into a sidecar `<name>.identity` file so re-runs can
# recover both for the activation email without re-prompting for password.
KEYSTORE_DIR="$INSTALL_DIR/data/wallets"
IDENTITY_FILE="$INSTALL_DIR/data/wallets/${NAME}.identity"
sudo mkdir -p "$KEYSTORE_DIR"
sudo chown "$USER:$USER" "$KEYSTORE_DIR"

# Detect existing keystore by scanning *.json. We can't pre-name it; the
# binary generates `<addr[2..10]>.json`. If exactly one is present + the
# identity sidecar exists, we treat that as "already installed".
existing=()
shopt -s nullglob
for f in "$KEYSTORE_DIR"/*.json; do existing+=("$f"); done
shopt -u nullglob

VALIDATOR_ADDR=""
VALIDATOR_PUBKEY=""
KEYSTORE_PATH=""

if (( ${#existing[@]} == 1 )) && [[ -f "$IDENTITY_FILE" ]]; then
    KEYSTORE_PATH="${existing[0]}"
    # shellcheck disable=SC1090
    source "$IDENTITY_FILE"
    ok "keystore already at ${KEYSTORE_PATH} (skipping generation)"
    info "rotate password later via: ${INSTALL_DIR}/sentrix wallet rekey ${KEYSTORE_PATH} --old-password … --new-password …"
elif (( ${#existing[@]} > 1 )); then
    fail "multiple keystores in ${KEYSTORE_DIR} — installer can't pick. Move or remove the old one(s) first."
else
    info "generating new keypair — set a strong passphrase (you'll need it for the systemd env file too)"
    info "lost password = lost validator. store it in a password manager + offline backup."
    read -r -s -p "    Keystore password: " KEYSTORE_PASSWORD
    echo
    read -r -s -p "    Confirm password:  " KEYSTORE_PASSWORD_CONFIRM
    echo
    if [[ "$KEYSTORE_PASSWORD" != "$KEYSTORE_PASSWORD_CONFIRM" ]]; then
        fail "passwords don't match"
    fi
    if [[ ${#KEYSTORE_PASSWORD} -lt 12 ]]; then
        fail "password too short (need ≥ 12 chars; pick something a password manager would generate)"
    fi

    # Run from $INSTALL_DIR with SENTRIX_DATA_DIR set so the keystore lands
    # at $INSTALL_DIR/data/wallets/<addr[2..10]>.json. Capture stdout so we
    # can pull Address + Public key + Keystore path out of it.
    GEN_OUTPUT=$(
        SENTRIX_DATA_DIR="$INSTALL_DIR/data" \
        "$INSTALL_DIR/sentrix" wallet generate --password "$KEYSTORE_PASSWORD"
    )
    VALIDATOR_ADDR=$(echo "$GEN_OUTPUT" | awk '/^[[:space:]]*Address:/ {print $2}')
    VALIDATOR_PUBKEY=$(echo "$GEN_OUTPUT" | awk '/^[[:space:]]*Public key:/ {print $3}')
    KEYSTORE_PATH=$(echo "$GEN_OUTPUT" | awk '/^[[:space:]]*Keystore:/ {print $2}')

    if [[ -z "$VALIDATOR_ADDR" || -z "$KEYSTORE_PATH" ]]; then
        echo "$GEN_OUTPUT" >&2
        fail "couldn't parse wallet generate output (see lines above)"
    fi
    sudo chmod 600 "$KEYSTORE_PATH"

    # Persist address/pubkey for re-run discoverability — the keystore
    # itself has the address but not the pubkey, so without this the
    # activation email becomes a manual derive-from-privkey step.
    cat > "$IDENTITY_FILE" <<EOF
# sentrix validator identity — non-secret
VALIDATOR_ADDR="$VALIDATOR_ADDR"
VALIDATOR_PUBKEY="$VALIDATOR_PUBKEY"
KEYSTORE_PATH="$KEYSTORE_PATH"
EOF
    chmod 644 "$IDENTITY_FILE"
    ok "keystore created at ${KEYSTORE_PATH}"
fi

ok "address: ${VALIDATOR_ADDR:-<missing — re-run installer>}"
ok "pubkey:  ${VALIDATOR_PUBKEY:-<missing — re-run installer or sentrix wallet decrypt to derive>}"

# ── Step 7: env file + systemd unit ─────────────────────────
step "systemd unit + env file"

ENV_FILE="/etc/sentrix/${NAME}.env"
UNIT_FILE="/etc/systemd/system/${NAME}.service"

sudo mkdir -p /etc/sentrix
sudo chmod 755 /etc/sentrix

if [[ -f "$ENV_FILE" ]]; then
    ok "env file exists at ${ENV_FILE} (preserving)"
else
    if [[ -z "${KEYSTORE_PASSWORD:-}" ]]; then
        info "keystore was pre-existing; you'll need to populate ${ENV_FILE} manually:"
        info "    SENTRIX_WALLET_PASSWORD=<your-keystore-password>"
        info "and chmod 600 it before starting the service."
    else
        sudo bash -c "cat > $ENV_FILE" <<EOF
# Sentrix validator env — mode 600. Do NOT commit this file.
SENTRIX_WALLET_PASSWORD=$KEYSTORE_PASSWORD
EOF
        sudo chmod 600 "$ENV_FILE"
        ok "env file written + chmod 600"
    fi
fi
unset KEYSTORE_PASSWORD KEYSTORE_PASSWORD_CONFIRM

# Bootstrap peers — for a brand-new validator we don't auto-connect to
# the reference fleet (operator runs the registration handshake first).
# After being added to the on-chain authority registry, the libp2p
# kademlia + advertised-multiaddr discovery picks up peers automatically.
# A manual --peers flag can be threaded in via the unit override after
# coordinating with validators@sentrixchain.com.

if [[ -f "$UNIT_FILE" ]]; then
    ok "unit exists at ${UNIT_FILE} (preserving)"
else
    EXEC_START="$INSTALL_DIR/sentrix start --validator-keystore $KEYSTORE_PATH"
    if [[ -n "$GENESIS_PATH" ]]; then
        EXEC_START="$EXEC_START --genesis $GENESIS_PATH"
    fi
    sudo bash -c "cat > $UNIT_FILE" <<EOF
[Unit]
Description=Sentrix validator (${NAME}) — ${NETWORK}
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$USER
Group=$USER
WorkingDirectory=$INSTALL_DIR
ExecStart=$EXEC_START
Restart=always
RestartSec=5
LimitNOFILE=65536
EnvironmentFile=$ENV_FILE
Environment=SENTRIX_DATA_DIR=$INSTALL_DIR/data
Environment=SENTRIX_ENCRYPTED_DISK=true

[Install]
WantedBy=multi-user.target
EOF
    sudo systemctl daemon-reload
    ok "unit installed at ${UNIT_FILE}"
fi

# Ensure data dir is writable by the service user
sudo chown -R "$USER:$USER" "$INSTALL_DIR"

# ── Step 8: enable + start + verify ─────────────────────────
step "Enable + start sentrix service"

if systemctl is-active --quiet "$NAME"; then
    ok "${NAME} already active — restarting to pick up changes"
    sudo systemctl restart "$NAME"
else
    sudo systemctl enable --now "$NAME" >/dev/null 2>&1
    ok "${NAME} enabled + started"
fi

sleep 4
if systemctl is-active --quiet "$NAME"; then
    ok "service is active"
    info "tailing journalctl for 8 seconds to confirm libp2p + chain bring-up..."
    sudo journalctl -u "$NAME" --no-pager -n 0 -f &
    JOURNAL_PID=$!
    sleep 8
    kill "$JOURNAL_PID" 2>/dev/null || true
    wait "$JOURNAL_PID" 2>/dev/null || true
else
    warn "service is NOT active — inspect:"
    info "    sudo journalctl -u ${NAME} -n 100 --no-pager"
fi

# ── Final block: next steps ─────────────────────────────────
cat <<EOF

${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}
${GREEN}${BOLD}Sentrix node installed.${RESET}

${BOLD}Service${RESET}
  status:   sudo systemctl status ${NAME}
  logs:     sudo journalctl -u ${NAME} -f
  restart:  sudo systemctl restart ${NAME}
  stop:     sudo systemctl stop ${NAME}

${BOLD}Health${RESET}
  curl http://localhost:8545/health
  curl http://localhost:8545/chain/info | jq

${BOLD}Validator activation${RESET}
  Your node is running but NOT YET A VALIDATOR — it's a peer.
  To get added to the on-chain authority registry, email
  ${GOLD}validators@sentrixchain.com${RESET} with:
    • address: ${BOLD}${VALIDATOR_ADDR:-<from sentrix wallet info>}${RESET}
    • pubkey:  ${BOLD}${VALIDATOR_PUBKEY:-<from sentrix wallet info>}${RESET}
    • operator name (e.g. "${NAME}")
    • self-stake amount (≥ 15,000 SRX) and funding source
    • jurisdiction + ops contact for incident coordination

  Reference: ${BLUE}https://docs.sentrixchain.com/operations/VALIDATOR_ONBOARDING${RESET}

${BOLD}Endpoints${RESET}
  RPC mainnet: https://rpc.sentrixchain.com   (chain 7119)
  RPC testnet: https://testnet-rpc.sentrixchain.com  (chain 7120)
  Explorer:    https://scan.sentrixchain.com
  Faucet:      https://faucet.sentrixchain.com  (testnet)
  gRPC:        https://grpc.sentrixchain.com   (port 443, HTTP/2 + gRPC-Web)
  Docs:        https://docs.sentrixchain.com/operations/

${DIM}Open issues / questions: https://github.com/sentrix-labs/sentrix/issues${RESET}
${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}
EOF
