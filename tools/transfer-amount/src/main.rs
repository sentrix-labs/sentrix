// transfer-amount — send a specific SRX amount from one address to another.
// Reads raw 64-hex private key from STDIN (no echo, no logs).
// Sibling to drain-once; same shape, different semantics: --amount-sentri
// instead of --leave-sentri.

use clap::Parser;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use sentrix_primitives::transaction::{MIN_TX_FEE, Transaction};
use sentrix_wallet::Wallet;
use std::io::{self, Read};
use std::time::Duration;

#[derive(Parser)]
struct Args {
    /// RPC base URL (e.g. http://localhost:8545)
    #[arg(long)]
    rpc: String,

    /// Receiver address (0x + 40 hex)
    #[arg(long)]
    receiver: String,

    /// Chain id (7119 mainnet, 7120 testnet)
    #[arg(long)]
    chain_id: u64,

    /// Amount to send in sentri (1 SRX = 100_000_000 sentri)
    #[arg(long)]
    amount_sentri: u64,

    /// Dry-run — build + sign but don't POST
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let mut privkey_hex = String::new();
    io::stdin().read_to_string(&mut privkey_hex)?;
    let privkey_hex = privkey_hex.trim();
    let privkey_hex = privkey_hex.strip_prefix("0x").unwrap_or(privkey_hex);
    if privkey_hex.len() != 64 {
        return Err(format!(
            "expected 64 hex chars on stdin, got {}",
            privkey_hex.len()
        )
        .into());
    }
    let sk_bytes = hex::decode(privkey_hex)?;
    let sk = SecretKey::from_slice(&sk_bytes)?;
    let secp = Secp256k1::new();
    let pk = PublicKey::from_secret_key(&secp, &sk);
    let from_address = Wallet::derive_address(&pk);

    println!("sender:   {from_address}");
    println!("receiver: {}", args.receiver);
    println!("rpc:      {}", args.rpc);
    println!("chain_id: {}", args.chain_id);
    println!(
        "amount:   {} sentri  ({:.8} SRX)",
        args.amount_sentri,
        args.amount_sentri as f64 / 1e8
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;
    let info_url = format!("{}/address/{}/info", args.rpc, from_address);
    let resp: serde_json::Value = client.get(&info_url).send().await?.json().await?;
    let nonce = resp["nonce"]
        .as_u64()
        .ok_or_else(|| format!("nonce missing from response: {resp}"))?;
    let balance = resp["balance_sentri"]
        .as_u64()
        .ok_or_else(|| format!("balance_sentri missing from response: {resp}"))?;
    println!(
        "nonce:    {nonce}\nbalance:  {balance} sentri  ({:.8} SRX)",
        balance as f64 / 1e8
    );

    let required = args
        .amount_sentri
        .checked_add(MIN_TX_FEE)
        .ok_or("amount + fee overflow")?;
    if balance < required {
        return Err(format!(
            "insufficient balance: have {balance} sentri, need {required} (amount + fee)"
        )
        .into());
    }

    let tx = Transaction::new(
        from_address.clone(),
        args.receiver.clone(),
        args.amount_sentri,
        MIN_TX_FEE,
        nonce,
        String::new(),
        args.chain_id,
        &sk,
        &pk,
    )?;
    println!("txid:     {}", tx.txid);

    if args.dry_run {
        println!("DRY RUN — not submitting");
        return Ok(());
    }

    let post_url = format!("{}/transactions", args.rpc);
    let body = serde_json::json!({ "transaction": tx });
    let r = client.post(&post_url).json(&body).send().await?;
    let status = r.status();
    let text = r.text().await?;
    println!("POST status: {status}");
    println!("response: {text}");
    if !status.is_success() {
        return Err(format!("submit failed: HTTP {status}").into());
    }
    println!("OK — submitted. Wait 1-2s for inclusion.");
    Ok(())
}
