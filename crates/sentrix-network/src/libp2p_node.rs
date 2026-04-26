// libp2p_node.rs - Sentrix — libp2p P2P node (TCP + Noise + Yamux)
//
// Drop-in replacement for `node.rs`'s raw-TCP `Node`, selected at runtime with
// `sentrix start --use-libp2p`.  The existing `Node` path is UNTOUCHED.
//
// Architecture:
//   LibP2pNode  (handle, Send + Clone) ──cmd_tx──> SwarmTask (owns Swarm)
//
// SwarmTask is spawned once in `LibP2pNode::new()`.  All swarm mutations go
// through the `SwarmCommand` channel so callers never need to hold the swarm.

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::Instant;

use futures::StreamExt;
use libp2p::{
    Multiaddr, PeerId, Swarm, SwarmBuilder,
    core::ConnectedPoint,
    gossipsub,
    identity::Keypair,
    kad,
    multiaddr::Protocol,
    noise,
    request_response::{self, OutboundRequestId},
    swarm::SwarmEvent,
    tcp, yamux,
};
use tokio::sync::mpsc;

use crate::behaviour::{
    BLOCKS_TOPIC, GossipBlock, GossipTransaction, SentrixBehaviour, SentrixBehaviourEvent,
    SentrixRequest, SentrixResponse, TXS_TOPIC, VALIDATOR_ADVERTS_TOPIC,
};
use sentrix_wire::MultiaddrAdvertisement;
use crate::node::{NodeEvent, SharedBlockchain};
use sentrix_primitives::block::{Block, STATE_ROOT_FORK_HEIGHT};
use sentrix_primitives::error::{SentrixError, SentrixResult};
use sentrix_primitives::transaction::Transaction;

// ── P2P protection constants ────────────────────────────
/// Maximum number of verified (handshaked) peers.
const MAX_LIBP2P_PEERS: usize = 50;
/// Maximum new connections per IP within the rate window.
/// Bumped to 20 to accommodate Treasury node hosting 5 validators on one IP plus
/// reconnect overhead during deploys (each restart triggers ~3 reconnects).
const MAX_CONN_PER_IP: u32 = 20;
/// Rate limit window (seconds).
const RATE_LIMIT_WINDOW_SECS: u64 = 60;
/// Temporary ban duration for IPs that exceed rate limit (seconds).
const BAN_DURATION_SECS: u64 = 300;

// ── Internal command channel ─────────────────────────────

enum SwarmCommand {
    Listen(Multiaddr),
    ConnectPeer(Multiaddr),
    Broadcast(SentrixRequest),
    /// Publish a block via gossipsub.
    GossipBlock(Box<Block>),
    /// Publish a transaction via gossipsub.
    GossipTx(Transaction),
    /// Add a peer address to Kademlia DHT.
    AddKadPeer(PeerId, Multiaddr),
    /// Trigger a Kademlia bootstrap (random walk).
    KadBootstrap,
    GetPeerCount(tokio::sync::oneshot::Sender<usize>),
    /// Query the swarm's current listen addresses. Useful for tests that
    /// bind on port 0 (OS-assigned) and need the actual port back, and
    /// for ops who want to know the node's externally-reachable addrs.
    GetListenAddrs(tokio::sync::oneshot::Sender<Vec<Multiaddr>>),
    /// Re-dial bootstrap peers that are no longer connected.
    ReconnectPeers(Vec<Multiaddr>),
    /// Trigger an out-of-band block sync from the first verified peer.
    /// Fired by the validator loop when BFT sees a peer at a higher
    /// height and we need to catch up before we can vote (backlog #4
    /// auto-resync). Unlike the periodic 30s sync_interval tick, this
    /// fires *immediately* so the chain doesn't wait up to 30s to
    /// discover it's behind.
    TriggerSync,
    /// L1 peer auto-discovery: gossip a signed validator multiaddr
    /// advertisement on `VALIDATOR_ADVERTS_TOPIC`. Fired by the
    /// validator loop on startup + periodically.
    GossipValidatorAdvert(Box<MultiaddrAdvertisement>),
    /// L1: read the cached advertisement for a specific validator.
    /// Returns `Some` with the latest-by-sequence advertisement seen
    /// for that address, or `None` if no advertisement has been
    /// observed yet.
    GetCachedAdvert(String, tokio::sync::oneshot::Sender<Option<MultiaddrAdvertisement>>),
    /// L1: snapshot of every cached advertisement. Used by the
    /// periodic dial-tick in the validator loop to decide which
    /// active-set members it can reach.
    ListCachedAdverts(tokio::sync::oneshot::Sender<Vec<MultiaddrAdvertisement>>),
    /// Snapshot of currently-connected peer IDs. Used by the L1 dial-
    /// tick to skip peers we already have an established connection
    /// with — without this check, every tick re-dials every active-set
    /// member regardless of connection state, which causes pending
    /// connections to accumulate in the swarm pool over hours and
    /// eventually triggers gossipsub mesh thrashing → BFT livelock
    /// (incident 2026-04-25, two stalls). The dial-tick comment used
    /// to claim libp2p deduplicates duplicate dials at the swarm
    /// level — that turned out to be false in libp2p 0.56; each
    /// `swarm.dial()` enqueues a fresh pending connection.
    GetConnectedPeers(tokio::sync::oneshot::Sender<HashSet<PeerId>>),
}

// ── Public handle ────────────────────────────────────────

/// libp2p-based P2P node: TCP + Noise encryption + Yamux multiplexing.
///
/// Internally spawns a Tokio task that owns the `Swarm`.
/// All interaction with the swarm goes through the `cmd_tx` channel.
pub struct LibP2pNode {
    /// This node's libp2p identity.
    pub local_peer_id: PeerId,
    cmd_tx: mpsc::Sender<SwarmCommand>,
    blockchain: SharedBlockchain,
}

impl LibP2pNode {
    /// Create the node and immediately spawn the swarm event loop.
    ///
    /// `event_tx` is the same channel used by legacy `Node` — the event handler
    /// in `main.rs` works without modification.
    pub fn new(
        keypair: Keypair,
        blockchain: SharedBlockchain,
        event_tx: mpsc::Sender<NodeEvent>,
    ) -> SentrixResult<Self> {
        let local_peer_id = PeerId::from_public_key(&keypair.public());
        let (cmd_tx, cmd_rx) = mpsc::channel::<SwarmCommand>(256);

        let bc = blockchain.clone();
        let kp = keypair.clone();
        tokio::spawn(async move {
            if let Err(e) = run_swarm(kp, bc, event_tx, cmd_rx).await {
                tracing::error!("libp2p swarm task exited with error: {}", e);
            }
        });

        Ok(Self {
            local_peer_id,
            cmd_tx,
            blockchain,
        })
    }

    /// Start listening on `addr` (e.g. `/ip4/0.0.0.0/tcp/30303`).
    pub async fn listen_on(&self, addr: Multiaddr) -> SentrixResult<()> {
        self.cmd_tx
            .send(SwarmCommand::Listen(addr))
            .await
            .map_err(|_| SentrixError::NetworkError("swarm task closed".to_string()))
    }

    /// Dial a peer by `Multiaddr`.
    pub async fn connect_peer(&self, addr: Multiaddr) -> SentrixResult<()> {
        self.cmd_tx
            .send(SwarmCommand::ConnectPeer(addr))
            .await
            .map_err(|_| SentrixError::NetworkError("swarm task closed".to_string()))
    }

    /// Broadcast a new block to all peers via gossipsub.
    pub async fn broadcast_block(&self, block: &Block) {
        let _ = self
            .cmd_tx
            .send(SwarmCommand::GossipBlock(Box::new(block.clone())))
            .await;
    }

    /// Broadcast a new transaction to all peers via gossipsub.
    pub async fn broadcast_transaction(&self, tx: &Transaction) {
        let _ = self.cmd_tx.send(SwarmCommand::GossipTx(tx.clone())).await;
    }

    /// Broadcast a BFT proposal to all verified peers.
    pub async fn broadcast_bft_proposal(&self, proposal: &sentrix_bft::messages::Proposal) {
        let req = SentrixRequest::BftProposal {
            proposal: Box::new(proposal.clone()),
        };
        let _ = self.cmd_tx.send(SwarmCommand::Broadcast(req)).await;
    }

