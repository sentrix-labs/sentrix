# Validator Guide — Cheat Sheet

> [!NOTE]
> Run a Sentrix validator in 5 minutes. The full onboarding doc with
> hardware sizing, recovery procedures, FAQ, and chain comparison
> lives at **[VALIDATOR_ONBOARDING.md](./VALIDATOR_ONBOARDING.md)**.

## Quickstart

```bash
# 1. Build
git clone https://github.com/sentrix-labs/sentrix.git && cd sentrix
cargo build --release -p sentrix-node

# 2. Generate keystore
./target/release/sentrix wallet generate --password "<strong-passphrase>"

# 3. Fund ≥15,000 SRX to the new address
#    Testnet: https://faucet.sentrixchain.com
#    Mainnet: buy / OTC

# 4. Install + run
sudo cp target/release/sentrix /opt/sentrix/
sudo systemctl enable --now sentrix-<your-name>

# 5. Submit RegisterValidator tx → block production rotates you in
```

## Hardware

| Resource | Minimum | Recommended |
|---|---|---|
| vCPU | 4 | 6 – 8 |
| RAM | 8 GiB | 16 GiB |
| Swap | 8 GiB persistent | 16 GiB |
| Disk | 60 GiB SSD | 120 GiB NVMe |
| Network | 100 Mbit | 1 Gbit |
| OS | Ubuntu 22.04+ | Ubuntu 24.04 |

> [!CAUTION]
> 2 GiB RAM / no swap was the pre-Voyager minimum. **Do not use it
> today** — chain.db is mmap'd and tight memory under load causes
> silent stalls. Stick to the table above.

## systemd unit

`/etc/systemd/system/sentrix-<your-name>.service`:

```ini
[Unit]
Description=Sentrix Validator (<your-name>)
After=network-online.target

[Service]
Type=simple
User=sentrix
WorkingDirectory=/opt/sentrix
ExecStart=/opt/sentrix/sentrix start \
    --validator-keystore /opt/sentrix/data/wallets/validator.keystore \
    --port 30303 \
    --peers <bootstrap-multiaddrs>
Restart=on-failure
RestartSec=5
LimitNOFILE=65535
EnvironmentFile=/etc/sentrix/sentrix-<your-name>.env
Environment=SENTRIX_DATA_DIR=/opt/sentrix/data
Environment=SENTRIX_API_PORT=8545
Environment=SENTRIX_ENCRYPTED_DISK=true
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
```

> [!IMPORTANT]
> Wallet password goes in `EnvironmentFile=/etc/sentrix/…env` (mode
> `600`, owner = service user). Never inline in the unit file —
> env files don't appear in `ps`; unit files do.

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now sentrix-<your-name>
sudo journalctl -u sentrix-<your-name> -f
```

## Verify

```bash
# Service running?
sudo systemctl status sentrix-<your-name>

# Health endpoint
curl http://localhost:8545/health

# Chain advancing?
watch -n 3 'curl -s http://localhost:8545/chain/info | jq .height'

# You appear in stake registry?
curl -s https://rpc.sentrixchain.com/staking/validators \
  | jq '.validators[] | select(.address=="0x<yours>")'
```

## Register as validator (permissionless)

> [!NOTE]
> Pre-Voyager docs said "contact the admin to register". That's
> obsolete since Voyager activated (h=579,047, 2026-04-25) and the
> candidate cap was lifted in v2.2.11 (2026-05-13).
>
> Today: bond ≥15,000 SRX and submit `StakingOp::RegisterValidator`.
> No email, no DM, no admin co-sign.

See [§8 of the onboarding doc](./VALIDATOR_ONBOARDING.md#8-register-as-a-validator-permissionless)
for the exact tx format.

## Monitoring essentials

```bash
# Prometheus metrics
curl http://localhost:8545/metrics

# Logs
sudo journalctl -u sentrix-<your-name> -f --since '5 min ago'

# Critical events only
sudo journalctl -u sentrix-<your-name> --since '1 hour ago' | grep CRITICAL
```

Alert on:
- `systemctl is-failed sentrix-<your-name>`
- `/chain/info .height` delta = 0 for >2 min
- Disk free < 10 GiB
- `CRITICAL` log lines

## Troubleshooting

| Symptom | Fix |
|---|---|
| Node not producing blocks | `systemctl status` + check logs. If running but no blocks, verify you're in active top-21 by stake. |
| `Error: Wrong password` | Env file password ≠ keystore password. Re-check. |
| `Error: disk encryption not confirmed` | Set `SENTRIX_ENCRYPTED_DISK=true` in env. |
| Height stuck across restart | Peer mesh gap — confirm `--peers` list current. |
| State root mismatch after upgrade | `sentrix chain reset-trie --i-understand-divergence-risk`, then sync. |
| High RAM usage | Normal mmap behaviour at 1M+ blocks. Tight swap is the actual problem; see hardware table. |
| Jailed (inactive in `validator list`) | Submit `StakingOp::Unjail` after cooldown. |

## Security checklist

- [ ] `chmod 600` on keystore + env file
- [ ] Wallet password in `EnvironmentFile`, not unit
- [ ] Disk encrypted (`SENTRIX_ENCRYPTED_DISK=true`)
- [ ] `--validator-key <hex>` CLI flag **never** used (removed v2.0.1, audit C-06 — leaks via `ps aux`)
- [ ] SSH: `PasswordAuthentication=no`, fail2ban, UFW restricting non-essential ports
- [ ] RPC port (`8545`) bound to `127.0.0.1` unless front-proxied with rate limit
- [ ] Keystore + password backed up offline (password manager + encrypted USB)
- [ ] Operator vulnerability disclosure: <security@sentrixchain.com>

## See also

- **[VALIDATOR_ONBOARDING.md](./VALIDATOR_ONBOARDING.md)** — full deep guide (hardware, recovery, FAQ, chain comparison)
- **[VALIDATORS.md](./VALIDATORS.md)** — current active set
- **[../tokenomics/STAKING.md](../tokenomics/STAKING.md)** — staking + delegation + reward mechanics
- **[CLAIM_REWARDS.md](./CLAIM_REWARDS.md)** — post-V4-fork reward claim flow
- **[MONITORING.md](./MONITORING.md)** — Prometheus + Grafana setup
- **[`SECURITY.md`](https://github.com/sentrix-labs/sentrix/blob/main/SECURITY.md)** — vulnerability disclosure policy
