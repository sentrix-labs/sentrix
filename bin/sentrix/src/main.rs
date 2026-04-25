// main.rs - Sentrix CLI entry point
#![allow(missing_docs)]

use clap::{Parser, Subcommand};
use libp2p::Multiaddr;
use sentrix::api::routes::{SharedState, create_router};
use sentrix::core::blockchain::{BLOCK_TIME_SECS, Blockchain};
use sentrix::core::transaction::{PROTOCOL_TREASURY, TOKEN_OP_ADDRESS, TokenOp, Transaction};
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

/// L2 pre-flight peer-mesh gate for Voyager activation.
///
/// Returns `Ok(())` when this validator has enough libp2p peers to
/// participate in BFT consensus — i.e. at least `active_set_len - 1`
/// peers, since we don't dial ourselves. Returns `Err` with a human
/// description otherwise; the caller should NOT flip into Voyager mode
/// and should re-check on the next loop tick.
///
/// The `force_override` arg comes from the
/// `SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS=1` env var and is read at the
/// call site. It exists as an emergency operator escape hatch but
/// SHOULD NOT be set during normal operations — it re-creates the
/// 2026-04-25 mainnet livelock condition where validators activated
/// BFT without a fully-formed mesh and got stuck in nil-supermajority
/// loops.
///
/// Active set of size 1 is treated as a degenerate single-validator
/// chain (testnet bootstrap, recovery scenarios) where peer count is
/// trivially satisfied.
fn check_bft_peer_mesh_eligible(
    peer_count: usize,
    active_set_len: usize,
    force_override: bool,
) -> Result<(), String> {
    if force_override {
        return Ok(());
    }
    // Single-validator chain: peer count is moot. We use `== 1` rather
    // than `<= 1` so an active_set_len == 0 produces an explicit error
    // instead of silently passing — a chain with zero active validators
    // should never be reaching the BFT activation path in the first
    // place, and silently approving it would mask a separate bug.
    if active_set_len == 1 {
        return Ok(());
    }
    if active_set_len == 0 {
        return Err(
            "BFT activation blocked: active_set is empty — no validators registered. \
             This indicates a separate bug in DPoS migration; check stake_registry."
                .to_string(),
        );
    }
    let required = active_set_len - 1;
    if peer_count < required {
        return Err(format!(
            "BFT activation blocked: need ≥{required} libp2p peers \
             (active_set={active_set_len}), have {peer_count}. \
             Verify --peers / wait for L1 multiaddr gossip / set \
             SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS=1 to override."
        ));
    }
    Ok(())
}