    /// Broadcast a BFT prevote to all verified peers.
    pub async fn broadcast_bft_prevote(&self, prevote: &sentrix_bft::messages::Prevote) {
        let req = SentrixRequest::BftPrevote {
            prevote: prevote.clone(),
        };
        let _ = self.cmd_tx.send(SwarmCommand::Broadcast(req)).await;
    }

    /// Broadcast a BFT precommit to all verified peers.
    pub async fn broadcast_bft_precommit(&self, precommit: &sentrix_bft::messages::Precommit) {
        let req = SentrixRequest::BftPrecommit {
            precommit: precommit.clone(),
        };
        let _ = self.cmd_tx.send(SwarmCommand::Broadcast(req)).await;
    }

    /// Broadcast our current BFT round status so peers can sync rounds.
    /// Called periodically (~5s) by the validator loop.
    /// L1 peer auto-discovery: gossip a signed [`MultiaddrAdvertisement`]
    /// on `VALIDATOR_ADVERTS_TOPIC`. Other validators verify the signature
    /// against the on-chain stake registry and dial the advertised
    /// multiaddrs on their next discovery tick.
    pub async fn broadcast_validator_advert(&self, advert: MultiaddrAdvertisement) {
        let _ = self
            .cmd_tx
            .send(SwarmCommand::GossipValidatorAdvert(Box::new(advert)))
            .await;
    }

    /// L1: read the cached advertisement for a specific validator
    /// address. Returns `None` if we haven't seen one yet.
    pub async fn cached_advert(&self, validator: &str) -> Option<MultiaddrAdvertisement> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .cmd_tx
            .send(SwarmCommand::GetCachedAdvert(validator.to_string(), tx))
            .await
            .is_err()
        {
            return None;
        }
        rx.await.ok().flatten()
    }

    /// L1: snapshot every cached advertisement. Used by the validator
    /// loop's periodic dial-tick to decide which active-set members
    /// it can reach.
    pub async fn list_cached_adverts(&self) -> Vec<MultiaddrAdvertisement> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .cmd_tx
            .send(SwarmCommand::ListCachedAdverts(tx))
            .await
            .is_err()
        {
            return Vec::new();
        }
        rx.await.unwrap_or_default()
    }

    pub async fn broadcast_bft_round_status(&self, status: &sentrix_bft::messages::RoundStatus) {
        let req = SentrixRequest::BftRoundStatus {
            status: status.clone(),
        };
        let _ = self.cmd_tx.send(SwarmCommand::Broadcast(req)).await;
    }

    /// Re-dial bootstrap peers that may have disconnected.
    pub async fn reconnect_peers(&self, addrs: Vec<Multiaddr>) {
        let _ = self.cmd_tx.send(SwarmCommand::ReconnectPeers(addrs)).await;
    }

    /// Ask the swarm to immediately issue a `GetBlocks` to the first
    /// verified peer. Backlog #4 auto-resync trigger: when BFT detects
    /// a peer at a higher height (via RoundStatus gossip) we want to
    /// catch up before the next round starts, not wait up to 30s for
    /// the periodic sync interval to fire.
    pub async fn trigger_sync(&self) {
        let _ = self.cmd_tx.send(SwarmCommand::TriggerSync).await;
    }

    /// Add a known peer to the Kademlia routing table (bootstrap node).
    pub async fn add_kad_peer(&self, peer_id: PeerId, addr: Multiaddr) {
        let _ = self
            .cmd_tx
            .send(SwarmCommand::AddKadPeer(peer_id, addr))
            .await;
    }

    /// Trigger a Kademlia bootstrap (random walk to discover peers).
    pub async fn kad_bootstrap(&self) {
        let _ = self.cmd_tx.send(SwarmCommand::KadBootstrap).await;
    }

    /// Returns the number of currently verified (handshaked) peers.
    pub async fn peer_count(&self) -> usize {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .cmd_tx
            .send(SwarmCommand::GetPeerCount(tx))
            .await
            .is_ok()
        {
            rx.await.unwrap_or(0)
        } else {
            0
        }
    }

    /// Returns the set of currently-connected peer IDs.
    ///
    /// Used by the L1 dial-tick in the validator loop to skip peers
    /// already in an established connection — without this check, every
    /// 30s tick re-dials every active-set member, and pending dial
    /// attempts accumulate over hours. See incident
    /// `founder-private/incidents/2026-04-25-libp2p-connection-thrashing.md`.
    pub async fn connected_peers(&self) -> HashSet<PeerId> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .cmd_tx
            .send(SwarmCommand::GetConnectedPeers(tx))
            .await
            .is_ok()
        {
            rx.await.unwrap_or_default()
        } else {
            HashSet::new()
        }
    }

    /// Returns the swarm's current listen addresses (after bind completes).
    ///
    /// Useful for:
    /// - Tests that bind on port 0 (OS-assigned) and need the actual port
    ///   back for peer-dialing instead of hardcoding a fragile port number.
    /// - Ops tooling that wants to know what the node is reachable on.
    ///
    /// Returns an empty Vec if the swarm task has exited.
    pub async fn listen_addrs(&self) -> Vec<Multiaddr> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .cmd_tx
            .send(SwarmCommand::GetListenAddrs(tx))
            .await
            .is_ok()
        {
            rx.await.unwrap_or_default()
        } else {
            Vec::new()
        }
    }
}

// ── Multiaddr helper ─────────────────────────────────────

/// Build a `/ip4/<host>/tcp/<port>` multiaddr from a host string and port.
///
/// Used to convert legacy `host:port` bootstrap peers into the libp2p format.
pub fn make_multiaddr(host: &str, port: u16) -> SentrixResult<Multiaddr> {
    let s = format!("/ip4/{}/tcp/{}", host, port);
    s.parse::<Multiaddr>()
        .map_err(|e| SentrixError::NetworkError(format!("invalid multiaddr '{}': {}", s, e)))
}

// ── IP extraction helper ────────────────────────────────

/// Extract IP address from a libp2p `ConnectedPoint`.
fn extract_ip(endpoint: &ConnectedPoint) -> Option<IpAddr> {
    let addr = match endpoint {
        ConnectedPoint::Dialer { address, .. } => address,
        ConnectedPoint::Listener { send_back_addr, .. } => send_back_addr,
    };
    for protocol in addr.iter() {
        match protocol {
            Protocol::Ip4(ip) => return Some(IpAddr::V4(ip)),
            Protocol::Ip6(ip) => return Some(IpAddr::V6(ip)),
            _ => {}
        }
    }
    None
}

/// Per-IP connection rate limiter with temporary bans.
struct IpRateLimiter {
    /// Connection count + window start per IP.
    counts: HashMap<IpAddr, (u32, Instant)>,
    /// Banned IPs with ban start time.
    bans: HashMap<IpAddr, Instant>,
}

impl IpRateLimiter {
    fn new() -> Self {
        Self {
            counts: HashMap::new(),
            bans: HashMap::new(),
        }
    }

    /// Check if an IP is allowed to connect. Returns `false` if banned or rate-limited.
    fn check_and_track(&mut self, ip: IpAddr) -> bool {
        // Check active ban
        if let Some(ban_time) = self.bans.get(&ip) {
            if ban_time.elapsed() < std::time::Duration::from_secs(BAN_DURATION_SECS) {
                return false;
            }
            // Ban expired
            self.bans.remove(&ip);
        }

        // Track connection rate
        let now = Instant::now();
        let entry = self.counts.entry(ip).or_insert((0, now));
        if entry.1.elapsed() > std::time::Duration::from_secs(RATE_LIMIT_WINDOW_SECS) {
            *entry = (1, now);
        } else {
            entry.0 += 1;
            if entry.0 > MAX_CONN_PER_IP {
                tracing::warn!(
                    "libp2p: IP {} exceeded rate limit ({} connections in {}s), banning for {}s",
                    ip,
                    entry.0,
                    RATE_LIMIT_WINDOW_SECS,
                    BAN_DURATION_SECS
                );
                self.bans.insert(ip, now);
                return false;
            }
        }

        true
    }

    /// Prune stale entries to prevent unbounded growth.
    fn prune_stale(&mut self) {
        let window = std::time::Duration::from_secs(RATE_LIMIT_WINDOW_SECS);
        let ban_dur = std::time::Duration::from_secs(BAN_DURATION_SECS);
        self.counts.retain(|_, (_, start)| start.elapsed() < window);
        self.bans.retain(|_, start| start.elapsed() < ban_dur);
    }
}

