# Emergency Rollback Procedure

## Quick Rollback from Binary Archive

Every CI/CD deploy archives the previous binary in `/opt/sentrix/releases/`.
To rollback to a previous version:

```bash
# 1. Stop the node
sudo systemctl stop sentrix-node  # or sentrix-core, sentrix-val1, etc.

# 2. List available versions
ls -lt /opt/sentrix/releases/

# 3. Restore the previous binary
sudo cp /opt/sentrix/releases/sentrix-v1.2.0-20260418120000 /opt/sentrix/sentrix

# 4. Restart
sudo systemctl start sentrix-node
```

For all validators on a VPS:

```bash
sudo systemctl stop sentrix-val{1..5}
sudo cp /opt/sentrix/releases/sentrix-v1.2.0-20260418120000 /opt/sentrix/sentrix
sudo systemctl start sentrix-val{1..5}
```

## Rollback via CI/CD (Preferred)

Force-push a known-good tag to `main` and let CI rebuild:

```bash
git checkout main
git reset --hard v1.2.0   # or pre-refactor-v1.2.0 tag
git push --force origin main
```

CI will build the correct Linux binary and deploy automatically.

## NEVER Do This

- **NEVER build on Windows and SCP to Linux VPS** — Windows produces PE
  executables, Linux needs ELF. The binary will fail with "Exec format error".

- **NEVER use `cargo build` on a non-Linux machine for VPS deploy** — always
  use CI/CD or cross-compile via Docker:

  ```bash
  docker run --rm -v "$(pwd):/build" -w /build rust:1.82-slim \
    bash -c "apt-get update && apt-get install -y libssl-dev pkg-config && \
    cargo build --release --target x86_64-unknown-linux-gnu"
  ```

## Chain Fork Recovery

If validators end up on different chain histories (different block hash at
same height), use the canonical chain.db copy procedure:

1. Identify the canonical validator (longest chain, correct history)
2. Stop all validators on the diverged VPS
3. Backup existing chain.db files
4. SCP canonical chain.db to all data directories
5. Set correct ownership (`chown`)
6. Restart all validators simultaneously

See: SESSION_HANDOFF for the 2026-04-18 chain fork recovery details.
