#!/usr/bin/env bash
# deploy-sntx-mainnet.sh — one-shot redeploy of the SNTX utility token on
# mainnet. SNTX was originally deployed on the pre-v2.0 sled-backed chain
# and was wiped during the MDBX migration + chain reset at v2.0.0. This
# script rebuilds the signed deploy tx and submits it via the public
# mainnet RPC. See backlog item #1c.
#
# Specs (match tokenomics doc):
#   name     = "Sentrix Utility Token"
#   symbol   = "SNTX"
#   decimals = 18
#   supply   = 10_000_000_000 (10 billion, fixed)
#
# Usage:
#   export SENTRIX_DEPLOYER_KEY=<founder-or-treasury-private-key-hex>
#   ./scripts/deploy-sntx-mainnet.sh
#
# The deployer key is NOT in this repo. It lives in the offline
# `Founder Private ++` vault on the founder's local drive (see
# reference_secrets_locations). Only the founder should run this. After
# a successful deploy, record the contract address (`SRC20_...`) in:
#   * docs/tokenomics/TOKEN_STANDARDS.md
#   * sentrix-wallet-web env config
#   * sentrix-scan env config
#   * founder-private/BIBLE.md (canonical list of deployed contracts)
#
# The chain code that actually performs the deploy has no foot-guns: a
# duplicate (name, symbol, deployer+nonce) on this chain will fail at
# mempool insertion. Contract address is deterministic from (deployer,
# nonce, name, symbol), so re-runs with the same inputs produce the
# same address — safe to re-submit if the first attempt is lost.

set -euo pipefail

# ── Config ───────────────────────────────────────────────
RPC="${SENTRIX_RPC:-https://sentrix-rpc.sentriscloud.com}"
CHAIN_ID="${SENTRIX_CHAIN_ID:-7119}"           # 7119 = mainnet PoA
TOKEN_NAME="Sentrix Utility Token"
TOKEN_SYMBOL="SNTX"
TOKEN_DECIMALS=18
TOKEN_SUPPLY=10000000000                       # 10 billion, fixed
DEPLOY_FEE=100000                              # 100k sentri; bump if needed

# ── Required env ─────────────────────────────────────────
if [[ -z "${SENTRIX_DEPLOYER_KEY:-}" ]]; then
    echo "ERROR: SENTRIX_DEPLOYER_KEY env var required (founder / treasury key)." >&2
    echo "       Never paste this into chat. Export locally:" >&2
    echo "         export SENTRIX_DEPLOYER_KEY=<hex>    # no 0x prefix" >&2
    exit 2
fi

# ── Build the tiny helper if not already present ────────
HELPER_DIR="${SENTRIX_HELPER_DIR:-/tmp/sntx-deploy-helper}"
if [[ ! -x "${HELPER_DIR}/target/release/sntx-deploy-helper" ]]; then
    echo "==> Building signing helper at ${HELPER_DIR}"
    rm -rf "${HELPER_DIR}"
    mkdir -p "${HELPER_DIR}/src"
    cat > "${HELPER_DIR}/Cargo.toml" <<'EOF'
[package]
name = "sntx-deploy-helper"
version = "0.1.0"
edition = "2024"

[dependencies]
sentrix-primitives = { path = "/home/sentriscloud/sentrix/crates/sentrix-primitives" }
sentrix-wallet = { path = "/home/sentriscloud/sentrix/crates/sentrix-wallet" }
anyhow = "1"
serde_json = "1"
reqwest = { version = "0.12", features = ["json", "blocking"] }
EOF
    cat > "${HELPER_DIR}/src/main.rs" <<'EOF'
use anyhow::{Context, Result};
use sentrix_primitives::transaction::{TOKEN_OP_ADDRESS, TokenOp, Transaction};
use sentrix_wallet::Wallet;