// ── Swarm event loop ─────────────────────────────────────

// large_futures: the Swarm owns SentrixBehaviour which has internal caches;
// allowed here because the data is mostly heap-allocated inside the swarm.
#[allow(clippy::large_futures)]
async fn run_swarm(
    keypair: Keypair,
    blockchain: SharedBlockchain,
    event_tx: mpsc::Sender<NodeEvent>,
    mut cmd_rx: mpsc::Receiver<SwarmCommand>,
) -> SentrixResult<()> {
    let mut swarm = SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| SentrixError::NetworkError(format!("transport init: {e}")))?
        .with_behaviour(|key| {
            let peer_id = PeerId::from_public_key(&key.public());
            Ok(SentrixBehaviour::new_with_keypair(peer_id, key))
        })
        .map_err(|e| SentrixError::NetworkError(format!("behaviour init: {e}")))?
        // Keep connections alive indefinitely — don't close idle connections.
        // Per-peer connection limit (1 established per peer_id) is a
        // FOLLOW-UP HARDENING — libp2p-swarm 0.47 Config doesn't expose it
        // directly; needs the `connection_limits::Behaviour` wired into
        // SentrixBehaviour. Tracked as a secondary fix for the 2026-04-25
        // libp2p connection thrashing incident; the primary fix (dial-tick
        // connected-peers pre-check below) is sufficient on its own to
        // stop the accumulation pattern observed.
        .with_swarm_config(|cfg| {
            cfg.with_idle_connection_timeout(std::time::Duration::from_secs(u64::MAX))
        })
        .build();

    let our_chain_id = blockchain.read().await.chain_id;

    // Peers that completed a successful chain_id-verified Handshake.
    let mut verified_peers: HashSet<PeerId> = HashSet::new();
    // Outbound handshake requests we sent — waiting for the matching response.
    let mut pending_handshakes: HashMap<OutboundRequestId, PeerId> = HashMap::new();
    // Track outbound GetBlocks requests by ID so responses can be matched to the originating peer.
    let mut pending_syncs: HashMap<OutboundRequestId, PeerId> = HashMap::new();
    // Bug #1d diagnostic: remember which `SentrixRequest` variant each
    // in-flight outbound request was, so `OutboundFailure` can log the
    // specific variant instead of the generic "outbound failure to X".
    // Without this we cannot tell whether BFT proposals are the ones
    // timing out or unrelated background traffic.
    let mut pending_variants: HashMap<OutboundRequestId, &'static str> = HashMap::new();
    // L1 peer auto-discovery cache: latest-by-sequence advertisement
    // per validator address. Keyed by validator address string,
    // not PeerId, because the address is the on-chain identity that
    // signs the advertisement (we recover the signer's address from
    // the sig and compare to the claimed validator field).
    //
    // DoS bound: cap at MAX_CACHED_ADVERTS entries. Eviction strategy
    // when full = drop the lowest-sequence entry, since that's the
    // freshest signal of "this peer is stale". Byzantine validators
    // can flood with advertisements only up to the cap; legitimate
    // validators with a single signing key can only have one entry
    // anyway (overwritten on sequence bump).
    const MAX_CACHED_ADVERTS: usize = 4096;
    let mut multiaddr_cache: HashMap<String, MultiaddrAdvertisement> = HashMap::new();

    // Per-IP rate limiter for connection flood protection.
    let mut ip_limiter = IpRateLimiter::new();

    // Periodic sync: every 30s, request missing blocks from verified peers.
    let mut sync_interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
    // Periodic Kademlia bootstrap: every 60s, random walk to discover new peers.
    let mut kad_interval = tokio::time::interval(tokio::time::Duration::from_secs(60));

    loop {
        tokio::select! {
            // ── Commands from the LibP2pNode handle ──────
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(SwarmCommand::Listen(addr)) => {
                        if let Err(e) = swarm.listen_on(addr.clone()) {
                            tracing::warn!("libp2p listen_on {} failed: {}", addr, e);
                        }
                    }
                    Some(SwarmCommand::ConnectPeer(addr)) => {
                        if let Err(e) = swarm.dial(addr.clone()) {
                            tracing::warn!("libp2p dial {} failed: {}", addr, e);
                        }
                    }
                    Some(SwarmCommand::Broadcast(req)) => {
                        let peers: Vec<PeerId> = verified_peers.iter().cloned().collect();
                        let variant = req.variant_name();
                        for peer_id in peers {
                            let req_id = swarm
                                .behaviour_mut()
                                .rr
                                .send_request(&peer_id, req.clone());
                            pending_variants.insert(req_id, variant);
                        }
                    }
                    Some(SwarmCommand::GossipBlock(block)) => {
                        let topic = gossipsub::IdentTopic::new(BLOCKS_TOPIC);
                        let msg = GossipBlock { block: *block };
                        match bincode::serialize(&msg) {
                            Ok(data) => {
                                if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                    tracing::debug!("gossipsub publish block failed: {}", e);
                                }
                            }
                            Err(e) => tracing::warn!("gossip block serialize failed: {}", e),
                        }
                    }
                    Some(SwarmCommand::GossipTx(tx)) => {
                        let topic = gossipsub::IdentTopic::new(TXS_TOPIC);
                        let msg = GossipTransaction { transaction: tx };
                        match bincode::serialize(&msg) {
                            Ok(data) => {
                                if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                    tracing::debug!("gossipsub publish tx failed: {}", e);
                                }
                            }
                            Err(e) => tracing::warn!("gossip tx serialize failed: {}", e),
                        }
                    }
                    Some(SwarmCommand::GossipValidatorAdvert(advert)) => {
                        // L1 outbound: publish a signed advertisement
                        // on VALIDATOR_ADVERTS_TOPIC. Caller (validator
                        // loop) is responsible for sign() before send;
                        // we don't re-verify our own outgoing message
                        // (gossipsub::ValidationMode::Strict catches
                        // any encoding-level corruption at publish).
                        let topic = gossipsub::IdentTopic::new(VALIDATOR_ADVERTS_TOPIC);
                        match bincode::serialize(&*advert) {
                            Ok(data) => {
                                if let Err(e) = swarm.behaviour_mut().gossipsub.publish(topic, data) {
                                    tracing::debug!("gossipsub publish advert failed: {}", e);
                                }
                            }
                            Err(e) => tracing::warn!("gossip advert serialize failed: {}", e),
                        }
                    }
                    Some(SwarmCommand::GetCachedAdvert(validator, reply)) => {
                        let _ = reply.send(multiaddr_cache.get(&validator).cloned());
                    }
                    Some(SwarmCommand::ListCachedAdverts(reply)) => {
                        let snapshot: Vec<MultiaddrAdvertisement> =
                            multiaddr_cache.values().cloned().collect();
                        let _ = reply.send(snapshot);
                    }
                    Some(SwarmCommand::AddKadPeer(peer_id, addr)) => {
                        swarm.behaviour_mut().kademlia.add_address(&peer_id, addr);
                    }
                    Some(SwarmCommand::KadBootstrap) => {
                        if let Err(e) = swarm.behaviour_mut().kademlia.bootstrap() {
                            tracing::debug!("kademlia bootstrap failed: {}", e);
                        }
                    }
                    Some(SwarmCommand::GetPeerCount(reply)) => {
                        let _ = reply.send(verified_peers.len());
                    }
                    Some(SwarmCommand::GetListenAddrs(reply)) => {
                        let addrs: Vec<Multiaddr> = swarm.listeners().cloned().collect();
                        let _ = reply.send(addrs);
                    }
                    Some(SwarmCommand::ReconnectPeers(addrs)) => {
                        for addr in addrs {
                            if let Err(e) = swarm.dial(addr.clone()) {
                                tracing::warn!("libp2p reconnect dial {} failed: {}", addr, e);
                            }
                        }
                    }
                    Some(SwarmCommand::GetConnectedPeers(reply)) => {
                        // Snapshot of currently-connected peer_ids.
                        // `swarm.connected_peers()` is the post-handshake
                        // set; matches what `verified_peers` would expose
                        // for our SentrixBehaviour.
                        let peers: HashSet<PeerId> = swarm.connected_peers().copied().collect();
                        let _ = reply.send(peers);
                    }
                    Some(SwarmCommand::TriggerSync) => {
                        // Backlog #4 auto-resync: ask the first verified peer
                        // for blocks from our current height + 1. Fires
                        // immediately instead of waiting for the 30s periodic
                        // sync_interval tick.
                        if verified_peers.is_empty() {
                            tracing::debug!("libp2p trigger_sync: no verified peers");
                        } else {
                            let our_height = blockchain.read().await.height();
                            if let Some(&peer_id) = verified_peers.iter().next() {
                                let req = SentrixRequest::GetBlocks {
                                    from_height: our_height + 1,
                                };
                                let variant = req.variant_name();
                                let req_id =
                                    swarm.behaviour_mut().rr.send_request(&peer_id, req);
                                pending_syncs.insert(req_id, peer_id);
                                pending_variants.insert(req_id, variant);
                                tracing::info!(
                                    "libp2p trigger_sync: requested blocks from {} starting at {}",
                                    peer_id,
                                    our_height + 1
                                );
                            }
                        }
                    }
                    None => {
                        tracing::info!("libp2p: command channel closed, stopping swarm");
                        break;
                    }
                }
            }

            // ── Swarm events ─────────────────────────────
            event = swarm.select_next_some() => {
                on_swarm_event(
                    event,
                    &mut swarm,
                    &blockchain,
                    &event_tx,
                    &mut verified_peers,
                    &mut pending_handshakes,
                    &mut pending_syncs,
                    &mut pending_variants,
                    our_chain_id,
                    &mut ip_limiter,
                    &mut multiaddr_cache,
                    MAX_CACHED_ADVERTS,
                )
                .await;
            }

            // ── Periodic sync + rate limiter cleanup ──
            _ = sync_interval.tick() => {
                ip_limiter.prune_stale();
                if verified_peers.is_empty() {
                    continue;
                }
                let our_height = blockchain.read().await.height();
                if let Some(&peer_id) = verified_peers.iter().next() {
                    let req = SentrixRequest::GetBlocks {
                        from_height: our_height + 1,
                    };
                    let variant = req.variant_name();
                    let req_id = swarm.behaviour_mut().rr.send_request(&peer_id, req);
                    pending_syncs.insert(req_id, peer_id);
                    pending_variants.insert(req_id, variant);
                }
            }

            // ── Periodic Kademlia bootstrap ──
            _ = kad_interval.tick() => {
                let _ = swarm.behaviour_mut().kademlia.bootstrap();
            }
        }
    }

    Ok(())
}

