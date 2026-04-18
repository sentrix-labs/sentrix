// main.rs - Sentrix CLI entry point
#![allow(missing_docs)]

use clap::{Parser, Subcommand};
use libp2p::Multiaddr;
use sentrix::api::routes::{SharedState, create_router};
use sentrix::core::blockchain::Blockchain;
use sentrix::core::transaction::{TOKEN_OP_ADDRESS, TokenOp, Transaction};
use sentrix::network::libp2p_node::{LibP2pNode, make_multiaddr};
use sentrix::network::node::{DEFAULT_PORT, NodeEvent};
use sentrix::storage::db::Storage;
use sentrix::wallet::keystore::Keystore;
use sentrix::wallet::wallet::Wallet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;

const DEFAULT_API_PORT: u16 = 8545;

fn get_api_port() -> u16 {
    std::env::var("SENTRIX_API_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_API_PORT)
}

/// C1: Bind host for the REST API listener. Defaults to `0.0.0.0` so the
/// public mainnet RPC keeps working without any env change. Testnet
/// validators behind nginx should set `SENTRIX_API_HOST=127.0.0.1` so the
/// raw API port is no longer exposed on the public interface.
fn get_api_host() -> String {
    std::env::var("SENTRIX_API_HOST").unwrap_or_else(|_| "0.0.0.0".to_string())
}

/// C1: Bind host for the libp2p P2P listener. Default `0.0.0.0` for
/// mainnet validators that must accept inbound peers from other VPSes.
/// Loopback-only testnets (val1..val4 peering via 127.0.0.1) should set
/// `SENTRIX_P2P_HOST=127.0.0.1` so external peers cannot reach them.
fn get_p2p_host() -> String {
    std::env::var("SENTRIX_P2P_HOST").unwrap_or_else(|_| "0.0.0.0".to_string())
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
    get_data_dir()
        .join("chain.db")
        .to_str()
        .unwrap_or("data/chain.db")
        .to_string()
}

fn get_wallets_dir() -> String {
    get_data_dir()
        .join("wallets")
        .to_str()
        .unwrap_or("data/wallets")
        .to_string()
}

#[derive(Parser)]
#[command(name = "sentrix")]
#[command(about = "Sentrix (SRX) — Layer-1 Blockchain")]
#[command(version = env!("CARGO_PKG_VERSION"))]
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
        /// Optional path to a genesis TOML. When absent, the embedded
        /// canonical mainnet config is used (default for mainnet nodes).
        #[arg(long)]
        genesis: Option<String>,
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
        /// Path to encrypted keystore file (alternative to --validator-key)
        #[arg(long)]
        validator_keystore: Option<String>,
        /// P2P port
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Bootstrap peers (comma-separated host:port)
        #[arg(long, default_value = "")]
        peers: String,
        /// Optional path to a genesis TOML. When absent, the binary uses the
        /// embedded canonical mainnet genesis (backward-compatible default).
        #[arg(long)]
        genesis: Option<String>,
    },
    /// Chain information
    Chain {
        #[command(subcommand)]
        action: ChainCommands,
    },
    /// Check account balance
    Balance { address: String },
    /// Transaction history for an address
    History { address: String },
    /// Token operations (SRX-20)
    Token {
        #[command(subcommand)]
        action: TokenCommands,
    },
    /// State export/import/snapshot tools
    State {
        #[command(subcommand)]
        action: StateCommands,
    },
    /// Mempool management
    Mempool {
        #[command(subcommand)]
        action: MempoolCommands,
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
    Info { keystore_file: String },
    /// Encrypt a private key to a keystore file
    Encrypt {
        private_key: String,
        #[arg(long)]
        password: Option<String>,
        /// Output file (default: data/wallets/<addr>.json)
        #[arg(long)]
        output: Option<String>,
    },
    /// Decrypt a keystore file to show the private key (for backup only)
    Decrypt {
        keystore_file: String,
        #[arg(long)]
        password: Option<String>,
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
    /// Unjail a validator that was jailed for downtime/slashing.
    /// Run while the node is STOPPED. Run on EACH validator's chain DB.
    Unjail {
        /// Validator address to unjail
        address: String,
    },
    /// Transfer the admin role to a new address (admin only).
    /// Use to rotate out a compromised admin key without a hard fork.
    /// Run on EACH validator's chain DB — the admin field is local node
    /// state, not part of block headers.
    TransferAdmin {
        /// New admin address (0x + 40 hex). Must be valid Sentrix format.
        new_admin: String,
        /// Current admin private key (prefer SENTRIX_ADMIN_KEY env var)
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
        #[arg(long)]
        name: String,
        #[arg(long)]
        symbol: String,
        #[arg(long, default_value_t = 18)]
        decimals: u8,
        #[arg(long)]
        supply: u64,
        /// Deployer private key (prefer SENTRIX_DEPLOYER_KEY env var)
        #[arg(long)]
        deployer_key: Option<String>,
        #[arg(long, default_value_t = 100_000)]
        fee: u64,
    },
    /// Transfer tokens
    Transfer {
        #[arg(long)]
        contract: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        amount: u64,
        /// Sender private key (prefer SENTRIX_FROM_KEY env var)
        #[arg(long)]
        from_key: Option<String>,
        #[arg(long, default_value_t = 10_000)]
        gas: u64,
    },
    /// Burn tokens (remove from circulation)
    Burn {
        #[arg(long)]
        contract: String,
        #[arg(long)]
        amount: u64,
        /// Sender private key (prefer SENTRIX_FROM_KEY env var)
        #[arg(long)]
        from_key: Option<String>,
        #[arg(long, default_value_t = 10_000)]
        gas: u64,
    },
    /// Check token balance
    Balance {
        #[arg(long)]
        contract: String,
        #[arg(long)]
        address: String,
    },
    /// Show token info
    Info {
        #[arg(long)]
        contract: String,
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
    /// Drop all trie state (trie_nodes, trie_values, trie_roots) so the next startup
    /// rebuilds the trie from scratch via V7-I-02 backfill.  Run this command while
    /// the node is STOPPED, then restart normally.
    ResetTrie,
}

#[derive(Subcommand)]
enum StateCommands {
    /// Export chain state at current height to a JSON snapshot file.
    /// Run while the node is STOPPED so the state is consistent.
    Export {
        /// Output file path (default: state_<height>.json)
        #[arg(long)]
        output: Option<String>,
    },
    /// Import chain state from a snapshot file, replacing current state.
    /// Run while the node is STOPPED.
    Import {
        /// Input snapshot file
        input: String,
        /// Skip confirmation prompt (required for non-interactive use)
        #[arg(long)]
        force: bool,
    },
    /// Verify a snapshot file's integrity without importing.
    Verify {
        /// Snapshot file to verify
        input: String,
    },
}

#[derive(Subcommand)]
enum MempoolCommands {
    /// Clear all pending transactions from the mempool.
    /// Run while the node is STOPPED. Useful after a stuck-mempool incident.
    Clear,
    /// Show mempool stats (can run while node is stopped).
    Stats,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    std::fs::create_dir_all(get_data_dir())?;
    std::fs::create_dir_all(get_wallets_dir())?;

    let cli = Cli::parse();

    match cli.command {
        Commands::Init { admin, genesis } => cmd_init(&admin, genesis.as_deref())?,

        Commands::Wallet { action } => match action {
            WalletCommands::Generate { password } => cmd_wallet_generate(password)?,
            WalletCommands::Import {
                private_key,
                password,
            } => cmd_wallet_import(&private_key, password)?,
            WalletCommands::Info { keystore_file } => cmd_wallet_info(&keystore_file)?,
            WalletCommands::Encrypt {
                private_key,
                password,
                output,
            } => cmd_wallet_encrypt(&private_key, password, output)?,
            WalletCommands::Decrypt {
                keystore_file,
                password,
            } => cmd_wallet_decrypt(&keystore_file, password)?,
        },

        Commands::Validator { action } => match action {
            ValidatorCommands::Add {
                address,
                name,
                public_key,
                admin_key,
            } => {
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
            ValidatorCommands::Rename {
                address,
                new_name,
                admin_key,
            } => {
                let key = resolve_key(admin_key, "SENTRIX_ADMIN_KEY", "admin key")?;
                cmd_validator_rename(&address, &new_name, &key)?;
            }
            ValidatorCommands::Unjail { address } => {
                cmd_validator_unjail(&address)?;
            }
            ValidatorCommands::TransferAdmin {
                new_admin,
                admin_key,
            } => {
                let key = resolve_key(admin_key, "SENTRIX_ADMIN_KEY", "admin key")?;
                cmd_validator_transfer_admin(&new_admin, &key)?;
            }
            ValidatorCommands::List => cmd_validator_list()?,
        },

        Commands::Start {
            validator_key,
            validator_keystore,
            port,
            peers,
            genesis,
        } => {
            // Load + validate genesis config before anything touches state.
            // When --genesis is absent, fall back to the embedded canonical
            // mainnet TOML (backward-compatible default). Fail loud if a
            // custom path is supplied but invalid — silently booting the
            // wrong chain would be a much worse failure mode.
            let genesis_cfg = match genesis.as_deref() {
                Some(path) => {
                    let g = sentrix::core::Genesis::from_path(path)?;
                    println!(
                        "Loaded genesis from {}: chain_id={} ({})",
                        path, g.chain.chain_id, g.chain.name
                    );
                    g
                }
                None => {
                    let g = sentrix::core::Genesis::mainnet()?;
                    println!(
                        "Using embedded mainnet genesis: chain_id={} ({})",
                        g.chain.chain_id, g.chain.name
                    );
                    g
                }
            };
            // Resolve validator key: --validator-key > --validator-keystore > env var
            let resolved_key = if let Some(key) = validator_key {
                Some(key)
            } else if let Some(ks_path) = validator_keystore {
                // Decrypt keystore to get private key
                let pwd = resolve_password(None)?;
                let keystore = Keystore::load(&ks_path)?;
                let wallet = keystore.decrypt(&pwd)?;
                println!("Keystore decrypted: {}", wallet.address);
                Some(wallet.secret_key_hex())
            } else {
                std::env::var("SENTRIX_VALIDATOR_KEY").ok()
            };
            let _ = genesis_cfg; // retained for future wiring into Blockchain::new
            cmd_start(resolved_key, port, peers).await?;
        }

        Commands::Chain { action } => match action {
            ChainCommands::Info => cmd_chain_info()?,
            ChainCommands::Validate => cmd_chain_validate()?,
            ChainCommands::Block { index } => cmd_chain_block(index)?,
            ChainCommands::ResetTrie => cmd_chain_reset_trie()?,
        },

        Commands::Token { action } => match action {
            TokenCommands::Deploy {
                name,
                symbol,
                decimals,
                supply,
                deployer_key,
                fee,
            } => {
                let key = resolve_key(deployer_key, "SENTRIX_DEPLOYER_KEY", "deployer key")?;
                cmd_token_deploy(&name, &symbol, decimals, supply, &key, fee)?;
            }
            TokenCommands::Transfer {
                contract,
                to,
                amount,
                from_key,
                gas,
            } => {
                let key = resolve_key(from_key, "SENTRIX_FROM_KEY", "from key")?;
                cmd_token_transfer(&contract, &to, amount, &key, gas)?;
            }
            TokenCommands::Burn {
                contract,
                amount,
                from_key,
                gas,
            } => {
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

        Commands::State { action } => match action {
            StateCommands::Export { output } => cmd_state_export(output)?,
            StateCommands::Import { input, force } => cmd_state_import(&input, force)?,
            StateCommands::Verify { input } => cmd_state_verify(&input)?,
        },

        Commands::Mempool { action } => match action {
            MempoolCommands::Clear => cmd_mempool_clear()?,
            MempoolCommands::Stats => cmd_mempool_stats()?,
        },

        Commands::Balance { address } => cmd_balance(&address)?,

        Commands::History { address } => cmd_history(&address)?,

        Commands::GenesisWallets => cmd_genesis_wallets()?,
    }

    Ok(())
}

// Resolve private key from CLI arg or env var; warn if passed via CLI (shell history risk)
fn resolve_key(cli_arg: Option<String>, env_var: &str, label: &str) -> anyhow::Result<String> {
    if let Some(ref key) = cli_arg {
        eprintln!(
            "WARNING: passing {} as CLI argument is insecure. Prefer {} env var.",
            label, env_var
        );
        return Ok(key.clone());
    }
    std::env::var(env_var).map_err(|_| {
        anyhow::anyhow!(
            "{} required. Use --{} or set {} env var",
            label,
            label.replace(' ', "-"),
            env_var
        )
    })
}

// ── Command implementations ──────────────────────────────

fn cmd_init(admin: &str, genesis_path: Option<&str>) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    if storage.has_blockchain() {
        println!("Chain already initialized.");
        return Ok(());
    }
    // Load + validate genesis config up front so a malformed config aborts
    // init before we touch storage. A custom --genesis path lets operators
    // bootstrap non-mainnet chains (testnet, devnet) from TOML without
    // rebuilding the binary.
    let genesis = match genesis_path {
        Some(path) => {
            let g = sentrix::core::Genesis::from_path(path)?;
            println!(
                "Loaded genesis from {}: chain_id={} ({})",
                path, g.chain.chain_id, g.chain.name
            );
            g
        }
        None => {
            let g = sentrix::core::Genesis::mainnet()?;
            println!(
                "Using embedded mainnet genesis: chain_id={} ({})",
                g.chain.chain_id, g.chain.name
            );
            g
        }
    };
    let bc = Blockchain::new_with_genesis(admin.to_string(), &genesis);
    storage.save_blockchain(&bc)?;
    let premine_srx = genesis.total_premine() / 100_000_000;
    println!("Chain initialized.");
    println!("Admin address: {}", admin);
    println!("Genesis block created. Height: 0");
    println!("Total premine: {} SRX", premine_srx);
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
    println!(
        "  KDF:     {} ({} iterations)",
        keystore.crypto.kdf, keystore.crypto.kdf_iterations
    );
    Ok(())
}

fn cmd_wallet_encrypt(
    private_key: &str,
    password: Option<String>,
    output: Option<String>,
) -> anyhow::Result<()> {
    let pwd = resolve_password(password)?;
    let wallet = Wallet::from_private_key(private_key)?;
    let keystore = Keystore::encrypt(&wallet, &pwd)?;
    let filename = output.unwrap_or_else(|| {
        let dir = get_wallets_dir();
        let _ = std::fs::create_dir_all(&dir);
        format!("{}/{}.json", dir, &wallet.address[2..10])
    });
    keystore.save(&filename)?;
    println!("Wallet encrypted:");
    println!("  Address:  {}", wallet.address);
    println!("  Saved to: {}", filename);
    println!("  KDF:      argon2id");
    Ok(())
}

fn cmd_wallet_decrypt(keystore_file: &str, password: Option<String>) -> anyhow::Result<()> {
    let pwd = resolve_password(password)?;
    let keystore = Keystore::load(keystore_file)?;
    let wallet = keystore.decrypt(&pwd)?;
    println!("Wallet decrypted:");
    println!("  Address:     {}", wallet.address);
    println!("  Public key:  {}", wallet.public_key);
    // Private key printed to stdout ONLY — never logged, never in API
    println!("  Private key: {}", wallet.secret_key_hex());
    Ok(())
}

/// Resolve password from CLI arg, SENTRIX_WALLET_PASSWORD env var, or terminal prompt.
fn resolve_password(cli_password: Option<String>) -> anyhow::Result<String> {
    if let Some(pw) = cli_password {
        return Ok(pw);
    }
    if let Ok(pw) = std::env::var("SENTRIX_WALLET_PASSWORD") {
        return Ok(pw);
    }
    // Prompt on terminal
    eprint!("Enter wallet password: ");
    let mut pw = String::new();
    std::io::stdin().read_line(&mut pw)?;
    let pw = pw.trim().to_string();
    if pw.is_empty() {
        anyhow::bail!("Password cannot be empty");
    }
    Ok(pw)
}

fn cmd_validator_add(
    address: &str,
    name: &str,
    public_key: &str,
    admin_key: &str,
) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
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

fn cmd_validator_unjail(address: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    let height = bc.height();
    bc.stake_registry.unjail(address, height)?;
    bc.stake_registry.update_active_set();

    storage.save_blockchain(&bc)?;
    println!("Validator unjailed: {}", address);
    println!(
        "Active set: {} validators",
        bc.stake_registry.active_count()
    );
    Ok(())
}

fn cmd_validator_transfer_admin(new_admin: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized. Run: sentrix init"))?;

    let admin_wallet = Wallet::from_private_key(admin_key)?;
    let old_admin = bc.authority.admin_address.clone();

    bc.authority
        .transfer_admin(&admin_wallet.address, new_admin.to_string())?;

    storage.save_blockchain(&bc)?;
    println!("Admin role transferred:");
    println!("  old: {}", old_admin);
    println!("  new: {}", new_admin);
    println!("Note: this only updates THIS node's chain DB. Run on every");
    println!("validator's DB to complete cluster-wide rotation.");
    Ok(())
}

fn cmd_validator_list() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    println!(
        "Validators ({} total, {} active):",
        bc.authority.validator_count(),
        bc.authority.active_count()
    );
    for v in bc.authority.active_validators() {
        println!(
            "  [{}] {} — {} blocks produced",
            if v.is_active { "ACTIVE" } else { "INACTIVE" },
            v.name,
            v.blocks_produced
        );
        println!("      Address: {}", v.address);
    }
    Ok(())
}

fn cmd_validator_remove(address: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let admin_wallet = Wallet::from_private_key(admin_key)?;
    bc.authority
        .remove_validator(&admin_wallet.address, address)?;
    storage.save_blockchain(&bc)?;
    println!("Validator removed: {}", address);
    Ok(())
}

fn cmd_validator_toggle(address: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let admin_wallet = Wallet::from_private_key(admin_key)?;
    let is_active = bc
        .authority
        .toggle_validator(&admin_wallet.address, address)?;
    storage.save_blockchain(&bc)?;
    let status = if is_active { "ACTIVE" } else { "INACTIVE" };
    println!("Validator {} toggled to: {}", address, status);
    Ok(())
}

fn cmd_validator_rename(address: &str, new_name: &str, admin_key: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let admin_wallet = Wallet::from_private_key(admin_key)?;
    bc.authority
        .rename_validator(&admin_wallet.address, address, new_name.to_string())?;
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
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized. Run: sentrix init"))?;

    let shared: SharedState = Arc::new(RwLock::new(bc));

    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<NodeEvent>(256);

    // ── P2P: libp2p TCP + Noise + Yamux ─────────────────
    println!("P2P transport: libp2p (Noise encrypted)");
    // Persist node identity keypair so PeerId stays stable across restarts.
    // A new PeerId on every restart breaks peer routing and libp2p's security model.
    // Store the node identity keypair in a dedicated sub-directory so that a naive
    // `cp -r data/` or `tar` of chain state between nodes does not inadvertently copy
    // the keypair — which would cause a PeerId collision and block peer connections.
    let identity_dir = get_data_dir().join("identity");
    std::fs::create_dir_all(&identity_dir)
        .map_err(|e| anyhow::anyhow!("create identity dir: {}", e))?;
    let keypair_path = identity_dir.join("node_keypair");
    let keypair = if keypair_path.exists() {
        let bytes = std::fs::read(&keypair_path)
            .map_err(|e| anyhow::anyhow!("read node keypair: {}", e))?;
        libp2p::identity::Keypair::from_protobuf_encoding(&bytes)
            .map_err(|e| anyhow::anyhow!("decode node keypair: {}", e))?
    } else {
        let kp = libp2p::identity::Keypair::generate_ed25519();
        let bytes = kp
            .to_protobuf_encoding()
            .map_err(|e| anyhow::anyhow!("encode node keypair: {}", e))?;
        std::fs::write(&keypair_path, bytes)
            .map_err(|e| anyhow::anyhow!("write node keypair: {}", e))?;
        tracing::info!("Generated new node identity, saved to {:?}", keypair_path);
        kp
    };
    tracing::info!("Node PeerId: {}", keypair.public().to_peer_id());
    let lp2p = Arc::new(
        LibP2pNode::new(keypair, shared.clone(), event_tx.clone())
            .map_err(|e| anyhow::anyhow!("libp2p init: {}", e))?,
    );

    let p2p_host = get_p2p_host();
    let listen_addr = make_multiaddr(&p2p_host, port).map_err(|e| anyhow::anyhow!("{}", e))?;
    lp2p.listen_on(listen_addr)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    println!("libp2p listening on /ip4/{}/tcp/{}", p2p_host, port);

    // Connect to bootstrap peers
    for peer_str in peers_str
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let parts: Vec<&str> = peer_str.splitn(2, ':').collect();
        if let [host, port_part] = parts.as_slice()
            && let Ok(p) = port_part.parse::<u16>()
            && let Ok(addr) = make_multiaddr(host, p)
        {
            let lp = lp2p.clone();
            let addr_str = addr.to_string();
            tokio::spawn(async move {
                match lp.connect_peer(addr).await {
                    Ok(()) => println!("Dialing peer {}", addr_str),
                    Err(e) => println!("Failed to dial {}: {}", addr_str, e),
                }
            });
        }
    }

    // Shutdown flag — set to true by the signal handler to stop the validator loop
    // cleanly before the process exits (guarantees trie.commit() is not interrupted).
    let shutdown_flag = Arc::new(AtomicBool::new(false));

    // BFT event channel — forwards P2P BFT votes from event handler to validator loop
    let (bft_tx, bft_rx) =
        tokio::sync::mpsc::channel::<sentrix::core::bft_messages::BftMessage>(256);

    // Validator loop
    if let Some(key_hex) = validator_key {
        let wallet = Wallet::from_private_key(&key_hex)?;
        println!("Validator mode: {}", wallet.address);
        let shared_clone = shared.clone();
        let storage_clone = storage.clone();
        let lp2p_clone = lp2p.clone();
        let shutdown_flag_clone = shutdown_flag.clone();
        let mut bft_rx = bft_rx; // move receiver into this task
        let validator_secret_key = wallet.get_secret_key()?;
        tokio::spawn(async move {
            use sentrix::core::bft::{BftAction, BftEngine};
            use sentrix::core::bft_messages::{BftMessage, Proposal};
            use sentrix::core::block::Block;

            let mut voyager_activated = false;
            let mut evm_activated = false;
            // Persistent BFT state for Voyager mode
            let mut bft_engine: Option<BftEngine> = None;
            let mut voyager_tick_count: u64 = 0;
            let mut proposed_block: Option<Block> = None;

            loop {
                if shutdown_flag_clone.load(Ordering::Acquire) {
                    tracing::info!("Validator loop: shutdown flag set — exiting");
                    break;
                }

                // ── Voyager fork activation (read lock first, write only if needed) ──
                if !voyager_activated {
                    let bc = shared_clone.read().await;
                    if Blockchain::is_voyager_height(bc.height().saturating_add(1)) {
                        drop(bc);
                        let mut bc = shared_clone.write().await;
                        tracing::info!(
                            "Voyager fork reached at height {} — activating DPoS",
                            bc.height()
                        );
                        if let Err(e) = bc.activate_voyager() {
                            tracing::warn!("activate_voyager failed: {}", e);
                        }
                        voyager_activated = true;
                    }
                }

                // ── EVM fork activation ──
                if !evm_activated {
                    let bc = shared_clone.read().await;
                    if Blockchain::is_evm_height(bc.height().saturating_add(1)) {
                        drop(bc);
                        let mut bc = shared_clone.write().await;
                        tracing::info!(
                            "EVM fork reached at height {} — activating EVM",
                            bc.height()
                        );
                        bc.activate_evm();
                        evm_activated = true;
                    }
                }

                // ════════════════════════════════════════════════
                // Pioneer mode: original 3s polling, no BFT
                // ════════════════════════════════════════════════
                if !voyager_activated {
                    tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

                    let result = {
                        let mut bc = shared_clone.write().await;
                        match bc.create_block(&wallet.address) {
                            Ok(block) => {
                                let height = block.index;
                                match bc.add_block(block) {
                                    Ok(()) => {
                                        let updated = bc.latest_block().ok().cloned();
                                        Some((height, updated))
                                    }
                                    Err(e) => {
                                        tracing::warn!("add_block failed: {}", e);
                                        None
                                    }
                                }
                            }
                            Err(_) => None,
                        }
                    };

                    if let Some((height, Some(block_to_save))) = result {
                        println!("Block {} produced by {}", height, wallet.address);
                        let _ = storage_clone.save_block(&block_to_save);
                        {
                            let bc = shared_clone.read().await;
                            let _ = storage_clone.save_blockchain(&bc);
                        }
                        lp2p_clone.broadcast_block(&block_to_save).await;
                    }
                    continue;
                }

                // ════════════════════════════════════════════════
                // Voyager mode: event-driven BFT consensus
                // ════════════════════════════════════════════════
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

                // Periodically broadcast our BFT round status (~5s) so
                // peers can catch up to our round via on_round_status.
                // This is the ONLY round-sync mechanism now that
                // vote-triggered catch-up has been removed.
                voyager_tick_count += 1;
                // Broadcast every 2s (20 ticks × 100ms) for fast convergence.
                if voyager_tick_count.is_multiple_of(20)
                    && let Some(ref bft) = bft_engine
                {
                    let status = bft.build_round_status();
                    lp2p_clone.broadcast_bft_round_status(&status).await;
                }

                // Compute total active stake and current chain height (read lock)
                let (current_height, total_active_stake) = {
                    let bc = shared_clone.read().await;
                    let total: u64 = bc
                        .stake_registry
                        .active_set
                        .iter()
                        .filter_map(|a| bc.stake_registry.get_validator(a))
                        .map(|v| v.total_stake())
                        .sum();
                    (bc.height(), total)
                };

                let next_height = current_height.saturating_add(1);

                // Initialize BFT engine for next height when chain has advanced
                let need_new_round = match &bft_engine {
                    None => true,
                    Some(bft) => bft.height() <= current_height,
                };
                if need_new_round {
                    let mut bft =
                        BftEngine::new(next_height, wallet.address.clone(), total_active_stake);
                    proposed_block = None;

                    // Check if we're the proposer for this height+round
                    let bc = shared_clone.read().await;
                    let we_are_proposer = bft.is_proposer(&bc.stake_registry);
                    let expected_proposer = bft.expected_proposer(&bc.stake_registry);
                    let active_count = bc.stake_registry.active_count();
                    tracing::info!(
                        "BFT round start: height={} round={} active={} proposer={:?} we_are={}",
                        next_height,
                        bft.round(),
                        active_count,
                        expected_proposer.as_deref().map(|a| &a[..12.min(a.len())]),
                        we_are_proposer,
                    );
                    drop(bc);

                    if we_are_proposer {
                        // We're the proposer — create block (Voyager: skip Pioneer authority)
                        let mut bc = shared_clone.write().await;
                        match bc.create_block_voyager(&wallet.address) {
                            Ok(block) => {
                                let block_hash = block.hash.clone();
                                let block_data = bincode::serialize(&block).unwrap_or_default();
                                let mut proposal = Proposal {
                                    height: next_height,
                                    round: bft.round(),
                                    block_hash: block_hash.clone(),
                                    block_data,
                                    proposer: wallet.address.clone(),
                                    signature: vec![],
                                };
                                proposal.sign(&validator_secret_key);
                                proposed_block = Some(block);
                                drop(bc);

                                // Broadcast signed proposal to peers
                                lp2p_clone.broadcast_bft_proposal(&proposal).await;

                                // Self-vote: on_own_proposal triggers prevote
                                let initial_action = bft.on_own_proposal(&block_hash);

                                // Cascading BFT action loop
                                let mut action = initial_action;
                                loop {
                                    match action {
                                        BftAction::BroadcastPrevote(ref prevote) => {
                                            let mut signed_pv = prevote.clone();
                                            signed_pv.sign(&validator_secret_key);
                                            lp2p_clone.broadcast_bft_prevote(&signed_pv).await;
                                            let bc = shared_clone.read().await;
                                            let our_stake = bc
                                                .stake_registry
                                                .get_validator(&wallet.address)
                                                .map(|v| v.total_stake())
                                                .unwrap_or(0);
                                            drop(bc);
                                            action = bft.on_prevote_weighted(prevote, our_stake);
                                            continue;
                                        }
                                        BftAction::BroadcastPrecommit(ref precommit) => {
                                            let mut signed_pc = precommit.clone();
                                            signed_pc.sign(&validator_secret_key);
                                            lp2p_clone.broadcast_bft_precommit(&signed_pc).await;
                                            let bc = shared_clone.read().await;
                                            let our_stake = bc
                                                .stake_registry
                                                .get_validator(&wallet.address)
                                                .map(|v| v.total_stake())
                                                .unwrap_or(0);
                                            drop(bc);
                                            action =
                                                bft.on_precommit_weighted(precommit, our_stake);
                                            continue;
                                        }
                                        BftAction::FinalizeBlock {
                                            height,
                                            round,
                                            block_hash: _,
                                            ref justification,
                                        } => {
                                            if let Some(mut blk) = proposed_block.take() {
                                                blk.round = round;
                                                blk.justification = Some(justification.clone());
                                                let proposer = blk.validator.clone();

                                                let mut bc = shared_clone.write().await;
                                                match bc.add_block(blk) {
                                                    Ok(()) => {
                                                        let updated =
                                                            bc.latest_block().ok().cloned();

                                                        // ── Post-block Voyager bookkeeping ──
                                                        let reward = bc.get_block_reward();
                                                        bc.epoch_manager.record_block(reward);

                                                        let active =
                                                            bc.stake_registry.active_set.clone();
                                                        let signers = vec![proposer.clone()];
                                                        bc.slashing.record_block_signatures(
                                                            &active, &signers, height,
                                                        );

                                                        let validator_fee = 0;
                                                        let _ =
                                                            bc.stake_registry.distribute_reward(
                                                                &proposer,
                                                                reward,
                                                                validator_fee,
                                                            );

                                                        if sentrix::core::epoch::EpochManager::is_epoch_boundary(height) {
                                                            tracing::info!("Epoch boundary at height {} — transitioning", height);
                                                            let released = bc.stake_registry.process_unbonding(height);
                                                            for (delegator, amount) in &released {
                                                                bc.accounts.credit(delegator, *amount)
                                                                    .unwrap_or_else(|e| tracing::warn!("unbonding credit failed: {}", e));
                                                            }
                                                            if !released.is_empty() {
                                                                tracing::info!("Released {} unbonding entries", released.len());
                                                            }

                                                            bc.stake_registry.update_active_set();
                                                            let active_set = bc.stake_registry.active_set.clone();
                                                            let total_staked: u64 = active_set.iter()
                                                                .filter_map(|a| bc.stake_registry.get_validator(a))
                                                                .map(|v| v.total_stake())
                                                                .sum();
                                                            bc.epoch_manager.record_block(0);
                                                            let finished = bc.epoch_manager.current_epoch.clone();
                                                            bc.epoch_manager.history.push(finished);
                                                            if bc.epoch_manager.history.len() > bc.epoch_manager.max_history {
                                                                bc.epoch_manager.history.remove(0);
                                                            }
                                                            let next_num = bc.epoch_manager.current_epoch.epoch_number + 1;
                                                            let next_start = next_num * sentrix::core::epoch::EPOCH_LENGTH;
                                                            bc.epoch_manager.current_epoch = sentrix::core::epoch::EpochInfo {
                                                                epoch_number: next_num,
                                                                start_height: next_start,
                                                                end_height: next_start + sentrix::core::epoch::EPOCH_LENGTH - 1,
                                                                validator_set: active_set.clone(),
                                                                total_staked,
                                                                total_rewards: 0,
                                                                total_blocks_produced: 0,
                                                            };
                                                            tracing::info!("Epoch {} started — {} validators, {} staked",
                                                                next_num, active_set.len(), total_staked);

                                                            let mut slashing = std::mem::take(&mut bc.slashing);
                                                            let slashed = slashing.check_liveness(
                                                                &mut bc.stake_registry, &active_set, height,
                                                            );
                                                            bc.slashing = slashing;
                                                            for (val, amt) in &slashed {
                                                                tracing::warn!("Slashed {} for {} sentri (downtime)", val, amt);
                                                                bc.accounts.burn(*amt);
                                                            }
                                                        }

                                                        tracing::info!(
                                                            "BFT finalized height={} round={}",
                                                            height,
                                                            round
                                                        );

                                                        drop(bc);
                                                        if let Some(ref saved_block) = updated {
                                                            println!(
                                                                "Block {} produced by {}",
                                                                height, proposer
                                                            );
                                                            let _ = storage_clone
                                                                .save_block(saved_block);
                                                            let bc = shared_clone.read().await;
                                                            let _ =
                                                                storage_clone.save_blockchain(&bc);
                                                            drop(bc);
                                                            lp2p_clone
                                                                .broadcast_block(saved_block)
                                                                .await;
                                                        }
                                                    }
                                                    Err(e) => tracing::warn!(
                                                        "BFT add_block failed: {}",
                                                        e
                                                    ),
                                                }
                                            }
                                            break;
                                        }
                                        BftAction::TimeoutAdvanceRound => {
                                            bft.advance_round();
                                            tracing::info!("BFT timeout — round {}", bft.round());
                                            break;
                                        }
                                        BftAction::SkipRound => {
                                            // Nil supermajority → advance round (DON'T reset engine)
                                            bft.advance_round();
                                            tracing::warn!(
                                                "BFT skip round — advanced to round {} at height {}",
                                                bft.round(),
                                                bft.height()
                                            );
                                            break;
                                        }
                                        BftAction::SyncNeeded { .. } => {
                                            tracing::info!("BFT: peer ahead, need block sync");
                                            break;
                                        }
                                        BftAction::Wait | BftAction::ProposeBlock => break,
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!("create_block failed: {}", e);
                                drop(bc);
                            }
                        }
                    }

                    bft_engine = Some(bft);
                }

                // Process incoming BFT messages from peers
                if let Some(ref mut bft) = bft_engine {
                    // Drain all available BFT messages
                    while let Ok(msg) = bft_rx.try_recv() {
                        let action = match msg {
                            BftMessage::Propose(proposal) => {
                                if proposal.height != bft.height() {
                                    continue;
                                }
                                // Only process proposals for our current round.
                                // No catch-up — rounds advance via deterministic timeouts only.
                                if proposal.round != bft.round() {
                                    continue;
                                }
                                if !proposal.verify_sig() {
                                    tracing::warn!(
                                        "Invalid proposal signature from {}",
                                        &proposal.proposer
                                    );
                                    continue;
                                }
                                if let Ok(block) =
                                    bincode::deserialize::<Block>(&proposal.block_data)
                                {
                                    proposed_block = Some(block);
                                    let bc = shared_clone.read().await;
                                    let a = bft.on_proposal(
                                        &proposal.block_hash,
                                        &proposal.proposer,
                                        &bc.stake_registry,
                                    );
                                    drop(bc);
                                    a
                                } else {
                                    tracing::warn!("Failed to deserialize block from BFT proposal");
                                    continue;
                                }
                            }
                            BftMessage::Prevote(prevote) => {
                                if !prevote.verify_sig() {
                                    tracing::warn!(
                                        "Invalid prevote signature from {}",
                                        &prevote.validator
                                    );
                                    continue;
                                }
                                let bc = shared_clone.read().await;
                                let stake = bc
                                    .stake_registry
                                    .get_validator(&prevote.validator)
                                    .map(|v| v.total_stake())
                                    .unwrap_or(0);
                                drop(bc);
                                bft.on_prevote_weighted(&prevote, stake)
                            }
                            BftMessage::Precommit(precommit) => {
                                if !precommit.verify_sig() {
                                    tracing::warn!(
                                        "Invalid precommit signature from {}",
                                        &precommit.validator
                                    );
                                    continue;
                                }
                                let bc = shared_clone.read().await;
                                let stake = bc
                                    .stake_registry
                                    .get_validator(&precommit.validator)
                                    .map(|v| v.total_stake())
                                    .unwrap_or(0);
                                drop(bc);
                                bft.on_precommit_weighted(&precommit, stake)
                            }
                            BftMessage::RoundStatus(status) => bft.on_round_status(&status),
                        };

                        // Cascading BFT action loop for peer messages
                        let mut action = action;
                        loop {
                            match action {
                                BftAction::BroadcastPrevote(ref prevote) => {
                                    let mut signed_pv = prevote.clone();
                                    signed_pv.sign(&validator_secret_key);
                                    lp2p_clone.broadcast_bft_prevote(&signed_pv).await;
                                    let bc = shared_clone.read().await;
                                    let our_stake = bc
                                        .stake_registry
                                        .get_validator(&wallet.address)
                                        .map(|v| v.total_stake())
                                        .unwrap_or(0);
                                    drop(bc);
                                    action = bft.on_prevote_weighted(prevote, our_stake);
                                    continue;
                                }
                                BftAction::BroadcastPrecommit(ref precommit) => {
                                    let mut signed_pc = precommit.clone();
                                    signed_pc.sign(&validator_secret_key);
                                    lp2p_clone.broadcast_bft_precommit(&signed_pc).await;
                                    let bc = shared_clone.read().await;
                                    let our_stake = bc
                                        .stake_registry
                                        .get_validator(&wallet.address)
                                        .map(|v| v.total_stake())
                                        .unwrap_or(0);
                                    drop(bc);
                                    action = bft.on_precommit_weighted(precommit, our_stake);
                                    continue;
                                }
                                BftAction::FinalizeBlock {
                                    height,
                                    round,
                                    block_hash: _,
                                    ref justification,
                                } => {
                                    if let Some(mut blk) = proposed_block.take() {
                                        blk.round = round;
                                        blk.justification = Some(justification.clone());
                                        let proposer = blk.validator.clone();

                                        let mut bc = shared_clone.write().await;
                                        match bc.add_block(blk) {
                                            Ok(()) => {
                                                let updated = bc.latest_block().ok().cloned();

                                                // ── Post-block Voyager bookkeeping ──
                                                let reward = bc.get_block_reward();
                                                bc.epoch_manager.record_block(reward);

                                                let active = bc.stake_registry.active_set.clone();
                                                let signers = vec![proposer.clone()];
                                                bc.slashing.record_block_signatures(
                                                    &active, &signers, height,
                                                );

                                                let validator_fee = 0;
                                                let _ = bc.stake_registry.distribute_reward(
                                                    &proposer,
                                                    reward,
                                                    validator_fee,
                                                );

                                                if sentrix::core::epoch::EpochManager::is_epoch_boundary(height) {
                                                    tracing::info!("Epoch boundary at height {} — transitioning", height);
                                                    let released = bc.stake_registry.process_unbonding(height);
                                                    for (delegator, amount) in &released {
                                                        bc.accounts.credit(delegator, *amount)
                                                            .unwrap_or_else(|e| tracing::warn!("unbonding credit failed: {}", e));
                                                    }
                                                    if !released.is_empty() {
                                                        tracing::info!("Released {} unbonding entries", released.len());
                                                    }

                                                    bc.stake_registry.update_active_set();
                                                    let active_set = bc.stake_registry.active_set.clone();
                                                    let total_staked: u64 = active_set.iter()
                                                        .filter_map(|a| bc.stake_registry.get_validator(a))
                                                        .map(|v| v.total_stake())
                                                        .sum();
                                                    bc.epoch_manager.record_block(0);
                                                    let finished = bc.epoch_manager.current_epoch.clone();
                                                    bc.epoch_manager.history.push(finished);
                                                    if bc.epoch_manager.history.len() > bc.epoch_manager.max_history {
                                                        bc.epoch_manager.history.remove(0);
                                                    }
                                                    let next_num = bc.epoch_manager.current_epoch.epoch_number + 1;
                                                    let next_start = next_num * sentrix::core::epoch::EPOCH_LENGTH;
                                                    bc.epoch_manager.current_epoch = sentrix::core::epoch::EpochInfo {
                                                        epoch_number: next_num,
                                                        start_height: next_start,
                                                        end_height: next_start + sentrix::core::epoch::EPOCH_LENGTH - 1,
                                                        validator_set: active_set.clone(),
                                                        total_staked,
                                                        total_rewards: 0,
                                                        total_blocks_produced: 0,
                                                    };
                                                    tracing::info!("Epoch {} started — {} validators, {} staked",
                                                        next_num, active_set.len(), total_staked);

                                                    let mut slashing = std::mem::take(&mut bc.slashing);
                                                    let slashed = slashing.check_liveness(
                                                        &mut bc.stake_registry, &active_set, height,
                                                    );
                                                    bc.slashing = slashing;
                                                    for (val, amt) in &slashed {
                                                        tracing::warn!("Slashed {} for {} sentri (downtime)", val, amt);
                                                        bc.accounts.burn(*amt);
                                                    }
                                                }

                                                tracing::info!(
                                                    "BFT finalized height={} round={}",
                                                    height,
                                                    round
                                                );

                                                drop(bc);
                                                if let Some(ref saved_block) = updated {
                                                    println!(
                                                        "Block {} produced by {}",
                                                        height, proposer
                                                    );
                                                    let _ = storage_clone.save_block(saved_block);
                                                    let bc = shared_clone.read().await;
                                                    let _ = storage_clone.save_blockchain(&bc);
                                                    drop(bc);
                                                    lp2p_clone.broadcast_block(saved_block).await;
                                                }
                                            }
                                            Err(e) => tracing::warn!("BFT add_block failed: {}", e),
                                        }
                                    }
                                    break;
                                }
                                BftAction::TimeoutAdvanceRound => {
                                    bft.advance_round();
                                    tracing::info!("BFT timeout — round {}", bft.round());
                                    // After round advance, check if WE are the new proposer
                                    // If yes, create a new block proposal for this round
                                    let bc_r = shared_clone.read().await;
                                    let we_propose = bft.is_proposer(&bc_r.stake_registry);
                                    drop(bc_r);
                                    if we_propose {
                                        let mut bc = shared_clone.write().await;
                                        if let Ok(block) = bc.create_block_voyager(&wallet.address)
                                        {
                                            let block_hash = block.hash.clone();
                                            let block_data =
                                                bincode::serialize(&block).unwrap_or_default();
                                            let mut proposal = Proposal {
                                                height: bft.height(),
                                                round: bft.round(),
                                                block_hash: block_hash.clone(),
                                                block_data,
                                                proposer: wallet.address.clone(),
                                                signature: vec![],
                                            };
                                            proposal.sign(&validator_secret_key);
                                            drop(bc);
                                            lp2p_clone.broadcast_bft_proposal(&proposal).await;
                                            proposed_block = Some(block);
                                            let _ = bft.on_own_proposal(&block_hash);
                                            tracing::info!(
                                                "BFT: proposed block for new round {}",
                                                bft.round()
                                            );
                                        }
                                    }
                                    break;
                                }
                                BftAction::SkipRound => {
                                    // Nil supermajority → advance round (DON'T reset engine)
                                    bft.advance_round();
                                    tracing::warn!(
                                        "BFT skip round — advanced to round {} at height {}",
                                        bft.round(),
                                        bft.height()
                                    );
                                    // After round advance, propose if we're the new round's proposer
                                    let bc_r = shared_clone.read().await;
                                    let we_propose = bft.is_proposer(&bc_r.stake_registry);
                                    drop(bc_r);
                                    if we_propose {
                                        let mut bc = shared_clone.write().await;
                                        if let Ok(block) = bc.create_block_voyager(&wallet.address)
                                        {
                                            let block_hash = block.hash.clone();
                                            let block_data =
                                                bincode::serialize(&block).unwrap_or_default();
                                            let mut proposal = Proposal {
                                                height: bft.height(),
                                                round: bft.round(),
                                                block_hash: block_hash.clone(),
                                                block_data,
                                                proposer: wallet.address.clone(),
                                                signature: vec![],
                                            };
                                            proposal.sign(&validator_secret_key);
                                            drop(bc);
                                            lp2p_clone.broadcast_bft_proposal(&proposal).await;
                                            proposed_block = Some(block);
                                            let _ = bft.on_own_proposal(&block_hash);
                                            tracing::info!(
                                                "BFT: proposed block after skip-round at round {}",
                                                bft.round()
                                            );
                                        }
                                    }
                                    break;
                                }
                                BftAction::SyncNeeded { peer_height } => {
                                    tracing::info!(
                                        "BFT: peer at height {}, need block sync",
                                        peer_height
                                    );
                                    break;
                                }
                                BftAction::Wait | BftAction::ProposeBlock => break,
                            }
                        }
                    }

                    // Check for BFT timeouts
                    if bft.is_timed_out() {
                        let timeout_action = bft.on_timeout();
                        let mut action = timeout_action;
                        loop {
                            match action {
                                BftAction::BroadcastPrevote(ref prevote) => {
                                    let mut signed_pv = prevote.clone();
                                    signed_pv.sign(&validator_secret_key);
                                    lp2p_clone.broadcast_bft_prevote(&signed_pv).await;
                                    let bc = shared_clone.read().await;
                                    let our_stake = bc
                                        .stake_registry
                                        .get_validator(&wallet.address)
                                        .map(|v| v.total_stake())
                                        .unwrap_or(0);
                                    drop(bc);
                                    action = bft.on_prevote_weighted(prevote, our_stake);
                                    continue;
                                }
                                BftAction::BroadcastPrecommit(ref precommit) => {
                                    let mut signed_pc = precommit.clone();
                                    signed_pc.sign(&validator_secret_key);
                                    lp2p_clone.broadcast_bft_precommit(&signed_pc).await;
                                    let bc = shared_clone.read().await;
                                    let our_stake = bc
                                        .stake_registry
                                        .get_validator(&wallet.address)
                                        .map(|v| v.total_stake())
                                        .unwrap_or(0);
                                    drop(bc);
                                    action = bft.on_precommit_weighted(precommit, our_stake);
                                    continue;
                                }
                                BftAction::TimeoutAdvanceRound => {
                                    bft.advance_round();
                                    tracing::info!(
                                        "BFT timeout — advanced to round {}",
                                        bft.round()
                                    );
                                    break;
                                }
                                BftAction::SkipRound => {
                                    // Nil supermajority → advance round (DON'T reset engine)
                                    // Resetting would cause desync vs other validators who are advancing
                                    bft.advance_round();
                                    tracing::warn!(
                                        "BFT skip round — advanced to round {} at height {}",
                                        bft.round(),
                                        bft.height()
                                    );
                                    break;
                                }
                                _ => break,
                            }
                        }
                    }
                }
            }
        });
    }

    // Event handler — persist P2P blocks to MDBX + forward BFT events
    // Sync is handled inside the libp2p swarm task (Step 3d).
    let storage_for_p2p = storage.clone();
    let bft_tx_clone = bft_tx;
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                NodeEvent::PeerConnected(addr) => tracing::info!("Peer connected: {}", addr),
                NodeEvent::PeerDisconnected(addr) => tracing::info!("Peer disconnected: {}", addr),
                NodeEvent::NewBlock(block) => {
                    tracing::info!("Block {} received from peer", block.index);
                    if let Err(e) = storage_for_p2p.save_block(&block) {
                        tracing::warn!("failed to persist P2P block {}: {}", block.index, e);
                    }
                }
                NodeEvent::NewTransaction(_) => {}
                NodeEvent::SyncNeeded {
                    peer_addr,
                    peer_height,
                } => {
                    tracing::info!("Sync needed from {} (height: {})", peer_addr, peer_height);
                }
                // BFT events — forward to validator loop for multi-validator consensus
                NodeEvent::BftProposal(p) => {
                    tracing::info!(
                        "BFT proposal: height={} round={} proposer={}",
                        p.height,
                        p.round,
                        &p.proposer[..p.proposer.len().min(12)]
                    );
                    let _ = bft_tx_clone
                        .send(sentrix::core::bft_messages::BftMessage::Propose(p))
                        .await;
                }
                NodeEvent::BftPrevote(v) => {
                    tracing::info!(
                        "BFT prevote: height={} round={} from={}",
                        v.height,
                        v.round,
                        &v.validator[..v.validator.len().min(12)]
                    );
                    let _ = bft_tx_clone
                        .send(sentrix::core::bft_messages::BftMessage::Prevote(v))
                        .await;
                }
                NodeEvent::BftPrecommit(c) => {
                    tracing::info!(
                        "BFT precommit: height={} round={} from={}",
                        c.height,
                        c.round,
                        &c.validator[..c.validator.len().min(12)]
                    );
                    let _ = bft_tx_clone
                        .send(sentrix::core::bft_messages::BftMessage::Precommit(c))
                        .await;
                }
                NodeEvent::BftRoundStatus(s) => {
                    tracing::debug!(
                        "BFT round-status: height={} round={} from={}",
                        s.height,
                        s.round,
                        &s.validator[..s.validator.len().min(12)]
                    );
                    let _ = bft_tx_clone
                        .send(sentrix::core::bft_messages::BftMessage::RoundStatus(s))
                        .await;
                }
            }
        }
    });

    // ── Periodic reconnect to bootstrap peers ────────────
    // Collect bootstrap multiaddrs for reconnection
    let bootstrap_addrs: Vec<Multiaddr> = peers_str
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|peer_str| {
            let parts: Vec<&str> = peer_str.splitn(2, ':').collect();
            if let [host, port_part] = parts.as_slice()
                && let Ok(p) = port_part.parse::<u16>()
            {
                return make_multiaddr(host, p).ok();
            }
            None
        })
        .collect();

    if !bootstrap_addrs.is_empty() {
        let lp2p_reconnect = lp2p.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(30)).await;
                let count = lp2p_reconnect.peer_count().await;
                if count < bootstrap_addrs.len() {
                    tracing::info!(
                        "Reconnecting: {} peers, expected {}",
                        count,
                        bootstrap_addrs.len()
                    );
                    lp2p_reconnect
                        .reconnect_peers(bootstrap_addrs.clone())
                        .await;
                }
            }
        });
    }

    // ── Shared: REST API (always started) ───────────────
    let app = create_router(shared.clone());
    let api_addr = format!("{}:{}", get_api_host(), get_api_port());
    println!("REST API listening on http://{}", api_addr);
    let listener = tokio::net::TcpListener::bind(&api_addr).await?;

    println!("Node started. Press Ctrl+C to stop.");

    // Graceful shutdown on SIGTERM/SIGINT — saves state before exit.
    // Without this, kill/systemctl stop corrupts in-flight state and causes chain forks.
    let shutdown_storage = storage.clone();
    let shutdown_shared = shared.clone();
    let shutdown_signal = async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(
                        "Failed to install SIGTERM handler: {} — shutdown via Ctrl+C only",
                        e
                    );
                    let _ = tokio::signal::ctrl_c().await;
                    tracing::info!("SIGINT received — shutting down");
                    return;
                }
            };
            tokio::select! {
                _ = sigterm.recv() => tracing::info!("SIGTERM received — shutting down"),
                _ = tokio::signal::ctrl_c() => tracing::info!("SIGINT received — shutting down"),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            tracing::info!("Ctrl+C received — shutting down");
        }

        // 1. Signal the validator loop to stop — prevents a new block cycle from
        //    starting while we are trying to save state.
        shutdown_flag.store(true, Ordering::Release);

        // 2. Acquire the write lock and immediately drop it.
        //    This waits for any in-progress add_block() (and therefore trie.commit())
        //    to finish before we take a snapshot — guarantees the trie root is committed.
        tracing::info!("Graceful shutdown: waiting for in-progress block to complete...");
        drop(shutdown_shared.write().await);

        // 3. Save state under a read lock so API requests can still be served
        //    until axum finishes its own graceful drain.
        tracing::info!("Graceful shutdown: saving state to disk...");
        let bc = shutdown_shared.read().await;
        if let Err(e) = shutdown_storage.save_blockchain(&bc) {
            tracing::error!("Failed to save state on shutdown: {}", e);
        } else {
            tracing::info!("State saved. Node exiting cleanly.");
        }
    };

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal)
        .await?;
    Ok(())
}

