// main.rs - Sentrix CLI entry point

use clap::{Parser, Subcommand};
use std::sync::Arc;
use tokio::sync::RwLock;
use sentrix::core::blockchain::Blockchain;
use sentrix::core::transaction::{Transaction, TokenOp, TOKEN_OP_ADDRESS};
use sentrix::wallet::wallet::Wallet;
use sentrix::wallet::keystore::Keystore;
use sentrix::storage::db::Storage;
use sentrix::api::routes::{create_router, SharedState};
use sentrix::network::node::{DEFAULT_PORT, Node, NodeEvent};
use sentrix::network::sync::ChainSync;

const DEFAULT_API_PORT: u16 = 8545;

fn get_api_port() -> u16 {
    std::env::var("SENTRIX_API_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_API_PORT)
}

fn get_data_dir() -> std::path::PathBuf {
    // Check SENTRIX_DATA_DIR env var first (Docker / custom deploy)
    if let Ok(dir) = std::env::var("SENTRIX_DATA_DIR") {
        return std::path::PathBuf::from(dir);
    }
    // Default: relative to binary location
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
#[command(about = "Sentrix (SRX) — Layer-1 PoA Blockchain")]
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
    /// Transaction history for an address
    History {
        address: String,
    },
    /// Token operations (SRX-20)
    Token {
        #[command(subcommand)]
        action: TokenCommands,
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
        /// Admin private key (prefer SENTRIX_ADMIN_KEY env var)
        #[arg(long)]
        admin_key: Option<String>,
    },
    /// Remove a validator (admin only)
    Remove {
        address: String,
        /// Admin private key (prefer SENTRIX_ADMIN_KEY env var)
        #[arg(long)]
        admin_key: Option<String>,
    },
    /// Toggle validator active/inactive (admin only)
    Toggle {
        address: String,
        /// Admin private key (prefer SENTRIX_ADMIN_KEY env var)
        #[arg(long)]
        admin_key: Option<String>,
    },
    /// Rename a validator without resetting blocks_produced (admin only)
    Rename {
        address: String,
        new_name: String,
        /// Admin private key (prefer SENTRIX_ADMIN_KEY env var)
        #[arg(long)]
        admin_key: Option<String>,
    },
    /// List all validators
    List,
}

#[derive(Subcommand)]
enum TokenCommands {
    /// Deploy a new SRX-20 token
    Deploy {
        #[arg(long)] name: String,
        #[arg(long)] symbol: String,
        #[arg(long, default_value_t = 18)] decimals: u8,
        #[arg(long)] supply: u64,
        /// Deployer private key (prefer SENTRIX_DEPLOYER_KEY env var)
        #[arg(long)] deployer_key: Option<String>,
        #[arg(long, default_value_t = 100_000)] fee: u64,
    },
    /// Transfer tokens
    Transfer {
        #[arg(long)] contract: String,
        #[arg(long)] to: String,
        #[arg(long)] amount: u64,
        /// Sender private key (prefer SENTRIX_FROM_KEY env var)
        #[arg(long)] from_key: Option<String>,
        #[arg(long, default_value_t = 10_000)] gas: u64,
    },
    /// Burn tokens (remove from circulation)
    Burn {
        #[arg(long)] contract: String,
        #[arg(long)] amount: u64,
        /// Sender private key (prefer SENTRIX_FROM_KEY env var)
        #[arg(long)] from_key: Option<String>,
        #[arg(long, default_value_t = 10_000)] gas: u64,
    },
    /// Check token balance
    Balance {
        #[arg(long)] contract: String,
        #[arg(long)] address: String,
    },
    /// Show token info
    Info {
        #[arg(long)] contract: String,
    },
    /// List all deployed tokens
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
                let key = resolve_key(admin_key, "SENTRIX_ADMIN_KEY", "admin key")?;
                cmd_validator_add(&address, &name, &public_key, &key)?;
            }
            ValidatorCommands::Remove { address, admin_key } => {
                let key = resolve_key(admin_key, "SENTRIX_ADMIN_KEY", "admin key")?;
                cmd_validator_remove(&address, &key)?;
            }
            ValidatorCommands::Toggle { address, admin_key } => {
                let key = resolve_key(admin_key, "SENTRIX_ADMIN_KEY", "admin key")?;
                cmd_validator_toggle(&address, &key)?;
            }
            ValidatorCommands::Rename { address, new_name, admin_key } => {
                let key = resolve_key(admin_key, "SENTRIX_ADMIN_KEY", "admin key")?;
                cmd_validator_rename(&address, &new_name, &key)?;
            }
            ValidatorCommands::List => cmd_validator_list()?,
        },

        Commands::Start { validator_key, port, peers } => {
            // H-04: validator_key can also come from env var
            let resolved_key = validator_key.or_else(|| std::env::var("SENTRIX_VALIDATOR_KEY").ok());
            cmd_start(resolved_key, port, peers).await?;
        }

        Commands::Chain { action } => match action {
            ChainCommands::Info => cmd_chain_info()?,
            ChainCommands::Validate => cmd_chain_validate()?,
            ChainCommands::Block { index } => cmd_chain_block(index)?,
        },

        Commands::Token { action } => match action {
            TokenCommands::Deploy { name, symbol, decimals, supply, deployer_key, fee } => {
                let key = resolve_key(deployer_key, "SENTRIX_DEPLOYER_KEY", "deployer key")?;
                cmd_token_deploy(&name, &symbol, decimals, supply, &key, fee)?;
            }
            TokenCommands::Transfer { contract, to, amount, from_key, gas } => {
                let key = resolve_key(from_key, "SENTRIX_FROM_KEY", "from key")?;
                cmd_token_transfer(&contract, &to, amount, &key, gas)?;
            }
            TokenCommands::Burn { contract, amount, from_key, gas } => {
                let key = resolve_key(from_key, "SENTRIX_FROM_KEY", "from key")?;
                cmd_token_burn(&contract, amount, &key, gas)?;
            }
            TokenCommands::Balance { contract, address } => {
                cmd_token_balance(&contract, &address)?;
            }
            TokenCommands::Info { contract } => {
                cmd_token_info(&contract)?;
            }
            TokenCommands::List => cmd_token_list()?,
        },

        Commands::Balance { address } => cmd_balance(&address)?,

        Commands::History { address } => cmd_history(&address)?,

        Commands::GenesisWallets => cmd_genesis_wallets()?,
    }

    Ok(())
}

// H-04 FIX: Resolve private key from CLI arg or env var, warn if CLI
fn resolve_key(cli_arg: Option<String>, env_var: &str, label: &str) -> anyhow::Result<String> {
    if let Some(ref key) = cli_arg {
        eprintln!("WARNING: passing {} as CLI argument is insecure. Prefer {} env var.", label, env_var);
        return Ok(key.clone());
    }
    std::env::var(env_var)
        .map_err(|_| anyhow::anyhow!("{} required. Use --{} or set {} env var", label, label.replace(' ', "-"), env_var))
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
        println!("  Private key: {}", wallet.secret_key_hex());
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

fn cmd_validator_remove(address: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let admin_wallet = Wallet::from_private_key(admin_key)?;
    bc.authority.remove_validator(&admin_wallet.address, address)?;
    storage.save_blockchain(&bc)?;
    println!("Validator removed: {}", address);
    Ok(())
}

fn cmd_validator_toggle(address: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let admin_wallet = Wallet::from_private_key(admin_key)?;
    let is_active = bc.authority.toggle_validator(&admin_wallet.address, address)?;
    storage.save_blockchain(&bc)?;
    let status = if is_active { "ACTIVE" } else { "INACTIVE" };
    println!("Validator {} toggled to: {}", address, status);
    Ok(())
}

fn cmd_validator_rename(address: &str, new_name: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let admin_wallet = Wallet::from_private_key(admin_key)?;
    bc.authority.rename_validator(&admin_wallet.address, address, new_name.to_string())?;
    storage.save_blockchain(&bc)?;
    println!("Validator {} renamed to: {}", address, new_name);
    Ok(())
}

async fn cmd_start(
    validator_key: Option<String>,
    port: u16,
    peers_str: String,
) -> anyhow::Result<()> {
    let storage = Arc::new(Storage::open(&get_db_path())?);
    let bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized. Run: sentrix init"))?;

    let shared: SharedState = Arc::new(RwLock::new(bc));

    // P2P event channel
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<NodeEvent>(256);

    // Create P2P node
    let node = Arc::new(Node::new(
        "0.0.0.0".to_string(),
        port,
        shared.clone(),
        event_tx.clone(),
    ));

    // Start P2P listener
    let p2p_bc = shared.clone();
    let p2p_peers = node.peers.clone();
    let p2p_etx = event_tx.clone();
    tokio::spawn(async move {
        if let Err(e) = Node::start_listener(port, p2p_bc, p2p_peers, p2p_etx).await {
            tracing::error!("P2P listener failed: {}", e);
        }
    });
    println!("P2P listening on port {}", port);

    // Start REST API
    let app = create_router(shared.clone());
    let api_addr = format!("0.0.0.0:{}", get_api_port());
    println!("REST API listening on http://{}", api_addr);

    let listener = tokio::net::TcpListener::bind(&api_addr).await?;

    // Connect to bootstrap peers
    if !peers_str.is_empty() {
        for peer_str in peers_str.split(',') {
            let peer = peer_str.trim().to_string();
            if peer.is_empty() { continue; }
            let node_clone = node.clone();
            tokio::spawn(async move {
                match node_clone.connect_peer(
                    peer.split(':').next().unwrap_or(""),
                    peer.split(':').nth(1).and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT),
                ).await {
                    Ok(()) => println!("Connected to peer: {}", peer),
                    Err(e) => println!("Failed to connect to {}: {}", peer, e),
                }
            });
        }
    }

    // Validator loop (if validator key provided)
    if let Some(key_hex) = validator_key {
        let wallet = Wallet::from_private_key(&key_hex)?;
        println!("Validator mode: {}", wallet.address);

        let shared_clone = shared.clone();
        let storage_clone = storage.clone();
        let node_clone = node.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
                let mut bc = shared_clone.write().await;
                if let Ok(block) = bc.create_block(&wallet.address) {
                    let height = block.index;
                    let block_clone = block.clone();
                    match bc.add_block(block) {
                        Ok(()) => {
                            println!("Block {} produced by {}", height, wallet.address);
                            let _ = storage_clone.save_blockchain(&bc);
                            let _ = storage_clone.save_height(height);
                            // Broadcast to peers
                            drop(bc);
                            node_clone.broadcast_block(&block_clone).await;
                        }
                        Err(e) => tracing::warn!("add_block failed: {}", e),
                    }
                }
            }
        });
    }

    // Event handler (log P2P events + handle sync triggers)
    let shared_for_events = shared.clone();
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                NodeEvent::PeerConnected(addr) => tracing::info!("Peer connected: {}", addr),
                NodeEvent::PeerDisconnected(addr) => tracing::info!("Peer disconnected: {}", addr),
                NodeEvent::NewBlock(block) => tracing::info!("Received block {} from peer", block.index),
                NodeEvent::NewTransaction(_) => {},
                NodeEvent::SyncNeeded { peer_addr, peer_height } => {
                    tracing::info!("Sync needed from {} (height: {})", peer_addr, peer_height);
                    let shared_sync = shared_for_events.clone();
                    tokio::spawn(async move {
                        match ChainSync::sync_from_peer(&peer_addr, &shared_sync).await {
                            Ok(n) if n > 0 => tracing::info!("Synced {} blocks from {}", n, peer_addr),
                            Ok(_) => {},
                            Err(e) => tracing::warn!("Sync from {} failed: {}", peer_addr, e),
                        }
                    });
                }
            }
        }
    });

    // Periodic sync: every 30s, pull any missing blocks from all known peers.
    // Prevents stall if initial handshake sync was missed (e.g. simultaneous restart).
    {
        let shared_ps = shared.clone();
        let node_ps = node.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                let peer_addrs: Vec<String> = node_ps.peers.read().await
                    .keys().cloned().collect();
                for addr in peer_addrs {
                    match ChainSync::sync_from_peer(&addr, &shared_ps).await {
                        Ok(n) if n > 0 => tracing::info!("Periodic sync: {} blocks from {}", n, addr),
                        Ok(_) => {},
                        Err(_) => {},
                    }
                }
            }
        });
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
    println!("Generating 7 genesis wallets for Sentrix...\n");

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
        println!("  Private key: {}", wallet.secret_key_hex());
        println!();

        wallets_json[*role] = serde_json::json!({
            "address": wallet.address,
            "public_key": wallet.public_key,
            "private_key": wallet.secret_key_hex(),
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

fn cmd_history(address: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let balance = bc.accounts.get_balance(address);
    let nonce = bc.accounts.get_nonce(address);
    let history = bc.get_address_history(address, 20, 0);

    println!("Address: {}", address);
    println!("Balance: {} sentri ({} SRX)", balance, balance as f64 / 100_000_000.0);
    println!("Nonce:   {}", nonce);
    println!("Transactions: {}\n", history.len());

    for tx in history.iter().rev().take(20) {
        let dir = tx["direction"].as_str().unwrap_or("?");
        let label = match dir {
            "reward" => "REWARD",
            "in"     => "IN    ",
            "out"    => "OUT   ",
            _        => "?     ",
        };
        println!("  [{}] {} | {} sentri | Block #{}",
            label,
            &tx["txid"].as_str().unwrap_or("?")[..24],
            tx["amount"],
            tx["block_index"],
        );
    }
    Ok(())
}

// ── Token commands ───────────────────────────────────────

fn cli_create_token_tx(bc: &mut Blockchain, wallet: &Wallet, token_op: TokenOp, fee: u64) -> anyhow::Result<String> {
    let sk = wallet.get_secret_key()?;
    let pk = wallet.get_public_key()?;
    let nonce = bc.accounts.get_nonce(&wallet.address);
    let data = token_op.encode()?;
    let tx = Transaction::new(
        wallet.address.clone(), TOKEN_OP_ADDRESS.to_string(),
        0, fee, nonce, data, bc.chain_id, &sk, &pk,
    )?;
    let txid = tx.txid.clone();
    bc.add_to_mempool(tx)?;
    Ok(txid)
}

fn cmd_token_deploy(name: &str, symbol: &str, decimals: u8, supply: u64, deployer_key: &str, fee: u64) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let wallet = Wallet::from_private_key(deployer_key)?;
    let token_op = TokenOp::Deploy { name: name.to_string(), symbol: symbol.to_string(), decimals, supply, max_supply: 0 };
    let txid = cli_create_token_tx(&mut bc, &wallet, token_op, fee)?;
    storage.save_blockchain(&bc)?;
    println!("Token deploy transaction submitted to mempool!");
    println!("  TxID:     {}", txid);
    println!("  Name:     {}", name);
    println!("  Symbol:   {}", symbol);
    println!("  Supply:   {}", supply);
    println!("  Status:   pending (will execute when block is mined)");
    Ok(())
}

fn cmd_token_transfer(contract: &str, to: &str, amount: u64, from_key: &str, gas: u64) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let wallet = Wallet::from_private_key(from_key)?;
    let token_op = TokenOp::Transfer { contract: contract.to_string(), to: to.to_string(), amount };
    let txid = cli_create_token_tx(&mut bc, &wallet, token_op, gas)?;
    storage.save_blockchain(&bc)?;
    println!("Token transfer transaction submitted to mempool!");
    println!("  TxID:     {}", txid);
    println!("  From:     {}", wallet.address);
    println!("  To:       {}", to);
    println!("  Amount:   {}", amount);
    println!("  Contract: {}", contract);
    println!("  Status:   pending (will execute when block is mined)");
    Ok(())
}

fn cmd_token_burn(contract: &str, amount: u64, from_key: &str, gas: u64) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let wallet = Wallet::from_private_key(from_key)?;
    let token_op = TokenOp::Burn { contract: contract.to_string(), amount };
    let txid = cli_create_token_tx(&mut bc, &wallet, token_op, gas)?;
    storage.save_blockchain(&bc)?;
    println!("Token burn transaction submitted to mempool!");
    println!("  TxID:     {}", txid);
    println!("  From:     {}", wallet.address);
    println!("  Amount:   {} burned", amount);
    println!("  Contract: {}", contract);
    println!("  Status:   pending (will execute when block is mined)");
    Ok(())
}

fn cmd_token_balance(contract: &str, address: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let balance = bc.token_balance(contract, address);
    println!("Token balance:");
    println!("  Address:  {}", address);
    println!("  Contract: {}", contract);
    println!("  Balance:  {}", balance);
    Ok(())
}

fn cmd_token_info(contract: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let info = bc.token_info(contract)?;
    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}

fn cmd_token_list() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage.load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let tokens = bc.list_tokens();
    if tokens.is_empty() {
        println!("No tokens deployed yet.");
        return Ok(());
    }
    println!("Deployed tokens ({}):", tokens.len());
    for token in &tokens {
        println!("  [{}] {} ({}) — supply: {}",
            token["contract_address"].as_str().unwrap_or(""),
            token["name"].as_str().unwrap_or(""),
            token["symbol"].as_str().unwrap_or(""),
            token["total_supply"],
        );
    }
    Ok(())
}
