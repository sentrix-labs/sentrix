//! Externalised genesis configuration (TOML-sourced).
//!
//! Replaces the hardcoded CHAIN_ID / GENESIS_TIMESTAMP / GENESIS_ALLOCATIONS
//! constants with a loadable [`Genesis`] struct. The canonical mainnet
//! configuration is embedded at compile time via `include_str!` so the
//! default `Blockchain::new()` path stays byte-for-byte identical with the
//! running chain; a custom config can be supplied by the node operator
//! through the `--genesis <path>` CLI flag.
//!
//! ## Invariants
//!
//! Any change that affects the genesis block hash (timestamp, parent_hash,
//! or the coinbase tx fields) will fork the chain. The `genesis_block()`
//! helper is intentionally tight — it only re-emits the same fields that
//! [`sentrix_primitives::block::Block::genesis`] uses today. Balance /
//! validator sections do not influence the block hash directly; they seed
//! initial state (balances credit into `AccountDB`) and inform DPoS
//! bootstrapping post-Voyager fork.

use sentrix_primitives::block::Block;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Embedded canonical mainnet genesis — compiled into the binary so the
/// node boots without any external file present.
pub const MAINNET_GENESIS_TOML: &str = include_str!("../../../genesis/mainnet.toml");

/// Errors surfaced by [`Genesis::parse`] and [`Genesis::validate`].
#[derive(Debug)]
pub enum GenesisError {
    /// TOML deserialisation failed.
    Parse(String),
    /// TOML parsed but a semantic invariant was violated.
    Invalid(String),
    /// Filesystem I/O failure when loading a genesis file.
    Io(String),
}

impl fmt::Display for GenesisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GenesisError::Parse(e) => write!(f, "genesis parse error: {}", e),
            GenesisError::Invalid(e) => write!(f, "genesis validation failed: {}", e),
            GenesisError::Io(e) => write!(f, "genesis i/o error: {}", e),
        }
    }
}

impl std::error::Error for GenesisError {}

/// Top-level genesis document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Genesis {
    pub chain: ChainMeta,
    pub genesis: GenesisCore,
}

/// `[chain]` section — chain identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainMeta {
    pub chain_id: u64,
    pub name: String,
}

/// `[genesis]` section — block-0 + initial state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisCore {
    pub timestamp: u64,
    pub parent_hash: String,

    #[serde(default)]
    pub validators: Vec<GenesisValidator>,

    #[serde(default)]
    pub balances: Vec<GenesisBalance>,
}

/// `[[genesis.validators]]` entry — bootstraps DPoS after Voyager fork.
/// `pubkey` is optional because PoA-era chains track validators via
/// AuthorityManager rather than on-chain stake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisValidator {
    pub address: String,
    pub stake: u64,
    #[serde(default)]
    pub pubkey: String,
}

/// `[[genesis.balances]]` entry — sentri-denominated premine allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisBalance {
    pub address: String,
    pub amount: u64,
}

impl Genesis {
    /// Load and validate the embedded mainnet genesis. Infallible in
    /// practice (the embedded file is shipped validated), but returns a
    /// Result for symmetry with `parse`.
    pub fn mainnet() -> Result<Self, GenesisError> {
        Self::parse(MAINNET_GENESIS_TOML)
    }

    /// Parse + validate a genesis document from a TOML string.
    pub fn parse(toml_str: &str) -> Result<Self, GenesisError> {
        let g: Genesis =
            toml::from_str(toml_str).map_err(|e| GenesisError::Parse(e.to_string()))?;
        g.validate()?;
        Ok(g)
    }