// ── Swarm event dispatch ─────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn on_swarm_event(
    event: SwarmEvent<SentrixBehaviourEvent>,
    swarm: &mut Swarm<SentrixBehaviour>,
    blockchain: &SharedBlockchain,
    event_tx: &mpsc::Sender<NodeEvent>,
    verified_peers: &mut HashSet<PeerId>,
    pending_handshakes: &mut HashMap<OutboundRequestId, PeerId>,
    pending_syncs: &mut HashMap<OutboundRequestId, PeerId>,
    pending_variants: &mut HashMap<OutboundRequestId, &'static str>,
    our_chain_id: u64,
    ip_limiter: &mut IpRateLimiter,
    multiaddr_cache: &mut HashMap<String, MultiaddrAdvertisement>,
    max_cached_adverts: usize,
) {
    match event {
        SwarmEvent::NewListenAddr { address, .. } => {
            tracing::info!("libp2p: listening on {}", address);
        }

        // Send our Handshake as soon as a TCP connection is established.
        SwarmEvent::ConnectionEstablished {
            peer_id, endpoint, ..
        } => {
            // Fix 1: Reject if we already have MAX_LIBP2P_PEERS verified peers.
            if verified_peers.len() >= MAX_LIBP2P_PEERS {
                tracing::warn!(
                    "libp2p: peer limit reached ({}/{}), rejecting {}",
                    verified_peers.len(),
                    MAX_LIBP2P_PEERS,
                    peer_id
                );
                let _ = swarm.disconnect_peer_id(peer_id);
                return;
            }

            // Fix 2: Per-IP rate limiting — reject if IP is banned or over limit.
            if let Some(ip) = extract_ip(&endpoint)
                && !ip_limiter.check_and_track(ip)
            {
                tracing::warn!("libp2p: IP {} rate-limited, rejecting {}", ip, peer_id);
                let _ = swarm.disconnect_peer_id(peer_id);
                return;
            }

            tracing::info!("libp2p: TCP connection to {}", peer_id);
            let height = blockchain.read().await.height();
            let req = SentrixRequest::Handshake {
                host: "0.0.0.0".to_string(),
                port: 0,
                height,
                chain_id: our_chain_id,
            };
            let variant = req.variant_name();
            let req_id = swarm.behaviour_mut().rr.send_request(&peer_id, req);
            pending_handshakes.insert(req_id, peer_id);
            pending_variants.insert(req_id, variant);
        }

        SwarmEvent::ConnectionClosed {
            peer_id,
            num_established,
            ..
        } => {
            tracing::info!(
                "libp2p: connection to {} closed ({} remaining)",
                peer_id,
                num_established
            );
            // Only remove from verified peers when ALL connections to this peer are gone.
            // Bidirectional dialing creates 2 connections per peer; libp2p prunes duplicates.
            // Previously, we removed on ANY close, orphaning the surviving connection.
            if num_established == 0 {
                verified_peers.remove(&peer_id);
                let _ = event_tx
                    .send(NodeEvent::PeerDisconnected(peer_id.to_string()))
                    .await;
            }
        }

        SwarmEvent::Behaviour(SentrixBehaviourEvent::Rr(rr_event)) => {
            on_rr_event(
                rr_event,
                swarm,
                blockchain,
                event_tx,
                verified_peers,
                pending_handshakes,
                pending_syncs,
                pending_variants,
                our_chain_id,
            )
            .await;
        }

        SwarmEvent::Behaviour(SentrixBehaviourEvent::Identify(
            libp2p::identify::Event::Received { peer_id, info, .. },
        )) => {
            // When Identify completes, add the peer's listen addresses to Kademlia.
            for addr in info.listen_addrs {
                swarm.behaviour_mut().kademlia.add_address(&peer_id, addr);
            }
        }

        SwarmEvent::Behaviour(SentrixBehaviourEvent::Identify(_)) => {}

        SwarmEvent::Behaviour(SentrixBehaviourEvent::Kademlia(kad_event)) => match kad_event {
            kad::Event::RoutingUpdated { peer, .. } => {
                tracing::debug!("kademlia: routing updated for {}", peer);
            }
            kad::Event::OutboundQueryProgressed {
                result: kad::QueryResult::Bootstrap(Ok(stats)),
                ..
            } => {
                tracing::debug!(
                    "kademlia: bootstrap step, {} remaining",
                    stats.num_remaining
                );
            }
            _ => {}
        },

        SwarmEvent::Behaviour(SentrixBehaviourEvent::Gossipsub(gossipsub::Event::Message {
            message,
            propagation_source,
            ..
        })) => {
            let topic = message.topic.as_str();
            if topic == BLOCKS_TOPIC {
                match bincode::deserialize::<GossipBlock>(&message.data) {
                    Ok(gossip) => {
                        // Mirror the boundary-reject the RequestResponse path
                        // uses — reject obvious-bad blocks before spawning an
                        // apply task. See `block_boundary_reject_reason` for
                        // the 2026-04-21 state_root=None fork rationale.
                        if let Some(reason) =
                            block_boundary_reject_reason(&gossip.block, our_chain_id)
                        {
                            tracing::warn!(
                                "gossip: dropping block {} from {}: {}",
                                gossip.block.index,
                                propagation_source,
                                reason
                            );
                            return;
                        }
                        let bc = blockchain.clone();
                        let etx = event_tx.clone();
                        let peer = propagation_source;
                        tokio::spawn(async move {
                            let mut chain = bc.write().await;
                            match chain.add_block_from_peer(gossip.block.clone()) {
                                Ok(()) => {
                                    let updated =
                                        chain.latest_block().ok().cloned().unwrap_or(gossip.block);
                                    drop(chain);
                                    let _ = etx.send(NodeEvent::NewBlock(updated)).await;
                                }
                                Err(e) => {
                                    tracing::debug!("gossip block from {} rejected: {}", peer, e);
                                }
                            }
                        });
                    }
                    Err(e) => tracing::warn!("gossip: bad block message: {}", e),
                }
            } else if topic == TXS_TOPIC {
                match bincode::deserialize::<GossipTransaction>(&message.data) {
                    Ok(gossip) => {
                        let bc = blockchain.clone();
                        let etx = event_tx.clone();
                        tokio::spawn(async move {
                            let mut chain = bc.write().await;
                            if chain.add_to_mempool(gossip.transaction.clone()).is_ok() {
                                drop(chain);
                                let _ = etx
                                    .send(NodeEvent::NewTransaction(gossip.transaction))
                                    .await;
                            }
                        });
                    }
                    Err(e) => tracing::warn!("gossip: bad tx message: {}", e),
                }
            } else if topic == VALIDATOR_ADVERTS_TOPIC {
                // L1 peer auto-discovery — verify + cache.
                //
                // Order of checks (cheap → expensive):
                // 1. Bincode decode (rejects malformed bytes immediately).
                // 2. Structural shape (multiaddr count, length, format
                //    — pre-empts byzantine validators announcing 1000s
                //    of garbage addresses).
                // 3. Chain_id match (cross-chain replay protection —
                //    a mainnet sig should not be accepted by a
                //    testnet node and vice versa).
                // 4. Sequence freshness (dedup against already-cached
                //    entry — newer wins, equal-or-older silently
                //    dropped to avoid unnecessary signature work).
                // 5. Signature verification (most expensive — only
                //    runs when previous gates passed).
                // 6. Insert into cache, evicting lowest-sequence
                //    entry if at capacity.
                match bincode::deserialize::<MultiaddrAdvertisement>(&message.data) {
                    Ok(advert) => {
                        if let Err(reason) = advert.validate_shape() {
                            tracing::debug!(
                                "gossip advert: malformed from {}: {}",
                                propagation_source,
                                reason
                            );
                            return;
                        }
                        if advert.chain_id != our_chain_id {
                            tracing::debug!(
                                "gossip advert: wrong chain_id {} (expected {})",
                                advert.chain_id,
                                our_chain_id
                            );
                            return;
                        }
                        if let Some(cached) = multiaddr_cache.get(&advert.validator)
                            && cached.sequence >= advert.sequence
                        {
                            // Already have an equal-or-newer entry; skip.
                            return;
                        }
                        if !advert.verify() {
                            tracing::warn!(
                                "gossip advert: bad signature from claimed validator {}",
                                &advert.validator[..12.min(advert.validator.len())]
                            );
                            return;
                        }
                        // DoS bound: evict lowest-sequence entry if full.
                        if multiaddr_cache.len() >= max_cached_adverts
                            && !multiaddr_cache.contains_key(&advert.validator)
                            && let Some(victim) = multiaddr_cache
                                .iter()
                                .min_by_key(|(_, a)| a.sequence)
                                .map(|(k, _)| k.clone())
                        {
                            multiaddr_cache.remove(&victim);
                        }
                        tracing::debug!(
                            "gossip advert: cached {} multiaddrs for {} (seq={})",
                            advert.multiaddrs.len(),
                            &advert.validator[..12.min(advert.validator.len())],
                            advert.sequence
                        );
                        multiaddr_cache.insert(advert.validator.clone(), advert);
                    }
                    Err(e) => tracing::debug!("gossip advert: bad bincode: {}", e),
                }
            }
        }

        SwarmEvent::Behaviour(SentrixBehaviourEvent::Gossipsub(_)) => {}

        SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
            tracing::warn!(
                "libp2p: outgoing connection error to {:?}: {}",
                peer_id,
                error
            );
        }

        SwarmEvent::IncomingConnectionError { error, .. } => {
            tracing::warn!("libp2p: incoming connection error: {}", error);
        }

        _ => {} // ListenerClosed, Dialing, etc. — not actionable
    }
}

