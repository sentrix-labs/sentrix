# Security Policy

## Reporting a Vulnerability

**Please do NOT report security vulnerabilities through public GitHub issues.**

If you discover a security vulnerability in Sentrix, we appreciate your responsible disclosure. Security is our top priority, and we take every report seriously.

### How to report

Email **<security@sentrixchain.com>** with the details below, or open
a private GitHub Security Advisory at
<https://github.com/sentrix-labs/sentrix/security/advisories/new>.

For abuse reports (network-level, spam, validator misconduct):
**<abuse@sentrixchain.com>**.

Please include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact assessment
- Suggested fix (if you have one)

### What to expect

| Timeline | Action |
|---|---|
| **24 hours** | We acknowledge receipt of your report |
| **72 hours** | We provide an initial assessment and severity rating |
| **7 days** | We develop and test a fix |
| **14 days** | We release a patched version |

### Severity levels

| Level | Description | Example |
|---|---|---|
| **Critical** | Can steal funds, halt chain, or compromise keys | Private key leak, consensus bypass |
| **High** | Can disrupt operations or lose data | DoS, state corruption |
| **Medium** | Can cause unexpected behavior | Balance calculation error |
| **Low** | Minor issues, no direct risk | UI bugs, log information leak |

### Safe harbor

We consider security research conducted in accordance with this policy to be authorized. We will not pursue legal action against researchers who:

- Make a good faith effort to avoid privacy violations, data destruction, and service disruption
- Provide us a reasonable amount of time to resolve the vulnerability before public disclosure
- Do not exploit the vulnerability beyond what is necessary to confirm it exists

### Scope

The following are **in scope**:
- Core blockchain engine (`crates/sentrix-core/`)
- BFT consensus (`crates/sentrix-bft/`)
- DPoS staking (`crates/sentrix-staking/`)
- State trie (`crates/sentrix-trie/`)
- Storage layer (`crates/sentrix-storage/`)
- EVM adapter and precompiles (`crates/sentrix-evm/`, `crates/sentrix-precompiles/`)
- Wallet and keystore (`crates/sentrix-wallet/`)
- Network protocol (`crates/sentrix-network/`)
- API endpoints (`crates/sentrix-rpc/`, `crates/sentrix-rpc-types/`)
- Cryptographic implementations across the workspace

The following are **out of scope**:
- Third-party dependencies (report to their maintainers)
- Theoretical attacks that require unrealistic conditions
- Social engineering

### Recognition

We maintain a [Hall of Fame](#hall-of-fame) for researchers who responsibly disclose vulnerabilities. We are committed to publicly acknowledging your contribution (unless you prefer to remain anonymous).

### Hall of Fame

*No vulnerabilities reported yet. Be the first responsible researcher!*

---

Thank you for helping keep Sentrix and its users safe.
