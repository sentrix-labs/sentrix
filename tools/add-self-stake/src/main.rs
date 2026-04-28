// add-self-stake — submit a `StakingOp::AddSelfStake` tx to bond real
// SRX into the sender's own validator self_stake. Designed as the
// supply-invariant-preserving recovery path for slashed validators
// whose self_stake dropped below MIN_SELF_STAKE — see the 2026-04-27
// self-stake-shortfall incident in operator runbooks for context.
//
// The tx must come from the validator's own wallet (apply-side
// dispatch checks `tx.from_address == validator address` via the
// stake_registry lookup). After this tx mines and brings self_stake
// back ≥ MIN_SELF_STAKE, the standard `validator unjail` admin op
// (or future `StakingOp::Unjail` tx-form) clears the jail flag.
//
// Reads raw 64-hex private key from STDIN (no echo, no logs).
//
// Usage:
//   echo "<64-hex-privkey>" | add-self-stake \
//     --rpc           http://localhost:8545 \
//     --chain-id      7119 \
//     --amount-sentri 1500000000
//   # add --dry-run to build + sign without POSTing
//
// Tx shape:
//   from_address = derived from the privkey on stdin (validator's wallet)
//   to_address   = PROTOCOL_TREASURY (0x0000...0002) — staking-op
//                  convention; chain rejects otherwise
//   amount       = --amount-sentri (must equal data.amount; the outer
//                  `accounts.transfer` at top of Pass 2 routes this
//                  into treasury as the escrow move)
//   fee          = MIN_TX_FEE
//   data         = JSON-encoded StakingOp::AddSelfStake { amount }
//   chain_id     = --chain-id arg
//
// Activation: dispatch is gated by `ADD_SELF_STAKE_HEIGHT` — the
// chain rejects pre-fork. Operators activate via halt-all +
// simultaneous-start with the env var set on every validator.

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
    /// RPC base URL (e.g. http://localhost:8545)
    #[arg(long)]
    rpc: String,

    /// Chain id (7119 mainnet, 7120 testnet)
    #[arg(long)]
    chain_id: u64,

    /// Amount to bond into self_stake, in sentri (1 SRX = 100_000_000 sentri)
    #[arg(long)]
    amount_sentri: u64,

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

    println!("validator:     {from_address}");
    println!(
        "amount_sentri: {} ({} SRX)",
        args.amount_sentri,
        args.amount_sentri as f64 / 1e8
    );
    println!("rpc:           {}", args.rpc);
    println!("chain_id:      {}", args.chain_id);
    println!("treasury:      {PROTOCOL_TREASURY}");

    if args.amount_sentri == 0 {
        return Err("amount_sentri must be > 0".into());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    // Fetch nonce + balance.
    let info_url = format!("{}/address/{}/info", args.rpc, from_address);
    let info: serde_json::Value = client.get(&info_url).send().await?.json().await?;
    let nonce = info["nonce"]
        .as_u64()
        .ok_or_else(|| format!("nonce missing from response: {info}"))?;
    let balance = info["balance_sentri"]
        .as_u64()
        .ok_or_else(|| format!("balance_sentri missing from response: {info}"))?;
    println!(
        "nonce:         {nonce}\nbalance:       {balance} sentri ({:.8} SRX)",
        balance as f64 / 1e8
    );
    let need = args.amount_sentri.saturating_add(MIN_TX_FEE);
    if balance < need {
        return Err(format!(
            "insufficient balance: need {need} sentri (amount + fee), have {balance}"
        )
        .into());
    }

    // Build + encode the StakingOp::AddSelfStake payload. tx.amount must
    // equal data.amount (apply enforces).
    let staking_op = StakingOp::AddSelfStake {
        amount: args.amount_sentri,
    };
    let data = staking_op.encode()?;

    // Build + sign the tx.
    let tx = Transaction::new(
        from_address.clone(),
        PROTOCOL_TREASURY.to_string(),
        args.amount_sentri,
        MIN_TX_FEE,
        nonce,
        data,
        args.chain_id,
        &sk,
        &pk,
    )?;
    println!("txid:          {}", tx.txid);
    println!("data:          {}", tx.data);

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
    println!(
        "OK — submitted. Wait 1-2s for inclusion; verify via /staking/validators \
         that self_stake bumped by amount_sentri (NOT total_delegated — \
         AddSelfStake routes to self_stake exclusively, distinguishing it \
         from a regular Delegate from the same wallet)."
    );
    Ok(())
}