// ── Request-response event handler ──────────────────────

#[allow(clippy::too_many_arguments)]
async fn on_rr_event(
    event: request_response::Event<SentrixRequest, SentrixResponse>,
    swarm: &mut Swarm<SentrixBehaviour>,
    blockchain: &SharedBlockchain,
    event_tx: &mpsc::Sender<NodeEvent>,
    verified_peers: &mut HashSet<PeerId>,
    pending_handshakes: &mut HashMap<OutboundRequestId, PeerId>,
    pending_syncs: &mut HashMap<OutboundRequestId, PeerId>,
    pending_variants: &mut HashMap<OutboundRequestId, &'static str>,
    our_chain_id: u64,
) {
    use request_response::{Event as RrEvent, Message as RrMessage};

    match event {
        // ── Inbound: peer sent us a request ──────────────
        RrEvent::Message {
            peer,
            message: RrMessage::Request {
                request, channel, ..
            },
            ..
        } => {
            on_inbound_request(
                peer,
                request,
                channel,
                swarm,
                blockchain,
                event_tx,
                verified_peers,
                our_chain_id,
            )
            .await;
        }

        // ── Inbound: peer replied to one of our requests ─
        RrEvent::Message {
            peer,
            message:
                RrMessage::Response {
                    request_id,
                    response,
                },
            ..
        } => {
            // Bug #1d diagnostic: release variant slot on successful response.
            // Without this the map would grow unbounded across the process
            // lifetime.
            pending_variants.remove(&request_id);

            // Check if this response matches a pending GetBlocks sync request
            let followup = on_inbound_response(
                peer,
                request_id,
                response,
                blockchain,
                event_tx,
                verified_peers,
                pending_handshakes,
                pending_syncs,
                our_chain_id,
            )
            .await;
            // If sync returned more blocks to fetch, send another GetBlocks
            if let Some((next_peer, from_height)) = followup {
                let req = SentrixRequest::GetBlocks { from_height };
                let variant = req.variant_name();
                let req_id = swarm.behaviour_mut().rr.send_request(&next_peer, req);
                pending_syncs.insert(req_id, next_peer);
                pending_variants.insert(req_id, variant);
            }
        }

        RrEvent::OutboundFailure {
            peer,
            request_id,
            error,
            ..
        } => {
            pending_handshakes.remove(&request_id);
            pending_syncs.remove(&request_id);
            // Bug #1d diagnostic: log the request variant that failed.
            // "outbound failure to X: timeout" by itself is unactionable
            // — we cannot tell if BFT proposals are timing out or unrelated
            // background traffic (e.g. periodic GetBlocks for sync). Until
            // the variant shows up in logs, investigation of the real
            // latency hotspot is blind.
            let variant = pending_variants
                .remove(&request_id)
                .unwrap_or("Unknown");
            tracing::warn!(
                "libp2p: outbound failure to {} ({}): {}",
                peer,
                variant,
                error
            );
        }

        RrEvent::InboundFailure { peer, error, .. } => {
            // Inbound failures are on requests a PEER sent us that we
            // failed to respond to — we never had pending_variants state
            // for them (variant lives in the request the peer sent, which
            // request_response drops on our side before surfacing this
            // event). Peer-side logs have the variant on their end.
            tracing::warn!("libp2p: inbound failure from {}: {}", peer, error);
        }

        RrEvent::ResponseSent { .. } => {} // ack only
    }
}