fn cmd_chain_info() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let stats = bc.chain_stats();
    println!("{}", serde_json::to_string_pretty(&stats)?);
    Ok(())
}

fn cmd_chain_validate() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let valid = bc.is_valid_chain_window();
    println!("Chain valid: {}", valid);
    println!("Height: {}", bc.height());
    println!("Total blocks: {}", bc.height() + 1);
    Ok(())
}

fn cmd_chain_block(index: u64) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    match bc.get_block(index) {
        Some(block) => println!("{}", serde_json::to_string_pretty(block)?),
        None => println!("Block {} not found", index),
    }
    Ok(())
}

fn cmd_chain_reset_trie() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    if !storage.has_blockchain() {
        anyhow::bail!("Chain not initialized.");
    }
    storage.reset_trie()?;
    println!(
        "Trie state cleared. Start the node normally — it will rebuild the trie from AccountDB."
    );
    Ok(())
}

fn cmd_state_export(output: Option<String>) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    let snapshot = bc.export_state()?;
    let out_path = output.unwrap_or_else(|| format!("state_{}.json", snapshot.metadata.height));
    let json = serde_json::to_string_pretty(&snapshot)?;
    std::fs::write(&out_path, &json)?;
    println!(
        "State exported: {} ({} accounts, {} validators, {:.4} SRX total)",
        out_path,
        snapshot.accounts.len(),
        snapshot.validators.len(),
        snapshot.accounts.iter().map(|a| a.balance).sum::<u64>() as f64 / 100_000_000.0
    );
    println!("Height: {}", snapshot.metadata.height);
    println!("Chain ID: {}", snapshot.metadata.chain_id);
    Ok(())
}

