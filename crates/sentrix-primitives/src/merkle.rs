// merkle.rs - Sentrix
//
// C-05 note: this implementation has the classic Bitcoin
// CVE-2012-2459 "odd-duplication" property — the last element of a
// level with odd length is duplicated to pad the pair, so trees
// [A, B, C] and [A, B, C, C] produce the same root. The consensus
// layer (`Block::validate_structure`) rejects any block whose
// transaction list contains duplicate txids, which closes the
// block-validity bypass without a chain-breaking merkle format change.
// A migration to RFC 6962 (no duplication + domain separation bytes)
// is tracked as a future-fork-height item in TODO.md.

use sha2::{Digest, Sha256};

pub fn merkle_root(txids: &[String]) -> String {
    if txids.is_empty() {
        return "0".repeat(64);
    }

    if txids.len() == 1 {
        return sha256_hex(txids[0].as_bytes());
    }

    let mut level: Vec<String> = txids.to_vec();

    while level.len() > 1 {
        // Duplicate last element if odd count.
        // See module-level C-05 note: this property is intentional on the
        // current chain and safe only because the block layer rejects
        // duplicate txids before merkle verification is reached.
        if !level.len().is_multiple_of(2)
            && let Some(last) = level.last().cloned()
        {
            level.push(last);
        }

        let mut next_level = Vec::new();
        for pair in level.chunks(2) {
            let combined = format!("{}{}", pair[0], pair[1]);
            let hash = sha256_hex(combined.as_bytes());
            next_level.push(hash);
        }
        level = next_level;
    }

    level.into_iter().next().unwrap_or_else(|| "0".repeat(64))
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

// Also add double SHA-256 (used for address checksum)
pub fn sha256d_hex(data: &[u8]) -> String {
    let first = Sha256::digest(data);
    let second = Sha256::digest(first);
    hex::encode(second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merkle_empty() {
        let root = merkle_root(&[]);
        assert_eq!(root.len(), 64);
    }

    #[test]
    fn test_merkle_single() {
        let txids = vec!["abc123".to_string()];
        let root = merkle_root(&txids);
        assert_eq!(root.len(), 64);
    }

    #[test]
    fn test_merkle_two() {
        let txids = vec!["tx1".to_string(), "tx2".to_string()];
        let root = merkle_root(&txids);
        assert_eq!(root.len(), 64);
    }

    #[test]
    fn test_merkle_odd() {
        let txids = vec!["tx1".to_string(), "tx2".to_string(), "tx3".to_string()];
        let root = merkle_root(&txids);
        assert_eq!(root.len(), 64);
    }

    #[test]
    fn test_merkle_deterministic() {
        let txids = vec!["tx1".to_string(), "tx2".to_string()];
        let root1 = merkle_root(&txids);
        let root2 = merkle_root(&txids);
        assert_eq!(root1, root2);
    }

    // C-05: document the CVE-2012-2459 "odd-duplication" property so a
    // future refactor of merkle_root is flagged by a failing test if it
    // silently changes the odd-level behaviour. The property is only
    // exploitable if a block with duplicate txids reaches merkle
    // verification — see test_duplicate_txids_rejected_at_block_layer in
    // block.rs for the companion consensus-layer guard.
    #[test]
    fn test_merkle_odd_duplication_property_documented() {
        let three = vec!["tx1".to_string(), "tx2".to_string(), "tx3".to_string()];
        let four_with_duplicate_last = vec![
            "tx1".to_string(),
            "tx2".to_string(),
            "tx3".to_string(),
            "tx3".to_string(),
        ];
        assert_eq!(
            merkle_root(&three),
            merkle_root(&four_with_duplicate_last),
            "merkle_root retains the odd-duplication property; consensus layer \
             (Block::validate_structure) must reject duplicate txids to prevent \
             block-validity bypass (CVE-2012-2459)"
        );
    }
}