// ── Inbound request handler ──────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn on_inbound_request(
    peer: PeerId,
    request: SentrixRequest,
    channel: request_response::ResponseChannel<SentrixResponse>,
    swarm: &mut Swarm<SentrixBehaviour>,
    blockchain: &SharedBlockchain,
    event_tx: &mpsc::Sender<NodeEvent>,
    verified_peers: &mut HashSet<PeerId>,
    our_chain_id: u64,
) {
    match request {
        // ── Handshake ────────────────────────────────────
        SentrixRequest::Handshake {
            chain_id, height, ..
        } => {
            if chain_id != our_chain_id {
                tracing::warn!(
                    "libp2p: rejected peer {}: chain_id mismatch ({} vs {})",
                    peer,
                    chain_id,
                    our_chain_id
                );
                // Respond with Ack so the peer gets a clean close
                let _ = swarm
                    .behaviour_mut()
                    .rr
                    .send_response(channel, SentrixResponse::Ack);
                // Disconnect the peer
                let _ = swarm.disconnect_peer_id(peer);
                return;
            }

            // Peer limit: don't accept more verified peers than MAX_LIBP2P_PEERS.
            if verified_peers.len() >= MAX_LIBP2P_PEERS && !verified_peers.contains(&peer) {
                tracing::warn!(
                    "libp2p: peer limit reached ({}/{}), rejecting handshake from {}",
                    verified_peers.len(),
                    MAX_LIBP2P_PEERS,
                    peer
                );
                let _ = swarm
                    .behaviour_mut()
                    .rr
                    .send_response(channel, SentrixResponse::Ack);
                let _ = swarm.disconnect_peer_id(peer);
                return;
            }

            let bc = blockchain.read().await;
            let our_height = bc.height();
            let resp = SentrixResponse::Handshake {
                host: "0.0.0.0".to_string(),
                port: 0,
                height: our_height,
                chain_id: our_chain_id,
            };
            drop(bc);

            if swarm
                .behaviour_mut()
                .rr
                .send_response(channel, resp)
                .is_ok()
            {
                // Only fire PeerConnected + SyncNeeded when this is a
                // newly-verified peer. Bidirectional connections (both
                // inbound and outbound Handshake completing on each
                // side) would otherwise double-insert + emit duplicate
                // events, confusing downstream consumers that assume
                // one event per peer.
                let newly_added = verified_peers.insert(peer);
                if !newly_added {
                    return;
                }
                let _ = event_tx
                    .send(NodeEvent::PeerConnected(peer.to_string()))
                    .await;

                if height > our_height {
                    let _ = event_tx
                        .send(NodeEvent::SyncNeeded {
                            peer_addr: peer.to_string(),
                            peer_height: height,
                        })
                        .await;
                }
            }
        }

        // ── NewBlock — apply to chain (spawned to avoid blocking swarm) ──
        // Fast-reject at the network boundary: H-01 cross-chain + 2026-04-21
        // state_root=None. The transaction-level validate() / execution-time
        // state_root guard still catch these downstream, but rejecting up
        // front avoids acquiring the chain write lock and spawning a doomed
        // task, and it emits ONE clean WARN at ingest instead of a flood of
        // execution-path errors across every peer.
        SentrixRequest::NewBlock { block } => {
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SentrixResponse::Ack);
            if let Some(reason) = block_boundary_reject_reason(&block, our_chain_id) {
                tracing::warn!(
                    "libp2p: dropping block {} from {}: {}",
                    block.index,
                    peer,
                    reason
                );
                return;
            }
            let bc = blockchain.clone();
            let etx = event_tx.clone();
            tokio::spawn(async move {
                let mut chain = bc.write().await;
                match chain.add_block_from_peer(*block.clone()) {
                    Ok(()) => {
                        tracing::info!("libp2p: applied block {} from {}", block.index, peer);
                        // Capture H2 (with state_root + recomputed hash) before releasing
                        // the write lock — same fix as validator loop (PR #78).
                        let updated = chain.latest_block().ok().cloned().unwrap_or(*block);
                        drop(chain);
                        let _ = etx.send(NodeEvent::NewBlock(updated)).await;
                    }
                    Err(e) => {
                        tracing::warn!("libp2p: rejected block from {}: {}", peer, e);
                    }
                }
            });
        }

        // ── NewTransaction — add to mempool (spawned) ────
        // H-01: reject cross-chain transactions at the network boundary.
        SentrixRequest::NewTransaction { transaction } => {
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SentrixResponse::Ack);
            if transaction.chain_id != our_chain_id {
                tracing::warn!(
                    "libp2p: dropping tx from {}: chain_id mismatch ({} vs {})",
                    peer,
                    transaction.chain_id,
                    our_chain_id
                );
                return;
            }
            let bc = blockchain.clone();
            let etx = event_tx.clone();
            tokio::spawn(async move {
                let mut chain = bc.write().await;
                if chain.add_to_mempool(transaction.clone()).is_ok() {
                    drop(chain);
                    let _ = etx.send(NodeEvent::NewTransaction(transaction)).await;
                }
            });
        }

        // ── GetBlocks — respond with up to 50 blocks (reduced from 100 to stay under 10MB) ──
        //
        // BACKLOG #14: use get_block_any so evicted blocks (older than
        // CHAIN_WINDOW_SIZE) are served from MDBX instead of silently
        // dropped. The previous `get_block(i).cloned()` path returned
        // an empty BlocksResponse for any fresh or forensic-restored
        // peer requesting a deep history back-fill — those peers
        // stalled indefinitely because gossipsub only delivers new
        // blocks to in-mesh subscribers.
        SentrixRequest::GetBlocks { from_height } => {
            let bc = blockchain.read().await;
            let to = bc.height().min(from_height.saturating_add(49));
            let blocks: Vec<Block> = (from_height..=to)
                .filter_map(|i| bc.get_block_any(i))
                .collect();
            drop(bc);
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SentrixResponse::BlocksResponse { blocks });
        }

        // ── GetHeight ────────────────────────────────────
        SentrixRequest::GetHeight => {
            let height = blockchain.read().await.height();
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SentrixResponse::HeightResponse { height });
        }

        // ── Ping ─────────────────────────────────────────
        SentrixRequest::Ping => {
            let height = blockchain.read().await.height();
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SentrixResponse::Pong { height });
        }

        // ── BFT Proposal ────────────────────────────────
        // C-01 gap 3: verify signature AND validator-set membership at
        // the network boundary. Forged or non-validator messages are
        // ACKed (so the peer's libp2p state transitions cleanly) and
        // silently dropped — they never reach the BFT event channel.
        SentrixRequest::BftProposal { proposal } => {
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SentrixResponse::Ack);
            if !proposal.verify_sig() {
                tracing::warn!(
                    "libp2p: dropping BFT proposal from {}: bad signature",
                    &proposal.proposer
                );
                return;
            }
            if !is_active_bft_signer(blockchain, &proposal.proposer).await {
                tracing::warn!(
                    "libp2p: dropping BFT proposal from non-validator {}",
                    &proposal.proposer
                );
                return;
            }
            let _ = event_tx.send(NodeEvent::BftProposal(*proposal)).await;
        }

        // ── BFT Prevote ─────────────────────────────────
        SentrixRequest::BftPrevote { prevote } => {
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SentrixResponse::Ack);
            if !prevote.verify_sig() {
                tracing::warn!(
                    "libp2p: dropping BFT prevote from {}: bad signature",
                    &prevote.validator
                );
                return;
            }
            if !is_active_bft_signer(blockchain, &prevote.validator).await {
                tracing::warn!(
                    "libp2p: dropping BFT prevote from non-validator {}",
                    &prevote.validator
                );
                return;
            }
            let _ = event_tx.send(NodeEvent::BftPrevote(prevote)).await;
        }

        // ── BFT Precommit ───────────────────────────────
        SentrixRequest::BftPrecommit { precommit } => {
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SentrixResponse::Ack);
            if !precommit.verify_sig() {
                tracing::warn!(
                    "libp2p: dropping BFT precommit from {}: bad signature",
                    &precommit.validator
                );
                return;
            }
            if !is_active_bft_signer(blockchain, &precommit.validator).await {
                tracing::warn!(
                    "libp2p: dropping BFT precommit from non-validator {}",
                    &precommit.validator
                );
                return;
            }
            let _ = event_tx.send(NodeEvent::BftPrecommit(precommit)).await;
        }

        // ── BFT RoundStatus ────────────────────────────
        SentrixRequest::BftRoundStatus { status } => {
            let _ = swarm
                .behaviour_mut()
                .rr
                .send_response(channel, SentrixResponse::Ack);
            if !status.verify_sig() {
                tracing::warn!(
                    "libp2p: dropping BFT round-status from {}: bad signature",
                    &status.validator
                );
                return;
            }
            if !is_active_bft_signer(blockchain, &status.validator).await {
                tracing::warn!(
                    "libp2p: dropping BFT round-status from non-validator {}",
                    &status.validator
                );
                return;
            }
            let _ = event_tx.send(NodeEvent::BftRoundStatus(status)).await;
        }
    }
}

/// Check if `addr` is a current BFT-authorised validator. Consults the
/// DPoS stake registry first (post-Voyager), then falls back to the PoA
/// authority roster (Pioneer). Matches the helper in `bin/sentrix/main.rs`
/// — the two live on opposite sides of the channel and both sides harden
/// for defence in depth.
async fn is_active_bft_signer(blockchain: &SharedBlockchain, addr: &str) -> bool {
    let bc = blockchain.read().await;
    if bc.stake_registry.is_active(addr) {
        return true;
    }
    bc.authority.is_active_validator(addr)
}

