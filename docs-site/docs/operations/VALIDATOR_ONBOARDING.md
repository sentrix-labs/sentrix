# Running a Sentrix validator

This is the end-to-end guide for an independent operator — **not the
Sentrix founder, not an internal team member** — who wants to run a
Sentrix validator node. You provide the hardware, the time, and the
stake (where staking applies). The chain does not care who you are or
where your host is; it only cares that your validator address is in
the on-chain authority registry and that your node produces valid
blocks when it's your turn.

This doc assumes you can read a Linux manpage, can use systemd, and
have shell access to a server under your control. No specific cloud
provider is required, no specific OS version is required, no "join
the operator's private fleet" is required.

---

## 1. What you're signing up for

### Consensus responsibility

Sentrix runs **Pioneer PoA** today (4-validator round-robin —
Foundation, Treasury, Core, Beacon — expanding as operators join)
and will upgrade to **Voyager DPoS + BFT**
(stake-weighted, unbounded validator set) at a fork height TBD. As a
validator:

- Your node is expected to be online **>99.5%**. The in-chain liveness
  tracker jails validators that miss more than 70% of their slots in a
  rolling 14,400-block window (~4 hours at 1 s block time).
- You sign every block in your slot; double-signing is slashable
  (stake cut 20% on Voyager; removal from the authority registry on
  Pioneer).
- You do **not** need to hold the chain's native token to validate on
  Pioneer. On Voyager you'll need to self-bond the DPoS minimum.

### Operational responsibility

- Running the `sentrix` binary under systemd.
- Firewall + SSH hardening. We assume UFW + fail2ban + `PasswordAuthentication=no`.
- Encrypted keystore (Argon2id v2). Never publish your private key,
  never ship it in an environment variable that leaks into process
  listings.
