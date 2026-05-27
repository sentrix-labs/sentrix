# Running a Sentrix Validator

> [!NOTE]
> Sentrix is **permissionless** under Voyager DPoS — anyone with the
> required self-stake can register as a validator without contacting the
> Foundation. The chain does not maintain a whitelist.

This is the end-to-end guide for an **independent operator** — not the
Sentrix team, not an internal contributor — who wants to run a Sentrix
validator node. You provide the hardware, the time, and the stake. The
chain does not care who you are or where your host is; it only cares
that your validator address is in the on-chain stake registry, that you
bonded `MIN_SELF_STAKE` (15,000 SRX), and that your node produces valid
blocks when it's your turn.

The doc assumes you can read a Linux manpage, can use `systemd`, and
have shell access to a server under your control. No specific cloud
provider, no specific OS version, no "join the operator's private
fleet" required.

---

## Table of contents

1. [What you're signing up for](#1-what-youre-signing-up-for)
2. [Active-set shape](#2-active-set-shape)
3. [5-minute quickstart](#3-5-minute-quickstart)
4. [Hardware + network](#4-hardware--network)
5. [Get the binary](#5-get-the-binary)
6. [Keystore](#6-keystore)
7. [systemd unit](#7-systemd-unit)
8. [Register as a validator (permissionless)](#8-register-as-a-validator-permissionless)
9. [Deploying updates](#9-deploying-updates)
10. [Monitoring](#10-monitoring)
11. [Recovery paths](#11-recovery-paths)
12. [FAQ](#12-faq)
13. [Where to ask](#13-where-to-ask)

---

## 1. What you're signing up for

### Consensus responsibility

Sentrix runs **Voyager DPoS + BFT** on mainnet, live since h=579,047
(2026-04-25). Stake-weighted, **uncapped candidate set** (cap was
lifted v2.2.11), 21 active validators rotate based on stake rank,
3-phase BFT round (propose / prevote / precommit) per block, 2/3+1 of
stake-weighted active set finalises. Block time targets ~2.5 s.

> [!IMPORTANT]
> - Node uptime expectation: **>99.5%**. The in-chain liveness tracker
>   jails validators that miss more than 70% of slots in a rolling
>   14,400-block window (~4 hours at 1 s block time).
> - You sign every block in your turn. **Double-signing is slashable
>   (20% stake cut + auto-jail).**
> - Self-stake minimum: **15,000 SRX** (1,500,000,000,000 sentri),
>   enforced by `register_validator` apply path.
> - You may delegate additional stake to yourself to climb the active-
>   set ranking (top-21 produce blocks).

### Operational responsibility

- Running the `sentrix` binary under systemd.
- Firewall + SSH hardening. Recommended: UFW + fail2ban + `PasswordAuthentication=no`.
- Encrypted keystore (Argon2id v2). Never publish your private key,
  never ship it in an env variable that ends up in process listings.
- Monitoring: read your own `journalctl -u sentrix-<your-name>` and
  know what `CRITICAL #1e: state_root mismatch` means (= you're
  diverging from canonical, recovery procedure in §11).
- Upgrades: track the chain's release channel, deploy new binaries
  within the announced maintenance window.

---

## 2. Active-set shape

| Parameter | Value |
|---|---|
| Active validators | 21 (top by total stake) |
| Candidate set | unlimited (any address with ≥ `MIN_SELF_STAKE`) |
| `MIN_SELF_STAKE` | 15,000 SRX |
| Permissionless | yes — no whitelist, no admin approval |
| Block time | ~2.5 s target |
| Finality | BFT supermajority (2/3+1 stake-weighted) |

The small active set is deliberate — BFT supermajority round-trips
stay short, block time stays low. Candidates beyond 21 sit on the
bench ranked by stake and rotate in when an active spot frees up
(jail, unbond, slash).

---

## 3. 5-minute quickstart

For operators who already run blockchain nodes and just want the
"happy path" — fill in `<…>` placeholders:

```bash
# 1. Pull + build (Rust 1.95+)
git clone https://github.com/sentrix-labs/sentrix.git && cd sentrix
cargo build --release -p sentrix-node    # binary: target/release/sentrix

# 2. Generate keystore (writes data/wallets/<addr>.json)
./target/release/sentrix wallet generate --password "<strong-passphrase>"

# 3. Fund the new address with ≥15,000 SRX (mainnet — buy / OTC / earn;
#    testnet — faucet at https://faucet.sentrixchain.com)

# 4. Install + run as systemd (see §7 for unit template)
sudo cp target/release/sentrix /opt/sentrix/
sudo systemctl enable --now sentrix-<your-name>

# 5. Submit RegisterValidator tx (see §8 for the exact JSON-RPC payload)
#    Or wait for the upcoming `sentrix validator register` CLI helper.
```

That's the whole permissionless flow. No email, no DM, no approval.

---

## 4. Hardware + network

Mainnet reference at h ≈ 1.74M (2026-05):

| Resource | Minimum | Comfortable |
|---|---|---|
| vCPU | 4 | 6 – 8 |
| RAM | **8 GiB** | 16 GiB |
| Swap | **8 GiB** persistent (`/etc/fstab`) | 16 GiB |
| Disk | 1 TB NVMe SSD | 2 TB NVMe SSD |
| Bandwidth | 100 Mbit sustained | 1 Gbit |

> [!CAUTION]
> RAM + swap floor is **non-negotiable**. `chain.db` is mmap'd; tight
> memory with zero swap → page-cache thrash under tx load → tokio
> worker stalls → silent halts. Empirically observed across 2026-04
> incidents. Any 64-bit Linux works (Ubuntu 22.04 + 24.04 verified).

The consensus binary is **OS-deterministic** across kernel, glibc,
and CPU family — verified across Ubuntu 22.04 (glibc 2.35) and 24.04
(glibc 2.39) hosts.

Open inbound ports:

- `30303/tcp` (or your `--port`) — libp2p P2P
- `22/tcp` — SSH (restrict to your IP / jumpbox if possible)

> [!WARNING]
> Do **not** expose the RPC port (`8545`) publicly without a reverse
> proxy + rate limit. Bind RPC to `127.0.0.1` and front it via
> Cloudflare / Caddy / nginx if you want public RPC. Otherwise keep
> local-only.

---

## 5. Get the binary

### Build from source (recommended)

```bash
git clone https://github.com/sentrix-labs/sentrix.git
cd sentrix
cargo build --release -p sentrix-node
# target/release/sentrix
```

Toolchain: Rust 1.95+ (stable). The reference reproducible build:

```bash
docker run --rm -v "$PWD:/w" -w /w rust:1.95-bullseye \
  cargo build --release -p sentrix-node
```

Compare `sha256sum target/release/sentrix` against the published
release-notes hash to verify your build matches canonical.

### Download a release

Signed tarballs at <https://github.com/sentrix-labs/sentrix/releases>.
Verify SHA-256 against the release notes. Extract `sentrix` and
`chmod +x`.

---

## 6. Keystore

Generate the validator keypair:

```bash
./sentrix wallet generate --password "<strong-passphrase>"
# Address: 0x...
# Keystore: data/wallets/<addr>.json
```

Or import an existing private key:

```bash
./sentrix wallet encrypt "<hex-private-key>" \
  --password "<pwd>" \
  --output /opt/sentrix/data/wallets/my-validator.keystore
```

Set permissions:

```bash
sudo chmod 600 /opt/sentrix/data/wallets/*.json
sudo chown <service-user>:<group> /opt/sentrix/data/wallets/*.json
```

> [!TIP]
> Wallet helpers (v2.2.10+) use a no-echo prompt (`rpassword`),
> confirm-twice on `rekey`, atomic backup+rename with rollback on
> failure, and zeroize passwords on drop. The `wallet rekey` command
> verifies a decrypt round-trip before overwriting your only copy,
> and leaves a `.bak-<ts>` you can `rm` after stable operation.

### Password hygiene

- Password lives in the systemd `EnvironmentFile` at mode `600`. Never
  in the unit file itself (env files don't show in `ps`; unit files do).
- Rotate with `sentrix wallet rekey <keystore> --old-password … --new-password …`.
- **Lost password = lost validator.** There is no recovery path. Use a
  password manager + encrypted backup.

---

## 7. systemd unit

`/etc/systemd/system/sentrix-<your-name>.service`:

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
  --peers <bootstrap-multiaddrs-from-channel>
Restart=always
RestartSec=5
LimitNOFILE=65536
EnvironmentFile=/etc/sentrix/sentrix-<your-name>.env
Environment=SENTRIX_DATA_DIR=/opt/sentrix/data
Environment=SENTRIX_ENCRYPTED_DISK=true

[Install]
WantedBy=multi-user.target
```

Env file `/etc/sentrix/sentrix-<your-name>.env` (mode `600`, owner =
service user):

```
SENTRIX_WALLET_PASSWORD=<your-keystore-password>
```

Start + tail:

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

> [!NOTE]
> Bootstrap peer multiaddrs are published in the chain's release
> notes and refreshed periodically. Ask in the operator channel
> if you're unsure which set is current.

---

## 8. Register as a validator (permissionless)

Your node is running as a peer. To produce blocks you must register
in the on-chain stake registry.

### Requirements

| Field | Value |
|---|---|
| Bond | **`tx.amount` must equal `self_stake` and be ≥ 15,000 SRX** |
| Commission rate | 0 – 100% (basis points; `1000` = 10%) |
| Validator address | Your wallet's address (= sender of the tx) |
| Public key | Uncompressed secp256k1 hex (printed by `wallet info`) |

The bond moves your SRX to the protocol treasury during apply; you
can unbond later via `StakingOp::Undelegate` (subject to a 7-day
unbonding period).

### Build + submit the transaction

The wire format is `StakingOp::RegisterValidator { self_stake, commission_rate, public_key }`
sent as a regular signed transaction with `to_address = TOKEN_OP_ADDRESS`
and `tx.amount == self_stake`.

Until the dedicated CLI helper ships, use any of:

- **`sentrix` REPL / scripted tx builder** — the chain ships
  `cli_create_token_tx` (same shape as token ops); analogous
  `cli_create_staking_tx` helper coming in a follow-up release.
- **JSON-RPC eth_sendRawTransaction** — encode `StakingOp` as the tx
  `data` field, sign with your wallet, broadcast.
- **Web tooling** — a simple form will land at
  `https://validators.sentrixchain.com` for non-CLI operators.

### Verify registration

```bash
# Stake registry (your address should appear)
curl -s https://rpc.sentrixchain.com/staking/validators | jq '.validators[] | select(.address=="0x<yours>")'

# Authority (round-robin scheduler — RegisterValidator mirrors here)
curl -s https://rpc.sentrixchain.com/chain/info | jq '.active_validators'
```

You'll appear immediately in stake_registry, and rotate into the
active set at the next epoch boundary if you're in the top-21 by
total stake (self + delegated).

> [!IMPORTANT]
> The protocol does **not** require operator approval, a moniker
> whitelist, or off-chain registration. Pre-2026-05-13 docs that
> described an email + admin-cosign flow are now obsolete — that was
> the PoA Pioneer path before Voyager DPoS activated. Voyager has
> been live since h=579,047 and the candidate cap (was 100) was
> lifted in v2.2.11.

---

## 9. Deploying updates

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

SCPs the binary, archives the previous copy, restarts the service,
health-checks it.

For a rolling restart across many validators, loop over the above.
`MIN_ACTIVE_VALIDATORS = 1` since v2.1.11 — the chain technically
tolerates a single active validator, but keep 3+ up during a rolling
deploy so block production never depends on a single host.

---

## 10. Monitoring

At minimum, alert on:

- `systemctl is-failed sentrix-<your-name>`
- `journalctl -u sentrix-<your-name> --since '5 min ago' | grep -c CRITICAL` > threshold
- `/chain/info` `.height` delta = 0 for >2 min
- Disk free < 10 GiB

Sentrix emits a rolling-window state_root-mismatch alarm (v2.1.9+)
that fires one LOUD log line if you start rejecting >100 peer blocks
per 5 min — the message includes the rsync-recovery playbook inline.

Grafana dashboard templates: see operator issue #625 (work-in-progress).

---

## 11. Recovery paths

### You missed a lot of blocks (< 1 week)

The node syncs from peers automatically on restart. The `GetBlocks`
handler serves evicted history from MDBX, so fresh nodes and long-
stalled nodes both catch up without a state snapshot.

### Your state diverges

> [!CAUTION]
> **Do not run `sentrix state import` on a post-genesis chain.**
> v2.1.5 and later refuse to start on a keystore built from that path.
> The correct recovery is **frozen-rsync** of `chain.db` from a peer
> you trust, with ALL validators halted.

Short procedure:

```bash
# 1. Stop your node + the trusted peer
systemctl stop sentrix-<your-name>            # local
ssh trusted "systemctl stop sentrix-<theirs>"

# 2. Rsync chain.db
rsync -avz trusted:/opt/sentrix/data/chain.db /opt/sentrix/data/

# 3. Start both
systemctl start sentrix-<your-name>
ssh trusted "systemctl start sentrix-<theirs>"
```

Full incident archive: internal operator runbooks.

### You lose your data directory

Restore from backup, or sync from scratch. The node re-fetches all
blocks from peers. On Voyager you may be jailed for downtime and
need a `StakingOp::Unjail` tx.

---

## 12. FAQ

<details>
<summary><strong>Do I need permission from the Sentrix team?</strong></summary>

No. Since v2.2.11 (2026-05-13), registration is fully permissionless
— any address with ≥15,000 SRX bonded can submit
`StakingOp::RegisterValidator`. The candidate cap was lifted.

</details>

<details>
<summary><strong>How do I get 15,000 SRX on mainnet?</strong></summary>

Buy on a DEX once liquid, OTC from existing holders, or earn through
ecosystem participation (faucet drops are too small for mainnet
bonding — testnet faucet has enough for testing).

</details>

<details>
<summary><strong>What's the difference between "candidate" and "active validator"?</strong></summary>

- **Candidate** — anyone registered in the stake registry. No cap.
- **Active** — top-21 by total stake (self + delegated), produces
  blocks via round-robin. The 22nd-highest candidate sits on the
  bench until an active spot frees up (jail, unbond, slash).

</details>

<details>
<summary><strong>How are blocks scheduled?</strong></summary>

Voyager uses round-robin over the active set: `proposer(h) =
active_set[h % active_set.len()]`. Active set is sorted by stake
(deterministic). Failed proposers timeout and the next-in-rotation
takes over (BFT skip-round).

</details>

<details>
<summary><strong>Can I run multiple validators from one host?</strong></summary>

Technically yes (multiple systemd units + different keystores +
different ports + different data dirs), but operationally fragile —
one disk failure or noisy-neighbour CPU spike takes down both. The
reference deployment co-tenants 3 validators per host only on hosts
sized 8 vCPU / 24 GiB; sub-that you should run one validator per
host.

</details>

<details>
<summary><strong>What happens if I get jailed?</strong></summary>

Your stake_registry entry flips `is_jailed = true`, you stop being
in the active set, no blocks scheduled to you, no rewards accrue.
Submit `StakingOp::Unjail` after the jail-cooldown to re-enter the
candidate pool. Repeated jails = stake slash + tombstone (permanent
removal) per the slashing parameters.

</details>

<details>
<summary><strong>Is my private key recoverable?</strong></summary>

**No.** Lost password = lost validator. Bonded SRX is unrecoverable
without the signing key. Treat keystore + password like a hardware
wallet seed.

</details>

---

## 13. Where to ask

| Channel | URL |
|---|---|
| Validator ops coordination | <validators@sentrixchain.com> |
| Public docs | <https://docs.sentrixchain.com/operations/> |
| Block explorer (mainnet) | <https://scan.sentrixchain.com> |
| Block explorer (testnet) | <https://scan-testnet.sentrixchain.com> |
| Public RPC mainnet (chain 7119) | <https://rpc.sentrixchain.com> |
| Public RPC testnet (chain 7120) | <https://testnet-rpc.sentrixchain.com> |
| Testnet faucet | <https://faucet.sentrixchain.com> |
| Sourcify verifier | <https://verify.sentrixchain.com> |
| gRPC + gRPC-Web | <https://grpc.sentrixchain.com> · <https://grpc-testnet.sentrixchain.com> |
| GitHub issues | <https://github.com/sentrix-labs/sentrix/issues> |
| Security advisories | `SECURITY.md` in repo root |

---

> [!NOTE]
> This doc describes a chain that supports many independent operators
> on diverse hosts and OS versions. If any step above assumes the
> reference operator's infrastructure or invokes a Foundation /
> Treasury / Core / Beacon label in a way that isn't marked as a
> historical reference, file a PR or open an issue.