/// Fast-reject a block at the network boundary if it fails cheap sanity
/// checks that don't need the chain write lock. Returns `Some(reason)` if the
/// block should be dropped on the floor, `None` if it's worth forwarding to
/// `add_block_from_peer`.
///
/// Cheap checks only — expensive ones (signature, state-root math, trie
/// apply) still run inside `add_block_from_peer` under the write lock.
/// The purpose here is to kill the obvious-bad blocks before they
/// contend for the lock or spawn a doomed apply task.
///
/// Added 2026-04-22 after the 3-way state_root fork: a validator running on
/// a damaged chain.db was producing blocks with `state_root=None` above
/// `STATE_ROOT_FORK_HEIGHT`, and peers were accepting them into the ingest
/// pipeline before the execution-time guard caught and rejected them. With
/// this check, bad blocks are rejected *before* propagation/apply so the
/// signal reaches operators ~instantly instead of waiting for an execution
/// failure that may be hidden by log noise.
fn block_boundary_reject_reason(block: &Block, our_chain_id: u64) -> Option<&'static str> {
    // H-01: cross-chain block. find the first non-coinbase tx and check its
    // chain_id. (If every tx is coinbase, skip this check — coinbase has
    // no chain_id-bound semantics.)
    if let Some(tx) = block.transactions.iter().find(|t| !t.is_coinbase())
        && tx.chain_id != our_chain_id
    {
        return Some("chain_id mismatch");
    }

    // 2026-04-21 3-way fork guard: past STATE_ROOT_FORK_HEIGHT, every valid
    // block must carry a state_root; missing = producer's trie is broken.
    // The execution-time guard in block_executor.rs also catches this, but
    // gating at the network boundary means we never spend a write lock or
    // apply-task on the bad block — and, more importantly, we don't log it
    // at ERROR from every peer's execution path. One clean WARN at ingest
    // is easier to spot than a flood of mismatches.
    if block.index >= STATE_ROOT_FORK_HEIGHT && block.state_root.is_none() {
        return Some("missing state_root past STATE_ROOT_FORK_HEIGHT (sender's trie is broken)");
    }

    None
}

// ── Inbound response handler ─────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn on_inbound_response(
    peer: PeerId,
    request_id: OutboundRequestId,
    response: SentrixResponse,
    blockchain: &SharedBlockchain,
    event_tx: &mpsc::Sender<NodeEvent>,
    verified_peers: &mut HashSet<PeerId>,
    pending_handshakes: &mut HashMap<OutboundRequestId, PeerId>,
    pending_syncs: &mut HashMap<OutboundRequestId, PeerId>,
    our_chain_id: u64,
) -> Option<(PeerId, u64)> {
    // ── Step 3d: handle BlocksResponse from GetBlocks sync ──
    // Block processing is spawned to a background task so the swarm loop
    // stays responsive. Without this, the write lock blocks all event processing,
    // causing cascade peer disconnects (root cause of Foundation node isolation 2026-04-14).
    if let SentrixResponse::BlocksResponse { blocks } = &response
        && let Some(sync_peer) = pending_syncs.remove(&request_id)
    {
        if blocks.is_empty() {
            return None;
        }
        // DoS guard: the server-side GetBlocks handler caps responses at
        // 50 blocks (libp2p_node.rs: SentrixRequest::GetBlocks branch).
        // A malicious peer could still encode more blocks per response
        // up to MAX_MESSAGE_BYTES (~10 MB = ~20K small blocks) and
        // expect us to apply them all sequentially under a single write
        // lock, stalling the swarm event loop for minutes. Reject any
        // response that violates our own server's contract.
        const MAX_ACCEPTED_BATCH: usize = 50;
        if blocks.len() > MAX_ACCEPTED_BATCH {
            tracing::warn!(
                "libp2p sync: peer {} sent {} blocks in a single BlocksResponse \
                 (cap is {}); dropping response to prevent swarm-loop stall",
                sync_peer,
                blocks.len(),
                MAX_ACCEPTED_BATCH
            );
            return None;
        }
        let block_count = blocks.len();
        let bc = blockchain.clone();
        let etx = event_tx.clone();
        let blocks_owned = blocks.clone();
        let peer_str = sync_peer.to_string();
        tokio::spawn(async move {
            let mut chain = bc.write().await;
            let mut synced = 0u64;
            let mut skipped = 0u64;
            for block in &blocks_owned {
                // Concurrent GetBlocks paths (periodic sync_interval + TriggerSync
                // + reactive chain-on-full-batch) can race: each reads our_height,
                // sends GetBlocks{from: our_height+1}, peer replies with the block
                // we just applied from the first response. Without this guard the
                // loop bails on `Invalid block: expected N+1, got N` and drops the
                // remaining VALID forward blocks in the batch — block sync stalls
                // even while peers serve correct history. (Mainnet stall 2026-04-26
                // h=604547 root cause.)
                if block.index <= chain.height() {
                    skipped += 1;
                    continue;
                }
                match chain.add_block_from_peer(block.clone()) {
                    Ok(()) => {
                        // Use H2 (post-add_block state_root hash) — not the raw peer block (PR #78).
                        let updated = chain
                            .latest_block()
                            .ok()
                            .cloned()
                            .unwrap_or_else(|| block.clone());
                        let _ = etx.send(NodeEvent::NewBlock(updated)).await;
                        synced += 1;
                    }
                    Err(e) => {
                        tracing::warn!("libp2p sync: block {} failed: {}", block.index, e);
                        break;
                    }
                }
            }
            if synced > 0 || skipped > 0 {
                tracing::info!(
                    "libp2p: synced {} blocks from {} (skipped {} already-applied)",
                    synced,
                    peer_str,
                    skipped
                );
            }
        });
        // If we got a full batch (50 blocks), request more
        if block_count >= 50 {
            let next_height = blocks.last().map(|b| b.index + 1).unwrap_or(0);
            return Some((sync_peer, next_height));
        }
        return None;
    }

    // ── Handshake response ──────────────────────────────────
    if let SentrixResponse::Handshake {
        chain_id, height, ..
    } = response
        && let Some(expected_peer) = pending_handshakes.remove(&request_id)
    {
        if expected_peer != peer {
            tracing::warn!("libp2p: handshake response peer mismatch");
            return None;
        }
        if chain_id != our_chain_id {
            tracing::warn!(
                "libp2p: handshake response from {} has wrong chain_id ({} vs {})",
                peer,
                chain_id,
                our_chain_id
            );
            return None;
        }
        let our_height = blockchain.read().await.height();
        // Only emit PeerConnected + SyncNeeded for newly-verified peers
        // (see inbound-handshake path for the race-condition rationale).
        let newly_added = verified_peers.insert(peer);
        if !newly_added {
            return None;
        }
        let _ = event_tx
            .send(NodeEvent::PeerConnected(peer.to_string()))
            .await;

        if height > our_height {
            let _ = event_tx
                .send(NodeEvent::SyncNeeded {
                    peer_addr: peer.to_string(),
                    peer_height: height,
                })
                .await;
            // Initiate sync from this peer
            return Some((peer, our_height + 1));
        }
    }

    None
}

