// main.rs - Sentrix CLI entry point
#![allow(missing_docs)]

use clap::{Parser, Subcommand};
use libp2p::Multiaddr;
use sentrix::api::events::EventBus;
use sentrix::api::routes::{SharedState, create_router_with_bus};
use sentrix::core::blockchain::{BLOCK_TIME_SECS, Blockchain};
use sentrix::network::libp2p_node::{LibP2pNode, make_multiaddr};
use sentrix::network::node::{DEFAULT_PORT, NodeEvent};
use sentrix::storage::db::Storage;
use sentrix::wallet::keystore::Keystore;
use sentrix::wallet::wallet::Wallet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;

mod commands;

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

pub(crate) fn get_db_path() -> String {
    get_data_dir()
        .join("chain.db")
        .to_str()
        .unwrap_or("data/chain.db")
        .to_string()
}

pub(crate) fn get_wallets_dir() -> String {
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
    required_peers: usize,
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
    // `required_peers` is now passed by caller so the gate can be
    // fork-relaxed independently of the function (BFT_GATE_RELAX_HEIGHT).
    // Pre-fork: caller passes `active_set_len - 1` (need full mesh).
    // Post-fork: caller passes `min_active_for_bft - 1` (need supermajority
    // mesh, allows 1-jail tolerance for N=4).
    if peer_count < required_peers {
        return Err(format!(
            "BFT activation blocked: need ≥{required_peers} libp2p peers \
             (active_set={active_set_len}), have {peer_count}. \
             Verify --peers / wait for L1 multiaddr gossip."
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

/// Path to the persisted L1 advertisement sequence counter. Stored
/// inside the data directory so each chain.db has its own sequence
/// space (testnet and mainnet validators on the same host don't share
/// state). Plain decimal-text format — easy to inspect via `cat`,
/// easy to bump manually for ops emergencies.
fn advert_sequence_path() -> std::path::PathBuf {
    get_data_dir().join(".advert-sequence")
}

/// Load the persisted advertisement sequence from disk. Returns 0 on
/// any failure (missing file on first run, parse error, IO error) —
/// the broadcast logic uses `saturating_add(1)` so 0 simply means
/// "start at 1 on first broadcast." Self-review found this critical:
/// without persistence, a validator restart resets sequence to 0;
/// peers cached `seq=N` from the previous lifetime silently drop the
/// new `seq=1` broadcast (newer-wins semantics). The validator's
/// updated multiaddrs would then take ~N broadcasts × 10min to
/// propagate, breaking IP-rotation recovery.
fn load_advert_sequence() -> u64 {
    let path = advert_sequence_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Persist the current advertisement sequence atomically. Writes to a
/// temp file then renames over the target so a crash mid-write doesn't
/// leave a truncated file that would parse to a smaller value (which
/// would re-introduce the regression bug).
fn store_advert_sequence(seq: u64) {
    let path = advert_sequence_path();
    let Some(parent) = path.parent() else { return };
    if let Err(e) = std::fs::create_dir_all(parent) {
        tracing::debug!("advert sequence: mkdir failed: {}", e);
        return;
    }
    let tmp = path.with_extension("tmp");
    if let Err(e) = std::fs::write(&tmp, seq.to_string()) {
        tracing::debug!("advert sequence: write tmp failed: {}", e);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        tracing::debug!("advert sequence: rename failed: {}", e);
    }
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
    /// Staking operations — proper TX-based path for validator + delegator
    /// state changes (register, add-self-stake, unjail, claim-rewards).
    ///
    /// Unlike `sentrix validator unjail` / `force-unjail` which mutate
    /// `stake_registry` directly in MDBX without updating `state_trie`
    /// (creating a one-way trap that needs cluster-wide trie reconciliation
    /// to recover), every command here builds a signed transaction, injects
    /// into mempool, and lets the chain's normal apply path execute the op
    /// — so `state_trie` stays consistent on every peer.
    Staking {
        #[command(subcommand)]
        action: StakingCommands,
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
enum StakingCommands {
    /// Register the sender as a validator. The sender wallet must hold
    /// `self_stake + fee` SRX; on apply, `self_stake` is escrowed to
    /// `PROTOCOL_TREASURY` and the sender enters the candidate pool.
    /// Active set entry happens at the next epoch boundary if total
    /// stake ranks in the top 21.
    Register {
        /// Path to the sender's keystore file (Argon2id v2 format).
        /// Password from `SENTRIX_WALLET_PASSWORD` env var or stdin prompt.
        #[arg(long)]
        keystore: String,
        /// Self-stake in whole SRX (must be >= 15000 for current MIN_SELF_STAKE).
        #[arg(long)]
        self_stake: u64,
        /// Commission rate in basis points (1000 = 10%, max 10000 = 100%).
        #[arg(long)]
        commission_rate: u16,
        /// Tx fee in sentri (1 SRX = 100_000_000 sentri).
        #[arg(long, default_value_t = 10_000)]
        fee: u64,
    },
    /// Top up the sender's self_stake by `amount` SRX. Common use is
    /// unblocking a jailed validator whose self_stake fell below
    /// `MIN_SELF_STAKE` after a downtime slash.
    ///
    /// **Dispatch is fork-gated**: `ADD_SELF_STAKE_HEIGHT` defaults to
    /// `u64::MAX` (dormant). Operator must set the env var on every
    /// validator and halt-all + simul-start before this tx will pass apply.
    AddSelfStake {
        /// Path to the sender's keystore file.
        #[arg(long)]
        keystore: String,
        /// Amount to add to self_stake, in whole SRX.
        #[arg(long)]
        amount: u64,
        /// Tx fee in sentri.
        #[arg(long, default_value_t = 10_000)]
        fee: u64,
    },
    /// Submit an Unjail tx — proper TX-based path that goes through
    /// apply_block so the state_trie stays consistent.
    ///
    /// Requires: `self_stake >= MIN_SELF_STAKE` (use `add-self-stake`
    /// first if slashed below), current height >= jail_until (jail
    /// period expired), not tombstoned.
    Unjail {
        /// Path to the sender's keystore file.
        #[arg(long)]
        keystore: String,
        /// Tx fee in sentri.
        #[arg(long, default_value_t = 10_000)]
        fee: u64,
    },
    /// Claim accumulated rewards (validator-side and delegator-side
    /// pending_rewards transfer from `PROTOCOL_TREASURY` into the
    /// sender's account balance).
    ClaimRewards {
        /// Path to the sender's keystore file.
        #[arg(long)]
        keystore: String,
        /// Tx fee in sentri.
        #[arg(long, default_value_t = 10_000)]
        fee: u64,
    },
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
    ResetTrie {
        /// Acknowledge the consensus-divergence risk on a non-genesis
        /// chain. Required when current height > 0. Without it, reset-trie
        /// refuses on production chains (see 2026-04-21 3-way fork
        /// incident). The cluster-wide recovery procedure: halt ALL
        /// peers, run reset-trie + any chain.db edits on the canonical
        /// peer, tar-pipe its chain.db to every other peer, simultaneous
        /// start. Each peer's init_trie then backfills from the
        /// (identical) AccountDB so all peers agree on the rebuilt trie
        /// shape. Running reset-trie on a single peer while others keep
        /// incrementally-built tries WILL silently fork.
        #[arg(long)]
        i_understand_divergence_risk: bool,
    },
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
        Commands::Init { admin, genesis } => commands::init::cmd_init(&admin, genesis.as_deref())?,

        Commands::Wallet { action } => match action {
            WalletCommands::Generate { password } => {
                commands::wallet::cmd_wallet_generate(password)?
            }
            WalletCommands::Import {
                private_key,
                password,
            } => commands::wallet::cmd_wallet_import(&private_key, password)?,
            WalletCommands::Info { keystore_file } => {
                commands::wallet::cmd_wallet_info(&keystore_file)?
            }
            WalletCommands::Encrypt {
                private_key,
                password,
                output,
            } => commands::wallet::cmd_wallet_encrypt(&private_key, password, output)?,
            WalletCommands::Decrypt {
                keystore_file,
                password,
            } => commands::wallet::cmd_wallet_decrypt(&keystore_file, password)?,
            WalletCommands::Rekey {
                keystore_file,
                old_password,
                new_password,
            } => commands::wallet::cmd_wallet_rekey(&keystore_file, old_password, new_password)?,
        },

        Commands::Validator { action } => match action {
            ValidatorCommands::Add {
                address,
                name,
                public_key,
                admin_key,
            } => {
                let key = resolve_key(admin_key, "SENTRIX_ADMIN_KEY", "admin key")?;
                commands::validator::cmd_validator_add(&address, &name, &public_key, &key)?;
            }
            ValidatorCommands::Remove { address, admin_key } => {
                let key = resolve_key(admin_key, "SENTRIX_ADMIN_KEY", "admin key")?;
                commands::validator::cmd_validator_remove(&address, &key)?;
            }
            ValidatorCommands::Toggle { address, admin_key } => {
                let key = resolve_key(admin_key, "SENTRIX_ADMIN_KEY", "admin key")?;
                commands::validator::cmd_validator_toggle(&address, &key)?;
            }
            ValidatorCommands::Rename {
                address,
                new_name,
                admin_key,
            } => {
                let key = resolve_key(admin_key, "SENTRIX_ADMIN_KEY", "admin key")?;
                commands::validator::cmd_validator_rename(&address, &new_name, &key)?;
            }
            ValidatorCommands::ForceUnjail {
                address,
                i_understand_phantom_stake,
            } => {
                commands::validator::cmd_validator_force_unjail(
                    &address,
                    i_understand_phantom_stake,
                )?;
            }
            ValidatorCommands::Unjail { address } => {
                commands::validator::cmd_validator_unjail(&address)?;
            }
            ValidatorCommands::TransferAdmin {
                new_admin,
                admin_key,
            } => {
                let key = resolve_key(admin_key, "SENTRIX_ADMIN_KEY", "admin key")?;
                commands::validator::cmd_validator_transfer_admin(&new_admin, &key)?;
            }
            ValidatorCommands::List => commands::validator::cmd_validator_list()?,
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
                let pwd = commands::wallet::resolve_password(None)?;
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
            ChainCommands::Info => commands::chain::cmd_chain_info()?,
            ChainCommands::Validate => commands::chain::cmd_chain_validate()?,
            ChainCommands::Block { index } => commands::chain::cmd_chain_block(index)?,
            ChainCommands::ResetTrie {
                i_understand_divergence_risk,
            } => commands::chain::cmd_chain_reset_trie(i_understand_divergence_risk)?,
            ChainCommands::VerifyDeep => commands::chain::cmd_chain_verify_deep()?,
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
                commands::token::cmd_token_deploy(&name, &symbol, decimals, supply, &key, fee)?;
            }
            TokenCommands::Transfer {
                contract,
                to,
                amount,
                from_key,
                gas,
            } => {
                let key = resolve_key(from_key, "SENTRIX_FROM_KEY", "from key")?;
                commands::token::cmd_token_transfer(&contract, &to, amount, &key, gas)?;
            }
            TokenCommands::Burn {
                contract,
                amount,
                from_key,
                gas,
            } => {
                let key = resolve_key(from_key, "SENTRIX_FROM_KEY", "from key")?;
                commands::token::cmd_token_burn(&contract, amount, &key, gas)?;
            }
            TokenCommands::Balance { contract, address } => {
                commands::token::cmd_token_balance(&contract, &address)?;
            }
            TokenCommands::Info { contract } => {
                commands::token::cmd_token_info(&contract)?;
            }
            TokenCommands::List => commands::token::cmd_token_list()?,
        },

        Commands::Staking { action } => match action {
            StakingCommands::Register {
                keystore,
                self_stake,
                commission_rate,
                fee,
            } => {
                commands::staking::cmd_staking_register(
                    &keystore,
                    self_stake,
                    commission_rate,
                    fee,
                )?;
            }
            StakingCommands::AddSelfStake {
                keystore,
                amount,
                fee,
            } => {
                commands::staking::cmd_staking_add_self_stake(&keystore, amount, fee)?;
            }
            StakingCommands::Unjail { keystore, fee } => {
                commands::staking::cmd_staking_unjail(&keystore, fee)?;
            }
            StakingCommands::ClaimRewards { keystore, fee } => {
                commands::staking::cmd_staking_claim_rewards(&keystore, fee)?;
            }
        },

        Commands::State { action } => match action {
            StateCommands::Export { output } => commands::state::cmd_state_export(output)?,
            StateCommands::Import { input, force } => {
                commands::state::cmd_state_import(&input, force)?
            }
            StateCommands::Verify { input } => commands::state::cmd_state_verify(&input)?,
        },

        Commands::Mempool { action } => match action {
            MempoolCommands::Clear => commands::mempool::cmd_mempool_clear()?,
            MempoolCommands::Stats => commands::mempool::cmd_mempool_stats()?,
        },

        Commands::Balance { address } => commands::misc::cmd_balance(&address)?,

        Commands::History { address } => commands::misc::cmd_history(&address)?,

        Commands::GenesisWallets => commands::misc::cmd_genesis_wallets()?,
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

async fn cmd_start(
    // Take the validator wallet by value so the caller's `Zeroizing` envelope
    // for the env-var path drops *before* we hold the `Wallet` here. The
    // wallet's own `Zeroizing<[u8; 32]>` keeps the secret bytes wiped on drop.
    validator: Option<Wallet>,
    port: u16,
    peers_str: String,
) -> anyhow::Result<()> {
    // Loud warning if any consensus-touching env var is armed in a known-
    // dangerous state. Currently covers JAIL_CONSENSUS_HEIGHT (the
    // LivenessTracker non-determinism halt class). Fires before the chain
    // even loads so an operator catching it can ctrl-C and reconfigure
    // without partially booting into a halt-bound state.
    sentrix::core::blockchain::warn_if_jail_consensus_armed();

    let storage = Arc::new(Storage::open(&get_db_path())?);
    let bc = storage
        .load_blockchain()?
        .ok_or_else(|| anyhow::anyhow!("Chain not initialized. Run: sentrix init"))?;

    let shared: SharedState = Arc::new(RwLock::new(bc));

    // Capacity 4096 (was 256). Each NodeEvent is at most a few hundred
    // bytes (BftPrevote/BftPrecommit are tiny, NewBlock is the largest)
    // so the worst-case memory footprint is on the order of MB, far
    // cheaper than the cost of a BFT-message backpressure stall. A 4-
    // validator cluster issuing prevote+precommit per block at 1 block/s
    // tops out at ~12 messages/s here; 256 slots covered ~20s of stall,
    // 4096 covers ~5 min — long enough to ride out any reasonable
    // validator-loop pause without losing the await-based send semantics
    // that BFT requires (dropped votes destabilise consensus more than
    // backpressure does).
    // 2026-05-08 v2.1.88: bumped 4096 → 16384 after testnet observed 8671
    // bft_tx FULL drops in 30 min on a fullnode under catch-up sync, which
    // produced BFT split-brain at h=3066004 (validators received proposals
    // at different rounds → 2-2 fork → halt). Channel back-pressure is
    // worse for consensus stability than the extra ~2 MB RAM cost of
    // deeper buffering.
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<NodeEvent>(16384);

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
    // Capacity history: 256 → 4096 (v2.1.65) → 16384 (v2.1.88).
    // 2026-05-08 v2.1.88: bumped 4096 → 16384 after testnet halt at
    // h=3066004 traced to BFT split-brain caused by upstream channel
    // back-pressure. fullnode-1 logged 8671 bft_tx FULL drops in 30 min
    // under catch-up sync; under that pressure validators received
    // proposals at different rounds → 2-2 fork → halt. Deeper buffer
    // gives the main loop time to drain MDBX-write backlogs before
    // dropping consensus messages. Cost: ~3 MB extra RAM per validator.
    let (bft_tx, bft_rx) =
        tokio::sync::mpsc::channel::<sentrix::core::bft_messages::BftMessage>(16384);

    // 2026-05-05 v2.1.68: cumulative count of BFT messages dropped because
    // bft_tx was full when the event-handler tokio task tried to forward
    // an inbound BFT message (Propose/Prevote/Precommit/RoundStatus) to
    // the validator main loop. Pre-v2.1.68 the event handler used
    // `bft_tx.send().await` which BLOCKED until the validator loop drained
    // bft_rx — that pattern wedged the event handler whenever the validator
    // main loop fell behind. The wedged event handler then backed up
    // event_tx (4096 cap) which in turn backed up the swarm task. This was
    // the LAST remaining unbounded-block point in the BFT message path
    // (cmd_tx → swarm → event_tx → bft_tx → validator).
    //
    // v2.1.68 switches to `try_send` + drop-on-Full + this counter. Mirror
    // of v2.1.65 (DROPPED_BFT_BROADCASTS for cmd_tx) and v2.1.67
    // (EVENT_TX_DROPPED for event_tx). Trade-off accepted: lossy inbound
    // BFT delivery under burst load, in exchange for never wedging the
    // event-handler task. Operator playbook: if this counter increments
    // during a halt, the validator main loop has fallen behind —
    // investigate consumer-side back-pressure (slow MDBX writes /
    // contended write locks / RPC handler block).
    static BFT_TX_DROPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn try_send_bft(
        bft_tx: &tokio::sync::mpsc::Sender<sentrix::core::bft_messages::BftMessage>,
        msg: sentrix::core::bft_messages::BftMessage,
        variant: &'static str,
    ) {
        let max = bft_tx.max_capacity();
        match bft_tx.try_send(msg) {
            Ok(()) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                let total = BFT_TX_DROPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                tracing::error!(
                    target: "bft_tx_drop",
                    "bft_tx FULL ({}/{}) — DROPPED inbound {} (total drops since \
                     boot: {}). Validator main loop not draining bft_rx fast enough; \
                     BFT message lost — investigate consumer-side back-pressure \
                     (slow MDBX writes / contended write lock).",
                    max, max, variant, total,
                );
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                tracing::error!(
                    target: "bft_tx_drop",
                    "bft_tx CLOSED — DROPPED inbound {} (validator main loop gone; \
                     process should be restarting)",
                    variant,
                );
            }
        }
    }

    // 2026-05-10: in-process validator-loop watchdog removed.
    //
    // Production blockchains (Tendermint/CometBFT, Geth, Cosmos SDK
    // app chains) don't ship in-process self-kill watchdogs. Restart
    // authority belongs to an external supervisor (systemd, docker,
    // sentrix-guardian) that can decide based on observed metrics —
    // /sentrix_status_extended exposes the chain head, peer count,
    // channel-drop counters, and the supervisor consumes those.
    //
    // The earlier in-process watchdog was added during incident
    // response and worked, but it hid the underlying BFT/libp2p
    // liveness issues by short-cycling the process. See PR #561 +
    // https://docs.sentrixchain.com/operations/guardian/ for the full
    // reasoning and the recommended supervisor policy.

    // Fix A (2026-05-10) — async chain.db save off the BFT critical path.
    // Pre-fix: validator-loop blocked on storage.save_blockchain inside the
    // FinalizeBlock arm. With a ~5 GB mainnet chain.db that fsync + JSON
    // serialise was 500 ms-1 s and held up the next round. Now we ship
    // every finalized height onto a save_tx channel and a dedicated writer
    // task drains it serially. Crash safety is preserved by the existing
    // B2 load-replay path (PR #556): if we die between in-memory commit
    // and the disk save, restart replays the missing block(s).
    let save_buffer: usize = std::env::var("SENTRIX_SAVE_QUEUE_DEPTH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);
    let (save_tx, mut save_rx) = tokio::sync::mpsc::channel::<u64>(save_buffer);
    {
        let writer_storage = storage.clone();
        let writer_shared = shared.clone();
        tokio::spawn(async move {
            while let Some(target_height) = save_rx.recv().await {
                // Drain coalesced heights: if the writer is behind, multiple
                // FinalizeBlock pushes can stack up. One snapshot covers all
                // of them since save_blockchain writes the full state blob.
                let mut latest = target_height;
                while let Ok(h) = save_rx.try_recv() {
                    latest = h;
                }
                let bc = writer_shared.read().await;
                let height_at_save = bc.height();
                match writer_storage.save_blockchain(&bc) {
                    Ok(()) => {
                        tracing::debug!(
                            target: "save_writer",
                            "background save_blockchain ok queued_for=h{} caught_up_to=h{}",
                            latest,
                            height_at_save,
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            target: "save_writer",
                            "background save_blockchain failed queued_for=h{} caught_up_to=h{}: {}",
                            latest, height_at_save, e,
                        );
                    }
                }
                drop(bc);
            }
            tracing::info!(target: "save_writer", "save channel closed; writer exiting");
        });
    }

    // Validator loop — capture the JoinHandle so the graceful-shutdown
    // path (C-08) can await the task's exit before save_blockchain
    // snapshots state. Without the handle the process could exit mid
    // add_block / trie.commit, tearing state between memory and disk.
    let validator_handle: Option<tokio::task::JoinHandle<()>> = if let Some(wallet) = validator {
        println!("Validator mode: {}", wallet.address);
        let shared_clone = shared.clone();
        let storage_clone = storage.clone();
        let save_tx_clone = save_tx.clone();
        let lp2p_clone = lp2p.clone();
        let shutdown_flag_clone = shutdown_flag.clone();
        let mut bft_rx = bft_rx; // move receiver into this task
        let validator_secret_key = wallet.get_secret_key()?;

        // LastSignBytes guard (Tendermint privval pattern, 2026-05-07
        // post-halt-class hardening). If `LAST_SIGN_GUARD_PATH` env is
        // set we persist the highest (height, round, step) tuple this
        // validator has signed at; subsequent sign attempts at-or-below
        // that tuple are refused. Closes the cascade-jail-at-restart
        // class — pre-fix a validator that crashed mid-round could
        // re-vote with different content after recovery, which under a
        // Byzantine interpretation looks like equivocation. With the
        // env var unset the guard is a no-op (legacy behaviour
        // bit-identical) so chain history stays valid.
        if let Ok(path) = std::env::var("LAST_SIGN_GUARD_PATH") {
            let p = std::path::PathBuf::from(&path);
            if let Err(e) = sentrix_bft::last_sign_guard::init(p) {
                tracing::error!(
                    "FATAL: LastSignBytes guard init failed at {}: {}. Refusing to start \
                     validator — fix path / permissions and restart.",
                    path,
                    e
                );
                return Err(anyhow::anyhow!(
                    "LastSignBytes guard init failed at {path}: {e}"
                ));
            }
        } else {
            tracing::warn!(
                "LAST_SIGN_GUARD_PATH not set — running without privval double-vote guard. \
                 Set to e.g. /var/lib/sentrix/last-sign.json for production. \
                 (Behaviour matches v2.1.83 unguarded baseline.)"
            );
        }
        Some(tokio::spawn(async move {
            use sentrix::core::bft::{BftAction, BftEngine, BftPhase};
            use sentrix::core::bft_messages::{BftMessage, Prevote, Proposal};
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
                            // F-D Variant A v3: embed proposer's own
                            // UNSIGNED prevote so receivers credit the
                            // proposer's vote at proposal-arrival time
                            // instead of waiting for the standalone
                            // gossipsub prevote hop. Authenticity flows
                            // from the proposal's outer signature; the
                            // prevote stays unsigned to avoid poisoning
                            // the LastSignBytes guard at step=1 before
                            // proposal.sign() records step=0 (the bug
                            // that broke reverted PR #572).
                            let cur_round = bft.round();
                            let proposer_prevote = Some(Prevote {
                                height,
                                round: cur_round,
                                block_hash: Some(cached_hash.clone()),
                                validator: wallet_address.to_string(),
                                signature: vec![],
                            });
                            let mut proposal = Proposal {
                                height,
                                round: cur_round,
                                block_hash: cached_hash,
                                block_data: cached_bytes,
                                proposer: wallet_address.to_string(),
                                signature: vec![],
                                proposer_prevote,
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
                        let cur_round = bft.round();
                        // F-D Variant A v3: embed proposer's own UNSIGNED
                        // prevote. See cached-hash branch above for the
                        // signing-order rationale.
                        let proposer_prevote = Some(Prevote {
                            height,
                            round: cur_round,
                            block_hash: Some(block_hash.clone()),
                            validator: wallet_address.to_string(),
                            signature: vec![],
                        });
                        let mut proposal = Proposal {
                            height,
                            round: cur_round,
                            block_hash,
                            block_data,
                            proposer: wallet_address.to_string(),
                            signature: vec![],
                            proposer_prevote,
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
            // v2.2.21 follow-up: BFT observability. Construct BftMetrics once
            // per validator process against the default Prometheus registry;
            // it lives for the duration of the validator task and gets cloned
            // into every BftEngine instantiation below. The /metrics endpoint
            // in sentrix-rpc exposes default_registry().gather() so scrapers
            // (sentrix-prom-exporter, Grafana, anything) see the bft_* series.
            let bft_metrics = match sentrix_bft::BftMetrics::new(prometheus::default_registry()) {
                Ok(m) => Some(m),
                Err(e) => {
                    // Non-fatal: if the registry already has our metrics (test
                    // process, hot-reload), keep running without observability.
                    // Production startup logs this once at info.
                    tracing::warn!(
                        "BFT observability disabled — metric registration \
                         failed: {} (typical cause: process restart in same \
                         registry namespace). Engine continues without metrics.",
                        e
                    );
                    None
                }
            };

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
            // 2026-05-10: vote rebroadcast tracking. Mirrors the proposal
            // rebroadcast above. Earlier we found cascade rounds where one
            // peer's prevote never reached one of the four validators
            // (single-attempt request-response delivery, no built-in retry
            // beyond the proposer's proposal rebroadcast). The receiver hit
            // prevote_timeout without seeing the supermajority and nil-
            // precommitted, then everyone cascaded.
            //
            // Re-broadcasting the SAME signed Prevote / Precommit at a
            // periodic tick during their respective phases gives a missed
            // peer ~6 more chances inside the 12 s phase budget. Same bytes
            // = same signature, no double-vote risk.
            // Vote rebroadcast state used to live here (REBROADCAST_INTERVAL
            // 0.5s × 6 attempts, mirrored for prevote + precommit). Dropped
            // 2026-05-10 with the gossipsub-for-BFT switch: gossipsub mesh
            // already retransmits missed votes via IHAVE/IWANT, so the
            // validator-side tick became double work.
            // v2.1.89: stash the originally-signed Proposal struct so the
            // #1d rebroadcast tick replays byte-identical bytes instead of
            // rebuilding + re-signing. Pre-fix, the rebroadcast path called
            // `bincode::serialize(block)` and `proposal.sign()` afresh; that
            // worked most of the time, but produced occasional "bad
            // signature" rejections on peers that had already accepted the
            // first emit (trace at 2026-05-08 23:32:41, proposer → peer
            // foundation). Stashing + replaying the original signed
            // Proposal removes the re-encode/re-sign step entirely, which
            // is the correct invariant: a rebroadcast is the same message,
            // not a new one.
            let mut last_signed_proposal: Option<Proposal> = None;
            // Fix C (speculative block-build) — after we finalize block N
            // and apply its state, if we're the deterministic proposer for
            // N+1 round 0 we pre-build the next block + sign its proposal
            // inside the same write-lock cycle. The validator-loop's next
            // round_start handler consumes this stash and skips the
            // ~100-200 ms build step. Stale stashes (round != 0, height
            // mismatch, post-epoch-boundary) fall back to the normal
            // build_or_reuse_proposal path. Tuple: (height, block, proposal).
            let mut speculative_proposal: Option<(u64, Block, Proposal)> = None;
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
            // internal docs
            // -plan.md (L1 + L4 baked in).
            let mut last_advert_broadcast_at: Option<tokio::time::Instant> = None;
            let mut last_l1_tick_at =
                tokio::time::Instant::now() - tokio::time::Duration::from_secs(31); // fire on first iter
            // Sequence MUST be loaded from disk on startup, otherwise
            // restart resets to 0 and peers (cached at the previous
            // lifetime's high-water mark) silently drop our broadcasts
            // until we overshoot the cached value. See self-review.
            let mut advert_sequence: u64 = load_advert_sequence();
            tracing::info!(
                "L1: advert sequence resumed at {} (next broadcast will be {})",
                advert_sequence,
                advert_sequence.saturating_add(1)
            );
            const L1_TICK_INTERVAL: tokio::time::Duration = tokio::time::Duration::from_secs(30);
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
                        // Filter out unreachable addresses — peers can't
                        // dial these. Loopback (`127.0.0.1`, `::1`) is
                        // self-only; `0.0.0.0` and `::` are bind-time
                        // wildcards that mean "all interfaces" not a
                        // routable address. libp2p often surfaces them
                        // anyway when SENTRIX_P2P_HOST=0.0.0.0 (the
                        // production default), so the filter must catch
                        // both classes. Cap at MAX_MULTIADDRS to stay
                        // within DoS budget on the receiver side.
                        //
                        // 2026-04-26: append `/p2p/<own_peer_id>` to each
                        // address so receivers can extract our peer_id
                        // for the dial-tick connected-peers pre-check
                        // (sentrix-labs/sentrix#319). Without the suffix,
                        // the dial-tick can't tell which peer_id a cached
                        // multiaddr resolves to and falls back to "dial
                        // anyway", which reintroduces the connection-
                        // accumulation pattern that was the root cause of
                        // the 2026-04-25 mainnet stalls.
                        let our_peer_id = lp2p_clone.local_peer_id;
                        let multiaddrs: Vec<String> = listen_addrs
                            .iter()
                            .map(|m| m.to_string())
                            .filter(|s| {
                                !s.starts_with("/ip4/127.")
                                    && !s.starts_with("/ip6/::1/")
                                    && !s.starts_with("/ip4/0.0.0.0/")
                                    && !s.starts_with("/ip6/::/")
                            })
                            .map(|s| {
                                // Skip if already has /p2p (defensive —
                                // listen_addrs() shouldn't include them
                                // but tolerate it).
                                if s.contains("/p2p/") {
                                    s
                                } else {
                                    format!("{}/p2p/{}", s, our_peer_id)
                                }
                            })
                            .take(sentrix_wire::MultiaddrAdvertisement::MAX_MULTIADDRS)
                            .collect();
                        if !multiaddrs.is_empty() {
                            advert_sequence = advert_sequence.saturating_add(1);
                            // Persist BEFORE broadcasting so a crash
                            // between bump and broadcast doesn't leave
                            // a sequence we never published. Worst
                            // case the validator skips a sequence
                            // number on next start; harmless because
                            // peers compare by greater-than, not
                            // sequential.
                            store_advert_sequence(advert_sequence);
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
                    // aren't currently peered with.
                    //
                    // CONNECTION-LEAK FIX (2026-04-25 incident): the
                    // previous comment claimed `connect_peer` was idempotent
                    // ("duplicate dials to an already-connected peer are
                    // no-ops"). That turned out to be FALSE in libp2p
                    // 0.56 / libp2p-swarm 0.47 — every `swarm.dial()`
                    // enqueues a fresh pending connection regardless of
                    // existing connection state. Without a connected-peers
                    // pre-check, this loop accumulated 568-918 pending +
                    // established connections per validator over a few
                    // hours, gossipsub mesh thrashed on the oversized pool,
                    // and BFT request_response messages dropped mid-round
                    // → consensus deadlock (h=583002, h=585217 stalls).
                    //
                    // Snapshot connected peers ONCE per tick, then skip any
                    // active-set member whose libp2p peer_id is already in
                    // the set. The peer_id is extracted from the cached
                    // multiaddr's `/p2p/<peer_id>` suffix (which validators
                    // include when broadcasting their advertisement).
                    // Multiaddrs without a peer_id suffix fall back to
                    // dialing (rare; only happens for legacy adverts from
                    // pre-PR #300 binaries that no longer exist on the
                    // production fleet).
                    let active_set: Vec<String> = {
                        let bc = shared_clone.read().await;
                        bc.stake_registry.active_set.clone()
                    };
                    if !active_set.is_empty() {
                        let connected = lp2p_clone.connected_peers().await;
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
                                // Skip if we already have an established
                                // connection to this peer (the leak fix).
                                let already_connected = ma.iter().any(|proto| {
                                    if let libp2p::multiaddr::Protocol::P2p(peer_id) = proto {
                                        connected.contains(&peer_id)
                                    } else {
                                        false
                                    }
                                });
                                if already_connected {
                                    tracing::trace!(
                                        "L1: skip dial — {} already connected (advert seq={})",
                                        &advert.validator[..12.min(advert.validator.len())],
                                        advert.sequence
                                    );
                                    continue;
                                }
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

                // ── L2 cold-start gate (2026-04-25 second-incident fix) ──
                //
                // The original L2 gate (post-this-block) only fires on
                // ACTIVATION TRANSITION, when voyager_activated is loaded
                // as false. On cold-start with chain.db's persistent
                // voyager_activated=true (e.g. after a previous activation),
                // validators enter BFT IMMEDIATELY — before the L1 mesh
                // has had time to converge. BFT proposal/precommit
                // messages travel via libp2p request_response (1-to-1),
                // not gossipsub, so they only reach peers connected at
                // the exact moment of broadcast. Activation #2 on
                // 2026-04-25 split-brained at h=578006 because not all
                // 4 validators had a fully-formed mesh at the moment
                // Foundation node broadcast its precommit.
                //
                // This second gate fires at EVERY loop iteration when
                // BFT mode is active. If peer count is insufficient,
                // sleep 5s and retry — by then L1 self-discovery has
                // had a chance to converge the mesh. Once mesh is
                // healthy the gate passes and BFT proceeds normally.
                //
                // Steady-state cost: one read-lock + one async peer_count
                // query per iteration = negligible (microseconds).
                if voyager_activated {
                    // BFT-gate-relax fork-aware required peer count:
                    // pre-fork: active_set_len - 1 (need full mesh).
                    // post-fork: min_active_for_bft - 1 (need supermajority mesh,
                    // = 2 for N=4, allows 1-jail tolerance — chain stays alive
                    // when 1 validator is down).
                    let (active_set_len, total_validators, current_height) = {
                        let bc = shared_clone.read().await;
                        (
                            bc.stake_registry.active_set.len(),
                            bc.stake_registry.validators.len(),
                            bc.height(),
                        )
                    };
                    let min_active = sentrix::core::blockchain::Blockchain::min_active_for_bft(
                        current_height,
                        total_validators,
                    );
                    let required_peers = min_active.saturating_sub(1);
                    let peer_count = lp2p_clone.peer_count().await;
                    let force_override = force_bft_insufficient_peers_set();
                    if let Err(reason) = check_bft_peer_mesh_eligible(
                        peer_count,
                        active_set_len,
                        required_peers,
                        force_override,
                    ) {
                        tracing::warn!(
                            "L2 cold-start gate: {} (gate-relax-fork-active={}) — sleeping 5s, will retry once L1 mesh converges",
                            reason,
                            sentrix::core::blockchain::Blockchain::is_bft_gate_relax_height(
                                current_height
                            ),
                        );
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                        continue;
                    }
                }

                // ── Voyager fork activation (read lock first, write only if needed) ──
                //
                // L2 pre-flight gate (2026-04-25 incident response): refuse to
                // flip into Voyager BFT mode if our libp2p peer count is below
                // `active_set.len() - 1`. The mainnet livelock at h=557244 was
                // caused by Beacon node having only 1 peer (Foundation node) at activation
                // moment — its proposals never reached Treasury node/Core node and the
                // chain ground out 30+ skip rounds in 16 minutes before the
                // emergency rollback. With this gate, a partitioned validator
                // stays in Pioneer instead and re-checks every loop tick;
                // once L1 multiaddr gossip ships, the mesh self-heals and
                // activation proceeds automatically.
                if !voyager_activated {
                    let bc = shared_clone.read().await;
                    // 2026-04-26: voyager_mode_for() runtime-aware check.
                    // For activation transition (voyager_activated == false),
                    // env-var fork-height drives activation. After activation
                    // the runtime flag takes over via voyager_mode_for's OR.
                    if bc.voyager_mode_for(bc.height().saturating_add(1)) {
                        let active_set_len = bc.stake_registry.active_set.len();
                        let total_validators = bc.stake_registry.validators.len();
                        let current_height = bc.height();
                        drop(bc);

                        let peer_count = lp2p_clone.peer_count().await;
                        let force_override = force_bft_insufficient_peers_set();
                        // BFT-gate-relax fork-aware required peer count
                        // (same as L2 cold-start gate above — see comment there).
                        let min_active = sentrix::core::blockchain::Blockchain::min_active_for_bft(
                            current_height,
                            total_validators,
                        );
                        let required_peers = min_active.saturating_sub(1);

                        match check_bft_peer_mesh_eligible(
                            peer_count,
                            active_set_len,
                            required_peers,
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
                        // H-09 + Patch B1 (v2.1.90): persist block bytes,
                        // height, hash index, and the Blockchain bincode
                        // blob (accounts, mempool, stake_registry, …) in
                        // one atomic MDBX transaction. The pre-fix path
                        // called save_block + save_blockchain as two
                        // separate commits, leaving a window where a crash
                        // could persist the block without the matching
                        // accounts state. save_blockchain now writes
                        // everything atomically.
                        let bc = shared_clone.read().await;
                        if let Err(e) = storage_clone.save_blockchain(&bc) {
                            tracing::error!(
                                "H-09: atomic save_blockchain failed at height {} produced by {}: {}; \
                                 skipping broadcast to prevent fork",
                                height,
                                wallet.address,
                                e
                            );
                            drop(bc);
                        } else {
                            drop(bc);
                            println!("Block {} produced by {}", height, wallet.address);
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
                        // BFT-gate-relax fork-aware threshold:
                        // Pre-fork: MIN_BFT_VALIDATORS (= 4 absolute).
                        // Post-fork: ⌈2/3 × total⌉ clamped to MIN_BFT_VALIDATORS.
                        // For 4-validator network post-fork: gate becomes 3 (allows 1-jail tolerance).
                        // See audits/jail-cascade-root-cause-analysis.md.
                        let total_validators = bc_check.stake_registry.validators.len();
                        let min_active = sentrix::core::blockchain::Blockchain::min_active_for_bft(
                            next_height,
                            total_validators,
                        );
                        if active < min_active {
                            tracing::warn!(
                                "P1: skipping BFT round at height {} — active set \
                                 {} < minimum {} for BFT safety (total={}, gate-relax-fork={})",
                                next_height,
                                active,
                                min_active,
                                total_validators,
                                sentrix::core::blockchain::Blockchain::is_bft_gate_relax_height(
                                    next_height
                                ),
                            );
                            drop(bc_check);
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            continue;
                        }
                    }

                    let mut bft = match &bft_metrics {
                        Some(m) => BftEngine::new_with_metrics(
                            next_height,
                            wallet.address.clone(),
                            total_active_stake,
                            m.clone(),
                        ),
                        None => {
                            BftEngine::new(next_height, wallet.address.clone(), total_active_stake)
                        }
                    };

                    // v2.1.85: resume at the correct round if we previously
                    // signed at this height pre-crash. Without this the v2.1.84
                    // LastSignBytes guard correctly refuses any sign at-or-below
                    // last_signed (h=N, r=R_prev, *), but the engine initialises
                    // at round 0, so the next sign is at (N, 0, Proposal) which
                    // is rejected as a double-vote attempt. Result: chicken-egg
                    // halt where chain can't advance until operator manually
                    // clears /var/lib/sentrix/last-sign.json. See v44 incident
                    // 2026-05-07 for the failure mode.
                    //
                    // v2.2.20: extended to also handle the (round=0, step>=1)
                    // case. Original condition `state.round > 0` skipped the
                    // common nil-precommit-at-round-0 stuck pattern: cluster
                    // loses quorum during rolling restart → vals precommit nil
                    // at round 0 → last-sign.json persists (h, 0, 2) → restart
                    // → engine boots at round 0 → guard rejects (h, 0, 0)
                    // because <= (h, 0, 2) → empty sig → cluster stalls until
                    // engine times out 12+ rounds. Now triggers when step>=1
                    // (vote cast at this round) AND advances to next round.
                    // 2026-05-29 testnet stall at h=5707944 was this pattern.
                    if let Some(state) = sentrix_bft::last_sign_guard::current_state()
                        && state.height == next_height
                    {
                        // If we cast a vote at this round (step >= Prevote),
                        // engine must skip past this round entirely. If we
                        // only proposed (step == Proposal), prevote and
                        // precommit at the same round are still legal.
                        let target_round = if state.step >= 1 {
                            state.round.saturating_add(1)
                        } else {
                            state.round
                        };
                        if target_round > 0 {
                            // BftEngine::advance_round bumps round + resets
                            // the vote collector + phase_start. Apply N times
                            // to skip the prior in-progress round(s).
                            for _ in 0..target_round {
                                bft.advance_round();
                            }
                            tracing::info!(
                                "BFT engine resume: prior sign at (h={}, r={}, s={}) — \
                                 advanced engine to round {} to bypass guard refusal",
                                state.height,
                                state.round,
                                state.step,
                                target_round,
                            );
                        }
                    }

                    proposed_block = None;
                    // #1d: reset rebroadcast tracking on new height.
                    proposal_broadcast_at = None;
                    proposal_rebroadcast_count = 0;
                    last_signed_proposal = None;

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
                        //
                        // Fix C: consume the speculative pre-built proposal from the
                        // prior FinalizeBlock arm if it matches (height, round=0,
                        // not locked at a different hash). Saves the create_block_voyager
                        // + bincode::serialize + sign roundtrip that otherwise lives
                        // on the BFT critical path.
                        let mut bc = shared_clone.write().await;
                        let stash = speculative_proposal.take().filter(|(h, _, _)| {
                            *h == next_height
                                && bft.round() == 0
                                && bft.locked_proposal_bytes().is_none()
                        });
                        let built = match stash {
                            Some((_, block, proposal)) => {
                                tracing::info!(
                                    target: "speculative_build",
                                    "consumed pre-built proposal h={} (skipped build)",
                                    next_height,
                                );
                                Some((block, proposal))
                            }
                            None => build_or_reuse_proposal(
                                &bft,
                                &mut bc,
                                &wallet.address,
                                &validator_secret_key,
                                next_height,
                            ),
                        };
                        match built {
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
                                // v2.1.89: stash for byte-identical rebroadcast.
                                last_signed_proposal = Some(proposal.clone());

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
                                            if lp2p_clone
                                                .broadcast_bft_prevote(&signed_pv)
                                                .await
                                                .is_err()
                                            {
                                                tracing::error!(
                                                    "BFT prevote broadcast dropped at h={} r={} — \
                                                     engine retains pending; outer loop re-emits",
                                                    prevote.height,
                                                    prevote.round,
                                                );
                                                break;
                                            }
                                            bft.mark_prevote_cast();
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
                                            if lp2p_clone
                                                .broadcast_bft_precommit(&signed_pc)
                                                .await
                                                .is_err()
                                            {
                                                tracing::error!(
                                                    "BFT precommit broadcast dropped at h={} r={} — \
                                                     engine retains pending; outer loop re-emits",
                                                    precommit.height,
                                                    precommit.round,
                                                );
                                                break;
                                            }
                                            bft.mark_precommit_cast();
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
                                            ref block_hash,
                                            ref justification,
                                        } => {
                                            // 2026-05-04 finalize-entry trace (per
                                            // audits/2026-04-30-eager-write-investigation.md
                                            // §Recommendation #1): emit local active-set view +
                                            // precommit accounting at every finalize attempt so
                                            // the next divergence event can be diagnosed by
                                            // diff'ing the four validators' log lines for the
                                            // same height. Active-set view divergence is the
                                            // working hypothesis behind the recurring chain.db
                                            // forks (mechanism #1 in that audit) but the
                                            // evidence to nail it down requires this log to
                                            // exist before the next event.
                                            {
                                                let bc_read = shared_clone.read().await;
                                                let active_count =
                                                    bc_read.stake_registry.active_set.len();
                                                let total_stake: u64 = bc_read
                                                    .stake_registry
                                                    .active_set
                                                    .iter()
                                                    .filter_map(|a| {
                                                        bc_read
                                                            .stake_registry
                                                            .get_validator(a)
                                                            .map(|v| v.total_stake())
                                                    })
                                                    .sum();
                                                drop(bc_read);
                                                let precommit_count =
                                                    justification.precommits.len();
                                                let precommit_stake: u64 = justification
                                                    .precommits
                                                    .iter()
                                                    .map(|p| p.stake_weight)
                                                    .sum();
                                                tracing::info!(
                                                    target: "finalize_trace",
                                                    "FinalizeBlock self-propose path: h={} round={} block={:.16}… \
                                                     active_count={} total_stake={} precommit_count={} \
                                                     precommit_stake={} our_addr={}",
                                                    height, round, block_hash, active_count,
                                                    total_stake, precommit_count, precommit_stake,
                                                    wallet.address,
                                                );
                                            }

                                            // 2026-05-05 chain-already-advanced check (v2.1.63).
                                            // Race we hit on 2026-05-04: libp2p NewBlock handler
                                            // applies the cluster-canonical block at this height
                                            // via `add_block_from_peer` concurrent with our BFT
                                            // engine reaching FinalizeBlock for the same height.
                                            // By the time we reach validate_block below, chain
                                            // already advanced — pre-validate fails with off-by-
                                            // one ("expected index N+1, got N"), we break, but
                                            // the BFT engine state is already at this height so
                                            // the same FinalizeBlock fires again next round,
                                            // wasting cycles. Worse: in the cluster-wide variant
                                            // (all 4 validators in the same race), the cascade
                                            // produces watchdog FATAL across the cluster within
                                            // a single 90s window and chain liveness collapses
                                            // (the silent-thread-death pattern documented in
                                            // SESSION_HANDOFF 2026-05-04 late-evening).
                                            //
                                            // Fix: if `bc.height() >= action.height`, the block
                                            // is already on our chain (canonical hash, applied
                                            // via gossip). Skip the local write silently. The
                                            // outer loop's need_new_round check resets the BFT
                                            // engine to bc.height()+1 on the next iteration.
                                            {
                                                let bc_read = shared_clone.read().await;
                                                if bc_read.height() >= height {
                                                    tracing::info!(
                                                        target: "finalize_trace",
                                                        "BFT finalize self-propose: chain.height={} \
                                                         already ≥ action.height={} — block applied \
                                                         via libp2p sync; skipping local write at \
                                                         round={}",
                                                        bc_read.height(), height, round,
                                                    );
                                                    drop(bc_read);
                                                    proposed_block = None;
                                                    break;
                                                }
                                            }

                                            // 2026-04-30 split-brain guard: if 2/3+ peer
                                            // stake-weight reports being at a higher round
                                            // than ours, the cluster has moved on. Finalising
                                            // on our local view risks landing a block that
                                            // conflicts with whatever the cluster finalised
                                            // next round (the chain.db divergence shape we
                                            // recovered from at h=921604 + h=932488). Catch
                                            // up + skip the local finalize. Validator-count-
                                            // agnostic — same supermajority math drives both
                                            // sides of the check.
                                            if let Some(target_round) =
                                                bft.peer_supermajority_higher_round()
                                            {
                                                tracing::warn!(
                                                    "BFT split-brain guard: aborting local \
                                                     finalise at h={} round={} — supermajority \
                                                     of peer stake at round {}+; catching up",
                                                    height,
                                                    round,
                                                    target_round,
                                                );
                                                let bc_read = shared_clone.read().await;
                                                let cu_result = bft.catch_up_round(
                                                    target_round,
                                                    &bc_read.stake_registry,
                                                );
                                                drop(bc_read);
                                                if let Some(mut prevote) = cu_result {
                                                    prevote.sign(&validator_secret_key);
                                                    let _ = lp2p_clone
                                                        .broadcast_bft_prevote(&prevote)
                                                        .await;
                                                }
                                                break;
                                            }

                                            // 2026-04-30 hash-mismatch guard: the bug behind the
                                            // recurring validator-pair chain.db divergences (see
                                            // audits/2026-04-30-eager-write-investigation.md).
                                            // proposed_block may hold a block from an earlier
                                            // round that didn't finalise; if no new proposal
                                            // arrived in this round but the cluster's precommits
                                            // for this round's actual block cross our local
                                            // supermajority threshold, the prior code would
                                            // .take() the stale stashed block, attach this
                                            // round's justification (pointing at a different
                                            // hash), and write the wrong block at this height.
                                            // Next height's parent_hash references the
                                            // cluster-canonical hash, our local height's hash
                                            // doesn't match, libp2p sync rejects forward
                                            // blocks, BFT can't progress: the divergence shape
                                            // recovered from at h=773012, h=921604, h=932488,
                                            // h=1014804, h=1015365.
                                            //
                                            // The fix is to refuse to write when the stashed
                                            // block's hash doesn't equal the FinalizeBlock
                                            // action's block_hash. Instead, log the mismatch
                                            // and break — the chain advances when a peer
                                            // gossip ships the canonical finalised block (with
                                            // its justification), which the libp2p add-block
                                            // path applies via the same add_block_from_peer
                                            // entry the recovery rsync target uses.
                                            if let Some(stashed) = proposed_block.as_ref()
                                                && &stashed.hash != block_hash
                                            {
                                                tracing::warn!(
                                                    "BFT finalize: stashed proposed_block hash \
                                                     {:.16}… ≠ FinalizeBlock action hash \
                                                     {:.16}… at h={} round={}; refusing write \
                                                     and waiting for peer-gossip canonical \
                                                     block (prevents chain.db divergence per \
                                                     audits/2026-04-30-eager-write-investigation.md)",
                                                    stashed.hash,
                                                    block_hash,
                                                    height,
                                                    round,
                                                );
                                                proposed_block = None;
                                                break;
                                            }

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
                                                                .map(|p| {
                                                                    (
                                                                        p.validator.clone(),
                                                                        p.stake_weight,
                                                                    )
                                                                })
                                                                .collect();
                                                        let validator_fee = 0;
                                                        let _ =
                                                            bc.stake_registry.distribute_reward(
                                                                &proposer,
                                                                &reward_signers,
                                                                reward,
                                                                validator_fee,
                                                            );

                                                        bc.run_epoch_bookkeeping(height);

                                                        tracing::info!(
                                                            "BFT finalized height={} round={}",
                                                            height,
                                                            round
                                                        );

                                                        // Fix C: speculative block-build for N+1.
                                                        // State is at height N here (just applied),
                                                        // active_set has been re-derived if this was
                                                        // an epoch boundary, so weighted_proposer for
                                                        // N+1 round 0 is now stable. If it's us, pre-
                                                        // build the proposal so the next round_start
                                                        // can skip the ~100-200 ms build step.
                                                        let next_h = bc.height().saturating_add(1);
                                                        let we_next = bc
                                                            .stake_registry
                                                            .weighted_proposer(next_h, 0)
                                                            .as_deref()
                                                            == Some(wallet.address.as_str());
                                                        if we_next {
                                                            match bc.create_block_voyager(
                                                                &wallet.address,
                                                            ) {
                                                                Ok(block) => {
                                                                    let block_hash =
                                                                        block.hash.clone();
                                                                    let block_data =
                                                                        bincode::serialize(&block)
                                                                            .unwrap_or_default();
                                                                    // F-D Variant A v3 embedded
                                                                    // unsigned proposer prevote.
                                                                    let proposer_prevote =
                                                                        Some(Prevote {
                                                                            height: next_h,
                                                                            round: 0,
                                                                            block_hash: Some(
                                                                                block_hash.clone(),
                                                                            ),
                                                                            validator: wallet
                                                                                .address
                                                                                .clone(),
                                                                            signature: vec![],
                                                                        });
                                                                    let mut prop = Proposal {
                                                                        height: next_h,
                                                                        round: 0,
                                                                        block_hash,
                                                                        block_data,
                                                                        proposer: wallet
                                                                            .address
                                                                            .clone(),
                                                                        signature: vec![],
                                                                        proposer_prevote,
                                                                    };
                                                                    prop.sign(
                                                                        &validator_secret_key,
                                                                    );
                                                                    tracing::debug!(
                                                                        target: "speculative_build",
                                                                        "pre-built proposal h={} from FinalizeBlock(self) arm",
                                                                        next_h,
                                                                    );
                                                                    speculative_proposal =
                                                                        Some((next_h, block, prop));
                                                                }
                                                                Err(e) => tracing::warn!(
                                                                    "speculative build for h={} failed: {}",
                                                                    next_h,
                                                                    e,
                                                                ),
                                                            }
                                                        } else {
                                                            speculative_proposal = None;
                                                        }

                                                        drop(bc);
                                                        if let Some(ref saved_block) = updated {
                                                            // Fix A: queue the disk save on the
                                                            // writer task instead of fsync-blocking
                                                            // here. B2 load-replay recovers if we
                                                            // crash between in-memory commit and
                                                            // the queued save.
                                                            if save_tx_clone
                                                                .try_send(height)
                                                                .is_err()
                                                            {
                                                                tracing::warn!(
                                                                    target: "save_writer",
                                                                    "save queue full at h={}; \
                                                                     B2 load-replay will catch up",
                                                                    height,
                                                                );
                                                            }
                                                            println!(
                                                                "Block {} produced by {}",
                                                                height, proposer
                                                            );
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
                                                    last_signed_proposal = Some(proposal.clone());
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
                                                    last_signed_proposal = Some(proposal.clone());
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
                                let action = bft.on_round_status_weighted(
                                    &status,
                                    stake,
                                    &bc.stake_registry,
                                );
                                drop(bc);
                                action
                            }
                        };

                        // Cascading BFT action loop for peer messages
                        let mut action = action;
                        loop {
                            match action {
                                BftAction::BroadcastPrevote(ref prevote) => {
                                    let mut signed_pv = prevote.clone();
                                    signed_pv.sign(&validator_secret_key);
                                    if lp2p_clone.broadcast_bft_prevote(&signed_pv).await.is_err() {
                                        tracing::error!(
                                            "BFT prevote broadcast dropped at h={} r={} — \
                                             engine retains pending; outer loop re-emits",
                                            prevote.height,
                                            prevote.round,
                                        );
                                        break;
                                    }
                                    bft.mark_prevote_cast();
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
                                    if lp2p_clone
                                        .broadcast_bft_precommit(&signed_pc)
                                        .await
                                        .is_err()
                                    {
                                        tracing::error!(
                                            "BFT precommit broadcast dropped at h={} r={} — \
                                             engine retains pending; outer loop re-emits",
                                            precommit.height,
                                            precommit.round,
                                        );
                                        break;
                                    }
                                    bft.mark_precommit_cast();
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
                                    ref block_hash,
                                    ref justification,
                                } => {
                                    // 2026-05-04 finalize-entry trace — same instrumentation as
                                    // the self-propose arm. Critical for diff'ing active-set
                                    // views across the cluster when divergence recurs.
                                    {
                                        let bc_read = shared_clone.read().await;
                                        let active_count = bc_read.stake_registry.active_set.len();
                                        let total_stake: u64 = bc_read
                                            .stake_registry
                                            .active_set
                                            .iter()
                                            .filter_map(|a| {
                                                bc_read
                                                    .stake_registry
                                                    .get_validator(a)
                                                    .map(|v| v.total_stake())
                                            })
                                            .sum();
                                        drop(bc_read);
                                        let precommit_count = justification.precommits.len();
                                        let precommit_stake: u64 = justification
                                            .precommits
                                            .iter()
                                            .map(|p| p.stake_weight)
                                            .sum();
                                        tracing::info!(
                                            target: "finalize_trace",
                                            "FinalizeBlock peer-propose path: h={} round={} block={:.16}… \
                                             active_count={} total_stake={} precommit_count={} \
                                             precommit_stake={} our_addr={}",
                                            height, round, block_hash, active_count,
                                            total_stake, precommit_count, precommit_stake,
                                            wallet.address,
                                        );
                                    }

                                    // 2026-05-05 chain-already-advanced check (v2.1.63) —
                                    // mirror of the self-propose arm. See the long comment
                                    // in the sibling arm for the full rationale; in short:
                                    // libp2p NewBlock handler can apply the cluster-canonical
                                    // block at this height concurrent with our BFT engine
                                    // reaching FinalizeBlock for the same height, producing
                                    // the off-by-one validate_block rejection that cascaded
                                    // into silent-thread-death across the cluster on
                                    // 2026-05-04. If chain.height() already covers this
                                    // action's height, the block is on our chain — skip
                                    // local write silently and let need_new_round reset
                                    // the engine on the next outer-loop iteration.
                                    {
                                        let bc_read = shared_clone.read().await;
                                        if bc_read.height() >= height {
                                            tracing::info!(
                                                target: "finalize_trace",
                                                "BFT finalize peer-propose: chain.height={} \
                                                 already ≥ action.height={} — block applied via \
                                                 libp2p sync; skipping local write at round={}",
                                                bc_read.height(), height, round,
                                            );
                                            drop(bc_read);
                                            proposed_block = None;
                                            break;
                                        }
                                    }

                                    // 2026-04-30 split-brain guard — same logic as the P1-A
                                    // FinalizeBlock arm (see comment up there for the full
                                    // rationale). Both round-driven finalize entry points have
                                    // to share the gate; otherwise a vote arriving via the
                                    // gossip path can race past it.
                                    if let Some(target_round) =
                                        bft.peer_supermajority_higher_round()
                                    {
                                        tracing::warn!(
                                            "BFT split-brain guard: aborting local finalise at \
                                             h={} round={} — supermajority of peer stake at \
                                             round {}+; catching up",
                                            height,
                                            round,
                                            target_round,
                                        );
                                        let bc_read = shared_clone.read().await;
                                        let cu_result = bft
                                            .catch_up_round(target_round, &bc_read.stake_registry);
                                        drop(bc_read);
                                        if let Some(mut prevote) = cu_result {
                                            prevote.sign(&validator_secret_key);
                                            let _ =
                                                lp2p_clone.broadcast_bft_prevote(&prevote).await;
                                        }
                                        break;
                                    }

                                    // 2026-04-30 hash-mismatch guard — sibling to the P1-A
                                    // arm above. Refuse to write a stashed block whose hash
                                    // doesn't match the FinalizeBlock action's block_hash;
                                    // log + drop the stale stash + break so peer-gossip
                                    // ships us the canonical block. See the long comment in
                                    // the sibling arm and audits/2026-04-30-eager-write-
                                    // investigation.md for the divergence shape this closes.
                                    if let Some(stashed) = proposed_block.as_ref()
                                        && &stashed.hash != block_hash
                                    {
                                        tracing::warn!(
                                            "BFT finalize: stashed proposed_block hash \
                                             {:.16}… ≠ FinalizeBlock action hash {:.16}… \
                                             at h={} round={}; refusing write and waiting \
                                             for peer-gossip canonical block",
                                            stashed.hash,
                                            block_hash,
                                            height,
                                            round,
                                        );
                                        proposed_block = None;
                                        break;
                                    }

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
                                                        .map(|p| {
                                                            (p.validator.clone(), p.stake_weight)
                                                        })
                                                        .collect();
                                                let validator_fee = 0;
                                                let _ = bc.stake_registry.distribute_reward(
                                                    &proposer,
                                                    &reward_signers,
                                                    reward,
                                                    validator_fee,
                                                );

                                                bc.run_epoch_bookkeeping(height);

                                                tracing::info!(
                                                    "BFT finalized height={} round={}",
                                                    height,
                                                    round
                                                );

                                                // Fix C: speculative pre-build N+1 on the peer-
                                                // finalize path too — if this validator is the
                                                // proposer for N+1 round 0 we save the build cost
                                                // when the next round_start fires.
                                                let next_h_pf = bc.height().saturating_add(1);
                                                let we_next_pf = bc
                                                    .stake_registry
                                                    .weighted_proposer(next_h_pf, 0)
                                                    .as_deref()
                                                    == Some(wallet.address.as_str());
                                                if we_next_pf {
                                                    match bc.create_block_voyager(&wallet.address) {
                                                        Ok(block) => {
                                                            let block_hash = block.hash.clone();
                                                            let block_data =
                                                                bincode::serialize(&block)
                                                                    .unwrap_or_default();
                                                            // F-D Variant A v3 embedded
                                                            // unsigned proposer prevote.
                                                            let proposer_prevote = Some(Prevote {
                                                                height: next_h_pf,
                                                                round: 0,
                                                                block_hash: Some(
                                                                    block_hash.clone(),
                                                                ),
                                                                validator: wallet.address.clone(),
                                                                signature: vec![],
                                                            });
                                                            let mut prop = Proposal {
                                                                height: next_h_pf,
                                                                round: 0,
                                                                block_hash,
                                                                block_data,
                                                                proposer: wallet.address.clone(),
                                                                signature: vec![],
                                                                proposer_prevote,
                                                            };
                                                            prop.sign(&validator_secret_key);
                                                            tracing::debug!(
                                                                target: "speculative_build",
                                                                "pre-built proposal h={} from FinalizeBlock(peer) arm",
                                                                next_h_pf,
                                                            );
                                                            speculative_proposal =
                                                                Some((next_h_pf, block, prop));
                                                        }
                                                        Err(e) => tracing::warn!(
                                                            "speculative build for h={} failed: {}",
                                                            next_h_pf,
                                                            e,
                                                        ),
                                                    }
                                                } else {
                                                    speculative_proposal = None;
                                                }

                                                drop(bc);
                                                if let Some(ref saved_block) = updated {
                                                    // H-09: persist block bytes before broadcast
                                                    // (small write, kept sync). The expensive full
                                                    // state snapshot moves to the writer queue
                                                    // (Fix A); B2 load-replay covers a crash
                                                    // between this point and the queued save.
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
                                                        if save_tx_clone.try_send(height).is_err() {
                                                            tracing::warn!(
                                                                target: "save_writer",
                                                                "save queue full at h={}; \
                                                                 B2 load-replay will catch up",
                                                                height,
                                                            );
                                                        }
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
                                            last_signed_proposal = Some(proposal.clone());
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
                                            last_signed_proposal = Some(proposal.clone());
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
                    // v2.2.2: tightened 2s → 500ms after mainnet bt RCA 2026-05-11.
                    // Round trace at h=1,690,662 (mainnet WAN between validator
                    // hosts) showed a dropped proposal costing ~1.5s before the 2s retry
                    // fired; with ~30% of rounds hitting at least one drop on
                    // the public-IPv4 path, mean bt sat at 2.5 s/blk vs the
                    // sub-1s target. Tighter retry brings recovery inside one
                    // round window. Total budget stays 14 s for cold-start
                    // peers — MAX_REBROADCASTS rises in lockstep.
                    const REBROADCAST_INTERVAL: std::time::Duration =
                        std::time::Duration::from_millis(500);
                    const MAX_REBROADCASTS: u32 = 28;
                    // v2.1.89 fix: replay the originally-signed proposal verbatim
                    // instead of rebuilding + re-signing. The pre-fix path called
                    // `bincode::serialize(block)` and `proposal.sign()` afresh on
                    // every retry; even though the signing payload only covers
                    // (height, round, block_hash) and ECDSA over secp256k1 is
                    // RFC-6979 deterministic, the rebuild path produced occasional
                    // bad-signature rejections on peers — trace 2026-05-08
                    // 23:32:41, proposer → peer foundation. Replaying the
                    // saved Proposal struct sidesteps every re-encode/re-sign
                    // hazard: same bytes, same signature, byte-for-byte identical
                    // wire image. Only fires when the saved proposal still
                    // matches the engine's current (height, round) — if either
                    // has advanced, we drop the stash and let the next propose
                    // path build a fresh proposal.
                    if let Some(ref signed_prop) = last_signed_proposal
                        && proposed_block.is_some()
                        && signed_prop.height == bft.height()
                        && signed_prop.round == bft.round()
                        && matches!(bft.phase(), BftPhase::Propose | BftPhase::Prevote)
                        && proposal_rebroadcast_count < MAX_REBROADCASTS
                        && proposal_broadcast_at
                            .is_some_and(|t| t.elapsed() >= REBROADCAST_INTERVAL)
                    {
                        lp2p_clone.broadcast_bft_proposal(signed_prop).await;
                        proposal_broadcast_at = Some(std::time::Instant::now());
                        proposal_rebroadcast_count += 1;
                        tracing::info!(
                            "BFT #1d: rebroadcast proposal at height={} round={} attempt={}/{} (replay)",
                            bft.height(),
                            bft.round(),
                            proposal_rebroadcast_count,
                            MAX_REBROADCASTS
                        );
                    }

                    // Vote rebroadcast tick (prevote + precommit) lived here
                    // until 2026-05-10. Removed when BFT votes moved from
                    // request-response to gossipsub — the mesh handles
                    // retransmission via IHAVE/IWANT and there's no point
                    // double-publishing from the validator loop.

                    // Check for BFT timeouts
                    if bft.is_timed_out() {
                        let timeout_action = bft.on_timeout();
                        let mut action = timeout_action;
                        loop {
                            match action {
                                BftAction::BroadcastPrevote(ref prevote) => {
                                    let mut signed_pv = prevote.clone();
                                    signed_pv.sign(&validator_secret_key);
                                    if lp2p_clone.broadcast_bft_prevote(&signed_pv).await.is_err() {
                                        tracing::error!(
                                            "BFT prevote broadcast dropped at h={} r={} — \
                                             engine retains pending; outer loop re-emits",
                                            prevote.height,
                                            prevote.round,
                                        );
                                        break;
                                    }
                                    bft.mark_prevote_cast();
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
                                    if lp2p_clone
                                        .broadcast_bft_precommit(&signed_pc)
                                        .await
                                        .is_err()
                                    {
                                        tracing::error!(
                                            "BFT precommit broadcast dropped at h={} r={} — \
                                             engine retains pending; outer loop re-emits",
                                            precommit.height,
                                            precommit.round,
                                        );
                                        break;
                                    }
                                    bft.mark_precommit_cast();
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
                    // 2026-05-05 v2.1.63: explicit log for the libp2p-applied
                    // path so the BFT-engine vs chain.height race we hit on
                    // 2026-05-04 is greppable in journalctl. Pair this with
                    // `finalize_trace: ... block applied via libp2p sync` to
                    // see when our local FinalizeBlock got pre-empted.
                    tracing::info!(
                        "libp2p NewBlock: applying block {} from peer; chain will advance, \
                         BFT engine will resync via need_new_round on next validator iter",
                        block.index
                    );
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
                    try_send_bft(
                        &bft_tx_clone,
                        sentrix::core::bft_messages::BftMessage::Propose(p),
                        "BftProposal",
                    );
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
                    try_send_bft(
                        &bft_tx_clone,
                        sentrix::core::bft_messages::BftMessage::Prevote(v),
                        "BftPrevote",
                    );
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
                    try_send_bft(
                        &bft_tx_clone,
                        sentrix::core::bft_messages::BftMessage::Precommit(c),
                        "BftPrecommit",
                    );
                }
                NodeEvent::BftRoundStatus(s) => {
                    tracing::debug!(
                        "BFT round-status: height={} round={} from={}",
                        s.height,
                        s.round,
                        &s.validator[..s.validator.len().min(12)]
                    );
                    try_send_bft(
                        &bft_tx_clone,
                        sentrix::core::bft_messages::BftMessage::RoundStatus(s),
                        "BftRoundStatus",
                    );
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
    // Construct the event bus FIRST so the same instance is wired into
    // both the consensus path (Blockchain emits via set_event_emitter)
    // and the WebSocket subscription handler (subscribers .subscribe()
    // on the broadcast channels). Without sharing, WebSocket clients
    // would never receive newHeads events.
    let event_bus = std::sync::Arc::new(EventBus::new());
    {
        let mut bc = shared.write().await;
        bc.set_event_emitter(Some(event_bus.clone()));
    }

    // libp2p tx-gossip pump — every mempool admit fires
    // emit_tx_for_gossip; this task forwards to the libp2p gossipsub
    // `txs` topic so peer mempools see the tx. Without it,
    // public-RPC fullnode admits never reach validator mempools and
    // user txs only land when the round-robin upstream happens to be
    // a validator. Closes #683.
    let lp2p_for_tx_gossip = lp2p.clone();
    let mut tx_gossip_rx = event_bus.tx_for_gossip.subscribe();
    tokio::spawn(async move {
        loop {
            match tx_gossip_rx.recv().await {
                Ok(tx) => lp2p_for_tx_gossip.broadcast_transaction(&tx).await,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(
                        "tx-gossip pump lagged {} txs — increase EventBus capacity",
                        n
                    );
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    tracing::info!("tx-gossip pump shutting down (channel closed)");
                    return;
                }
            }
        }
    });

    let app = create_router_with_bus(shared.clone(), event_bus.clone());
    let api_addr = format!("{}:{}", get_api_host(), get_api_port());
    println!("REST API listening on http://{}", api_addr);
    let listener = tokio::net::TcpListener::bind(&api_addr).await?;

    // 2026-05-05 v2.1.69: side-car Tonic gRPC server. Default OFF so
    // the v2.1.68 production behaviour is unchanged unless an operator
    // explicitly opts in via `SENTRIX_GRPC_ENABLED=1`. When enabled, the
    // server binds `SENTRIX_GRPC_ADDR` (default `0.0.0.0:50051`) in a
    // side-car tokio task — a wedged gRPC handler can stall its own
    // task without affecting the validator main loop or the axum HTTP
    // server. Read paths share the same `Arc<RwLock<Blockchain>>` as
    // the JSON-RPC stack; same lock-contention profile as adding
    // another axum handler.
    if std::env::var("SENTRIX_GRPC_ENABLED")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        let grpc_state = shared.clone();
        let grpc_event_bus = event_bus.clone();
        let grpc_addr_str =
            std::env::var("SENTRIX_GRPC_ADDR").unwrap_or_else(|_| "0.0.0.0:50051".to_string());
        match grpc_addr_str.parse::<std::net::SocketAddr>() {
            Ok(grpc_addr) => {
                tracing::info!(
                    "starting sentrix-grpc side-car at {} (env-var gated)",
                    grpc_addr
                );
                tokio::spawn(async move {
                    let server = sentrix_grpc::server_factory(grpc_state, grpc_event_bus);
                    // v2.1.70: accept_http1(true) + GrpcWebLayer so browsers
                    // can hit the same port. Pure gRPC clients (HTTP/2 +
                    // application/grpc) still work — the layer dispatches by
                    // content-type. CORS handled at Caddy edge, not here.
                    if let Err(e) = tonic::transport::Server::builder()
                        .accept_http1(true)
                        .layer(tonic_web::GrpcWebLayer::new())
                        .add_service(server)
                        .serve(grpc_addr)
                        .await
                    {
                        tracing::error!(
                            "sentrix-grpc server crashed (validator unaffected): {}",
                            e
                        );
                    }
                });
            }
            Err(e) => {
                tracing::error!(
                    "SENTRIX_GRPC_ADDR={} is not a valid socket address: {} — \
                     gRPC side-car NOT started",
                    grpc_addr_str,
                    e
                );
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    /// L2 gate: 4-validator mesh requires 3 peers (active_set.len() - 1).
    /// 2026-04-25 incident reproduction — Beacon node had 1 peer, would have
    /// been blocked by this check.
    /// (required_peers = active_set_len - 1 for pre-fork legacy behavior.)
    #[test]
    fn peer_mesh_gate_blocks_partitioned_validator() {
        let result = check_bft_peer_mesh_eligible(1, 4, 3, false);
        assert!(result.is_err(), "1 peer in 4-val mesh must be rejected");
        let msg = result.unwrap_err();
        assert!(
            msg.contains("need ≥3"),
            "error must state requirement: {msg}"
        );
        assert!(
            msg.contains("have 1"),
            "error must state actual count: {msg}"
        );
    }

    /// Healthy fully-meshed 4-validator chain: 3 peers passes.
    #[test]
    fn peer_mesh_gate_allows_fully_meshed_validator() {
        assert!(check_bft_peer_mesh_eligible(3, 4, 3, false).is_ok());
    }

    /// Above-threshold (more peers than active set members - 1) is also fine
    /// — non-validator peers count toward the libp2p peer count too.
    #[test]
    fn peer_mesh_gate_allows_extra_peers() {
        assert!(check_bft_peer_mesh_eligible(10, 4, 3, false).is_ok());
    }

    /// Operator emergency override (`SENTRIX_FORCE_BFT_INSUFFICIENT_PEERS=1`)
    /// must bypass the gate even with zero peers. Re-creates the
    /// 2026-04-25 livelock condition deliberately — used only when an
    /// operator decides the partition risk is acceptable.
    #[test]
    fn peer_mesh_gate_force_override_allows_zero_peers() {
        assert!(check_bft_peer_mesh_eligible(0, 4, 3, true).is_ok());
    }

    /// Single-validator chain (testnet bootstrap, recovery scenario):
    /// peer count is trivially satisfied because `active_set - 1 == 0`.
    #[test]
    fn peer_mesh_gate_single_validator_chain_always_passes() {
        assert!(check_bft_peer_mesh_eligible(0, 1, 0, false).is_ok());
    }

    // Note: a previous test asserted `check_bft_peer_mesh_eligible(0, 0, false).is_ok()`
    // — that test was based on the original `<= 1` short-circuit, which masked
    // the real bug of an empty active_set reaching activation. Replaced by
    // `peer_mesh_gate_empty_active_set_errors_explicitly` below.

    /// 2-validator chain edge case: `active_set - 1 == 1` peer required.
    #[test]
    fn peer_mesh_gate_two_validator_chain() {
        assert!(check_bft_peer_mesh_eligible(0, 2, 1, false).is_err());
        assert!(check_bft_peer_mesh_eligible(1, 2, 1, false).is_ok());
    }

    /// Boundary: peer_count exactly equal to threshold passes.
    #[test]
    fn peer_mesh_gate_boundary_equal_passes() {
        assert!(check_bft_peer_mesh_eligible(3, 4, 3, false).is_ok());
    }

    /// Boundary: one below threshold fails.
    #[test]
    fn peer_mesh_gate_boundary_below_fails() {
        assert!(check_bft_peer_mesh_eligible(2, 4, 3, false).is_err());
    }

    /// Post-fork (BFT_GATE_RELAX_HEIGHT active): 4-validator network with
    /// only 2 peers (= 1 jail tolerance) must PASS. Pre-fork required 3
    /// peers; post-fork requires only `min_active_for_bft - 1 = 3 - 1 = 2`.
    /// Regression test for the 2026-04-27 jail-induction stall finding —
    /// without this relaxation, chain stalls when 1 of 4 validators is down.
    #[test]
    fn peer_mesh_gate_post_fork_allows_jail_tolerance() {
        // Post-fork required_peers = 2 (= ⌈2/3 × 4⌉ - 1 = 3 - 1).
        // peer_count=2 must pass (1-jail tolerance scenario).
        assert!(check_bft_peer_mesh_eligible(2, 4, 2, false).is_ok());
        // peer_count=1 still fails (would mean 2-jail = no supermajority).
        assert!(check_bft_peer_mesh_eligible(1, 4, 2, false).is_err());
    }

    /// Empty active_set produces explicit error (post-self-review fix).
    /// The `<= 1` shortcut was previously silently passing this case,
    /// masking a potential DPoS-migration bug where stake_registry ends
    /// up empty post-migration.
    #[test]
    fn peer_mesh_gate_empty_active_set_errors_explicitly() {
        let result = check_bft_peer_mesh_eligible(0, 0, 0, false);
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
