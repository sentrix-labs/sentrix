# Run a Full Node

A fullnode follows the chain and serves local RPC/API data, but it does not
produce blocks. In this codebase, `sentrix start` runs as a non-producer when
no validator key source is provided.

Validator key sources are documented by `sentrix start --help`:

- `--validator-keystore <path>`
- `SENTRIX_VALIDATOR_KEY`

A fullnode must not set either one.

## Build The Binary

From this repository:

```bash
cargo build --release -p sentrix-node
```

The binary is:

```text
target/release/sentrix
```

The Dockerfile in this repo builds the same binary with:

```bash
cargo build --release --bin sentrix -p sentrix-node
```

## Required Environment

The node needs a persistent data directory:

```bash
export SENTRIX_DATA_DIR=./data/fullnode
export SENTRIX_ENCRYPTED_DISK=true
```

`SENTRIX_ENCRYPTED_DISK=true` is required by the storage layer. Ensure the
host disk or volume is encrypted according to your own operations policy.

For local-only RPC:

```bash
export SENTRIX_API_HOST=127.0.0.1
export SENTRIX_API_PORT=8545
```

For testnet, use the checked-in testnet genesis:

```bash
export SENTRIX_CHAIN_ID=7120
```

Mainnet uses the embedded canonical genesis when `--genesis` is omitted.
Do not use mainnet deployment targets while mainnet operations are paused.

## Run A Non-Validator Fullnode

Testnet:

```bash
mkdir -p ./data/fullnode
SENTRIX_DATA_DIR=./data/fullnode \
SENTRIX_ENCRYPTED_DISK=true \
SENTRIX_CHAIN_ID=7120 \
SENTRIX_API_HOST=127.0.0.1 \
SENTRIX_API_PORT=8545 \
./target/release/sentrix start --genesis genesis/testnet.toml --peers ""
```

Mainnet:

```bash
TODO: confirm exact command and current mainnet bootnodes before publishing.
```

The important fullnode rule is: do not pass `--validator-keystore`, and do not
set `SENTRIX_VALIDATOR_KEY`.

## Check Sync Height

HTTP API:

```bash
curl http://localhost:8545/health
curl http://localhost:8545/chain/info | jq
```

Local binary:

```bash
SENTRIX_DATA_DIR=./data/fullnode \
SENTRIX_ENCRYPTED_DISK=true \
./target/release/sentrix chain info
```

## Stop And Restart

If running under systemd, use the service name you created:

```bash
sudo systemctl restart <fullnode-service-name>
sudo systemctl stop <fullnode-service-name>
```

If running in a shell, stop with `Ctrl+C` and restart with the same command.

## Common Troubleshooting

- Check logs for `Invalid block`, `bft_tx FULL`, `apply_watchdog`, or repeated
  sync warnings.
- Check disk space:

```bash
df -h
du -sh ./data/fullnode/chain.db
```

- Confirm the expected network:
  - testnet uses `--genesis genesis/testnet.toml` and `SENTRIX_CHAIN_ID=7120`;
  - mainnet omits `--genesis` and uses embedded genesis.
- Confirm peer connectivity and current bootstrap peers.

DO NOT delete the data directory or `chain.db` as the first recovery step.
