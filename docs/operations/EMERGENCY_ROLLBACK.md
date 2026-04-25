# Emergency Rollback Procedure

Three rollback layers exist, in increasing cost / decreasing speed:

1. **Env-var override** (`SENTRIX_FORCE_PIONEER_MODE=1`) — fastest;
   used 2026-04-25 to roll back the Voyager activation livelock.
2. **Binary rollback** (`fast-deploy.sh` with prior tag) — for a bad
   binary that hasn't corrupted state.
3. **chain.db restore** (frozen-rsync from a healthy validator) — for
   state divergence; canonical recovery, but slowest.

Always escalate from the cheapest layer first.

---

## 1. Fast Rollback: `SENTRIX_FORCE_PIONEER_MODE=1`

When a Voyager DPoS+BFT activation goes wrong (e.g. BFT livelock at
the activation height), the **fastest** rollback is the env-var
override that forces the binary back into Pioneer PoA without a
re-deploy:

```bash
# On every validator host (VPS1/VPS2/VPS3), via the systemd
# EnvironmentFile (mode 600, sentrix:sentrix):
sudo bash -c 'echo "SENTRIX_FORCE_PIONEER_MODE=1" \
  >> /etc/sentrix/sentrix-<unit>.env'

sudo systemctl restart sentrix-<unit>
```

The binary checks `SENTRIX_FORCE_PIONEER_MODE` at every block-mode
decision and short-circuits the Voyager path if the override is set.
Effective on next block.

This is what was used on 2026-04-25 when the Voyager activation
attempted at h=557244 livelocked on V2 BFT wiring (issue
[#292](https://github.com/sentrix-labs/sentrix/issues/292)). Total
recovery time: ~1 min per validator vs ~30+ min for a full chain.db
restore.

While the override is in place, also park the Voyager fork height:

```bash
# In the same env file
VOYAGER_FORK_HEIGHT=18446744073709551615   # u64::MAX
```

This makes the activation inert for any future block until #292 ships
and the height is re-set.

---

## 2. Binary Rollback (no state corruption)

Every `fast-deploy.sh` run archives the previous binary in
`/opt/sentrix/releases/`. To roll back the binary only:

```bash
# Roll all 3 mainnet VPS back to a prior release with one command:
SENTRIX_ROLLBACK=/opt/sentrix/releases/sentrix-v2.1.24-<timestamp> \
  ./scripts/fast-deploy.sh mainnet
```

`fast-deploy.sh` honours `SENTRIX_ROLLBACK` and skips the build step,
shipping the named binary instead. Same rolling stop/start order, same
health check. ~2 min end-to-end.

If you must do it by hand on a single host:

```bash
# 1. Stop the unit
sudo systemctl stop sentrix-<unit>

# 2. List archived versions
ls -lt /opt/sentrix/releases/

# 3. Restore
sudo cp /opt/sentrix/releases/sentrix-v2.1.24-<timestamp> /opt/sentrix/sentrix
sudo chmod +x /opt/sentrix/sentrix

# 4. Restart
sudo systemctl start sentrix-<unit>
```

Current production binary at the time of writing: **v2.1.25**
(`md5 5ad7804c0d7e68f8cab47872f7dbc7ac`). Prior good release on
mainnet: v2.1.24 (`md5 a25f9d771648f6c851a6ee11867fe958`, also the
testnet binary).

---

## 3. State Recovery (chain.db restore)

When state has diverged (different block hash at the same height,
state_root mismatch, etc.), the canonical recovery is a **frozen
rsync** of `chain.db` from a healthy peer with **all** validators
halted. See [STATE_EXPORT.md](STATE_EXPORT.md) for why
`sentrix state export/import` is **not** the right path for a
post-genesis chain.

The full procedure lives in `founder-private/runbooks/state-divergence-recovery.md`
(internal). Outline:

1. Pick the canonical validator (matches the most peers; longest valid
   chain at consensus root).
2. Stop **all** validators on the diverged hosts.
3. Backup the diverged `chain.db` to a sibling directory.
4. `rsync -aP` the canonical `chain.db` to each diverged host while
   the source is frozen (canonical node is also stopped during the
   copy).
5. `chown sentrix:sentrix` on the destination, ensure perms match.
6. Start all validators **simultaneously** (or in the standard
   primary-first order).

State_root is recomputed from the canonical chain.db on the next block
and the divergence is gone.

---

## NEVER Do This

- **Never `git push --force` to roll back.** The CI/CD deploy job is
  disabled — a force-push to main does **not** redeploy. Use
  `fast-deploy.sh` with a `SENTRIX_ROLLBACK=...` arg instead. Force-push
  also rewrites public history; CI test artifacts and PR-comment links
  start pointing at vanished commits.

- **Never build on Windows and SCP to Linux VPS.** Windows produces PE
  executables, Linux needs ELF. The binary will fail with "Exec format
  error". `fast-deploy.sh` uses a Linux container precisely so this
  can't happen.

- **Never run admin CLI separately per-VPS during recovery.** Run the
  validator add/remove/toggle on a single chain.db, then rsync to the
  rest. The admin_log holds wall-clock timestamps; running the same op
  three times produces three different timestamps and three different
  state_roots.

- **Never use `sentrix state export/import` to recover a post-genesis
  chain.** v2.1.5+ refuses to start on a keystore built from import.
  Use frozen-rsync of chain.db (path 3 above).