/// Strict env-var check for the BFT peer-mesh gate override. Only the
/// literal string `"1"` enables the override; any other value (typoed
/// `"true"`, accidentally-empty `""` from shell `VAR=$missing`,
/// whitespace) is rejected and the gate stays active. This avoids the
/// "empty-string-is-truthy" footgun where a misconfigured env file
/// silently disables the safety net during exactly the operational
/// scenarios it exists to protect.
fn force_bft_insufficient_peers_set() -> bool {
    std::env::var("SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS")
        .map(|v| v == "1")
        .unwrap_or(false)
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
    /// Start the node (P2P + API + validator loop).
    ///
    /// Validator key sources, tried in order:
    ///   1. `--validator-keystore <path>` (encrypted Argon2id v2 keystore;
    ///      password from `SENTRIX_WALLET_PASSWORD` env or interactive prompt)
    ///   2. `SENTRIX_VALIDATOR_KEY` env var (raw hex private key)
    ///
    /// Without either, the node runs in relay (non-producer) mode.
    ///
    /// The legacy `--validator-key <hex>` flag was removed in v2.0.1 (audit
    /// C-06): CLI args are visible in `ps aux` and shell history.
    Start {
        /// Path to encrypted keystore file (preferred validator key source).
        #[arg(long)]
        validator_keystore: Option<String>,
        /// P2P port
        #[arg(long, default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Bootstrap peers (comma-separated host:port).
        ///
        /// This is a SEED list for the first-time connect; it is NOT the
        /// validator set and does NOT need to be kept in sync with it.
        /// After any one of these peers is reachable, Kademlia DHT
        /// auto-discovers every other peer on the mesh (periodic 60 s
        /// random walk + Identify-driven routing-table updates). Adding
        /// a new validator therefore only requires (a) the new node
        /// boots with `--peers` pointing at ONE existing operator's
        /// public endpoint, and (b) the admin runs
        /// `sentrix validator add`. No existing validator needs a
        /// systemd-unit edit, a restart, or a `--peers` update.
        ///
        /// Recommended: point at 1–3 stable reference bootnodes rather
        /// than every known validator, so the list doesn't churn when
        /// the operator community grows.
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
    /// Token operations (SRC-20)
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
    /// Rotate a keystore's password without ever exposing the private
    /// key to disk or logs. Atomic: writes the new keystore to a
    /// sibling tempfile and renames into place only after a verify
    /// round-trip succeeds; leaves a timestamped `.bak-<TS>` so a
    /// failed rotation is always recoverable.
    Rekey {
        keystore_file: String,
        /// Old password (prefer `SENTRIX_WALLET_OLD_PASSWORD` env var
        /// or the interactive prompt — passing on the CLI leaves the
        /// password in shell history).
        #[arg(long)]
        old_password: Option<String>,
        /// New password (prefer `SENTRIX_WALLET_NEW_PASSWORD` env var
        /// or the interactive prompt).
        #[arg(long)]
        new_password: Option<String>,
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
    /// Operator-only recovery: unjail + restore self_stake to
    /// MIN_SELF_STAKE when slashing has knocked the validator below
    /// the eligibility floor. Skips the jail-period cooldown.
    ///
    /// PHANTOM STAKE WARNING: restoring self_stake via direct DB edit
    /// does NOT mint SRX. The supply invariant
    /// `sum(balances) + sum(stakes + delegations) == circulating_supply`
    /// gets violated by the shortfall. Safe on testnet (no real value);
    /// on mainnet (chain_id 7119) this command refuses to run unless
    /// `--i-understand-phantom-stake` is passed. Mainnet operators
    /// should prefer a real self-delegate TX from the validator's own
    /// wallet whenever possible, and use this break-glass only when
    /// the chain is so stuck that no TX can be mined.
    ///
    /// Use this when the chain is stuck because all validators were
    /// auto-slashed (BFT `active_set` empty → can't mine blocks →
    /// can't submit unjail/stake TXs). Run while the node is STOPPED,
    /// and run on EACH validator's chain DB for every jailed address
    /// so all peers agree on the recovered state before BFT resumes.
    ForceUnjail {
        /// Validator address to force-unjail
        address: String,
        /// Required on mainnet to acknowledge the supply-invariant
        /// violation this command introduces. Testnet does not require
        /// the flag.
        #[arg(long)]
        i_understand_phantom_stake: bool,
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
    /// Deploy a new SRC-20 token
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
    /// Deep cross-table consistency check: walk every AccountDB entry and verify
    /// the trie leaf at that address encodes matching (balance, nonce). Detects
    /// mixed-timestamp chain.db that arises from rsync-while-live (the #268
    /// 2026-04-25 incident root cause). Run with the node STOPPED. Exits 0 if
    /// consistent, non-zero with a per-address report if any mismatches found.
    VerifyDeep,
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

    // P1 (panic supervisor): escalate every panic — whether on the main
    // thread or inside a tokio::spawn'd task — to a loud log line plus
    // process abort. Without this, a tokio task can panic, have its
    // unwind payload stored in its JoinHandle, and then the runtime
    // keeps scheduling other tasks indefinitely: the validator loop
    // silently stops producing, consensus gossip silently stops being
    // forwarded, and the only signal is that the chain height freezes.
    // Aborting here lets systemd (`Restart=always` on the sentrix-*
    // units) bring the process back in a clean state; the next peer
    // re-syncs any block we were in the middle of.
    //
    // The existing tracing subscriber is already installed above, so
    // the `tracing::error!` call is captured by journalctl before the
    // abort. `std::process::abort()` is used (not `exit(1)`) to skip
    // destructors — any locked Tokio primitives would otherwise hang
    // shutdown for the graceful-shutdown timeout.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Delegate to the default hook first so the panic message and
        // backtrace land on stderr in the normal Rust format.
        default_hook(info);
        tracing::error!(
            target: "panic_supervisor",
            "FATAL panic in tokio task or main thread: {} — aborting so \
             systemd restarts the node cleanly",
            info
        );
        std::process::abort();
    }));

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
            WalletCommands::Rekey {
                keystore_file,
                old_password,
                new_password,
            } => cmd_wallet_rekey(&keystore_file, old_password, new_password)?,
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
            ValidatorCommands::ForceUnjail {
                address,
                i_understand_phantom_stake,
            } => {
                cmd_validator_force_unjail(&address, i_understand_phantom_stake)?;
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
            // Resolve validator wallet: --validator-keystore > SENTRIX_VALIDATOR_KEY env.
            // The raw `--validator-key <hex>` CLI flag was removed in v2.0.1 (C-06):
            // CLI arguments leak via `ps aux`, shell history, and process snapshots.
            //
            // Construct the `Wallet` here so the secret never flows through the
            // call chain as a heap `String` (which would not be zeroed on drop).
            // `Wallet`'s `secret_key_bytes: Zeroizing<[u8; 32]>` field guarantees
            // the secret is wiped from memory when the wallet drops.
            let validator: Option<Wallet> = if let Some(ks_path) = validator_keystore {
                let pwd = resolve_password(None)?;
                let keystore = Keystore::load(&ks_path)?;
                let wallet = keystore.decrypt(&pwd)?;
                println!("Keystore decrypted: {}", wallet.address);
                Some(wallet)
            } else if let Ok(raw) = std::env::var("SENTRIX_VALIDATOR_KEY") {
                // Wrap the env var in `Zeroizing` so the source `String`'s
                // backing allocation is wiped after we derive the wallet.
                let key_hex = zeroize::Zeroizing::new(raw);
                Some(Wallet::from_private_key(&key_hex)?)
            } else {
                None
            };
            let _ = genesis_cfg; // retained for future wiring into Blockchain::new
            cmd_start(validator, port, peers).await?;
        }

        Commands::Chain { action } => match action {
            ChainCommands::Info => cmd_chain_info()?,
            ChainCommands::Validate => cmd_chain_validate()?,
            ChainCommands::Block { index } => cmd_chain_block(index)?,
            ChainCommands::ResetTrie => cmd_chain_reset_trie()?,
            ChainCommands::VerifyDeep => cmd_chain_verify_deep()?,
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

/// Rotate a keystore's password without exposing the private key to
/// disk or logs. Atomic: decrypt → re-encrypt → verify round-trip →
/// backup old file → rename new file over old.
///
/// The private key lives only inside the in-memory `Wallet` struct,
/// which zeroises its secret on drop (`Zeroizing<[u8;32]>`). No
/// stdout/stderr output reveals the key. Only the ADDRESS is printed
/// for operator confirmation.
fn cmd_wallet_rekey(
    keystore_file: &str,
    old_password: Option<String>,
    new_password: Option<String>,
) -> anyhow::Result<()> {
    use std::path::Path;

    // Resolve old password via CLI / SENTRIX_WALLET_OLD_PASSWORD env /
    // prompt. Prompt happens if both unset.
    let old_pwd = resolve_password_named(
        old_password,
        "SENTRIX_WALLET_OLD_PASSWORD",
        "Enter OLD wallet password",
    )?;
    // New password: same resolution path + confirm-twice on prompt.
    let new_pwd = resolve_password_named(
        new_password,
        "SENTRIX_WALLET_NEW_PASSWORD",
        "Enter NEW wallet password",
    )?;
    if old_pwd == new_pwd {
        anyhow::bail!("new password is identical to old — rotation would be a no-op");
    }

    // Step 1 — decrypt old keystore (this also validates old_pwd).
    let old_keystore = Keystore::load(keystore_file)?;
    let wallet = old_keystore
        .decrypt(&old_pwd)
        .map_err(|e| anyhow::anyhow!("old password rejected: {}", e))?;
    let address = wallet.address.clone();

    // Step 2 — re-encrypt with new_pwd (fresh salt, nonce, mac).
    let new_keystore = Keystore::encrypt(&wallet, &new_pwd)?;

    // Step 3 — verify round-trip BEFORE touching the original file.
    // If any implementation bug produces an un-decryptable keystore,
    // we catch it here instead of after overwriting the operator's
    // only copy.
    let verify = new_keystore
        .decrypt(&new_pwd)
        .map_err(|e| anyhow::anyhow!("new keystore failed self-decrypt — aborting: {}", e))?;
    if verify.address != address {
        anyhow::bail!(
            "address mismatch after rekey self-verify (got {}, expected {}); aborting",
            verify.address,
            address
        );
    }

    // Step 4 — atomic replace via sibling tempfile + rename.
    let path = Path::new(keystore_file);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("keystore_file has no parent directory"))?;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let tmp_path = parent.join(format!(".rekey-tmp-{}", ts));
    new_keystore.save(tmp_path.to_str().ok_or_else(|| {
        anyhow::anyhow!("tempfile path contains non-UTF-8 bytes — refusing to save")
    })?)?;

    // Timestamped backup of the old file. Operator can `rm` after a
    // stable period (suggested 48 h).
    let bak_path = parent.join(format!(
        "{}.bak-{}",
        path.file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("keystore"),
        ts
    ));
    std::fs::rename(path, &bak_path)?;
    std::fs::rename(&tmp_path, path)?;

    // Drop the in-memory plaintext as early as possible. `Wallet`
    // already zeroises its secret on drop, but explicit drop pins
    // the timing.
    drop(old_pwd);
    drop(new_pwd);
    drop(wallet);
    drop(verify);

    println!("Keystore rekeyed:");
    println!("  Address:   {}", address);
    println!("  File:      {}", keystore_file);
    println!("  Old copy:  {}", bak_path.display());
    println!();
    println!("Next steps (operator):");
    println!("  1. Update SENTRIX_WALLET_PASSWORD in the env file to the new password.");
    println!("  2. Restart the validator service (e.g. `systemctl restart sentrix-node`).");
    println!("  3. Confirm 'Validator mode: {}' appears in journalctl.", address);
    println!("  4. After the node runs stable for 48h, delete {}.", bak_path.display());
    Ok(())
}

/// Like `resolve_password` but with a named env var + custom prompt.
/// Lets `rekey` distinguish OLD vs NEW password sources cleanly.
fn resolve_password_named(
    cli_password: Option<String>,
    env_var: &str,
    prompt: &str,
) -> anyhow::Result<String> {
    if let Some(pw) = cli_password {
        return Ok(pw);
    }
    if let Ok(pw) = std::env::var(env_var) {
        return Ok(pw);
    }
    eprint!("{}: ", prompt);
    let mut pw = String::new();
    std::io::stdin().read_line(&mut pw)?;
    let pw = pw.trim().to_string();
    if pw.is_empty() {
        anyhow::bail!("Password cannot be empty");
    }
    Ok(pw)
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

fn cmd_validator_force_unjail(
    address: &str,
    acknowledged_phantom_stake: bool,
) -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    const MAINNET_CHAIN_ID: u64 = 7119;
    if bc.chain_id == MAINNET_CHAIN_ID && !acknowledged_phantom_stake {
        anyhow::bail!(
            "mainnet (chain_id 7119) detected: force-unjail creates phantom \
             stake (restores self_stake without minting SRX), which violates \
             the supply invariant. Prefer a real self-delegate TX from the \
             validator's wallet. If the chain is genuinely stuck and this is \
             the last option, re-run with `--i-understand-phantom-stake`."
        );
    }
    if bc.chain_id == MAINNET_CHAIN_ID {
        eprintln!(
            "WARNING: force-unjail on mainnet. Phantom stake will be created \
             if self_stake < MIN_SELF_STAKE. Document the recovery decision \
             before proceeding."
        );
    }

    let before = bc
        .stake_registry
        .get_validator(address)
        .map(|v| (v.self_stake, v.is_jailed, v.jail_until));
    bc.stake_registry.force_unjail(address)?;
    bc.stake_registry.update_active_set();
    let after = bc
        .stake_registry
        .get_validator(address)
        .map(|v| (v.self_stake, v.is_jailed, v.jail_until));

    storage.save_blockchain(&bc)?;
    println!("Validator force-unjailed: {}", address);
    if let (Some(b), Some(a)) = (before, after) {
        println!("  self_stake: {} → {}", b.0, a.0,);
        println!("  is_jailed:  {} → {}", b.1, a.1,);
        println!("  jail_until: {} → {}", b.2, a.2,);
    }
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
    // Take the validator wallet by value so the caller's `Zeroizing` envelope
    // for the env-var path drops *before* we hold the `Wallet` here. The
    // wallet's own `Zeroizing<[u8; 32]>` keeps the secret bytes wiped on drop.
    validator: Option<Wallet>,
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

    // Validator loop — capture the JoinHandle so the graceful-shutdown
    // path (C-08) can await the task's exit before save_blockchain
    // snapshots state. Without the handle the process could exit mid
    // add_block / trie.commit, tearing state between memory and disk.
    let validator_handle: Option<tokio::task::JoinHandle<()>> = if let Some(wallet) = validator {
        println!("Validator mode: {}", wallet.address);
        let shared_clone = shared.clone();
        let storage_clone = storage.clone();
        let lp2p_clone = lp2p.clone();
        let shutdown_flag_clone = shutdown_flag.clone();
        let mut bft_rx = bft_rx; // move receiver into this task
        let validator_secret_key = wallet.get_secret_key()?;
        Some(tokio::spawn(async move {
            use sentrix::core::bft::{BftAction, BftEngine, BftPhase};
            use sentrix::core::bft_messages::{BftMessage, Proposal};
            use sentrix::core::block::Block;

            // V2 M-15 Step 4+5 helper: produce a signed Proposal for the
            // current (height, round). If the engine is locked and has a
            // cached block (populated via Step 3 promotion), re-broadcast
            // the cached bytes — this is what breaks the locked-nil-prevote
            // livelock pattern when a locked validator rotates into the
            // proposer slot at a later round. Otherwise fall through to the
            // existing `create_block_voyager` path.
            //
            // Design: audits/v2-locked-block-repropose-implementation-plan.md
            fn build_or_reuse_proposal(
                bft: &BftEngine,
                bc: &mut Blockchain,
                wallet_address: &str,
                validator_sk: &secp256k1::SecretKey,
                height: u64,
            ) -> Option<(Block, Proposal)> {
                if let Some((cached_hash, cached_bytes)) = bft.locked_proposal_bytes() {
                    match bincode::deserialize::<Block>(&cached_bytes) {
                        Ok(block) => {
                            tracing::info!(
                                "V2 M-15: re-proposing locked block {:.16}... at height {} round {}",
                                cached_hash,
                                height,
                                bft.round()
                            );
                            let mut proposal = Proposal {
                                height,
                                round: bft.round(),
                                block_hash: cached_hash,
                                block_data: cached_bytes,
                                proposer: wallet_address.to_string(),
                                signature: vec![],
                            };
                            proposal.sign(validator_sk);
                            return Some((block, proposal));
                        }
                        Err(e) => {
                            tracing::warn!(
                                "V2 re-propose: cached bytes failed to deserialize: {} — falling back to create_block_voyager",
                                e
                            );
                        }
                    }
                }
                match bc.create_block_voyager(wallet_address) {
                    Ok(block) => {
                        let block_hash = block.hash.clone();
                        let block_data = bincode::serialize(&block).unwrap_or_default();
                        let mut proposal = Proposal {
                            height,
                            round: bft.round(),
                            block_hash,
                            block_data,
                            proposer: wallet_address.to_string(),
                            signature: vec![],
                        };
                        proposal.sign(validator_sk);
                        Some((block, proposal))
                    }
                    Err(e) => {
                        tracing::warn!("create_block_voyager failed: {}", e);
                        None
                    }
                }
            }

            // Sync local fast-path booleans from persistent on-chain flags so
            // a validator restarting post-fork skips the activation re-entry
            // entirely (no warn-spam, no redundant update_active_set call).
            // The Blockchain methods themselves are also idempotent via the
            // same flags — local boolean here just avoids taking the write
            // lock on every loop tick once the chain has crossed the fork.
            let (mut voyager_activated, mut evm_activated) = {
                let bc = shared_clone.read().await;
                (bc.voyager_activated, bc.evm_activated)
            };
            // Emergency rollback: SENTRIX_FORCE_PIONEER_MODE=1 forces the local
            // mode flag to Pioneer regardless of persistent voyager_activated
            // flag in chain.db. Used when Voyager activation hits a known issue
            // (e.g. V2 locked-block-repropose wiring gap) and operator needs to
            // resume Pioneer block production. The persistent flag stays set on
            // chain.db; clearing requires a separate chain.db edit operation.
            if std::env::var("SENTRIX_FORCE_PIONEER_MODE").is_ok() {
                tracing::warn!(
                    "SENTRIX_FORCE_PIONEER_MODE set — forcing Pioneer mode regardless of \
                     persistent voyager_activated flag. Block production will use round-robin \
                     PoA until env is unset and validator restarted."
                );
                voyager_activated = false;
                evm_activated = false;
            }
            // Persistent BFT state for Voyager mode
            let mut bft_engine: Option<BftEngine> = None;
            let mut voyager_tick_count: u64 = 0;
            let mut proposed_block: Option<Block> = None;
            // #1d fix: proposer rebroadcast. libp2p request-response drops
            // Proposal messages to peers that aren't in verified_peers at
            // broadcast time (e.g. just-reconnected validators), causing
            // the persistent "proposal only reached 2/4 peers" livelock we
            // diagnosed from the nil-majority tally logs on 2026-04-20.
            // Tracking the last broadcast time + a bounded rebroadcast
            // count lets the proposer retry every few seconds until
            // enough peers have acked, without spamming the network.
            let mut proposal_broadcast_at: Option<std::time::Instant> = None;
            let mut proposal_rebroadcast_count: u32 = 0;
            // Pioneer mode: track last block time for a fine-grained poll loop.
            // Poll every PIONEER_TICK, but only attempt to build a block when
            // at least BLOCK_TIME_SECS has elapsed since the last one. Gives
            // a consistent ~1s cadence without blocking the loop for 3s when
            // nothing is happening (previous 3s sleep made the effective
            // block time oscillate around 3s instead of the configured 1s).
            let mut pioneer_last_block =
                tokio::time::Instant::now() - tokio::time::Duration::from_secs(BLOCK_TIME_SECS);

            // L1 peer auto-discovery state. Every L1_TICK_INTERVAL the
            // loop checks whether our own advertisement needs
            // re-broadcasting (every ADVERT_BROADCAST_INTERVAL) and
            // whether we should dial any active-set members we have
            // cached but no live connection to. Per the impl plan at
            // founder-private/audits/peer-auto-discovery-implementation
            // -plan.md (L1 + L4 baked in).
            let mut last_advert_broadcast_at: Option<tokio::time::Instant> = None;
            let mut last_l1_tick_at = tokio::time::Instant::now()
                - tokio::time::Duration::from_secs(31); // fire on first iter
            let mut advert_sequence: u64 = 0;
            const L1_TICK_INTERVAL: tokio::time::Duration =
                tokio::time::Duration::from_secs(30);
            const ADVERT_BROADCAST_INTERVAL: tokio::time::Duration =
                tokio::time::Duration::from_secs(600); // 10 minutes

            loop {
                if shutdown_flag_clone.load(Ordering::Acquire) {
                    tracing::info!("Validator loop: shutdown flag set — exiting");
                    break;
                }

                // ── L1 peer auto-discovery tick ──
                if last_l1_tick_at.elapsed() >= L1_TICK_INTERVAL {
                    last_l1_tick_at = tokio::time::Instant::now();

                    // Broadcast our advert if due (first run + every
                    // ADVERT_BROADCAST_INTERVAL). Skipped silently when
                    // we have no public listen addresses (loopback-only
                    // testnets, paused listeners).
                    let need_broadcast = match last_advert_broadcast_at {
                        None => true,
                        Some(t) => t.elapsed() >= ADVERT_BROADCAST_INTERVAL,
                    };
                    if need_broadcast {
                        let listen_addrs = lp2p_clone.listen_addrs().await;
                        let chain_id = {
                            let bc = shared_clone.read().await;
                            bc.chain_id
                        };
                        // Filter out loopback-only addresses — peers
                        // can't reach those. Cap at MAX_MULTIADDRS to
                        // stay within DoS budget on the receiver side.
                        let multiaddrs: Vec<String> = listen_addrs
                            .iter()
                            .map(|m| m.to_string())
                            .filter(|s| !s.starts_with("/ip4/127.") && !s.starts_with("/ip6/::1/"))
                            .take(sentrix_wire::MultiaddrAdvertisement::MAX_MULTIADDRS)
                            .collect();
                        if !multiaddrs.is_empty() {
                            advert_sequence = advert_sequence.saturating_add(1);
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            let mut advert = sentrix_wire::MultiaddrAdvertisement {
                                validator: wallet.address.clone(),
                                multiaddrs,
                                sequence: advert_sequence,
                                timestamp,
                                chain_id,
                                signature: Vec::new(),
                            };
                            advert.sign(&validator_secret_key);
                            tracing::info!(
                                "L1: broadcasting multiaddr advertisement seq={} ({} addrs)",
                                advert.sequence,
                                advert.multiaddrs.len()
                            );
                            lp2p_clone.broadcast_validator_advert(advert).await;
                            last_advert_broadcast_at = Some(tokio::time::Instant::now());
                        } else {
                            tracing::debug!(
                                "L1: skipping advertisement — no non-loopback listen addrs"
                            );
                        }
                    }

                    // Dial any active-set members we have cached but
                    // aren't currently peered with. Caller (libp2p) is
                    // idempotent — duplicate dials to an already-
                    // connected peer are no-ops at the swarm level.
                    let active_set: Vec<String> = {
                        let bc = shared_clone.read().await;
                        bc.stake_registry.active_set.clone()
                    };
                    if !active_set.is_empty() {
                        let cached = lp2p_clone.list_cached_adverts().await;
                        for advert in &cached {
                            if advert.validator == wallet.address {
                                continue;
                            }
                            if !active_set.contains(&advert.validator) {
                                continue;
                            }
                            // Try the first listed multiaddr — preference
                            // order is the advertising validator's.
                            if let Some(ma_str) = advert.multiaddrs.first()
                                && let Ok(ma) = ma_str.parse::<libp2p::Multiaddr>()
                            {
                                tracing::debug!(
                                    "L1: dialing {} at {} (cached advert seq={})",
                                    &advert.validator[..12.min(advert.validator.len())],
                                    ma_str,
                                    advert.sequence
                                );
                                let _ = lp2p_clone.connect_peer(ma).await;
                            }
                        }
                    }
                }

                // ── Voyager fork activation (read lock first, write only if needed) ──
                //
                // L2 pre-flight gate (2026-04-25 incident response): refuse to
                // flip into Voyager BFT mode if our libp2p peer count is below
                // `active_set.len() - 1`. The mainnet livelock at h=557244 was
                // caused by VPS5 having only 1 peer (VPS1) at activation
                // moment — its proposals never reached VPS2/VPS3 and the
                // chain ground out 30+ skip rounds in 16 minutes before the
                // emergency rollback. With this gate, a partitioned validator
                // stays in Pioneer instead and re-checks every loop tick;
                // once L1 multiaddr gossip ships, the mesh self-heals and
                // activation proceeds automatically.
                if !voyager_activated {
                    let bc = shared_clone.read().await;
                    if Blockchain::is_voyager_height(bc.height().saturating_add(1)) {
                        let active_set_len = bc.stake_registry.active_set.len();
                        drop(bc);

                        let peer_count = lp2p_clone.peer_count().await;
                        let force_override = force_bft_insufficient_peers_set();

                        match check_bft_peer_mesh_eligible(
                            peer_count,
                            active_set_len,
                            force_override,
                        ) {
                            Ok(()) => {
                                let mut bc = shared_clone.write().await;
                                tracing::info!(
                                    "Voyager fork reached at height {} — activating DPoS \
                                     (peers={} active_set={})",
                                    bc.height(),
                                    peer_count,
                                    active_set_len
                                );
                                if let Err(e) = bc.activate_voyager() {
                                    tracing::warn!("activate_voyager failed: {}", e);
                                }
                                voyager_activated = true;
                            }
                            Err(reason) => {
                                tracing::error!("{}", reason);
                                // Stay in Pioneer; loop re-checks next tick.
                                // Do NOT call activate_voyager() — chain.db
                                // persistent flag must not get set when the
                                // local node can't safely join BFT.
                            }
                        }
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
                // Pioneer mode: 200ms poll, produce block once per
                // BLOCK_TIME_SECS. This replaces the original 3s fixed
                // sleep which made the effective block time oscillate
                // around 3s instead of the configured 1s.
                // ════════════════════════════════════════════════
                if !voyager_activated {
                    const PIONEER_TICK: tokio::time::Duration =
                        tokio::time::Duration::from_millis(200);
                    tokio::time::sleep(PIONEER_TICK).await;

                    // Gate block production on BLOCK_TIME_SECS so the tighter
                    // poll doesn't produce a burst of sub-second blocks.
                    if pioneer_last_block.elapsed()
                        < tokio::time::Duration::from_secs(BLOCK_TIME_SECS)
                    {
                        continue;
                    }

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
                        pioneer_last_block = tokio::time::Instant::now();
                        // H-09: only broadcast after the block is durably
                        // persisted. A broadcast of a block we can't recover
                        // on restart is a fork risk — peers would accept
                        // blocks extending ours, but after our next restart
                        // we would rewind to the last saved block and
                        // diverge from the chain we just helped produce.
                        if let Err(e) = storage_clone.save_block(&block_to_save) {
                            tracing::error!(
                                "H-09: failed to persist block {} produced by {}: {}; \
                                 skipping broadcast to prevent fork",
                                height,
                                wallet.address,
                                e
                            );
                        } else {
                            println!("Block {} produced by {}", height, wallet.address);
                            {
                                let bc = shared_clone.read().await;
                                if let Err(e) = storage_clone.save_blockchain(&bc) {
                                    tracing::warn!(
                                        "save_blockchain snapshot failed at height {}: {} \
                                         (block already persisted, continuing)",
                                        height,
                                        e
                                    );
                                }
                            }
                            lp2p_clone.broadcast_block(&block_to_save).await;
                        }
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
                    // C-01: sign RoundStatus before broadcast. Unsigned statuses
                    // are rejected at the network boundary.
                    let mut status = bft.build_round_status();
                    status.sign(&validator_secret_key);
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
                    // P1: refuse to start a BFT round when the active set is
                    // too small for byzantine-fault tolerance. BFT requires
                    // N ≥ 4 for f = ⌊(N-1)/3⌋ ≥ 1 — at N < 4 a single
                    // byzantine validator cannot be tolerated, so running
                    // BFT is worse than PoA fallback. We log and skip this
                    // iteration instead of initialising the engine; the
                    // outer loop will retry once the active set recovers.
                    {
                        let bc_check = shared_clone.read().await;
                        let active = bc_check.stake_registry.active_count();
                        if active < sentrix::core::staking::MIN_BFT_VALIDATORS {
                            tracing::warn!(
                                "P1: skipping BFT round at height {} — active set \
                                 {} < minimum {} for BFT safety",
                                next_height,
                                active,
                                sentrix::core::staking::MIN_BFT_VALIDATORS
                            );
                            drop(bc_check);
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            continue;
                        }
                    }

                    let mut bft =
                        BftEngine::new(next_height, wallet.address.clone(), total_active_stake);
                    proposed_block = None;
                    // #1d: reset rebroadcast tracking on new height.
                    proposal_broadcast_at = None;
                    proposal_rebroadcast_count = 0;

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
                        // We're the proposer — create block (Voyager: skip Pioneer authority).
                        // V2 M-15 Step 4+5: helper checks locked_proposal_bytes first
                        // and re-broadcasts the cached block if we're locked, which is
                        // what unsticks the chain when an earlier round's prevote
                        // supermajority didn't precommit.
                        let mut bc = shared_clone.write().await;
                        match build_or_reuse_proposal(
                            &bft,
                            &mut bc,
                            &wallet.address,
                            &validator_secret_key,
                            next_height,
                        ) {
                            Some((block, proposal)) => {
                                let block_hash = block.hash.clone();
                                let block_data = proposal.block_data.clone();
                                proposed_block = Some(block);
                                drop(bc);

                                // Broadcast signed proposal to peers
                                lp2p_clone.broadcast_bft_proposal(&proposal).await;
                                // #1d rebroadcast tracking: record when we sent
                                // the proposal so the tick can retry after a few
                                // seconds if prevote supermajority isn't reached.
                                proposal_broadcast_at = Some(std::time::Instant::now());
                                proposal_rebroadcast_count = 0;

                                // V2 M-15: stash bytes so if prevote-supermajority
                                // forms on this hash, they get promoted into
                                // locked_block for a future round's re-propose.
                                bft.stash_proposal_bytes(&block_hash, block_data);
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

                                                // P1 (write-lock split): pre-validate under
                                                // a read lock so an invalid finalized block
                                                // is rejected without blocking RPC readers
                                                // behind the write lock for the ~50ms of
                                                // signature verification + state lookups.
                                                {
                                                    let bc_read = shared_clone.read().await;
                                                    if let Err(e) = bc_read.validate_block(&blk) {
                                                        drop(bc_read);
                                                        tracing::warn!(
                                                            "BFT finalize: pre-validate \
                                                             rejected block {}: {}",
                                                            blk.index,
                                                            e
                                                        );
                                                        break;
                                                    }
                                                }

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
                                                        // #253: liveness-signers bug — the old
                                                        // `signers = vec![proposer]` marked every
                                                        // non-proposer as MISSED each block, so on
                                                        // a 4-validator BFT chain each validator
                                                        // signed only 25% of blocks vs the 30%
                                                        // MIN_SIGNED_PER_WINDOW threshold, driving
                                                        // deterministic cascade-jail every 14400
                                                        // blocks (~80min). Correct model: every
                                                        // precommit signer in the justification
                                                        // signed the block, not just the proposer.
                                                        let signers: Vec<String> = justification
                                                            .precommits
                                                            .iter()
                                                            .map(|p| p.validator.clone())
                                                            .collect();
                                                        bc.slashing.record_block_signatures(
                                                            &active, &signers, height,
                                                        );

                                                        // V4 Step 2: pay every signer pro-rata
                                                        // by stake, not just the proposer. Extract
                                                        // (validator, stake_weight) tuples from the
                                                        // justification's precommit list.
                                                        let reward_signers: Vec<(String, u64)> =
                                                            justification
                                                                .precommits
                                                                .iter()
                                                                .map(|p| (p.validator.clone(), p.stake_weight))
                                                                .collect();
                                                        let validator_fee = 0;
                                                        let _ =
                                                            bc.stake_registry.distribute_reward(
                                                                &proposer,
                                                                &reward_signers,
                                                                reward,
                                                                validator_fee,
                                                            );

                                                        if sentrix::core::epoch::EpochManager::is_epoch_boundary(height) {
                                                            tracing::info!("Epoch boundary at height {} — transitioning", height);
                                                            let released = bc.stake_registry.process_unbonding(height);
                                                            for (delegator, amount) in &released {
                                                                // V4 Step 3: post-reward-v2 fork, unbonded
                                                                // stake returns from treasury (where it
                                                                // was escrowed on Delegate), not a fresh
                                                                // mint. Pre-fork path keeps legacy credit
                                                                // behaviour for existing chain.db state.
                                                                let r = if Blockchain::is_reward_v2_height(height) {
                                                                    bc.accounts.transfer(
                                                                        PROTOCOL_TREASURY,
                                                                        delegator,
                                                                        *amount,
                                                                        0,
                                                                    )
                                                                } else {
                                                                    bc.accounts.credit(delegator, *amount)
                                                                };
                                                                r.unwrap_or_else(|e| tracing::warn!("unbonding release failed: {}", e));
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
                                                            // H-09: persist before broadcast.
                                                            if let Err(e) = storage_clone
                                                                .save_block(saved_block)
                                                            {
                                                                tracing::error!(
                                                                    "H-09: failed to persist \
                                                                     BFT block {} by {}: {}; \
                                                                     skipping broadcast",
                                                                    height,
                                                                    proposer,
                                                                    e
                                                                );
                                                            } else {
                                                                println!(
                                                                    "Block {} produced by {}",
                                                                    height, proposer
                                                                );
                                                                let bc = shared_clone.read().await;
                                                                if let Err(e) = storage_clone
                                                                    .save_blockchain(&bc)
                                                                {
                                                                    tracing::warn!(
                                                                        "save_blockchain \
                                                                         snapshot failed at \
                                                                         {}: {}",
                                                                        height,
                                                                        e
                                                                    );
                                                                }
                                                                drop(bc);
                                                                lp2p_clone
                                                                    .broadcast_block(saved_block)
                                                                    .await;
                                                            }
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
                                            // P1: re-propose if we are the proposer for the
                                            // new round. Without this the testnet stalls at
                                            // a height indefinitely: the proposer for the
                                            // new round never emits a proposal, peers prevote
                                            // nil, precommit nil, skip-round, and loop.
                                            let bc_r = shared_clone.read().await;
                                            let we_propose = bft.is_proposer(&bc_r.stake_registry);
                                            drop(bc_r);
                                            if we_propose {
                                                let mut bc = shared_clone.write().await;
                                                if let Some((block, proposal)) =
                                                    build_or_reuse_proposal(
                                                        &bft,
                                                        &mut bc,
                                                        &wallet.address,
                                                        &validator_secret_key,
                                                        bft.height(),
                                                    )
                                                {
                                                    let block_hash = block.hash.clone();
                                                    let block_data = proposal.block_data.clone();
                                                    drop(bc);
                                                    lp2p_clone
                                                        .broadcast_bft_proposal(&proposal)
                                                        .await;
                                                    proposal_broadcast_at =
                                                        Some(std::time::Instant::now());
                                                    proposal_rebroadcast_count = 0;
                                                    proposed_block = Some(block);
                                                    bft.stash_proposal_bytes(
                                                        &block_hash,
                                                        block_data,
                                                    );
                                                    let _ = bft.on_own_proposal(&block_hash);
                                                    tracing::info!(
                                                        "BFT: proposed block after timeout \
                                                         at round {}",
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
                                            // P1: re-propose on skip-round if we are the new
                                            // round's proposer. Same stall pattern as above.
                                            let bc_r = shared_clone.read().await;
                                            let we_propose = bft.is_proposer(&bc_r.stake_registry);
                                            drop(bc_r);
                                            if we_propose {
                                                let mut bc = shared_clone.write().await;
                                                if let Some((block, proposal)) =
                                                    build_or_reuse_proposal(
                                                        &bft,
                                                        &mut bc,
                                                        &wallet.address,
                                                        &validator_secret_key,
                                                        bft.height(),
                                                    )
                                                {
                                                    let block_hash = block.hash.clone();
                                                    let block_data = proposal.block_data.clone();
                                                    drop(bc);
                                                    lp2p_clone
                                                        .broadcast_bft_proposal(&proposal)
                                                        .await;
                                                    proposal_broadcast_at =
                                                        Some(std::time::Instant::now());
                                                    proposal_rebroadcast_count = 0;
                                                    proposed_block = Some(block);
                                                    bft.stash_proposal_bytes(
                                                        &block_hash,
                                                        block_data,
                                                    );
                                                    let _ = bft.on_own_proposal(&block_hash);
                                                    tracing::info!(
                                                        "BFT: proposed block after skip-round \
                                                         at round {}",
                                                        bft.round()
                                                    );
                                                }
                                            }
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
                            None => {
                                // build_or_reuse_proposal already tracing::warn!'d
                                // the specific failure reason (deserialize of
                                // cached bytes or create_block_voyager Err).
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
                                // Signature + validator-set membership are
                                // now enforced at the libp2p network boundary
                                // (see `is_active_bft_signer` in libp2p_node.rs);
                                // by construction every proposal reaching this
                                // point has already passed both checks.
                                if let Ok(block) =
                                    bincode::deserialize::<Block>(&proposal.block_data)
                                {
                                    proposed_block = Some(block);
                                    // V2 M-15 Step 4: stash the block bytes so
                                    // if prevote-supermajority forms on this
                                    // proposal's hash (Step 3 in engine.rs),
                                    // they get promoted into locked_block and
                                    // remain available for re-propose when we
                                    // become proposer in a later round at this
                                    // height.
                                    bft.stash_proposal_bytes(
                                        &proposal.block_hash,
                                        proposal.block_data.clone(),
                                    );
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
                            // Messages reaching this point have already been
                            // signature-verified AND membership-checked at the
                            // libp2p network boundary (C-01 gaps 1/2/3).
                            BftMessage::Prevote(prevote) => {
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
                                let bc = shared_clone.read().await;
                                let stake = bc
                                    .stake_registry
                                    .get_validator(&precommit.validator)
                                    .map(|v| v.total_stake())
                                    .unwrap_or(0);
                                drop(bc);
                                bft.on_precommit_weighted(&precommit, stake)
                            }
                            BftMessage::RoundStatus(status) => {
                                let bc = shared_clone.read().await;
                                let stake = bc
                                    .stake_registry
                                    .get_validator(&status.validator)
                                    .map(|v| v.total_stake())
                                    .unwrap_or(0);
                                drop(bc);
                                bft.on_round_status_weighted(&status, stake)
                            }
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

                                        // P1: pre-validate under read lock (see P1-A
                                        // path above for rationale).
                                        {
                                            let bc_read = shared_clone.read().await;
                                            if let Err(e) = bc_read.validate_block(&blk) {
                                                drop(bc_read);
                                                tracing::warn!(
                                                    "BFT finalize: pre-validate rejected \
                                                     block {}: {}",
                                                    blk.index,
                                                    e
                                                );
                                                break;
                                            }
                                        }

                                        let mut bc = shared_clone.write().await;
                                        match bc.add_block(blk) {
                                            Ok(()) => {
                                                let updated = bc.latest_block().ok().cloned();

                                                // ── Post-block Voyager bookkeeping ──
                                                let reward = bc.get_block_reward();
                                                bc.epoch_manager.record_block(reward);

                                                let active = bc.stake_registry.active_set.clone();
                                                // #253: see the sibling site above for rationale.
                                                // Peer-finalize branch — same fix, same model.
                                                let signers: Vec<String> = justification
                                                    .precommits
                                                    .iter()
                                                    .map(|p| p.validator.clone())
                                                    .collect();
                                                bc.slashing.record_block_signatures(
                                                    &active, &signers, height,
                                                );

                                                // V4 Step 2 — see sibling site above for rationale.
                                                let reward_signers: Vec<(String, u64)> =
                                                    justification
                                                        .precommits
                                                        .iter()
                                                        .map(|p| (p.validator.clone(), p.stake_weight))
                                                        .collect();
                                                let validator_fee = 0;
                                                let _ = bc.stake_registry.distribute_reward(
                                                    &proposer,
                                                    &reward_signers,
                                                    reward,
                                                    validator_fee,
                                                );

                                                if sentrix::core::epoch::EpochManager::is_epoch_boundary(height) {
                                                    tracing::info!("Epoch boundary at height {} — transitioning", height);
                                                    let released = bc.stake_registry.process_unbonding(height);
                                                    for (delegator, amount) in &released {
                                                        // V4 Step 3 — mirror of the self-produced
                                                        // finalize handler above; unbonded stake
                                                        // returns from treasury post-fork.
                                                        let r = if Blockchain::is_reward_v2_height(height) {
                                                            bc.accounts.transfer(
                                                                PROTOCOL_TREASURY,
                                                                delegator,
                                                                *amount,
                                                                0,
                                                            )
                                                        } else {
                                                            bc.accounts.credit(delegator, *amount)
                                                        };
                                                        r.unwrap_or_else(|e| tracing::warn!("unbonding release failed: {}", e));
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
                                                    // H-09: persist before broadcast.
                                                    if let Err(e) =
                                                        storage_clone.save_block(saved_block)
                                                    {
                                                        tracing::error!(
                                                            "H-09: failed to persist BFT block \
                                                             {} by {}: {}; skipping broadcast",
                                                            height,
                                                            proposer,
                                                            e
                                                        );
                                                    } else {
                                                        println!(
                                                            "Block {} produced by {}",
                                                            height, proposer
                                                        );
                                                        let bc = shared_clone.read().await;
                                                        if let Err(e) =
                                                            storage_clone.save_blockchain(&bc)
                                                        {
                                                            tracing::warn!(
                                                                "save_blockchain snapshot \
                                                                 failed at {}: {}",
                                                                height,
                                                                e
                                                            );
                                                        }
                                                        drop(bc);
                                                        lp2p_clone
                                                            .broadcast_block(saved_block)
                                                            .await;
                                                    }
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
                                        if let Some((block, proposal)) = build_or_reuse_proposal(
                                            bft,
                                            &mut bc,
                                            &wallet.address,
                                            &validator_secret_key,
                                            bft.height(),
                                        ) {
                                            let block_hash = block.hash.clone();
                                            let block_data = proposal.block_data.clone();
                                            drop(bc);
                                            lp2p_clone.broadcast_bft_proposal(&proposal).await;
                                            proposal_broadcast_at = Some(std::time::Instant::now());
                                            proposal_rebroadcast_count = 0;
                                            proposed_block = Some(block);
                                            bft.stash_proposal_bytes(&block_hash, block_data);
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
                                        if let Some((block, proposal)) = build_or_reuse_proposal(
                                            bft,
                                            &mut bc,
                                            &wallet.address,
                                            &validator_secret_key,
                                            bft.height(),
                                        ) {
                                            let block_hash = block.hash.clone();
                                            let block_data = proposal.block_data.clone();
                                            drop(bc);
                                            lp2p_clone.broadcast_bft_proposal(&proposal).await;
                                            proposal_broadcast_at = Some(std::time::Instant::now());
                                            proposal_rebroadcast_count = 0;
                                            proposed_block = Some(block);
                                            bft.stash_proposal_bytes(&block_hash, block_data);
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

                    // #1d rebroadcast (v2.1.4 — extended after first round of
                    // testnet bake showed 3 attempts × 3s = 9s isn't enough to
                    // catch persistently-late peers). The shape we kept seeing:
                    // proposer fires the proposal, 2 of 4 peers prevote it in
                    // time, 1 peer takes ~10s before it shows up in
                    // `verified_peers` post-restart, by which time the
                    // already-fast peers have already nil-precommit'd because
                    // their prevote window closed. v2.1.4 widens the retry
                    // window to 14s (7 × 2s) so a slow peer has a real chance
                    // to enter `verified_peers` during the proposer's send loop
                    // before the proposer's own propose timeout fires (20s).
                    // Stays in Propose AND Prevote phases — sometimes peers
                    // need the proposal even after we've moved to prevote
                    // collection so they can validate the prevotes they're
                    // receiving from us.
                    const REBROADCAST_INTERVAL: std::time::Duration =
                        std::time::Duration::from_secs(2);
                    const MAX_REBROADCASTS: u32 = 7;
                    if let Some(ref block) = proposed_block
                        && matches!(bft.phase(), BftPhase::Propose | BftPhase::Prevote)
                        && proposal_rebroadcast_count < MAX_REBROADCASTS
                        && proposal_broadcast_at
                            .is_some_and(|t| t.elapsed() >= REBROADCAST_INTERVAL)
                    {
                        let block_hash = block.hash.clone();
                        let block_data = bincode::serialize(block).unwrap_or_default();
                        let mut proposal = Proposal {
                            height: bft.height(),
                            round: bft.round(),
                            block_hash,
                            block_data,
                            proposer: wallet.address.clone(),
                            signature: vec![],
                        };
                        proposal.sign(&validator_secret_key);
                        lp2p_clone.broadcast_bft_proposal(&proposal).await;
                        proposal_broadcast_at = Some(std::time::Instant::now());
                        proposal_rebroadcast_count += 1;
                        tracing::info!(
                            "BFT #1d: rebroadcast proposal at height={} round={} attempt={}/{}",
                            bft.height(),
                            bft.round(),
                            proposal_rebroadcast_count,
                            MAX_REBROADCASTS
                        );
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
                                    // P1: re-propose if we are the new-round proposer.
                                    // Without this the testnet stalls indefinitely —
                                    // the new round has no proposal, peers prevote nil,
                                    // precommit nil, skip-round, and loop.
                                    let bc_r = shared_clone.read().await;
                                    let we_propose = bft.is_proposer(&bc_r.stake_registry);
                                    drop(bc_r);
                                    if we_propose {
                                        let mut bc = shared_clone.write().await;
                                        if let Some((block, proposal)) = build_or_reuse_proposal(
                                            bft,
                                            &mut bc,
                                            &wallet.address,
                                            &validator_secret_key,
                                            bft.height(),
                                        ) {
                                            let block_hash = block.hash.clone();
                                            let block_data = proposal.block_data.clone();
                                            drop(bc);
                                            lp2p_clone.broadcast_bft_proposal(&proposal).await;
                                            proposal_broadcast_at = Some(std::time::Instant::now());
                                            proposal_rebroadcast_count = 0;
                                            proposed_block = Some(block);
                                            bft.stash_proposal_bytes(&block_hash, block_data);
                                            let _ = bft.on_own_proposal(&block_hash);
                                            tracing::info!(
                                                "BFT: proposed block after timeout at round {}",
                                                bft.round()
                                            );
                                        }
                                    }
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
                                    // P1: re-propose on skip-round if we are the new
                                    // round's proposer.
                                    let bc_r = shared_clone.read().await;
                                    let we_propose = bft.is_proposer(&bc_r.stake_registry);
                                    drop(bc_r);
                                    if we_propose {
                                        let mut bc = shared_clone.write().await;
                                        if let Some((block, proposal)) = build_or_reuse_proposal(
                                            bft,
                                            &mut bc,
                                            &wallet.address,
                                            &validator_secret_key,
                                            bft.height(),
                                        ) {
                                            let block_hash = block.hash.clone();
                                            let block_data = proposal.block_data.clone();
                                            drop(bc);
                                            lp2p_clone.broadcast_bft_proposal(&proposal).await;
                                            proposal_broadcast_at = Some(std::time::Instant::now());
                                            proposal_rebroadcast_count = 0;
                                            proposed_block = Some(block);
                                            bft.stash_proposal_bytes(&block_hash, block_data);
                                            let _ = bft.on_own_proposal(&block_hash);
                                            tracing::info!(
                                                "BFT: proposed block after skip-round at \
                                                 round {}",
                                                bft.round()
                                            );
                                        }
                                    }
                                    break;
                                }
                                _ => break,
                            }
                        }
                    }
                }
            }
        }))
    } else {
        None
    };

    // Event handler — persist P2P blocks to MDBX + forward BFT events
    // Sync is handled inside the libp2p swarm task (Step 3d).
    let storage_for_p2p = storage.clone();
    let bft_tx_clone = bft_tx;
    let lp2p_for_events = lp2p.clone();
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                NodeEvent::PeerConnected(addr) => tracing::info!("Peer connected: {}", addr),
                NodeEvent::PeerDisconnected(addr) => tracing::info!("Peer disconnected: {}", addr),
                NodeEvent::NewBlock(block) => {
                    tracing::info!("Block {} received from peer", block.index);
                    if let Err(e) = storage_for_p2p.save_block(&block) {
                        // BACKLOG #16: a `warn` here was silent enough that the
                        // 2026-04-20-era mainnet chain.db ended up with 7,352
                        // missing `block:N` TABLE_META keys (longest contiguous
                        // run 5,042 blocks at h=139,703 per PR #226's sweep
                        // test). Root cause pattern: the block IS already
                        // applied to in-memory state via
                        // `add_block_from_peer` in the spawned gossip task
                        // (libp2p_node.rs:675) BEFORE this handler runs — so
                        // by the time save_block fails here, the chain has
                        // already advanced. If MDBX writes fail for a
                        // contiguous window, CHAIN_WINDOW_SIZE (1000 blocks)
                        // later rolls that block out of in-memory history
                        // too, leaving a permanent gap invisible to any
                        // validator that restarts.
                        //
                        // ERROR level surfaces the failure to journalctl +
                        // any grep/alert. Incrementing `PEER_BLOCK_SAVE_FAILS`
                        // lets Prometheus alert on `rate(... > 0)` — gap
                        // gets caught at the moment of accident, not weeks
                        // later via sweep test.
                        //
                        // Durable fix is making `add_block_from_peer` atomic
                        // with `save_block` (apply rolls back on persist
                        // failure). That needs storage plumbing into
                        // sentrix-core and is out of scope for this observability
                        // patch.
                        tracing::error!(
                            "BACKLOG #16: failed to persist P2P block {} (hash={}): {}. \
                             Chain state has ALREADY advanced in memory — this will \
                             leave a permanent TABLE_META gap once CHAIN_WINDOW_SIZE \
                             rolls past. Check MDBX disk / lock / permissions.",
                            block.index,
                            block.hash,
                            e
                        );
                        sentrix::api::routes::ops::PEER_BLOCK_SAVE_FAILS
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
                NodeEvent::NewTransaction(_) => {}
                NodeEvent::SyncNeeded {
                    peer_addr,
                    peer_height,
                } => {
                    tracing::info!("Sync needed from {} (height: {})", peer_addr, peer_height);
                    // Backlog #4 auto-resync: BFT RoundStatus gossip told us a
                    // peer is at a higher height. Request blocks right now
                    // instead of waiting up to 30s for the periodic
                    // sync_interval tick. If the trigger is dropped (channel
                    // closed), we simply fall back to the periodic path —
                    // no error surfacing needed for that case.
                    lp2p_for_events.trigger_sync().await;
                }
                // BFT events — forward to validator loop for multi-validator consensus.
                //
                // C-07: do not swallow SendError. `send` returns Err only if
                // the receiver has been dropped (i.e. the validator loop has
                // exited), so every BFT message after that point is
                // unreachable and consensus on this node is effectively
                // halted. Log at ERROR so the failure is visible in
                // journalctl and operators can restart the node instead of
                // silently dropping votes/proposals.
                NodeEvent::BftProposal(p) => {
                    tracing::info!(
                        "BFT proposal: height={} round={} proposer={} block_hash={}",
                        p.height,
                        p.round,
                        &p.proposer[..p.proposer.len().min(12)],
                        &p.block_hash[..p.block_hash.len().min(16)]
                    );
                    if let Err(e) = bft_tx_clone
                        .send(sentrix::core::bft_messages::BftMessage::Propose(p))
                        .await
                    {
                        tracing::error!(
                            "C-07: BFT proposal forward failed (validator loop gone): {}",
                            e
                        );
                    }
                }
                NodeEvent::BftPrevote(v) => {
                    let hash_tag = match &v.block_hash {
                        Some(h) => format!("block={}", &h[..h.len().min(16)]),
                        None => "block=nil".to_string(),
                    };
                    tracing::info!(
                        "BFT prevote: height={} round={} from={} {}",
                        v.height,
                        v.round,
                        &v.validator[..v.validator.len().min(12)],
                        hash_tag
                    );
                    if let Err(e) = bft_tx_clone
                        .send(sentrix::core::bft_messages::BftMessage::Prevote(v))
                        .await
                    {
                        tracing::error!(
                            "C-07: BFT prevote forward failed (validator loop gone): {}",
                            e
                        );
                    }
                }
                NodeEvent::BftPrecommit(c) => {
                    let hash_tag = match &c.block_hash {
                        Some(h) => format!("block={}", &h[..h.len().min(16)]),
                        None => "block=nil".to_string(),
                    };
                    tracing::info!(
                        "BFT precommit: height={} round={} from={} {}",
                        c.height,
                        c.round,
                        &c.validator[..c.validator.len().min(12)],
                        hash_tag
                    );
                    if let Err(e) = bft_tx_clone
                        .send(sentrix::core::bft_messages::BftMessage::Precommit(c))
                        .await
                    {
                        tracing::error!(
                            "C-07: BFT precommit forward failed (validator loop gone): {}",
                            e
                        );
                    }
                }
                NodeEvent::BftRoundStatus(s) => {
                    tracing::debug!(
                        "BFT round-status: height={} round={} from={}",
                        s.height,
                        s.round,
                        &s.validator[..s.validator.len().min(12)]
                    );
                    if let Err(e) = bft_tx_clone
                        .send(sentrix::core::bft_messages::BftMessage::RoundStatus(s))
                        .await
                    {
                        tracing::debug!(
                            "C-07: BFT round-status forward failed (validator loop gone): {}",
                            e
                        );
                    }
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

        // 2b. C-08: await the validator task's full exit before saving. The
        //     shutdown flag + write-lock drain above together cover an
        //     in-progress add_block, but a task that is between block
        //     cycles (waiting on bft_rx, inside a BFT message handler, or
        //     just looping) can still mutate `self.accounts` /
        //     `self.contracts` after we snapshot and before the process
        //     dies. Holding the JoinHandle and awaiting it here guarantees
        //     the task is no longer observing shared state when we call
        //     save_blockchain.
        //
        //     Bounded by a timeout so a stuck validator loop can't block
        //     shutdown indefinitely. If the timeout fires we log and fall
        //     through — the state snapshot will still be more consistent
        //     than a SIGKILL mid-commit because step 2 drained the write
        //     lock.
        if let Some(handle) = validator_handle {
            tracing::info!("Graceful shutdown: awaiting validator task exit...");
            match tokio::time::timeout(std::time::Duration::from_secs(10), handle).await {
                Ok(Ok(())) => tracing::info!("Validator task exited cleanly"),
                Ok(Err(join_err)) => {
                    tracing::warn!("C-08: validator task joined with panic: {}", join_err)
                }
                Err(_) => tracing::warn!(
                    "C-08: validator task did not exit within 10s; \
                     proceeding to save state snapshot anyway"
                ),
            }
        }

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

    // P1 RPC security: expose ConnectInfo so extract_client_ip can read
    // the real socket peer address. Without `into_make_service_with_connect_info`
    // the `ConnectInfo<SocketAddr>` extension is never populated, and
    // rate-limit bucketing falls back to "unknown" for every request.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
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
    match bc.get_block_any(index) {
        Some(block) => println!("{}", serde_json::to_string_pretty(&block)?),
        None => println!("Block {} not found", index),
    }
    Ok(())
}

fn cmd_chain_reset_trie() -> anyhow::Result<()> {
    let storage = Storage::open(&get_db_path())?;
    if !storage.has_blockchain() {
        anyhow::bail!("Chain not initialized.");
    }

    // 2026-04-21 mainnet 3-way fork root cause: pre-v2.1.5 `state_import` on
    // production validators reset the trie to empty and re-populated it from
    // the imported account set. The backfilled trie produced a state_root
    // that disagreed with peers whose trie was built incrementally from
    // genesis — silent fork. v2.1.5 added a boot-time backfill-vs-header
    // guard, and PR #206 added a full trie-reachability check, but the
    // cleanest protection is to refuse reset-trie on a production chain
    // in the first place. The rsync-from-peer recovery preserves the
    // incremental trie shape and is the canonical path for mainnet.
    let height = storage
        .load_height()
        .map_err(|e| anyhow::anyhow!("reading chain height: {e}"))?;
    if height > 0 {
        // The env-var escape hatch is checked INSIDE this branch — a prior
        // draft of this guard checked it *after* `bail!` which meant the
        // override was dead code. Keep the check here and nowhere else.
        let override_set = std::env::var("SENTRIX_ALLOW_RESET_TRIE_ON_NONZERO_HEIGHT")
            .map(|v| v == "1")
            .unwrap_or(false);
        if !override_set {
            anyhow::bail!(
                "Refusing reset-trie on a chain at height {height} > 0.\n\
                 This command wipes trie_nodes/trie_values/trie_roots and rebuilds \
                 from AccountDB on next boot. On a chain past genesis the rebuilt \
                 trie CAN differ from peers' incrementally-built tries (see the \
                 2026-04-21 3-way fork incident for what that looks like in prod).\n\
                 \n\
                 Correct recoveries for a damaged trie on a non-genesis chain:\n\
                 1. Stop this node.\n\
                 2. rsync /opt/sentrix/data/chain.db from a healthy peer (all validators stopped).\n\
                 3. Restart. The boot-time integrity check will confirm the copy is intact.\n\
                 \n\
                 If you REALLY need reset-trie on a non-genesis chain (devnet / \
                 isolated testing only), set `SENTRIX_ALLOW_RESET_TRIE_ON_NONZERO_HEIGHT=1` \
                 in your environment. This flag does not exist on release builds \
                 that operators should be using."
            );
        }
        tracing::warn!(
            "reset-trie proceeding on non-zero height (h={height}) via env override — \
             you are on your own; fork is very likely on a shared chain"
        );
    }

    storage.reset_trie()?;
    println!(
        "Trie state cleared. Start the node normally — it will rebuild the trie from AccountDB."
    );
    Ok(())
}

/// Deep cross-table consistency check (issue #268 2026-04-25 RCA).
///
/// Walks every AccountDB entry with balance > 0, computes the expected trie
/// value via `account_value_bytes(balance, nonce)`, and compares to the
/// actual leaf the trie returns for `address_to_key(address)`. Catches
/// mixed-timestamp chain.db produced by rsync-while-live: trie tables and
/// AccountDB at different MDBX commit snapshots, internally inconsistent,
/// boots silently, diverges on first block apply.
///
/// Run with node STOPPED (MDBX is single-writer). Returns exit code 0 on
/// match, 1 on mismatch with a per-address summary on stdout.
fn cmd_chain_verify_deep() -> anyhow::Result<()> {
    use sentrix::core::trie::{account_value_bytes, address_to_key};
    use std::sync::Arc;

    let storage = Storage::open(&get_db_path())?;
    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    let mdbx = storage.mdbx_arc();
    bc.init_trie(Arc::clone(&mdbx))?;

    let height = bc.height();
    let stored_root = bc.trie_root_at(height).map(hex::encode);
    println!("chain height: {height}");
    println!("stored trie root @ height: {:?}", stored_root);

    // First gate: cryptographic relationship within the trie itself.
    // Catches rsync-while-live MDBX corruption where nodes load cleanly but
    // parent-hash relationships are broken — the actual #268 v2.1.21 canary
    // failure mode that the simpler AccountDB ↔ trie consistency check
    // (below) cannot detect.
    if let Some(trie) = bc.state_trie.as_ref() {
        match trie.verify_integrity_strict() {
            Ok(()) => println!("trie strict-integrity: OK (all node hashes match content)"),
            Err(e) => {
                println!("trie strict-integrity: FAIL");
                println!("  {}", e);
                println!();
                println!(
                    "Recovery: this chain.db is unsafe to start. Halt all peer \
                     validators (verify with `pgrep sentrix` returning empty), then \
                     rsync chain.db from a confirmed-halted canonical peer. Re-run \
                     `sentrix chain verify-deep` to confirm clean."
                );
                anyhow::bail!("trie strict-integrity check failed");
            }
        }
    }

    let trie = bc
        .state_trie
        .as_mut()
        .ok_or_else(|| anyhow::anyhow!("trie not initialised"))?;

    let total_accounts = bc.accounts.accounts.len();
    let mut checked = 0usize;
    let mut zero_balance_skipped = 0usize;
    let mut mismatches: Vec<(String, u64, u64, Option<Vec<u8>>)> = Vec::new();

    // Sort for deterministic output.
    let mut entries: Vec<&sentrix::core::account::Account> =
        bc.accounts.accounts.values().collect();
    entries.sort_by(|a, b| a.address.cmp(&b.address));

    for account in entries {
        if account.balance == 0 {
            zero_balance_skipped += 1;
            continue;
        }
        let key = address_to_key(&account.address);
        let expected = account_value_bytes(account.balance, account.nonce);
        let actual = trie.get(&key)?;
        match &actual {
            Some(bytes) if *bytes == expected => {}
            _ => {
                mismatches.push((
                    account.address.clone(),
                    account.balance,
                    account.nonce,
                    actual.clone(),
                ));
            }
        }
        checked += 1;
    }

    println!(
        "scanned {} accounts ({} checked with balance > 0, {} skipped with balance = 0)",
        total_accounts, checked, zero_balance_skipped
    );

    if mismatches.is_empty() {
        println!("VERDICT: trie ↔ AccountDB CONSISTENT");
        Ok(())
    } else {
        println!(
            "VERDICT: {} MISMATCHES — chain.db is internally inconsistent (likely rsync-while-live origin)",
            mismatches.len()
        );
        for (addr, balance, nonce, trie_leaf) in mismatches.iter().take(20) {
            println!(
                "  {} accountdb=(balance={}, nonce={}) trie_leaf={}",
                addr,
                balance,
                nonce,
                trie_leaf
                    .as_ref()
                    .map(hex::encode)
                    .unwrap_or_else(|| "<missing>".to_string())
            );
        }
        if mismatches.len() > 20 {
            println!("  ... and {} more", mismatches.len() - 20);
        }
        println!();
        println!("Recovery: this chain.db is unsafe to start. Halt all peer validators,");
        println!("rsync from a confirmed-halted canonical peer (NOT a live one), then re-run");
        println!("`sentrix chain verify-deep` to confirm clean before starting the validator.");
        anyhow::bail!("trie ↔ AccountDB inconsistency detected");
    }
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

    // 2026-04-21 mainnet 3-way fork root cause: pre-v2.1.5 `state_import` on
    // production validators re-populated the account set without rebuilding
    // the trie identically to peers'. The v2.1.5 trie-reset-on-import fix
    // + v2.1.6 strict state_root enforcement + PR #206 boot-time integrity
    // check now catch the damage, but the safest contract is: never allow
    // state_import on a non-genesis chain at all. On mainnet / an existing
    // network, the right recovery is rsync-from-peer (preserves incremental
    // trie shape, matches peers bit-for-bit). state_import is only correct
    // for fresh genesis bootstrapping or isolated devnet testing.
    let current_height = storage
        .load_height()
        .map_err(|e| anyhow::anyhow!("reading chain height: {e}"))?;
    if current_height > 0 {
        // Env override check lives INSIDE this branch. A prior draft ordered
        // `bail!` first and the override after, making the override dead code.
        let override_set = std::env::var("SENTRIX_ALLOW_STATE_IMPORT_ON_NONZERO_HEIGHT")
            .map(|v| v == "1")
            .unwrap_or(false);
        if !override_set {
            anyhow::bail!(
                "Refusing state import on a chain at height {current_height} > 0.\n\
                 This command wipes and rebuilds AccountDB from the snapshot, \
                 then resets the trie so init_trie rebuilds it on next boot. On \
                 a chain past genesis that rebuild CAN produce a state_root that \
                 disagrees with peers who built their trie incrementally block by \
                 block (see the 2026-04-21 3-way fork incident for what that \
                 looks like — took ~30h to recover).\n\
                 \n\
                 Correct recoveries on a non-genesis chain:\n\
                 1. Stop this node.\n\
                 2. rsync /opt/sentrix/data/chain.db from a healthy peer (all validators stopped).\n\
                 3. Restart. Boot-time integrity check confirms the copy is intact.\n\
                 \n\
                 If you really need state_import on a non-genesis chain (isolated \
                 devnet / one-off testing only), set `SENTRIX_ALLOW_STATE_IMPORT_ON_NONZERO_HEIGHT=1` \
                 in your environment. There is no supported use of this flag on a shared chain."
            );
        }
        tracing::warn!(
            "state import proceeding on non-zero height (h={current_height}) via env override — \
             fork is very likely on a shared chain"
        );
    }

    let mut bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized."))?;

    let count = bc.import_state(&snapshot)?;
    storage.save_blockchain(&bc)?;

    // ROOT CAUSE fix (2026-04-21 deploy rollback post-mortem): import only
    // rewrites `accounts` and counters. The trie storage (trie_nodes +
    // trie_values + trie_roots MDBX tables) is untouched. On the next
    // `sentrix start`, `init_trie` finds the existing committed root for
    // the current height + its nodes still present → uses the stale trie
    // that reflects the PRE-import accounts. Every block applied after
    // restart then computes a state_root from the stale trie, diverging
    // from peers whose trie matches their (non-imported) accounts. The
    // `#1e strict reject` guard fires and the chain halts.
    //
    // Resetting the trie here forces `init_trie` to backfill from the
    // freshly imported accounts on next startup. The backfill produces
    // the SAME root any validator would compute from the same account
    // set, restoring cross-validator determinism.
    storage.reset_trie()?;

    println!(
        "State imported: {} accounts from snapshot at height {}",
        count, snapshot.metadata.height
    );
    println!("Trie storage reset — next startup will rebuild it from the imported accounts.");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// L2 gate: 4-validator mesh requires 3 peers (active_set.len() - 1).
    /// 2026-04-25 incident reproduction — VPS5 had 1 peer, would have
    /// been blocked by this check.
    #[test]
    fn peer_mesh_gate_blocks_partitioned_validator() {
        let result = check_bft_peer_mesh_eligible(1, 4, false);
        assert!(result.is_err(), "1 peer in 4-val mesh must be rejected");
        let msg = result.unwrap_err();
        assert!(msg.contains("need ≥3"), "error must state requirement: {msg}");
        assert!(msg.contains("have 1"), "error must state actual count: {msg}");
    }

    /// Healthy fully-meshed 4-validator chain: 3 peers passes.
    #[test]
    fn peer_mesh_gate_allows_fully_meshed_validator() {
        assert!(check_bft_peer_mesh_eligible(3, 4, false).is_ok());
    }

    /// Above-threshold (more peers than active set members - 1) is also fine
    /// — non-validator peers count toward the libp2p peer count too.
    #[test]
    fn peer_mesh_gate_allows_extra_peers() {
        assert!(check_bft_peer_mesh_eligible(10, 4, false).is_ok());
    }

    /// Operator emergency override (`SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS=1`)
    /// must bypass the gate even with zero peers. Re-creates the
    /// 2026-04-25 livelock condition deliberately — used only when an
    /// operator decides the partition risk is acceptable.
    #[test]
    fn peer_mesh_gate_force_override_allows_zero_peers() {
        assert!(check_bft_peer_mesh_eligible(0, 4, true).is_ok());
    }

    /// Single-validator chain (testnet bootstrap, recovery scenario):
    /// peer count is trivially satisfied because `active_set - 1 == 0`.
    #[test]
    fn peer_mesh_gate_single_validator_chain_always_passes() {
        assert!(check_bft_peer_mesh_eligible(0, 1, false).is_ok());
    }

    // Note: a previous test asserted `check_bft_peer_mesh_eligible(0, 0, false).is_ok()`
    // — that test was based on the original `<= 1` short-circuit, which masked
    // the real bug of an empty active_set reaching activation. Replaced by
    // `peer_mesh_gate_empty_active_set_errors_explicitly` below.

    /// 2-validator chain edge case: `active_set - 1 == 1` peer required.
    #[test]
    fn peer_mesh_gate_two_validator_chain() {
        assert!(check_bft_peer_mesh_eligible(0, 2, false).is_err());
        assert!(check_bft_peer_mesh_eligible(1, 2, false).is_ok());
    }

    /// Boundary: peer_count exactly equal to threshold passes.
    #[test]
    fn peer_mesh_gate_boundary_equal_passes() {
        assert!(check_bft_peer_mesh_eligible(3, 4, false).is_ok());
    }

    /// Boundary: one below threshold fails.
    #[test]
    fn peer_mesh_gate_boundary_below_fails() {
        assert!(check_bft_peer_mesh_eligible(2, 4, false).is_err());
    }

    /// Empty active_set produces explicit error (post-self-review fix).
    /// The `<= 1` shortcut was previously silently passing this case,
    /// masking a potential DPoS-migration bug where stake_registry ends
    /// up empty post-migration.
    #[test]
    fn peer_mesh_gate_empty_active_set_errors_explicitly() {
        let result = check_bft_peer_mesh_eligible(0, 0, false);
        assert!(result.is_err(), "empty active_set must return error");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("active_set is empty"),
            "error must point at empty-active-set bug: {msg}"
        );
    }

    /// Strict env-var check: only literal `"1"` enables override.
    /// Empty string (`SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS=` from a
    /// shell `VAR=$missing` typo) must NOT silently disable the gate.
    /// This is the post-self-review fix — `.is_ok()` was accepting any
    /// set value including empty, defeating the safety net during
    /// exactly the operational scenarios it exists to protect.
    #[test]
    fn force_override_strict_check_rejects_empty_string() {
        // Sandbox the env var so this test doesn't pollute the global
        // state — set it to empty, run the check, then unset.
        // SAFETY: tests run sequentially in this module by default
        // (Cargo's per-binary test harness uses a single thread per
        // test by default; #[test] without #[tokio::test(flavor)]
        // means single-threaded). If any future test parallelism is
        // introduced, this needs a mutex.
        unsafe { std::env::set_var("SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS", "") };
        assert!(
            !force_bft_insufficient_peers_set(),
            "empty string must NOT enable override"
        );

        unsafe { std::env::set_var("SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS", "true") };
        assert!(
            !force_bft_insufficient_peers_set(),
            "non-1 value must NOT enable override"
        );

        unsafe { std::env::set_var("SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS", "1") };
        assert!(
            force_bft_insufficient_peers_set(),
            "literal '1' must enable override"
        );

        unsafe { std::env::remove_var("SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS") };
        assert!(
            !force_bft_insufficient_peers_set(),
            "unset env var must NOT enable override"
        );
    }
}