    /// Load + validate from a filesystem path.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> Result<Self, GenesisError> {
        let raw = std::fs::read_to_string(path.as_ref())
            .map_err(|e| GenesisError::Io(format!("read {}: {}", path.as_ref().display(), e)))?;
        Self::parse(&raw)
    }

    /// Structural + semantic validation. See module-level notes for rules.
    pub fn validate(&self) -> Result<(), GenesisError> {
        // Chain id must be non-zero (zero is reserved / means "unspecified").
        if self.chain.chain_id == 0 {
            return Err(GenesisError::Invalid("chain_id must be non-zero".into()));
        }

        // Parent hash format: 64 hex chars, optional 0x prefix.
        let ph = self.genesis.parent_hash.trim_start_matches("0x");
        if ph.len() != 64 || !ph.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(GenesisError::Invalid(format!(
                "parent_hash must be 64 hex chars (got {})",
                self.genesis.parent_hash
            )));
        }

        // Timestamp cannot be in the future — catches typos/misconfigured
        // testnet configs before they poison the chain.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| GenesisError::Invalid(format!("system clock error: {}", e)))?
            .as_secs();
        if self.genesis.timestamp > now {
            return Err(GenesisError::Invalid(format!(
                "genesis timestamp {} is in the future (now = {})",
                self.genesis.timestamp, now
            )));
        }

        // Minimum one validator (DPoS/BFT readiness requirement).
        if self.genesis.validators.is_empty() {
            return Err(GenesisError::Invalid(
                "at least one validator required in genesis".into(),
            ));
        }

        // Duplicate validator addresses would double-count stake + confuse
        // the authority set.
        let mut seen_v: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for v in &self.genesis.validators {
            if !seen_v.insert(v.address.as_str()) {
                return Err(GenesisError::Invalid(format!(
                    "duplicate validator address: {}",
                    v.address
                )));
            }
        }

        // Duplicate balance addresses would collide on credit(): one entry
        // silently overwrites the other.
        let mut seen_b: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for b in &self.genesis.balances {
            if !seen_b.insert(b.address.as_str()) {
                return Err(GenesisError::Invalid(format!(
                    "duplicate balance address: {}",
                    b.address
                )));
            }
        }

        // Total premine ≤ MAX_SUPPLY. Uses checked_add to catch overflow
        // from hostile configs before hitting AccountDB.
        let mut total: u64 = 0;
        for b in &self.genesis.balances {
            total = total.checked_add(b.amount).ok_or_else(|| {
                GenesisError::Invalid("balance sum overflows u64".into())
            })?;
        }
        if total > crate::blockchain::MAX_SUPPLY {
            return Err(GenesisError::Invalid(format!(
                "total premine {} exceeds MAX_SUPPLY {}",
                total,
                crate::blockchain::MAX_SUPPLY
            )));
        }

        Ok(())
    }

    /// Compute the sum of all premine balances in sentri units.
    pub fn total_premine(&self) -> u64 {
        self.genesis
            .balances
            .iter()
            .map(|b| b.amount)
            .fold(0u64, |acc, v| acc.saturating_add(v))
    }

    /// Construct the genesis [`Block`] from this config. The block is
    /// bit-identical with `Block::genesis()` when the TOML carries the
    /// canonical timestamp — this is the regression barrier that keeps
    /// us from forking the live chain.
    pub fn build_block(&self) -> Block {
        use sentrix_primitives::transaction::Transaction;
        let genesis_tx =
            Transaction::new_coinbase("GENESIS".to_string(), 0, 0, self.genesis.timestamp);
        let txids: Vec<String> = vec![genesis_tx.txid.clone()];
        let merkle = sentrix_primitives::merkle::merkle_root(&txids);
        let mut block = Block {
            index: 0,
            previous_hash: normalise_parent_hash(&self.genesis.parent_hash),
            transactions: vec![genesis_tx],
            timestamp: self.genesis.timestamp,
            merkle_root: merkle,
            validator: "GENESIS".to_string(),
            hash: String::new(),
            state_root: None,
            round: 0,
            justification: None,
        };
        block.hash = block.calculate_hash();
        block
    }
}