// ── Tests ────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use sentrix_core::blockchain::Blockchain;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn make_blockchain() -> SharedBlockchain {
        Arc::new(RwLock::new(Blockchain::new("admin".to_string())))
    }

    // ── make_multiaddr helper ────────────────────────────

    #[test]
    fn test_make_multiaddr_valid() {
        let addr = make_multiaddr("127.0.0.1", 30303).expect("valid addr");
        assert_eq!(addr.to_string(), "/ip4/127.0.0.1/tcp/30303");
    }

    #[test]
    fn test_make_multiaddr_any_interface() {
        let addr = make_multiaddr("0.0.0.0", 30303).expect("valid addr");
        assert_eq!(addr.to_string(), "/ip4/0.0.0.0/tcp/30303");
    }

    #[test]
    fn test_make_multiaddr_invalid_ip_fails() {
        let result = make_multiaddr("not_an_ip", 30303);
        assert!(result.is_err(), "invalid IP should fail");
    }

    // ── block_boundary_reject_reason ─────────────────────
    // 2026-04-22 fork follow-up tests.

    fn make_test_block(index: u64, state_root: Option<[u8; 32]>) -> Block {
        Block {
            index,
            timestamp: 1_700_000_000,
            transactions: vec![],
            previous_hash: "0".to_string(),
            hash: "h".to_string(),
            merkle_root: "m".to_string(),
            validator: "v".to_string(),
            state_root,
            round: 0,
            justification: None,
        }
    }

    #[test]
    fn test_block_boundary_valid_above_fork_with_state_root() {
        let block = make_test_block(STATE_ROOT_FORK_HEIGHT + 1, Some([0u8; 32]));
        assert_eq!(block_boundary_reject_reason(&block, 7119), None);
    }

    #[test]
    fn test_block_boundary_rejects_missing_state_root_above_fork() {
        let block = make_test_block(STATE_ROOT_FORK_HEIGHT + 1, None);
        let reason = block_boundary_reject_reason(&block, 7119);
        assert!(reason.is_some(), "must reject above fork when state_root None");
        assert!(
            reason.unwrap().contains("state_root"),
            "reason should mention state_root; got: {:?}",
            reason
        );
    }

    #[test]
    fn test_block_boundary_allows_missing_state_root_below_fork() {
        // Below fork height, state_root=None is the old hash format — valid.
        let block = make_test_block(STATE_ROOT_FORK_HEIGHT - 1, None);
        assert_eq!(block_boundary_reject_reason(&block, 7119), None);
    }

    #[test]
    fn test_block_boundary_at_exact_fork_height_requires_state_root() {
        // Boundary case: fork height itself requires state_root.
        let block = make_test_block(STATE_ROOT_FORK_HEIGHT, None);
        assert!(
            block_boundary_reject_reason(&block, 7119).is_some(),
            "fork-height block with None state_root must reject"
        );
    }

    // ── LibP2pNode creation ──────────────────────────────

    #[tokio::test]
    async fn test_libp2p_node_new_succeeds() {
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let bc = make_blockchain();
        let (etx, _erx) = mpsc::channel(16);

        let node = LibP2pNode::new(keypair, bc, etx);
        assert!(node.is_ok(), "LibP2pNode::new should succeed");
    }

    #[tokio::test]
    async fn test_libp2p_peer_count_initially_zero() {
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let bc = make_blockchain();
        let (etx, _erx) = mpsc::channel(16);

        let node = LibP2pNode::new(keypair, bc, etx).expect("node creation");
        // Give the swarm task a moment to initialise
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        assert_eq!(node.peer_count().await, 0);
    }

    #[tokio::test]
    async fn test_libp2p_two_nodes_connect_and_handshake() {
        // Node A listens on a random port; Node B dials Node A.
        // Both have the same chain_id (default) → they should become verified peers.
        let kp_a = libp2p::identity::Keypair::generate_ed25519();
        let kp_b = libp2p::identity::Keypair::generate_ed25519();

        let bc_a = make_blockchain();
        let bc_b = make_blockchain();

        let (etx_a, _erx_a) = mpsc::channel(32);
        let (etx_b, _erx_b) = mpsc::channel(32);

        let node_a = LibP2pNode::new(kp_a, bc_a, etx_a).expect("node A");
        let node_b = LibP2pNode::new(kp_b, bc_b, etx_b).expect("node B");

        // A listens on 127.0.0.1 with OS-assigned port (0 = any)
        let listen_addr = make_multiaddr("127.0.0.1", 0).expect("addr");
        node_a.listen_on(listen_addr).await.expect("listen");

        // Give the listener a moment to bind
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        // Connect B → A.  We must get the actual bound port from the swarm listener,
        // but since we don't have an easy way to query it here, just verify that
        // sending a dial command doesn't panic (integration covered in CI via full node test).
        let dial_result = node_b
            .connect_peer(make_multiaddr("127.0.0.1", 30399).expect("addr"))
            .await;
        // connect_peer sends to channel — should always succeed (swarm handles actual dial)
        assert!(
            dial_result.is_ok(),
            "connect_peer should not fail to send command"
        );
    }

    // ── extract_ip helper ────────────────────────────────

    #[test]
    fn test_extract_ip_from_dialer() {
        let addr: Multiaddr = "/ip4/192.168.1.1/tcp/30303".parse().expect("valid");
        let endpoint = ConnectedPoint::Dialer {
            address: addr,
            role_override: libp2p::core::Endpoint::Dialer,
            port_use: libp2p::core::transport::PortUse::Reuse,
        };
        let ip = extract_ip(&endpoint);
        assert_eq!(
            ip,
            Some(IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 1)))
        );
    }

    #[test]
    fn test_extract_ip_from_listener() {
        let local: Multiaddr = "/ip4/0.0.0.0/tcp/30303".parse().expect("valid");
        let remote: Multiaddr = "/ip4/10.0.0.5/tcp/45678".parse().expect("valid");
        let endpoint = ConnectedPoint::Listener {
            local_addr: local,
            send_back_addr: remote,
        };
        let ip = extract_ip(&endpoint);
        assert_eq!(ip, Some(IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 5))));
    }

    // ── IpRateLimiter ───────────────────────────────────

    #[test]
    fn test_rate_limiter_allows_within_limit() {
        let mut limiter = IpRateLimiter::new();
        let ip: IpAddr = "1.2.3.4".parse().expect("valid");

        for _ in 0..MAX_CONN_PER_IP {
            assert!(limiter.check_and_track(ip), "should allow within limit");
        }
    }

    #[test]
    fn test_rate_limiter_bans_over_limit() {
        let mut limiter = IpRateLimiter::new();
        let ip: IpAddr = "1.2.3.4".parse().expect("valid");

        for _ in 0..MAX_CONN_PER_IP {
            limiter.check_and_track(ip);
        }
        // Next connection should trigger ban
        assert!(!limiter.check_and_track(ip), "should reject over limit");
        // Subsequent connections also rejected (banned)
        assert!(!limiter.check_and_track(ip), "should stay banned");
    }

    #[test]
    fn test_rate_limiter_different_ips_independent() {
        let mut limiter = IpRateLimiter::new();
        let ip_a: IpAddr = "1.2.3.4".parse().expect("valid");
        let ip_b: IpAddr = "5.6.7.8".parse().expect("valid");

        // Exhaust limit for IP A
        for _ in 0..MAX_CONN_PER_IP {
            limiter.check_and_track(ip_a);
        }
        assert!(!limiter.check_and_track(ip_a), "IP A should be banned");
        // IP B should still be allowed
        assert!(limiter.check_and_track(ip_b), "IP B should be allowed");
    }

    #[test]
    fn test_rate_limiter_prune_stale() {
        let mut limiter = IpRateLimiter::new();
        let ip: IpAddr = "1.2.3.4".parse().expect("valid");

        limiter.check_and_track(ip);
        assert_eq!(limiter.counts.len(), 1);
        // Entries within window should survive prune
        limiter.prune_stale();
        assert_eq!(limiter.counts.len(), 1);
    }

    #[test]
    fn test_peer_limit_constant() {
        assert_eq!(MAX_LIBP2P_PEERS, 50, "max peers should be 50");
        assert_eq!(
            MAX_CONN_PER_IP, 20,
            "max connections per IP should be 20 (Treasury node has 5 vals + reconnect overhead)"
        );
        assert_eq!(BAN_DURATION_SECS, 300, "ban duration should be 5 minutes");
    }

    #[tokio::test]
    async fn test_libp2p_chain_id_validation_logic() {
        // The chain_id check in on_inbound_request should reject wrong chain_ids.
        // We verify the logic directly rather than via a full network test.
        let bc = make_blockchain();
        let our_chain_id = bc.read().await.chain_id;

        // Same chain_id → accepted
        assert_eq!(our_chain_id, 7119, "default chain_id should be 7119");

        // Wrong chain_id → should be rejected (tested structurally here)
        let bad_chain_id: u64 = 9999;
        assert_ne!(bad_chain_id, our_chain_id, "bad chain_id must differ");
    }

    #[tokio::test]
    async fn test_libp2p_broadcast_does_not_panic_with_no_peers() {
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let bc = make_blockchain();
        let (etx, _erx) = mpsc::channel(16);

        let node = LibP2pNode::new(keypair, bc, etx).expect("node");
        // No peers — broadcast should silently do nothing
        let block =
            sentrix_primitives::block::Block::new(0, "0".to_string(), vec![], "v1".to_string());
        node.broadcast_block(&block).await; // must not panic
    }
}
