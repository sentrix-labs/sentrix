---
name: Feature request
about: Propose a new contract or extension to existing canonical set
title: "[feat] "
labels: enhancement
---

**Proposal**

What new contract or feature should be added to the canonical set? One-line summary.

**Use case**

What dApps / integrations would consume this? Concrete example helps.

**Why canonical** (vs ad-hoc deploy)

Canonical contracts are immutable, audited, and Sourcify-verified across the chain. They earn that status because:
- Many independent dApps need the same logic, OR
- The contract sets a standard (e.g., WSRX as canonical wrapped token), OR
- Operator wants treasury / governance to live behind a known address

Pick which applies + explain.

**Spec sketch**

Solidity-style sketch of the public interface:

```solidity
function exampleFunction(...) external returns (...);
```

**Alternatives considered**

- Existing canonical contract that could be extended:
- Off-the-shelf (OZ / Solady) reference:
- Reasons those don't fit:

**Risk**

- Pausable? Upgradeable? Owner-controlled?
- Audit complexity (lines of code, novelty):
- Migration path if v2 ever needed:
