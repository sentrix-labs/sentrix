// main.rs - Sentrix Chain CLI entry point

use clap::{Parser, Subcommand};
use std::sync::Arc;
use tokio::sync::RwLock;
use sentrix::core::blockchain::Blockchain;
use sentrix::wallet::wallet::Wallet;
use sentrix::wallet::keystore::Keystore;
use sentrix::storage::db::Storage;
use sentrix::api::routes::{create_router, SharedState};
use sentrix::network::node::DEFAULT_PORT;

const API_PORT: u16 = 8545;

fn get_data_dir() -> std::path::PathBuf {
    let exe_path = std::env::current_exe().unwrap_or_default();
    let exe_dir = exe_path.parent().unwrap_or(std::path::Path::new("."));
    exe_dir.join("data")
}

fn get_db_path() -> String {
    get_data_dir().join("chain.db").to_str().unwrap().to_string()
}

fn get_wallets_dir() -> String {
    get_data_dir().join("wallets").to_str().unwrap().to_string()
}

#[derive(Parser)]
#[command(name = "sentrix")]
#[command(about = "Sentrix Chain (SRX) — Layer-1 PoA Blockchain")]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new chain
    Init {
        /// Admin address (controls validator set)
        #[arg(long)]
        admin: String,
    },
    /// Wallet commands
    Wallet {
        #[command(subcommand)]
        action: WalletCommands,
    },
    /// Validator management
    Validator {
        #[command(subcommand)]
        action: ValidatorCommands,
    },
    /// Start the node (P2P + API + validator loop)
    Start {
        /// Validator private key hex (optional — node runs in relay mode if not set)
        #[arg(long)]
        validator_key: Option<String>,
        /// P2P port
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Bootstrap peers (comma-separated host:port)
        #[arg(long, default_value = "")]
        peers: String,
    },
    /// Chain information
    Chain {
        #[command(subcommand)]
        action: ChainCommands,
    },
    /// Check account balance
    Balance {
        address: String,
    },
    /// Generate all genesis wallets
    GenesisWallets,
}

#[derive(Subcommand)]
enum WalletCommands {
    /// Generate a new wallet
    Generate {
        #[arg(long)]
        password: Option<String>,
    },
    /// Import wallet from private key
    Import {
        private_key: String,
        #[arg(long)]
        password: Option<String>,
    },
    /// Show wallet info from keystore file
    Info {
        keystore_file: String,
    },
}

#[derive(Subcommand)]
enum ValidatorCommands {
    /// Add a validator (admin only)
    Add {
        address: String,
        name: String,
        public_key: String,
        #[arg(long)]
        admin_key: String,
    },
    /// List all validators
    List,
}

#[derive(Subcommand)]
enum ChainCommands {
    /// Show chain statistics
    Info,
    /// Validate chain integrity
    Validate,
    /// Show block details
    Block { index: u64 },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    std::fs::create_dir_all(get_data_dir())?;
    std::fs::create_dir_all(get_wallets_dir())?;

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { admin } => cmd_init(&admin)?,

        Commands::Wallet { action } => match action {
            WalletCommands::Generate { password } => cmd_wallet_generate(password)?,
            WalletCommands::Import { private_key, password } => cmd_wallet_import(&private_key, password)?,
            WalletCommands::Info { keystore_file } => cmd_wallet_info(&keystore_file)?,
        },

        Commands::Validator { action } => match action {
            ValidatorCommands::Add { address, name, public_key, admin_key } => {
                cmd_validator_add(&address, &name, &public_key, &admin_key)?;
            }
            ValidatorCommands::List => cmd_validator_list()?,
        },

        Commands::Start { validator_key, port, peers } => {
            cmd_start(validator_key, port, peers).await?;
        }

        Commands::Chain { action } => match action {
            ChainCommands::Info => cmd_chain_info()?,
            ChainCommands::Validate => cmd_chain_validate()?,
            ChainCommands::Block { index } => cmd_chain_block(index)?,
        },

        Commands::Balance { address } => cmd_balance(&address)?,

        Commands::GenesisWallets => cmd_genesis_wallets()?,
    }

    Ok(())
}

// ── Command implementations ──────────────────────────────

fn cmd_init(admin: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    if storage.has_blockchain() {
        println!("Chain already initialized.");
        return Ok(());
    }
    let bc = Blockchain::new(admin.to_string());
    storage.save_blockchain(&bc)?;
    println!("Chain initialized.");
    println!("Admin address: {}", admin);
    println!("Genesis block created. Height: 0");
    println!("Total premine: 63,000,000 SRX");
    Ok(())
}

fn cmd_wallet_generate(password: Option<String>) -> anyhow::Result<()> {
    let wallet = Wallet::generate();
    println!("\nNew wallet generated:");
    println!("  Address:     {}", wallet.address);
    println!("  Public key:  {}", wallet.public_key);

    if let Some(pwd) = password {
        let keystore = Keystore::encrypt(&wallet, &pwd)?;
        let filename = format!("{}/{}.json", get_wallets_dir(), &wallet.address[2..10]);
        keystore.save(&filename)?;
        println!("  Keystore:    {}", filename);
        println!("\nWARNING: Back up your keystore file and password securely.");
    } else {
        println!("  Private key: {}", wallet.secret_key_hex);
        println!("\nWARNING: Save your private key securely. It will not be shown again.");
    }
    Ok(())
}

