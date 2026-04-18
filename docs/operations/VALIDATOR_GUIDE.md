# Validator Guide

Run a Sentrix validator node and participate in block production.

## Requirements

| | Minimum | Recommended |
|---|---|---|
| CPU | 2 cores | 4 cores |
| RAM | 2 GB | 4 GB |
| Disk | 20 GB SSD | 50 GB NVMe |
| Network | 10 Mbps | 100 Mbps |
| OS | Ubuntu 22.04+ | Ubuntu 24.04 |

## Quick Start

### 1. Get the binary

```bash
# Option A: build from source
git clone https://github.com/satyakwok/sentrix.git
cd sentrix
cargo build --release
cp target/release/sentrix /opt/sentrix/sentrix

# Option B: download from latest release
curl -L https://github.com/satyakwok/sentrix/releases/latest/download/sentrix -o /opt/sentrix/sentrix
chmod +x /opt/sentrix/sentrix
```

### 2. Generate a validator wallet

```bash
sentrix wallet generate
# Save the address + private key securely.
# Then encrypt:
sentrix wallet encrypt <private_key_hex> --output /opt/sentrix/data/wallets/validator.keystore
```

### 3. Register as validator

Contact the chain admin to register your address + public key. On Pioneer (PoA), validators are admin-added. On Voyager (DPoS), you self-register via staking.

### 4. Configure systemd

Create `/etc/systemd/system/sentrix-validator.service`:

```ini
[Unit]
Description=Sentrix Validator
After=network.target

[Service]
Type=simple
User=sentrix
WorkingDirectory=/opt/sentrix
ExecStart=/opt/sentrix/sentrix start \
    --validator-keystore /opt/sentrix/data/wallets/validator.keystore \
    --port 30303 \
    --peers <bootstrap_node>:30303
Restart=on-failure
RestartSec=5
LimitNOFILE=65535

# Required environment
Environment=SENTRIX_DATA_DIR=/opt/sentrix/data
Environment=SENTRIX_API_PORT=8545
Environment=SENTRIX_ENCRYPTED_DISK=true
Environment=SENTRIX_WALLET_PASSWORD=<your_keystore_password>
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

**Important:** `chmod 600` the service file to hide the wallet password from non-root users.

### 5. Start

```bash
sudo systemctl daemon-reload
sudo systemctl enable sentrix-validator
sudo systemctl start sentrix-validator
```

### 6. Verify

```bash
# Service running?
sudo systemctl status sentrix-validator

# Health OK?
curl http://localhost:8545/health

# Chain syncing?
curl http://localhost:8545/chain/info

# Block height increasing?
watch -n 3 'curl -s http://localhost:8545/chain/info | python3 -m json.tool | grep height'
```

## Monitoring

### Health check

```bash
curl http://localhost:8545/health
# {"status":"ok","node":"sentrix-chain"}
```

### Prometheus metrics

```bash
curl http://localhost:8545/metrics
# Returns: sentrix_block_height, sentrix_active_validators, sentrix_tx_pool_size, etc.
```

Add to your `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: sentrix
    static_configs:
      - targets: ['localhost:8545']
    metrics_path: /metrics
    scrape_interval: 15s
```

### Logs

```bash
sudo journalctl -u sentrix-validator -f
```

## Troubleshooting

| Symptom | Fix |
|---|---|
| Node not producing blocks | Check `ps aux` — is the process running? Check logs for errors. |
| `Error: Wrong password` | Password in systemd doesn't match keystore. Re-check. |
| `Error: disk encryption not confirmed` | Set `SENTRIX_ENCRYPTED_DISK=true` in systemd. |
| Height stuck | Check peers — `curl localhost:8545/chain/info` should show `active_validators > 0`. |
| State root mismatch after upgrade | Run `sentrix chain reset-trie` (stops and rebuilds trie from AccountDB). |
| High memory usage | Normal for ~200K+ blocks. Sled caches aggressively. Restart clears cache. |

## Staking (Voyager DPoS — Testnet)

On the Voyager testnet, validators self-register via staking:

```bash
# Minimum self-stake: 15,000 SRX
curl -X POST http://localhost:9545/staking/register \
  -H "Content-Type: application/json" \
  -d '{"address":"0x...", "stake": 1500000000000, "commission": 1000}'
```

See [docs/tokenomics/STAKING.md](../tokenomics/STAKING.md) for full staking mechanics.

## Security

- **Never share your private key or keystore password.**
- Always load the validator key via `--validator-keystore <path>` or the
  `SENTRIX_VALIDATOR_KEY` env var. The legacy `--validator-key <hex>` CLI
  flag was removed in v2.0.1 (audit C-06) — CLI args leak via `ps aux`,
  shell history, and process snapshots.
- Source `SENTRIX_WALLET_PASSWORD` from a systemd `EnvironmentFile=` (or
  equivalent secret store) so it never appears in `systemctl show` output
  or `journalctl` logs.
- `chmod 600` all systemd unit files that contain passwords.
- Enable disk encryption on the validator host (`SENTRIX_ENCRYPTED_DISK=true`).
- See [SECURITY.md](../../SECURITY.md) for the vulnerability disclosure policy.