fn cmd_state_import(input: &str, force: bool) -> anyhow::Result<()> {
    if !force {
        anyhow::bail!(
            "State import replaces ALL current accounts, validators, and contracts.\n\
             This is destructive. Pass --force to confirm."
        );
    }

    let json = std::fs::read_to_string(input)?;
    let snapshot: sentrix::core::state_export::StateSnapshot = serde_json::from_str(&json)?;

    // Verify first
    Blockchain::verify_snapshot(&snapshot)?;

    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    let count = bc.import_state(&snapshot)?;
    storage.save_blockchain(&bc)?;

    println!(
        "State imported: {} accounts from snapshot at height {}",
        count, snapshot.metadata.height
    );
    println!("Restart the node to rebuild the state trie.");
    Ok(())
}

fn cmd_state_verify(input: &str) -> anyhow::Result<()> {
    let json = std::fs::read_to_string(input)?;
    let snapshot: sentrix::core::state_export::StateSnapshot = serde_json::from_str(&json)?;
    let summary = Blockchain::verify_snapshot(&snapshot)?;
    println!("{}", summary);
    Ok(())
}

fn cmd_mempool_clear() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let old_size = bc.mempool_size();
    bc.clear_mempool();
    storage.save_blockchain(&bc)?;
    println!("Mempool cleared: {} transactions removed", old_size);
    Ok(())
}