- Monitoring: read your own `journalctl -u sentrix-<your-name>` and
  know what a `CRITICAL #1e: state_root mismatch` line means (it means
  you're diverging from canonical — page us).
- Upgrades: watch the chain's release channel, deploy the new binary
  within the announced maintenance window.

---

## 2. Hardware + network

Minimum (reference mainnet today):

| Resource | Minimum | Comfortable |
|---|---|---|
| vCPU  | 4       | 6 – 8 |
| RAM   | 4 GiB   | 8 – 16 GiB |
| Disk  | 60 GiB SSD | 120 GiB NVMe |
| Bandwidth | 100 Mbit sustained | 1 Gbit |

Any mainstream 64-bit Linux works. **We have deployed on Ubuntu 22.04
and 24.04 in production; the consensus binary is OS-deterministic
across kernel, glibc, and CPU family** — see the
2026-04-23 Core node RCA addendum #9 in the project's incident archive for
the cross-host determinism test result.

Open inbound ports:

- `30303/tcp` (or whatever you configure via `--port`) — libp2p P2P.
- `22/tcp` — SSH. Restrict to your own IP or a jumpbox if you can.

Do **not** expose the RPC port (`8545` etc) publicly without a
reverse proxy + rate limit. Bind RPC to `127.0.0.1` and front it with
Cloudflare / Caddy / nginx if you want to offer public RPC; otherwise
keep it local-only.

---

## 3. Get the binary

You have two paths:

### Build from source (recommended)

```bash
git clone https://github.com/sentrix-labs/sentrix.git
cd sentrix
cargo build --release -p sentrix-node
# binary lands at target/release/sentrix
```

Rust 1.95+. The docker build (`docker run --rm -v $PWD:/w -w /w
rust:1.95-bullseye cargo build --release -p sentrix-node`) is what the
reference operator uses and produces a byte-reproducible binary —
recommended if you want to compare MD5 against the published release
hash.

### Download a release

Signed tarballs are published at
`https://github.com/sentrix-labs/sentrix/releases`. Verify the
SHA256 against the release notes. Extract the `sentrix` binary and
`chmod +x`.

---

## 4. Keystore

Generate your validator keypair:

```bash
./sentrix wallet generate --password "<strong-passphrase>"
# Address: 0x...
# Keystore saved to data/wallets/<addr>.json
```

Or, if you already have a private key (e.g. migrating from another
setup):

```bash
./sentrix wallet encrypt "<hex-private-key>" --password "<pwd>" \
  --output /opt/sentrix/data/wallets/my-validator.keystore
```

Set file permissions:

```bash
sudo chmod 600 /opt/sentrix/data/wallets/*.keystore
sudo chown <systemd-unit-user>:<group> /opt/sentrix/data/wallets/*.keystore
```

### Password hygiene

- Password goes in the systemd env file at `mode 600`, never in the
  unit file itself (env files are not in `ps`; unit files are).
- Rotate with `sentrix wallet rekey <keystore> --old-password …
  --new-password …`. The rekey is atomic: it verifies a decrypt
  round-trip on the new keystore before renaming over the old copy,
  and leaves a `.bak-<ts>` behind.
- Lost password = lost validator. There is no recovery path. Store the
  password offline (password manager + encrypted backup).

---

## 5. systemd unit

Create `/etc/systemd/system/sentrix-<your-name>.service`:

```ini
[Unit]
Description=Sentrix validator (<your-name>)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=<unprivileged-service-user>
WorkingDirectory=/opt/sentrix
ExecStart=/opt/sentrix/sentrix start \
  --validator-keystore /opt/sentrix/data/wallets/<my>.keystore \
  --peers <comma-separated list of bootstrap peers — ask in the
           operator channel for current bootstrap multiaddrs>
Restart=always
RestartSec=5
LimitNOFILE=65536
EnvironmentFile=/etc/sentrix/sentrix-<your-name>.env
Environment=SENTRIX_DATA_DIR=/opt/sentrix/data
Environment=SENTRIX_ENCRYPTED_DISK=true

[Install]
WantedBy=multi-user.target
```

Create the env file at `/etc/sentrix/sentrix-<your-name>.env`
(mode 600, owner = service user):

```
SENTRIX_WALLET_PASSWORD=<your-keystore-password>
```

Enable + start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now sentrix-<your-name>
sudo journalctl -u sentrix-<your-name> -f
```

You should see:

```
Validator mode: 0x<your-validator-address>
P2P transport: libp2p (Noise encrypted)
Peer connected: 12D3KooW…
```

---

## 6. Get added to the authority registry

Your node is now running, but it's not yet a VALIDATOR — it's just a
peer. To become an authority:

1. Send your validator address + uncompressed public key (both are
   printed by `./sentrix wallet info <keystore>`) to the chain's
   current admin (see `docs/operations/GOVERNANCE.md` for the
   current admin address + contact channel).
2. The admin runs `sentrix validator add <your-addr> "<your-name>"
   <your-pubkey> --admin-key <admin-key>`.
3. Once added, you'll appear in `GET /chain/info → validators` and in
   the explorer at `sentrixscan.sentriscloud.com/validators`.

Admin op is verified on-chain — your admission cannot be tampered with
once in a block.

**The `<your-name>` string lands in the on-chain validator registry and
drives the block-explorer label. Choose it to represent your operation
(e.g. `"Acme Validator Co"`, `"Operator Alice"`). It's not a
hostname — it's a public-facing identity.**

---

## 7. Deploying updates

Use the generic `scripts/deploy-validator.sh` in the repo:

```bash
./scripts/deploy-validator.sh \
  --ssh-key  ~/.ssh/my_operator_key \
  --host     op@my-validator.example.com \
  --service  sentrix-my-name \
  --bin-dir  /opt/sentrix \
  --rpc-url  http://127.0.0.1:8545 \
  --binary   ./target/release/sentrix
```

This SCPs the binary, archives the previous copy, restarts the
service, and health-checks it. It's the same primitive the reference
operator uses in their multi-host fleet — there's no "special" tool
for us vs you.

For a rolling restart across many validators, loop over the above for
each host. `MIN_ACTIVE_VALIDATORS = 1` since PR #234 (v2.1.11) — the
chain technically tolerates a single active validator, but in practice
keep 3+ up during a rolling deploy so block production never depends on
one host.

---

## 8. Monitoring

At minimum, alert on:

- Systemd unit failed (`systemctl is-failed sentrix-<your-name>`).
- `journalctl -u sentrix-<your-name> --since '5 min ago' | grep -c
  CRITICAL` > threshold.
- Height not advancing for 2 min (`/chain/info` `.height` delta).
- Disk free < 10 GiB.

Sentrix emits a rolling-window state_root-mismatch alarm (PR #217,
v2.1.9+) that fires one LOUD log line if you start rejecting >100
peer blocks per 5 min — the message includes the rsync-recovery
playbook inline.

---

## 9. Recovery paths

### You missed a lot of blocks (< 1 week)

The node will sync from peers automatically on restart. The
`GetBlocks` handler serves evicted history from MDBX (PR #225), so
fresh nodes and long-stalled nodes both catch up without a state
snapshot.

### Your state diverges

Described in `docs/operations/DEPLOYMENT.md` and the incident archive
at `internal operator runbook`. The short
version: **frozen-rsync** your chain.db from a peer you trust, with
ALL validators halted. Do not use `sentrix state export/import` on
a post-genesis chain — v2.1.5 + later refuse to start on a keystore
built from that path.

### You lose your data directory

Restore from backup, or sync from scratch. The node will re-fetch all
blocks from peers. On Pioneer PoA you rejoin as soon as you're back;
on Voyager you may be jailed and need an unjail op.

---

## 10. Where to ask

- GitHub issues: https://github.com/sentrix-labs/sentrix/issues
- Security advisories: see `SECURITY.md` in the repo root.
- Operator chat: see the pinned link in the repo README.

**This doc describes a chain that supports many independent operators
on diverse hosts and OS versions. If any step above assumes the operator's
infrastructure or invokes a Foundation node/Treasury node/Core node label in a way that isn't
marked as a historical reference, that's a bug in the doc — please
file a PR.**
