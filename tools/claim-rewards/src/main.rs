// claim-rewards — submit a `StakingOp::ClaimRewards` tx to drain a
// validator's accumulated `pending_rewards` from PROTOCOL_TREASURY into
// the validator's balance.
//
// Reads raw 64-hex private key from STDIN (no echo, no logs). The
// derived address must match a registered validator on the chain;
// otherwise the chain rejects the op.
//
// Usage:
//   echo "<64-hex-privkey>" | claim-rewards \
//     --rpc       http://10.20.0.2:8545 \
//     --chain-id  7119
//   # add --dry-run to build + sign without POSTing
//
// Tx shape:
//   from_address = derived from the privkey on stdin (validator addr)
//   to_address   = PROTOCOL_TREASURY (0x0000...0002) — required by
//                  staking-op convention; chain rejects otherwise
//   amount       = 0 (no transfer payload — the op's effect is the
//                  treasury → validator credit done in apply, not the
//                  on-tx amount)
//   fee          = MIN_TX_FEE
//   data         = JSON-encoded StakingOp::ClaimRewards (no fields)
//   chain_id     = --chain-id arg

use clap::Parser;
use secp256k1::{PublicKey, Secp256k1, SecretKey};
use sentrix_primitives::transaction::{
    MIN_TX_FEE, PROTOCOL_TREASURY, StakingOp, Transaction,
};
use sentrix_wallet::Wallet;
use std::io::{self, Read};
use std::time::Duration;

#[derive(Parser)]
struct Args {
    /// RPC base URL (e.g. http://10.20.0.2:8545)
    #[arg(long)]
    rpc: String,

    /// Chain id (7119 mainnet, 7120 testnet)
    #[arg(long)]
    chain_id: u64,

    /// Dry-run — build + sign but don't POST
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Read raw 64-hex privkey from stdin (no echo).
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

    println!("validator: {from_address}");
    println!("rpc:       {}", args.rpc);
    println!("chain_id:  {}", args.chain_id);
    println!("treasury:  {PROTOCOL_TREASURY}");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    // Fetch this validator's pending_rewards from /staking/validators
    let staking_url = format!("{}/staking/validators", args.rpc);
    let resp: serde_json::Value = client.get(&staking_url).send().await?.json().await?;
    let validators = resp["validators"]
        .as_array()
        .ok_or_else(|| format!("/staking/validators didn't return validators array: {resp}"))?;
    let me = validators
        .iter()
        .find(|v| v["address"].as_str() == Some(from_address.as_str()))
        .ok_or_else(|| {
            format!(
                "validator {} not found in active set — is it registered?",
                from_address
            )
        })?;
    let pending_sentri = me["pending_rewards"]
        .as_u64()
        .ok_or_else(|| format!("pending_rewards missing from validator: {me}"))?;
    let pending_srx = pending_sentri as f64 / 1e8;
    println!("pending:   {pending_sentri} sentri ({pending_srx:.8} SRX)");

    if pending_sentri == 0 {
        println!("nothing to claim — pending_rewards = 0");
        return Ok(());
    }

    // Fetch nonce + sanity-check balance >= MIN_TX_FEE
    let info_url = format!("{}/address/{}/info", args.rpc, from_address);
    let info: serde_json::Value = client.get(&info_url).send().await?.json().await?;
    let nonce = info["nonce"]
        .as_u64()
        .ok_or_else(|| format!("nonce missing from response: {info}"))?;
    let balance = info["balance_sentri"]
        .as_u64()
        .ok_or_else(|| format!("balance_sentri missing from response: {info}"))?;
    println!(
        "nonce:     {nonce}\nbalance:   {balance} sentri ({:.8} SRX)",
        balance as f64 / 1e8
    );
    if balance < MIN_TX_FEE {
        return Err(format!(
            "insufficient balance for MIN_TX_FEE={MIN_TX_FEE}: have {balance} sentri"
        )
        .into());
    }

    // Build + encode the StakingOp::ClaimRewards payload.
    let staking_op = StakingOp::ClaimRewards;
    let data = staking_op.encode()?;

    // Build + sign the tx.
    let tx = Transaction::new(
        from_address.clone(),
        PROTOCOL_TREASURY.to_string(),
        0, // amount = 0; the claim transfers via apply, not via tx.amount
        MIN_TX_FEE,
        nonce,
        data,
        args.chain_id,
        &sk,
        &pk,
    )?;
    println!("txid:      {}", tx.txid);
    println!("data:      {}", tx.data);

    if args.dry_run {
        println!("DRY RUN — not submitting");
        return Ok(());
    }

    // Submit.
    let post_url = format!("{}/transactions", args.rpc);
    let body = serde_json::json!({ "transaction": tx });
    let r = client.post(&post_url).json(&body).send().await?;
    let status = r.status();
    let text = r.text().await?;
    println!("POST status: {status}");
    println!("response:    {text}");
    if !status.is_success() {
        return Err(format!("submit failed: HTTP {status}").into());
    }
    println!("OK — submitted. Wait 1-2s for inclusion + verify via /staking/validators that pending_rewards reset to 0.");
    Ok(())
}
