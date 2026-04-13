# Contributing to Sentrix

Thank you for considering contributing to Sentrix! We welcome contributions from the community and are grateful for every pull request, bug report, and feature suggestion.

---

## Getting Started

### Prerequisites

- **Rust 1.94+** — `rustup install stable`
- **Visual Studio Build Tools** (Windows) or **GCC** (Linux/macOS)
- **Git**

### Setup

```bash
# Clone the repository
git clone https://github.com/satyakwok/sentrix.git
cd sentrix-chain

# Build
cargo build

# Run tests
cargo test

# Build release
cargo build --release
```

### Project Structure

```
src/
├── core/
│   ├── blockchain.rs      # Blockchain struct, genesis, constants
│   ├── mempool.rs         # Mempool management (add, prune, limits)
│   ├── block_producer.rs  # Block creation (create_block)
│   ├── block_executor.rs  # Block validation + commit (add_block)
│   ├── token_ops.rs       # SRX-20 operations
│   ├── chain_queries.rs   # Read-only chain queries
│   ├── block.rs           # Block struct + hash
│   ├── transaction.rs     # TX struct + ECDSA sign/verify
│   ├── account.rs         # Balance + nonce state
│   ├── authority.rs       # PoA validator management
│   ├── merkle.rs          # SHA-256 Merkle tree
│   └── vm.rs              # SRX-20 token engine
├── network/
│   ├── node.rs            # Legacy TCP P2P
│   ├── sync.rs            # Chain sync protocol
│   ├── transport.rs       # libp2p TCP + Noise + Yamux
│   ├── behaviour.rs       # libp2p SentrixBehaviour
│   └── libp2p_node.rs     # libp2p node runner
├── wallet/                # Key generation, Argon2id keystore
├── storage/               # sled per-block persistence
├── api/                   # REST API, JSON-RPC, block explorer
├── types/                 # Shared error types
├── lib.rs                 # Library root
└── main.rs                # CLI entry point (17 commands)
tests/
├── common/mod.rs          # Shared test helpers
├── integration_restart.rs
├── integration_sync.rs
├── integration_tx.rs
├── integration_token.rs
├── integration_mempool.rs
├── integration_supply.rs
├── integration_chain_validation.rs
└── integration_sliding_window.rs
```

---

## How to Contribute

### Reporting Bugs

1. Check [existing issues](https://github.com/satyakwok/sentrix-chain/issues) to avoid duplicates
2. Open a new issue with:
   - Clear title describing the bug
   - Steps to reproduce
   - Expected vs actual behavior
   - Rust version (`rustc --version`)
   - OS and architecture

### Suggesting Features

Open an issue with the `feature-request` label. Include:
- What problem does this solve?
- Proposed solution
- Alternatives you've considered

### Submitting Pull Requests

1. **Fork** the repository
2. **Create a branch** from `master`: `git checkout -b feat/my-feature`
3. **Make your changes** — keep commits focused and atomic
4. **Add tests** for any new functionality
5. **Run the test suite**: `cargo test` — all tests must pass
6. **Run clippy**: `cargo clippy` — no warnings
7. **Push** to your fork and open a Pull Request

---

## Code Style

### Rust conventions

- **4-space indent** (Rust default)
- **snake_case** for functions and variables
- **PascalCase** for types and structs
- **UPPER_CASE** for constants
- Run `cargo fmt` before committing

### Commit messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat: add staking contract
fix: correct nonce validation in mempool
docs: update API reference
test: add authority round-robin edge cases
refactor: extract fee calculation into helper
```

### File organization

- Keep modules focused — one responsibility per file
- Add tests in the same file using `#[cfg(test)] mod tests`
- Public types get `pub`, internals stay private

---

## Pull Request Checklist

Before submitting, make sure:

- [ ] `cargo build` compiles without errors
- [ ] `cargo test` — all tests pass
- [ ] `cargo clippy` — no warnings
- [ ] `cargo fmt` — code is formatted
- [ ] New code has tests
- [ ] No sensitive data (keys, passwords, .env) in the commit
- [ ] Commit messages follow conventional format
- [ ] PR description explains **what** and **why**

---

## Testing

We take testing seriously. The project has **81+ tests** across 10 suites.

### Running tests

```bash
# All tests
cargo test

# Specific module
cargo test core::blockchain
cargo test wallet::keystore
cargo test storage::db

# With output
cargo test -- --nocapture
```

### Writing tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_descriptive_name() {
        // Arrange
        let mut db = AccountDB::new();
        db.credit("alice", 10_000);

        // Act
        let result = db.transfer("alice", "bob", 5_000, 100);

        // Assert
        assert!(result.is_ok());
        assert_eq!(db.get_balance("bob"), 5_000);
    }
}
```

---

## Architecture Guidelines

### Core principles

1. **No unsafe code** — we don't use `unsafe` blocks
2. **Integer arithmetic** — balances use `u64` (sentri), never `f64`
3. **Atomic validation** — blocks are validated in two passes (dry-run → commit)
4. **Fail fast** — use `SentrixResult<T>` and propagate errors with `?`
5. **Deterministic** — same input always produces same output (critical for consensus)

### What makes a good contribution

- **Bug fixes** with a regression test
- **Performance improvements** with benchmarks
- **New features** aligned with the [roadmap](README.md#roadmap)
- **Documentation** improvements
- **Test coverage** for uncovered code paths

### What we probably won't merge

- Breaking changes to the consensus protocol without discussion
- Dependencies with C bindings (we prefer pure Rust)
- Features not aligned with the roadmap
- Code without tests

---

## Security

If you find a security vulnerability, **do NOT open a public issue**. See [SECURITY.md](SECURITY.md) for responsible disclosure instructions.

---

## Community

- **GitHub Issues** — bugs and feature requests
- **Pull Requests** — code contributions
- **Discussions** — questions and ideas

---

## License

By contributing to Sentrix, you agree that your contributions will be licensed under the [BUSL-1.1](LICENSE) license.

---

Thank you for being part of building Sentrix!