fn cmd_wallet_import(private_key: &str, password: Option<String>) -> anyhow::Result<()> {
    let wallet = Wallet::from_private_key(private_key)?;
    println!("Wallet imported:");
    println!("  Address:    {}", wallet.address);
    println!("  Public key: {}", wallet.public_key);

    if let Some(pwd) = password {
        let keystore = Keystore::encrypt(&wallet, &pwd)?;
        let filename = format!("{}/{}.json", get_wallets_dir(), &wallet.address[2..10]);
        keystore.save(&filename)?;
        println!("  Saved to:   {}", filename);
    }
    Ok(())
}

fn cmd_wallet_info(keystore_file: &str) -> anyhow::Result<()> {
    let keystore = Keystore::load(keystore_file)?;
    println!("Keystore info:");
    println!("  Address: {}", keystore.address);
    println!("  Cipher:  {}", keystore.crypto.cipher);
    println!("  KDF:     {} ({} iterations)", keystore.crypto.kdf, keystore.crypto.kdf_iterations);
    Ok(())
}

fn cmd_validator_add(address: &str, name: &str, public_key: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized. Run: sentrix init"))?;

    let admin_wallet = Wallet::from_private_key(admin_key)?;
    bc.authority.add_validator(
        &admin_wallet.address,
        address.to_string(),
        name.to_string(),
        public_key.to_string(),
    )?;

    storage.save_blockchain(&bc)?;
    println!("Validator added: {} ({})", name, address);
    Ok(())
}

fn cmd_validator_list() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    println!("Validators ({} total, {} active):",
        bc.authority.validator_count(), bc.authority.active_count());
    for v in bc.authority.active_validators() {
        println!("  [{}] {} — {} blocks produced",
            if v.is_active { "ACTIVE" } else { "INACTIVE" },
            v.name, v.blocks_produced);
        println!("      Address: {}", v.address);
    }
    Ok(())
}

async fn cmd_start(
    validator_key: Option<String>,
    _port: u16,
    peers_str: String,
) -> anyhow::Result<()> {
    let storage = Arc::new(Storage::open(&get_db_path())?);
    let bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized. Run: sentrix init"))?;

    let shared: SharedState = Arc::new(RwLock::new(bc));

    // Start REST API
    let app = create_router(shared.clone());
    let api_addr = format!("0.0.0.0:{}", API_PORT);
    println!("REST API listening on http://{}", api_addr);

    let listener = tokio::net::TcpListener::bind(&api_addr).await?;

    // Validator loop (if validator key provided)
    if let Some(key_hex) = validator_key {
        let wallet = Wallet::from_private_key(&key_hex)?;
        println!("Validator mode: {}", wallet.address);

        let shared_clone = shared.clone();
        let storage_clone = storage.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                let mut bc = shared_clone.write().await;
                match bc.create_block(&wallet.address) {
                    Ok(block) => {
                        let height = block.index;
                        match bc.add_block(block) {
                            Ok(()) => {
                                println!("Block {} produced by {}", height, wallet.address);
                                let _ = storage_clone.save_blockchain(&bc);
                                let _ = storage_clone.save_height(height);
                            }
                            Err(e) => tracing::warn!("add_block failed: {}", e),
                        }
                    }
                    Err(_) => {} // Not our turn — silent
                }
            }
        });
    }

    // Connect to bootstrap peers
    if !peers_str.is_empty() {
        for peer in peers_str.split(',') {
            println!("Bootstrap peer: {}", peer.trim());
        }
    }

    println!("Node started. Press Ctrl+C to stop.");
    axum::serve(listener, app).await?;
    Ok(())
}

fn cmd_chain_info() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let stats = bc.chain_stats();
    println!("{}", serde_json::to_string_pretty(&stats)?);
    Ok(())
}

fn cmd_chain_validate() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let valid = bc.is_valid_chain();
    println!("Chain valid: {}", valid);
    println!("Height: {}", bc.height());
    println!("Total blocks: {}", bc.chain.len());
    Ok(())
}

fn cmd_chain_block(index: u64) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    match bc.get_block(index) {
        Some(block) => println!("{}", serde_json::to_string_pretty(block)?),
        None => println!("Block {} not found", index),
    }
    Ok(())
}

fn cmd_balance(address: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let balance = bc.accounts.get_balance(address);
    println!("Address: {}", address);
    println!("Balance: {} sentri ({} SRX)", balance, balance as f64 / 100_000_000.0);
    Ok(())
}

fn cmd_genesis_wallets() -> anyhow::Result<()> {
    println!("Generating 7 genesis wallets for Sentrix Chain...\n");

    let roles = [
        "founder",
        "ecosystem_fund",
        "early_validator",
        "reserve",
        "genesis_node_1",
        "genesis_node_2",
        "genesis_node_3",
    ];

    let mut wallets_json = serde_json::json!({});

    for role in &roles {
        let wallet = Wallet::generate();
        println!("[{}]", role.to_uppercase());
        println!("  Address:     {}", wallet.address);
        println!("  Public key:  {}", wallet.public_key);
        println!("  Private key: {}", wallet.secret_key_hex);
        println!();

        wallets_json[*role] = serde_json::json!({
            "address": wallet.address,
            "public_key": wallet.public_key,
            "private_key": wallet.secret_key_hex,
        });
    }

    // Save to file
    let output_path = format!("{}/genesis_wallets.json", get_wallets_dir());
    std::fs::write(&output_path, serde_json::to_string_pretty(&wallets_json)?)?;
    println!("Saved to: {}", output_path);
    println!("\nCRITICAL: Back up genesis_wallets.json offline immediately.");
    println!("          Delete from this machine after backup.");
    Ok(())
}
