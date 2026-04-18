# Deployment

How to deploy a Sentrix node on a Linux server.

## Requirements

- Linux (Ubuntu 22.04+ / Debian 12+)
- 2 CPU, 2 GB RAM, 20 GB SSD
- Ports open: 8545 (API), 30303 (P2P)

## Build from Source

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

git clone https://github.com/sentrix-labs/sentrix.git
cd sentrix
cargo build --release
# → target/release/sentrix
```

## Docker

```bash
git clone https://github.com/sentrix-labs/sentrix.git && cd sentrix
docker compose up -d --build
```

Ports 8545 + 30303 exposed, data in named volume, health check on `/health`, auto-restart.

## Bootstrap a Node

```bash
# Generate wallet
sentrix wallet generate

# Init chain
sentrix init --admin-address 0x<your_address>

# Add validator
sentrix validator add --address 0x<addr> --public-key 04<pubkey> --name "Name"

# Start (preferred: encrypted keystore)
SENTRIX_WALLET_PASSWORD=<pass> \
  sentrix start --validator-keystore /opt/sentrix/data/wallets/<addr>.json \
                --peers [PEER_IP]:30303

# Or via env var (raw hex private key)
SENTRIX_VALIDATOR_KEY=<key> sentrix start --peers [PEER_IP]:30303
```

## Systemd

```ini
# /etc/systemd/system/sentrix-node.service
[Unit]
Description=Sentrix Chain Node
After=network.target

[Service]
Type=simple
User=sentrix
WorkingDirectory=/opt/sentrix
ExecStart=/opt/sentrix/sentrix start \
  --validator-keystore /opt/sentrix/data/wallets/validator.json \
  --peers [PEER_IP]:30303 \
  --data-dir /opt/sentrix/data
Restart=on-failure
RestartSec=5
LimitNOFILE=65535
# Wallet password — sourced from EnvironmentFile so it never appears in
# `systemctl show` output, journalctl, or `ps aux`.
EnvironmentFile=/etc/sentrix/wallet.env  # contains: SENTRIX_WALLET_PASSWORD=...
Environment=SENTRIX_API_KEY=<key>
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload && sudo systemctl enable --now sentrix-node
```

## Environment Variables

| Var | Default | What |
|-----|---------|------|
| `SENTRIX_API_KEY` | (none) | Auth for write endpoints. Unset = all public |
| `SENTRIX_DATA_DIR` | `./data` | Chain data path |
| `SENTRIX_CORS_ORIGIN` | (none) | CORS origin. Unset = restrictive |
| `RUST_LOG` | `info` | Log level |

## Firewall

```bash
sudo ufw allow 22/tcp && sudo ufw allow 8545/tcp && sudo ufw allow 30303/tcp && sudo ufw enable
```

## Data Directory

```
data/
├── chain.db/         # MDBX (blocks, state, index)
├── identity/
│   └── node_keypair  # Ed25519 for libp2p PeerId
└── wallets/          # encrypted keystores
```

## Joining the Network

```bash
sentrix init --admin-address 0x<genesis_admin>
sentrix start --peers [BOOTSTRAP]:30303
# Connects via libp2p, verifies chain_id 7119, syncs from genesis
```

Validator registration needs admin auth — contact the network admin.

## Multiple Validators on One Machine

Different ports + data dirs per validator:

```bash
SENTRIX_VALIDATOR_KEY=<key1> SENTRIX_DATA_DIR=data1 \
  sentrix start --port 8545 --p2p-port 30303
SENTRIX_VALIDATOR_KEY=<key2> SENTRIX_DATA_DIR=data2 \
  sentrix start --port 8546 --p2p-port 30304
```

Each needs its own systemd service file and firewall rules.

## Testnet

Run a testnet node alongside mainnet on the same machine. Same binary, different chain_id and ports.

```bash
# Set testnet chain_id
export SENTRIX_CHAIN_ID=7120
export SENTRIX_DATA_DIR=/opt/sentrix-testnet/data
export SENTRIX_API_PORT=9545

# Init testnet genesis
sentrix init --admin 0x<testnet_admin_address>

# Add validator
export SENTRIX_ADMIN_KEY=<admin_private_key>
sentrix validator add <address> "Testnet Validator" <public_key>

# Start (env var; or use --validator-keystore + SENTRIX_WALLET_PASSWORD)
SENTRIX_VALIDATOR_KEY=<key> sentrix start --port 31303
```

Systemd service example:

```ini
# /etc/systemd/system/sentrix-testnet.service
[Unit]
Description=Sentrix Testnet Node
After=network.target

[Service]
Type=simple
User=sentrix
WorkingDirectory=/opt/sentrix-testnet
ExecStart=/opt/sentrix/sentrix start --validator-keystore /opt/sentrix-testnet/data/wallets/validator.json --port 31303
Restart=on-failure
RestartSec=5
EnvironmentFile=/etc/sentrix/testnet-wallet.env  # SENTRIX_WALLET_PASSWORD=...
Environment=SENTRIX_DATA_DIR=/opt/sentrix-testnet/data
Environment=SENTRIX_CHAIN_ID=7120
Environment=SENTRIX_API_PORT=9545
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

Key differences from mainnet:

| | Mainnet | Testnet |
|-|---------|---------|
| Chain ID | 7119 | 7120 |
| API port | 8545 | 9545 |
| P2P port | 30303 | 31303 |
| Data dir | /opt/sentrix/data | /opt/sentrix-testnet/data |

The two networks are completely isolated — different chain_id means peers reject each other on handshake.
