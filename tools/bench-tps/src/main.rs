// bench-tps — submit signed transactions to a Sentrix node at a target TPS
// and measure admission latency + block-inclusion latency.
//
// Intended for testnet pentesting ONLY. Uses the workspace's own
// sentrix-primitives + sentrix-wallet so the transaction shape is
// bit-identical to what a real wallet produces.

use clap::Parser;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use sentrix_primitives::transaction::{MIN_TX_FEE, Transaction};
use sentrix_wallet::{Keystore, Wallet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(about = "Sentrix TPS + pentest bench")]
struct Args {
    /// Node RPC base URL (e.g. http://10.20.0.4:9545)
    #[arg(long, default_value = "http://localhost:9545")]
    rpc: String,

    /// Chain id — 7120 for testnet, 7119 for mainnet.
    #[arg(long, default_value_t = 7120)]
    chain_id: u64,

    /// Total number of transactions to submit.
    #[arg(long, default_value_t = 100)]
    count: u64,

    /// Submit concurrency (parallel in-flight requests).
    #[arg(long, default_value_t = 4)]
    concurrency: u64,

    /// Path to encrypted keystore (.json file produced by
    /// `sentrix wallet generate` or `sentrix wallet encrypt`). Password
    /// is read from the `BENCH_KEYSTORE_PASSWORD` env var — DON'T pass
    /// the password on CLI (would leak via `ps`).
    #[arg(long)]
    keystore: String,