/// Strip optional `0x` prefix and normalise to 64 lowercase hex chars.
fn normalise_parent_hash(s: &str) -> String {
    let stripped = s.trim_start_matches("0x");
    stripped.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mainnet_embedded_parses_and_validates() {
        let g = Genesis::mainnet().expect("embedded mainnet must parse");
        assert_eq!(g.chain.chain_id, 7119);
    }

    // CRITICAL regression barrier: the block produced from the embedded
    // mainnet.toml MUST be byte-identical with Block::genesis(). If this
    // ever breaks, the chain forks on startup.
    #[test]
    fn test_mainnet_genesis_block_hash_matches_hardcoded() {
        let g = Genesis::mainnet().expect("mainnet.toml");
        let from_toml = g.build_block();
        let from_code = Block::genesis();
        assert_eq!(
            from_toml.hash, from_code.hash,
            "genesis block hash mismatch — TOML: {}, hardcoded: {}",
            from_toml.hash, from_code.hash
        );
        assert_eq!(from_toml.timestamp, from_code.timestamp);
        assert_eq!(from_toml.merkle_root, from_code.merkle_root);
        assert_eq!(from_toml.previous_hash, from_code.previous_hash);
        assert_eq!(from_toml.validator, from_code.validator);
    }

    #[test]
    fn test_validate_rejects_zero_chain_id() {
        let toml = r#"
[chain]
chain_id = 0
name = "Bad"

[genesis]
timestamp = 1712620800
parent_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"

[[genesis.validators]]
address = "0x328d56b8174697ef6c9e40e19b7663797e16fa47"
stake = 1
"#;
        let err = Genesis::parse(toml).unwrap_err();
        assert!(matches!(err, GenesisError::Invalid(_)));
        assert!(err.to_string().contains("chain_id"));
    }

    #[test]
    fn test_validate_rejects_no_validators() {
        let toml = r#"
[chain]
chain_id = 7119
name = "NoValidators"

[genesis]
timestamp = 1712620800
parent_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"
"#;
        let err = Genesis::parse(toml).unwrap_err();
        assert!(err.to_string().contains("at least one validator"));
    }

    #[test]
    fn test_validate_rejects_future_timestamp() {
        let future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 1_000_000;
        let toml = format!(
            r#"
[chain]
chain_id = 7119
name = "Future"

[genesis]
timestamp = {future}
parent_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"

[[genesis.validators]]
address = "0x328d56b8174697ef6c9e40e19b7663797e16fa47"
stake = 1
"#
        );
        let err = Genesis::parse(&toml).unwrap_err();
        assert!(err.to_string().contains("future"));
    }

    #[test]
    fn test_validate_rejects_duplicate_validator() {
        let toml = r#"
[chain]
chain_id = 7119
name = "Dup"

[genesis]
timestamp = 1712620800
parent_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"

[[genesis.validators]]
address = "0x328d56b8174697ef6c9e40e19b7663797e16fa47"
stake = 1

[[genesis.validators]]
address = "0x328d56b8174697ef6c9e40e19b7663797e16fa47"
stake = 2
"#;
        let err = Genesis::parse(toml).unwrap_err();
        assert!(err.to_string().contains("duplicate validator"));
    }

    #[test]
    fn test_validate_rejects_balance_exceeding_max_supply() {
        let toml = format!(
            r#"
[chain]
chain_id = 7119
name = "TooRich"

[genesis]
timestamp = 1712620800
parent_hash = "0x0000000000000000000000000000000000000000000000000000000000000000"

[[genesis.validators]]
address = "0x328d56b8174697ef6c9e40e19b7663797e16fa47"
stake = 1

[[genesis.balances]]
address = "0x252f8cfed5acfa9d00d99a65e2ac91f395a35d78"
amount = {}
"#,
            crate::blockchain::MAX_SUPPLY + 1
        );
        let err = Genesis::parse(&toml).unwrap_err();
        assert!(err.to_string().contains("exceeds MAX_SUPPLY"));
    }

    #[test]
    fn test_validate_rejects_malformed_parent_hash() {
        let toml = r#"
[chain]
chain_id = 7119
name = "BadHash"

[genesis]
timestamp = 1712620800
parent_hash = "not_hex"

[[genesis.validators]]
address = "0x328d56b8174697ef6c9e40e19b7663797e16fa47"
stake = 1
"#;
        let err = Genesis::parse(toml).unwrap_err();
        assert!(err.to_string().contains("parent_hash"));
    }

    #[test]
    fn test_total_premine_matches_hardcoded() {
        let g = Genesis::mainnet().unwrap();
        assert_eq!(g.total_premine(), crate::blockchain::TOTAL_PREMINE);
    }
}
