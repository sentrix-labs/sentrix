//! state-diff — compare trie roots + block hashes across two chain.db
//! dirs to investigate state_root v2 drift halts.
//!
//! Usage:
//!   state-diff <path-A> <path-B> [--from <height>] [--to <height>]
//!
//! Prints, for the height range:
//!   - block hash from A vs B (≠ → received different blocks)
//!   - trie root from A vs B (≠ → diverged on state apply for the same block)
//!
//! For state_root v2 drift halts, the typical signature is:
//!   block_hash == block_hash but trie_root differs at h=N
//!     ↘ both validators applied the SAME block but produced different
//!       post-state. Look at the block at h=N for non-deterministic apply.

use anyhow::{Context, Result, anyhow};
use sentrix_storage::MdbxStorage;
use sentrix_storage::tables::{TABLE_BLOCKS, TABLE_TRIE_ROOTS};
use std::path::Path;

fn print_usage() {
    eprintln!(
        "Usage: state-diff <path-A> <path-B> [--from <height>] [--to <height>]\n\
         \n\
         Compares trie roots + block hashes between two chain.db dirs head-to-head.\n\
         Hex-prints divergent rows so the operator can spot the height where the\n\
         state diverged.\n"
    );
}

fn parse_args() -> Result<(String, String, Option<u64>, Option<u64>)> {
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() < 3 {
        print_usage();
        return Err(anyhow!("missing path-A and/or path-B"));
    }
    let path_a = argv[1].clone();
    let path_b = argv[2].clone();
    let mut from: Option<u64> = None;
    let mut to: Option<u64> = None;
    let mut i = 3;
    while i < argv.len() {
        match argv[i].as_str() {
            "--from" => {
                from = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .or_else(|| {
                        eprintln!("--from requires a u64");
                        None
                    });
                i += 2;
            }
            "--to" => {
                to = argv
                    .get(i + 1)
                    .and_then(|s| s.parse().ok())
                    .or_else(|| {
                        eprintln!("--to requires a u64");
                        None
                    });
                i += 2;
            }
            unknown => {
                eprintln!("unknown arg: {unknown}");
                print_usage();
                return Err(anyhow!("unknown arg"));
            }
        }
    }
    Ok((path_a, path_b, from, to))
}

fn read_trie_roots(db: &MdbxStorage) -> Result<Vec<(u64, Vec<u8>)>> {
    let entries = db
        .iter(TABLE_TRIE_ROOTS)
        .context("iter TABLE_TRIE_ROOTS")?;
    let mut out = Vec::with_capacity(entries.len());
    for (k, v) in entries {
        if k.len() == 8 {
            let mut h = [0u8; 8];
            h.copy_from_slice(&k);
            out.push((u64::from_be_bytes(h), v));
        }
    }
    out.sort_by_key(|(h, _)| *h);
    Ok(out)
}

fn read_block_hashes(db: &MdbxStorage) -> Result<std::collections::BTreeMap<u64, String>> {
    use sentrix_primitives::block::Block;
    let entries = db.iter(TABLE_BLOCKS).context("iter TABLE_BLOCKS")?;
    let mut out = std::collections::BTreeMap::new();
    for (k, v) in entries {
        if k.len() != 8 {
            continue;
        }
        let mut h = [0u8; 8];
        h.copy_from_slice(&k);
        let height = u64::from_be_bytes(h);
        // Block is bincode-encoded; decode the hash field only via a partial
        // deserialize would be ideal, but bincode doesn't support that.
        // Decode the whole block — small allocation, runs once at tool startup.
        if let Ok(block) = bincode::deserialize::<Block>(&v) {
            out.insert(height, block.hash);
        }
    }
    Ok(out)
}

fn main() -> Result<()> {
    // Inspector — disk-encryption gate not relevant for read-only forensic work.
    // SAFETY: tool is single-threaded; setting env once at startup.
    unsafe {
        std::env::set_var("SENTRIX_ALLOW_UNENCRYPTED_DISK", "true");
    }

    let (path_a, path_b, from, to) = parse_args()?;

    let db_a = MdbxStorage::open(Path::new(&path_a))
        .with_context(|| format!("open {path_a}"))?;
    let db_b = MdbxStorage::open(Path::new(&path_b))
        .with_context(|| format!("open {path_b}"))?;

    let roots_a = read_trie_roots(&db_a)?;
    let roots_b = read_trie_roots(&db_b)?;
    let blocks_a = read_block_hashes(&db_a)?;
    let blocks_b = read_block_hashes(&db_b)?;

    let max_height_a = roots_a.last().map(|(h, _)| *h).unwrap_or(0);
    let max_height_b = roots_b.last().map(|(h, _)| *h).unwrap_or(0);

    println!("# state-diff");
    println!("# A = {path_a}  (max trie_root height: {max_height_a})");
    println!("# B = {path_b}  (max trie_root height: {max_height_b})");
    println!("# trie_roots: A={} B={}", roots_a.len(), roots_b.len());
    println!("# blocks:     A={} B={}", blocks_a.len(), blocks_b.len());

    let from = from.unwrap_or_else(|| {
        // Default: last 50 heights either has — useful for halt-time
        // inspection where the divergence is recent.
        let pivot = max_height_a.min(max_height_b);
        pivot.saturating_sub(50)
    });
    let to = to.unwrap_or_else(|| max_height_a.max(max_height_b));

    let map_a: std::collections::BTreeMap<u64, Vec<u8>> =
        roots_a.into_iter().collect();
    let map_b: std::collections::BTreeMap<u64, Vec<u8>> =
        roots_b.into_iter().collect();

    println!();
    println!("height                 |  trie_root_diff |  block_hash_diff |  A_root              | B_root              | A_block_hash        | B_block_hash");
    println!("{:-<200}", "");
    let mut diff_count = 0usize;
    for h in from..=to {
        let ra = map_a.get(&h);
        let rb = map_b.get(&h);
        let ba = blocks_a.get(&h);
        let bb = blocks_b.get(&h);
        let trie_diff = match (ra, rb) {
            (Some(a), Some(b)) => a != b,
            (None, None) => false, // neither has it — beyond range, skip
            _ => true,             // one has, the other doesn't — diverged
        };
        let block_diff = match (ba, bb) {
            (Some(a), Some(b)) => a != b,
            (None, None) => false,
            _ => true,
        };
        if !trie_diff && !block_diff {
            continue;
        }
        diff_count += 1;
        let short = |bytes: Option<&Vec<u8>>| -> String {
            bytes
                .map(|v| {
                    let h = hex::encode(v);
                    if h.len() > 16 {
                        format!("{}…", &h[..16])
                    } else {
                        h
                    }
                })
                .unwrap_or_else(|| "—".to_string())
        };
        let short_str = |s: Option<&String>| -> String {
            s.map(|h| {
                if h.len() > 16 {
                    format!("{}…", &h[..16])
                } else {
                    h.clone()
                }
            })
            .unwrap_or_else(|| "—".to_string())
        };
        println!(
            "{:>10}             |  {:>13} |  {:>15} |  {:18}  | {:18}  | {:18}  | {:18}",
            h,
            if trie_diff { "≠" } else { "=" },
            if block_diff { "≠" } else { "=" },
            short(ra),
            short(rb),
            short_str(ba),
            short_str(bb),
        );
    }
    println!();
    println!("# divergent rows: {diff_count} (heights {from}..={to})");
    if diff_count == 0 {
        println!("# (no divergence in range — either both DBs agree, or pick a wider range)");
    }
    Ok(())
}