fn cmd_mempool_stats() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    println!("Mempool size: {} transactions", bc.mempool_size());
    Ok(())
}

fn cmd_balance(address: &str) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let balance = bc.accounts.get_balance(address);
    println!("Address: {}", address);
    println!(
        "Balance: {} sentri ({} SRX)",
        balance,
        balance as f64 / 100_000_000.0
    );
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
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let balance = bc.accounts.get_balance(address);
    let nonce = bc.accounts.get_nonce(address);
    let history = bc.get_address_history(address, 20, 0);

    println!("Address: {}", address);
    println!(
        "Balance: {} sentri ({} SRX)",
        balance,
        balance as f64 / 100_000_000.0
    );
    println!("Nonce:   {}", nonce);
    println!("Transactions: {}\n", history.len());

    for tx in history.iter().rev().take(20) {
        let dir = tx["direction"].as_str().unwrap_or("?");
        let label = match dir {
            "reward" => "REWARD",
            "in" => "IN    ",
            "out" => "OUT   ",
            _ => "?     ",
        };
        println!(
            "  [{}] {} | {} sentri | Block #{}",
            label,
            &tx["txid"].as_str().unwrap_or("?")[..24],
            tx["amount"],
            tx["block_index"],
        );
    }
    Ok(())
}

// ── Token commands ───────────────────────────────────────

