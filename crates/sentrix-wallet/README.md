# sentrix-wallet

[![crates.io](https://img.shields.io/crates/v/sentrix-wallet.svg)](https://crates.io/crates/sentrix-wallet)
[![docs.rs](https://docs.rs/sentrix-wallet/badge.svg)](https://docs.rs/sentrix-wallet)

Key generation, address derivation, signing, and encrypted keystore for Sentrix Chain.

## Why this crate exists

Both the validator binary and the CLI tooling need to load a secp256k1 secret key,
derive the 20-byte chain address from its pubkey (Keccak-256 of the uncompressed
pubkey, last 20 bytes), sign transactions / votes, and read AES-256-GCM encrypted
keystore files. Centralising that surface here keeps the crypto in one audited
place, with `zeroize` on secret material and `subtle` for constant-time comparison
of authentication tags.

Keystores support two KDFs: Argon2id (v2, the default for new files) and PBKDF2
(v1, kept for backwards compatibility with older keystore files). Consumed by
[sentrix-bft](../sentrix-bft) for vote signing and by
[sentrix-wire](../sentrix-wire) tests for the multiaddr-advertisement signature
path.

## Usage

```toml
[dependencies]
sentrix-wallet = { path = "../sentrix-wallet" }
```

```rust
use sentrix_wallet::{Wallet, Keystore};

// Generate a new wallet, or import via `Wallet::from_private_key(hex)`.
let wallet = Wallet::generate();
let pubkey = wallet.get_public_key()?;
let address = Wallet::derive_address(&pubkey);  // "0x..." 42-char hex

// Save into an encrypted keystore file (Argon2id by default).
let keystore = Keystore::encrypt(&wallet, "passphrase")?;
keystore.save("validator.keystore.json")?;

// Load + decrypt later.
let loaded = Keystore::load("validator.keystore.json")?;
let wallet2 = loaded.decrypt("passphrase")?;
```

Key re-exports: `Wallet`, `Keystore`.

## License

BUSL-1.1 — same as the rest of the Sentrix Chain workspace. Transitions to a permissive open-source license after the Change Date.
