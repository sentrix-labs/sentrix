#!/usr/bin/env bash
# CI integration smoke: boot a fresh N-validator testnet from genesis on
# loopback, let it produce blocks, and assert every validator agrees on the
# block hash at a common height (consensus convergence) + the chain stays live.
#
# Scope: this exercises real boot, BFT, libp2p peering, and block production —
# the layer the in-process tests can't reach. It does NOT assert state_root
# (that fork engages at height 100_000, unreachable in a short CI run); the
# state_root/determinism class is guarded by the in-process trie + 4-validator
# determinism tests.
#
# Self-contained: generates keystores + genesis, no external state. Designed to
# run on a clean CI runner; locally it uses high ports to avoid a live testnet.
set -euo pipefail

BIN="${SENTRIX_BIN:-./target/release/sentrix}"
WORK="${WORK_DIR:-$(mktemp -d)}"
N="${VALIDATORS:-3}"
P2P_BASE="${P2P_BASE:-41303}"
API_BASE="${API_BASE:-19545}"
PW="ci-testnet-pw"
TARGET_HEIGHT="${TARGET_HEIGHT:-15}"   # must exceed VOYAGER_FORK_HEIGHT (10)
TIMEOUT="${TIMEOUT:-180}"              # seconds to reach TARGET_HEIGHT

PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

echo "== sentrix CI testnet convergence smoke (N=$N, work=$WORK) =="
"$BIN" --version
mkdir -p "$WORK/wallets"

# Common testnet fork schedule (mirrors the live testnet env so consensus
# behaves the same: DPoS at 10, reward-v2 at 100, jail disabled).
# VOYAGER_FORK_HEIGHT=1 activates DPoS from the first block so the genesis
# validators ARE the active set immediately. (Pre-Voyager "Pioneer" PoA treats
# genesis.validators as informational and needs admin-added validators, which a
# fresh chain has none of — so it would never produce a block to cross the fork.)
COMMON_ENV=(
  SENTRIX_CHAIN_ID=7120
  VOYAGER_FORK_HEIGHT="${VOYAGER_FORK_HEIGHT:-1}"
  VOYAGER_EVM_HEIGHT=752
  VOYAGER_REWARD_V2_HEIGHT=100
  TOKENOMICS_V2_HEIGHT=381651
  BFT_GATE_RELAX_HEIGHT=551500
  JAIL_CONSENSUS_HEIGHT=18446744073709551615
  SENTRIX_GRPC_ENABLED=0
  SENTRIX_API_HOST=127.0.0.1
  SENTRIX_P2P_HOST=127.0.0.1
  SENTRIX_ALLOW_UNENCRYPTED_DISK=true
  RUST_LOG=warn
)

# 1) Generate N validator keystores, collect addresses.
ADDRS=()
for i in $(seq 1 "$N"); do
  out=$(env SENTRIX_DATA_DIR="$WORK/gen" "$BIN" wallet generate --password "$PW" 2>/dev/null)
  addr=$(echo "$out" | grep -oE '0x[0-9a-fA-F]{40}' | head -1)
  ksfile=$(echo "$out" | grep -oE '/[^ ]+\.json' | head -1)
  mkdir -p "$WORK/n$i/wallets"
  cp "$ksfile" "$WORK/n$i/wallets/val.keystore"
  ADDRS+=("$addr")
  echo "  val$i = $addr"
done

# 2) Build genesis with all validators + balances.
GEN="$WORK/genesis.toml"
{
  echo '[chain]'
  echo 'chain_id = 7120'
  echo 'name = "Sentrix CI Testnet"'
  echo '[genesis]'
  echo 'timestamp = 1714200000'
  echo 'parent_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"'
  for a in "${ADDRS[@]}"; do
    echo '[[genesis.validators]]'; echo "address = \"$a\""; echo 'stake = 1050000000000000'
  done
  for a in "${ADDRS[@]}"; do
    echo '[[genesis.balances]]'; echo "address = \"$a\""; echo 'amount = 1050000000000000'
  done
} > "$GEN"

