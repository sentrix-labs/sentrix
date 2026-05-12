---
name: Bug report
about: Report a bug in a canonical contract or deploy script
title: "[bug] "
labels: bug
---

**Component**

- [ ] WSRX
- [ ] Multicall3
- [ ] SentrixSafe
- [ ] TokenFactory
- [ ] Deploy script (specify which)
- [ ] CI / repo tooling

**Network** (if on-chain)

- [ ] Mainnet (7119)
- [ ] Testnet (7120)
- [ ] Local fork

**Reproducer**

Step-by-step (or paste a `forge test` command + output):

```
```

**Expected vs actual**

- Expected:
- Actual:

**Environment**

- forge version: `forge --version`
- solc version (from `foundry.toml`): 0.8.24
- OS:

**Severity**

- [ ] Low (cosmetic / docs)
- [ ] Medium (logic correctness, no fund loss)
- [ ] High (fund loss possible, denial-of-service)
- [ ] Critical (immediate exploit) → DO NOT FILE PUBLICLY. Email `security@sentrixchain.com`.