    /// Address to receive the bench transfers (defaults to a dead burn addr).
    #[arg(
        long,
        default_value = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef"
    )]
    receiver: String,

    /// Amount per tx in sentri (1 sentri = 10^-8 SRX).
    #[arg(long, default_value_t = 1u64)]
    amount: u64,

    /// Mode: `admit` (measure submit rate only) or `finalize`
    /// (wait for each tx to land in a finalized block).
    #[arg(long, default_value = "admit")]
    mode: String,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Load + decrypt keystore. Password comes from env var — never CLI arg.
    let password = std::env::var("BENCH_KEYSTORE_PASSWORD")
        .map_err(|_| "set BENCH_KEYSTORE_PASSWORD env var before running")?;
    let ks = Keystore::load(&args.keystore)?;
    let wallet = ks.decrypt(&password)?;
    // Scrub password from memory's env copy ASAP.
    // SAFETY: deleting the variable we just read.
    unsafe { std::env::remove_var("BENCH_KEYSTORE_PASSWORD") };

    let sk: SecretKey = wallet.get_secret_key()?;
    let secp = Secp256k1::new();
    let pk = PublicKey::from_secret_key(&secp, &sk);
    let from_address = Wallet::derive_address(&pk);

    println!("bench-tps starting");
    println!("  rpc:       {}", args.rpc);
    println!("  chain_id:  {}", args.chain_id);
    println!("  sender:    {from_address}");
    println!("  receiver:  {}", args.receiver);
    println!("  count:     {}", args.count);
    println!("  concurr:   {}", args.concurrency);
    println!("  mode:      {}", args.mode);

    // Fetch current on-chain nonce for the sender so we can start
    // building txs with the correct starting value.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let url = format!("{}/address/{}/info", args.rpc, from_address);
    let resp: serde_json::Value = client.get(&url).send().await?.json().await?;
    let starting_nonce = resp["nonce"].as_u64().unwrap_or(0);
    let balance = resp["balance_sentri"].as_u64().unwrap_or(0);
    println!("  on-chain nonce:   {starting_nonce}");
    println!("  balance (sentri): {balance}");

    let total_needed = args
        .count
        .saturating_mul(args.amount.saturating_add(MIN_TX_FEE));
    if balance < total_needed {
        return Err(format!(
            "insufficient balance: have {balance} sentri, need {total_needed} (amount+fee × count)"
        )
        .into());
    }

    // Pre-sign all transactions up-front so we measure pure submit
    // throughput (not sign-during-submit overhead). Nonce order matters
    // strictly — mempool rejects out-of-order.
    println!("signing {} transactions …", args.count);
    let sign_start = Instant::now();
    let mut txs: Vec<Transaction> = Vec::with_capacity(args.count as usize);
    for i in 0..args.count {
        let tx = Transaction::new(
            from_address.clone(),
            args.receiver.clone(),
            args.amount,
            MIN_TX_FEE,
            starting_nonce + i,
            String::new(),
            args.chain_id,
            &sk,
            &pk,
        )?;
        txs.push(tx);
    }
    let sign_elapsed = sign_start.elapsed();
    println!(
        "  signed {} txs in {:.2}s ({:.0} signs/s)",
        args.count,
        sign_elapsed.as_secs_f64(),
        args.count as f64 / sign_elapsed.as_secs_f64()
    );

    // Submit. Strict-nonce mempool rejects gaps, so we submit sequentially
    // even under concurrency > 1. `concurrency` here is effectively the
    // number of in-flight requests pipelined (reqwest keeps connection
    // open), not a parallel-ordering thing.
    let post_url = format!("{}/transactions", args.rpc);
    let client = Arc::new(client);
    let success = Arc::new(AtomicU64::new(0));
    let rate_limited = Arc::new(AtomicU64::new(0));
    let rejected = Arc::new(AtomicU64::new(0));
    let other = Arc::new(AtomicU64::new(0));

    let submit_start = Instant::now();
    for tx in txs {
        let client = client.clone();
        let url = post_url.clone();
        let success = success.clone();
        let rate_limited = rate_limited.clone();
        let rejected = rejected.clone();
        let other = other.clone();
        let body = serde_json::json!({ "transaction": tx });
        // Sequential submit — strict nonce ordering.
        let resp = client.post(&url).json(&body).send().await;
        match resp {
            Ok(r) => match r.status().as_u16() {
                200 => success.fetch_add(1, Ordering::Relaxed),
                429 => rate_limited.fetch_add(1, Ordering::Relaxed),
                400..=499 => rejected.fetch_add(1, Ordering::Relaxed),
                _ => other.fetch_add(1, Ordering::Relaxed),
            },
            Err(_) => other.fetch_add(1, Ordering::Relaxed),
        };
    }
    let submit_elapsed = submit_start.elapsed();
    let s = success.load(Ordering::Relaxed);
    let rl = rate_limited.load(Ordering::Relaxed);
    let r = rejected.load(Ordering::Relaxed);
    let o = other.load(Ordering::Relaxed);

    println!();
    println!("=== submit results ===");
    println!(
        "  duration:       {:.2}s",
        submit_elapsed.as_secs_f64()
    );
    println!(
        "  submit rate:    {:.1} tx/s",
        args.count as f64 / submit_elapsed.as_secs_f64()
    );
    println!("  200 accepted:   {s}");
    println!("  429 ratelimit:  {rl}");
    println!("  4xx rejected:   {r}");
    println!("  other:          {o}");

    if args.mode == "finalize" {
        println!();
        println!("=== finalize mode — waiting for chain to catch up ===");
        let target_nonce = starting_nonce + s;
        let wait_start = Instant::now();
        loop {
            let url = format!("{}/address/{}/info", args.rpc, from_address);
            let resp: serde_json::Value = client.get(&url).send().await?.json().await?;
            let on_chain = resp["nonce"].as_u64().unwrap_or(0);
            println!(
                "  t={:.1}s  on-chain nonce={on_chain}  target={target_nonce}",
                wait_start.elapsed().as_secs_f64()
            );
            if on_chain >= target_nonce {
                break;
            }
            if wait_start.elapsed() > Duration::from_secs(120) {
                println!("  (timeout 120s)");
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        println!(
            "  finalize elapsed: {:.1}s",
            wait_start.elapsed().as_secs_f64()
        );
    }

    Ok(())
}