# 3) init + start each node with isolated ports and a peer list of the others.
for i in $(seq 1 "$N"); do
  p2p=$((P2P_BASE + i - 1)); api=$((API_BASE + i - 1))
  peers=""
  for j in $(seq 1 "$N"); do
    [ "$j" = "$i" ] && continue
    peers="${peers:+$peers,}127.0.0.1:$((P2P_BASE + j - 1))"
  done
  env "${COMMON_ENV[@]}" SENTRIX_DATA_DIR="$WORK/n$i" \
    "$BIN" init --admin "${ADDRS[0]}" --genesis "$GEN" >"$WORK/n$i.init.log" 2>&1
  env "${COMMON_ENV[@]}" SENTRIX_DATA_DIR="$WORK/n$i" SENTRIX_API_PORT="$api" SENTRIX_WALLET_PASSWORD="$PW" \
    "$BIN" start --genesis "$GEN" --validator-keystore "$WORK/n$i/wallets/val.keystore" \
    --port "$p2p" --peers "$peers" >"$WORK/n$i.log" 2>&1 &
  PIDS+=($!)
  echo "  started val$i (p2p=$p2p api=$api pid=${PIDS[-1]})"
done

# 4) Poll until all nodes report TARGET_HEIGHT (liveness), or fail on timeout.
echo "== waiting for all $N validators to reach height $TARGET_HEIGHT (timeout ${TIMEOUT}s) =="
# Tolerant readers: an API that isn't up yet (or a transient curl failure) must
# yield an empty string, never abort the script under `set -euo pipefail`.
height_of() { curl -s --max-time 3 "http://127.0.0.1:$1/chain/info" 2>/dev/null | grep -oE '"height":[0-9]+' | grep -oE '[0-9]+' | head -1 || true; }
hash_at()   { curl -s --max-time 3 "http://127.0.0.1:$1/blocks/$2"   2>/dev/null | grep -oE '"hash":"[0-9a-fx]+"' | head -1 || true; }

start=$(date +%s); ok=0
while [ $(( $(date +%s) - start )) -lt "$TIMEOUT" ]; do
  for p in "${PIDS[@]}"; do kill -0 "$p" 2>/dev/null || { echo "FAIL: a validator process died"; tail -20 "$WORK"/n*.log; exit 1; }; done
  min=999999999
  for i in $(seq 1 "$N"); do
    h=$(height_of $((API_BASE + i - 1)) || true); h=${h:-0}
    [ "$h" -lt "$min" ] && min=$h
  done
  echo "  min height across validators: $min"
  if [ "$min" -ge "$TARGET_HEIGHT" ]; then ok=1; break; fi
  sleep 5
done
[ "$ok" = 1 ] || { echo "FAIL: validators did not reach height $TARGET_HEIGHT within ${TIMEOUT}s (livelock/stall)"; exit 1; }

# 5) Convergence: every validator must report the SAME block hash at a height
#    all of them have committed (TARGET_HEIGHT - 2, a safe common floor).
CHECK_H=$((TARGET_HEIGHT - 2))
echo "== convergence check: block hash agreement at height $CHECK_H =="
ref=""
for i in $(seq 1 "$N"); do
  hh=$(hash_at $((API_BASE + i - 1)) "$CHECK_H")
  echo "  val$i @ $CHECK_H -> ${hh:-MISSING}"
  [ -z "$hh" ] && { echo "FAIL: val$i has no block at $CHECK_H"; exit 1; }
  if [ -z "$ref" ]; then ref="$hh"; elif [ "$hh" != "$ref" ]; then
    echo "FAIL: block-hash divergence at height $CHECK_H — validators forked"; exit 1
  fi
done

echo "PASS: $N validators live (height >= $TARGET_HEIGHT) and converged on block hash at $CHECK_H"
