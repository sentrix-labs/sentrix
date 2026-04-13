// merkle.rs - Sentrix

use sha2::{Sha256, Digest};

pub fn merkle_root(txids: &[String]) -> String {
    if txids.is_empty() {
        return "0".repeat(64);
    }

    if txids.len() == 1 {
        return sha256_hex(txids[0].as_bytes());
    }

    let mut level: Vec<String> = txids.to_vec();

    while level.len() > 1 {
        // Duplicate last element if odd count
        if !level.len().is_multiple_of(2)
            && let Some(last) = level.last().cloned() {
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
}