fn main() -> Result<()> {
    let rpc = std::env::var("SENTRIX_RPC").context("SENTRIX_RPC")?;
    let key = std::env::var("SENTRIX_DEPLOYER_KEY").context("SENTRIX_DEPLOYER_KEY")?;
    let chain_id: u64 = std::env::var("SENTRIX_CHAIN_ID").unwrap_or_else(|_| "7119".into()).parse()?;
    let name = std::env::var("TOKEN_NAME").context("TOKEN_NAME")?;
    let symbol = std::env::var("TOKEN_SYMBOL").context("TOKEN_SYMBOL")?;
    let decimals: u8 = std::env::var("TOKEN_DECIMALS").unwrap_or_else(|_| "18".into()).parse()?;
    let supply: u64 = std::env::var("TOKEN_SUPPLY").context("TOKEN_SUPPLY")?.parse()?;
    let fee: u64 = std::env::var("FEE").unwrap_or_else(|_| "100000".into()).parse()?;

    let wallet = Wallet::from_private_key(&key)?;
    let sk = wallet.get_secret_key()?;
    let pk = wallet.get_public_key()?;

    let client = reqwest::blocking::Client::new();
    let nonce_resp: serde_json::Value = client
        .post(format!("{}/rpc", rpc))
        .json(&serde_json::json!({
            "jsonrpc":"2.0","method":"eth_getTransactionCount",
            "params":[wallet.address, "latest"],"id":1
        }))
        .send()?.json()?;
    let nonce_hex = nonce_resp["result"].as_str().context("nonce missing")?;
    let nonce = u64::from_str_radix(nonce_hex.trim_start_matches("0x"), 16)?;
    eprintln!("address={} nonce={} chain_id={} supply={}", wallet.address, nonce, chain_id, supply);

    let op = TokenOp::Deploy {
        name: name.clone(), symbol: symbol.clone(), decimals, supply, max_supply: 0,
    };
    let data = op.encode()?;
    let tx = Transaction::new(
        wallet.address.clone(),
        TOKEN_OP_ADDRESS.to_string(),
        0, fee, nonce, data, chain_id,
        &sk, &pk,
    )?;
    eprintln!("txid={}", tx.txid);

    let resp: serde_json::Value = client
        .post(format!("{}/tokens/deploy", rpc))
        .json(&serde_json::json!({"transaction": tx}))
        .send()?.json()?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}
EOF
    (cd "${HELPER_DIR}" && cargo build --release --quiet)
fi

# ── Dry-run summary ──────────────────────────────────────
cat <<EOF

==> Deploy summary
    RPC        = ${RPC}
    chain_id   = ${CHAIN_ID}
    name       = ${TOKEN_NAME}
    symbol     = ${TOKEN_SYMBOL}
    decimals   = ${TOKEN_DECIMALS}
    supply     = ${TOKEN_SUPPLY}
    fee        = ${DEPLOY_FEE}

EOF
read -p "Proceed with mainnet deploy? Type 'yes' to confirm: " confirm
if [[ "${confirm}" != "yes" ]]; then
    echo "Aborted."
    exit 1
fi

# ── Submit ───────────────────────────────────────────────
SENTRIX_RPC="${RPC}" \
SENTRIX_CHAIN_ID="${CHAIN_ID}" \
TOKEN_NAME="${TOKEN_NAME}" \
TOKEN_SYMBOL="${TOKEN_SYMBOL}" \
TOKEN_DECIMALS="${TOKEN_DECIMALS}" \
TOKEN_SUPPLY="${TOKEN_SUPPLY}" \
FEE="${DEPLOY_FEE}" \
    "${HELPER_DIR}/target/release/sntx-deploy-helper"

echo
echo "==> Post-deploy checklist:"
echo "    [ ] GET ${RPC}/tokens/<SRC20_…> confirms metadata"
echo "    [ ] Record address in docs/tokenomics/TOKEN_STANDARDS.md"
echo "    [ ] Record address in sentrix-wallet-web env config"
echo "    [ ] Record address in sentrix-scan env config"
echo "    [ ] Record address in founder-private/BIBLE.md"