fn cli_create_token_tx(
    bc: &mut Blockchain,
    wallet: &Wallet,
    token_op: TokenOp,
    fee: u64,
) -> anyhow::Result<String> {
    let sk = wallet.get_secret_key()?;
    let pk = wallet.get_public_key()?;
    let nonce = bc.accounts.get_nonce(&wallet.address);
    let data = token_op.encode()?;
    let tx = Transaction::new(
        wallet.address.clone(),
        TOKEN_OP_ADDRESS.to_string(),
        0,
        fee,
        nonce,
        data,
        bc.chain_id,
        &sk,
        &pk,
    )?;
    let txid = tx.txid.clone();
    bc.add_to_mempool(tx)?;
    Ok(txid)
}

fn cmd_token_deploy(
    name: &str,
    symbol: &str,
    decimals: u8,
    supply: u64,
    deployer_key: &str,
    fee: u64,
) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let wallet = Wallet::from_private_key(deployer_key)?;
    let token_op = TokenOp::Deploy {
        name: name.to_string(),
        symbol: symbol.to_string(),
        decimals,
        supply,
        max_supply: 0,
    };
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

fn cmd_token_transfer(
    contract: &str,
    to: &str,
    amount: u64,
    from_key: &str,
    gas: u64,
) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let wallet = Wallet::from_private_key(from_key)?;
    let token_op = TokenOp::Transfer {
        contract: contract.to_string(),
        to: to.to_string(),
        amount,
    };
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
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let wallet = Wallet::from_private_key(from_key)?;
    let token_op = TokenOp::Burn {
        contract: contract.to_string(),
        amount,
    };
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
    let bc = storage
        .load_blockchain()?
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
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let info = bc.token_info(contract)?;
    println!("{}", serde_json::to_string_pretty(&info)?);
    Ok(())
}

fn cmd_token_list() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;
    let tokens = bc.list_tokens();
    if tokens.is_empty() {
        println!("No tokens deployed yet.");
        return Ok(());
    }
    println!("Deployed tokens ({}):", tokens.len());
    for token in &tokens {
        println!(
            "  [{}] {} ({}) — supply: {}",
            token["contract_address"].as_str().unwrap_or(""),
            token["name"].as_str().unwrap_or(""),
            token["symbol"].as_str().unwrap_or(""),
            token["total_supply"],
        );
    }
    Ok(())
}
