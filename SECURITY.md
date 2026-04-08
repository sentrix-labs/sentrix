# Security Policy

## Reporting a Vulnerability

**Please do NOT report security vulnerabilities through public GitHub issues.**

If you discover a security vulnerability in Sentrix Chain, we appreciate your responsible disclosure. Security is our top priority, and we take every report seriously.

### How to report

Send an email to: **sentriscloud@gmail.com**

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
- Core blockchain engine (`src/core/`)
- Wallet and keystore (`src/wallet/`)
- Network protocol (`src/network/`)
- API endpoints (`src/api/`)
- Cryptographic implementations

The following are **out of scope**:
- Third-party dependencies (report to their maintainers)
- Theoretical attacks that require unrealistic conditions
- Social engineering

### Recognition

We maintain a [Hall of Fame](#hall-of-fame) for researchers who responsibly disclose vulnerabilities. We are committed to publicly acknowledging your contribution (unless you prefer to remain anonymous).

### Hall of Fame

*No vulnerabilities reported yet. Be the first responsible researcher!*

---

Thank you for helping keep Sentrix Chain and its users safe.
