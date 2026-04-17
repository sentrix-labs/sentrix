// tests/integration_p2p.rs — P2P mesh formation integration test
//
// Spawns multiple in-process libp2p nodes and verifies they can:
//   1. Connect via TCP
//   2. Complete handshake (chain_id validation)
//   3. Form gossipsub mesh
//   4. Exchange blocks via gossipsub
//
// This test prevents v1.3.0-style regressions where compilation and unit
// tests pass but runtime P2P networking is broken.

#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use sentrix::core::blockchain::Blockchain;
use sentrix::network::libp2p_node::{LibP2pNode, make_multiaddr};
use sentrix::network::node::NodeEvent;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

type SharedBlockchain = Arc<RwLock<Blockchain>>;

fn make_blockchain() -> SharedBlockchain {
    Arc::new(RwLock::new(Blockchain::new(
        "0x4f3319a747fd564136209cd5d9e7d1a1e4d142be".to_string(),
    )))
}

/// Spawn a libp2p node listening on 127.0.0.1 with a random port.
/// Returns (node, event_receiver, actual_listen_port).
async fn spawn_node() -> (Arc<LibP2pNode>, mpsc::Receiver<NodeEvent>) {
    let keypair = libp2p::identity::Keypair::generate_ed25519();
    let bc = make_blockchain();
    let (etx, erx) = mpsc::channel(256);

    let node = Arc::new(LibP2pNode::new(keypair, bc, etx).expect("node creation"));

    // Listen on 127.0.0.1 with OS-assigned port
    let addr = make_multiaddr("127.0.0.1", 0).expect("make addr");
    node.listen_on(addr).await.expect("listen");

    // Give the listener time to bind
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    (node, erx)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_two_nodes_connect_and_verify_peers() {
    // Spawn two nodes
    let (_node_a, _events_a) = spawn_node().await;
    let (_node_b, _events_b) = spawn_node().await;

    // We need node A's actual listening port. Since we used port 0,
    // we can't know the exact port without querying the swarm.
    // Instead, we use a known port approach — spawn A on a specific port.
    drop(_node_a);
    drop(_events_a);
    drop(_node_b);
    drop(_events_b);

    // Use specific ports for deterministic test
    let kp_a = libp2p::identity::Keypair::generate_ed25519();
    let kp_b = libp2p::identity::Keypair::generate_ed25519();
    let bc_a = make_blockchain();
    let bc_b = make_blockchain();
    let (etx_a, mut erx_a) = mpsc::channel(256);
    let (etx_b, mut erx_b) = mpsc::channel(256);

    let node_a = Arc::new(LibP2pNode::new(kp_a, bc_a, etx_a).expect("node A"));
    let node_b = Arc::new(LibP2pNode::new(kp_b, bc_b, etx_b).expect("node B"));

    // A listens on port 39101
    let addr_a = make_multiaddr("127.0.0.1", 39101).expect("addr");
    node_a.listen_on(addr_a).await.expect("A listen");
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // B connects to A
    let dial_addr = make_multiaddr("127.0.0.1", 39101).expect("dial addr");
    node_b.connect_peer(dial_addr).await.expect("B connect");

    // Wait for handshake events (up to 5 seconds)
    let mut a_connected = false;
    let mut b_connected = false;

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);

    loop {
        tokio::select! {
            Some(event) = erx_a.recv() => {
                if matches!(event, NodeEvent::PeerConnected(_)) {
                    a_connected = true;
                }
            }
            Some(event) = erx_b.recv() => {
                if matches!(event, NodeEvent::PeerConnected(_)) {
                    b_connected = true;
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                break;
            }
        }
        if a_connected && b_connected {
            break;
        }
    }

    assert!(a_connected, "Node A should see B as connected peer");
    assert!(b_connected, "Node B should see A as connected peer");

    // Verify peer counts
    assert!(node_a.peer_count().await >= 1, "A should have >= 1 peer");
    assert!(node_b.peer_count().await >= 1, "B should have >= 1 peer");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_gossipsub_block_propagation() {
    // Spawn two connected nodes. Use create_block() to produce a valid block
    // so the receiving node's add_block() accepts it and emits NewBlock.
    use sentrix::wallet::wallet::Wallet;

    let kp_a = libp2p::identity::Keypair::generate_ed25519();
    let kp_b = libp2p::identity::Keypair::generate_ed25519();

    let admin = "0x4f3319a747fd564136209cd5d9e7d1a1e4d142be";
    let bc_a = Arc::new(RwLock::new(Blockchain::new(admin.to_string())));
    let bc_b = Arc::new(RwLock::new(Blockchain::new(admin.to_string())));

    // Generate a validator and register it on BOTH chains
    let val = Wallet::generate();
    for bc in [&bc_a, &bc_b] {
        let mut chain = bc.write().await;
        chain
            .authority
            .add_validator(
                admin,
                val.address.clone(),
                "TestVal".to_string(),
                val.public_key.clone(),
            )
            .expect("add_validator");
    }

    // Create a valid block on chain A (so it has correct prev_hash, merkle root, etc.)
    let block = {
        let mut chain_a = bc_a.write().await;
        let b = chain_a
            .create_block(&val.address)
            .expect("create_block");
        chain_a.add_block(b.clone()).expect("add_block on A");
        b
    };

    let (etx_a, _erx_a) = mpsc::channel(256);
    let (etx_b, mut erx_b) = mpsc::channel(256);

    let node_a = Arc::new(LibP2pNode::new(kp_a, bc_a, etx_a).expect("node A"));
    let node_b = Arc::new(LibP2pNode::new(kp_b, bc_b, etx_b).expect("node B"));

    // A listens on port 39102
    node_a
        .listen_on(make_multiaddr("127.0.0.1", 39102).unwrap())
        .await
        .unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // B connects to A
    node_b
        .connect_peer(make_multiaddr("127.0.0.1", 39102).unwrap())
        .await
        .unwrap();

    // Wait for connection + gossipsub mesh formation (2 heartbeats = 10s)
    tokio::time::sleep(tokio::time::Duration::from_secs(12)).await;

    // Verify peers connected
    assert!(node_a.peer_count().await >= 1, "A needs >= 1 peer");
    assert!(node_b.peer_count().await >= 1, "B needs >= 1 peer");

    // A broadcasts the valid block via gossipsub
    node_a.broadcast_block(&block).await;

    // B should receive and accept the block → emit NewBlock
    let mut received_block = false;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(10);

    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            Some(event) = erx_b.recv() => {
                if matches!(event, NodeEvent::NewBlock(_)) {
                    received_block = true;
                    break;
                }
            }
            _ = tokio::time::sleep_until(deadline) => { break; }
        }
    }

    assert!(
        received_block,
        "Node B should receive block from A via gossipsub. \
         If this fails, gossipsub mesh is not forming (v1.3.0-style regression)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_three_node_mesh() {
    // Spawn three nodes and verify they all discover each other
    let bc_a = make_blockchain();
    let bc_b = make_blockchain();
    let bc_c = make_blockchain();
    let (etx_a, mut erx_a) = mpsc::channel(256);
    let (etx_b, mut erx_b) = mpsc::channel(256);
    let (etx_c, mut erx_c) = mpsc::channel(256);

    let kp_a = libp2p::identity::Keypair::generate_ed25519();
    let kp_b = libp2p::identity::Keypair::generate_ed25519();
    let kp_c = libp2p::identity::Keypair::generate_ed25519();

    let node_a = Arc::new(LibP2pNode::new(kp_a, bc_a, etx_a).expect("A"));
    let node_b = Arc::new(LibP2pNode::new(kp_b, bc_b, etx_b).expect("B"));
    let node_c = Arc::new(LibP2pNode::new(kp_c, bc_c, etx_c).expect("C"));

    // A listens on 39103
    node_a
        .listen_on(make_multiaddr("127.0.0.1", 39103).unwrap())
        .await
        .unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // B and C connect to A
    node_b
        .connect_peer(make_multiaddr("127.0.0.1", 39103).unwrap())
        .await
        .unwrap();
    node_c
        .connect_peer(make_multiaddr("127.0.0.1", 39103).unwrap())
        .await
        .unwrap();

    // Wait for all connections + handshakes
    let mut peers_a = 0u32;
    let mut peers_b = 0u32;
    let mut peers_c = 0u32;

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(8);

    loop {
        tokio::select! {
            Some(event) = erx_a.recv() => {
                if matches!(event, NodeEvent::PeerConnected(_)) { peers_a += 1; }
            }
            Some(event) = erx_b.recv() => {
                if matches!(event, NodeEvent::PeerConnected(_)) { peers_b += 1; }
            }
            Some(event) = erx_c.recv() => {
                if matches!(event, NodeEvent::PeerConnected(_)) { peers_c += 1; }
            }
            _ = tokio::time::sleep_until(deadline) => { break; }
        }
        if peers_a >= 2 && peers_b >= 1 && peers_c >= 1 {
            break;
        }
    }

    // A should have 2 peers (B and C), B and C should have at least 1 (A)
    assert!(
        peers_a >= 2,
        "Node A (hub) should have >= 2 connected peers, got {}",
        peers_a
    );
    assert!(
        peers_b >= 1,
        "Node B should have >= 1 connected peer, got {}",
        peers_b
    );
    assert!(
        peers_c >= 1,
        "Node C should have >= 1 connected peer, got {}",
        peers_c
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_chain_id_mismatch_rejected() {
    // Two nodes with different chain_ids should NOT become verified peers
    let kp_a = libp2p::identity::Keypair::generate_ed25519();
    let kp_b = libp2p::identity::Keypair::generate_ed25519();

    // A: default chain_id (7119)
    let bc_a = make_blockchain();

    // B: different chain_id
    let mut chain_b = Blockchain::new("0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string());
    chain_b.chain_id = 9999;
    let bc_b = Arc::new(RwLock::new(chain_b));

    let (etx_a, mut erx_a) = mpsc::channel(256);
    let (etx_b, mut _erx_b) = mpsc::channel(256);

    let node_a = Arc::new(LibP2pNode::new(kp_a, bc_a, etx_a).expect("A"));
    let _node_b = Arc::new(LibP2pNode::new(kp_b, bc_b, etx_b).expect("B"));

    node_a
        .listen_on(make_multiaddr("127.0.0.1", 39104).unwrap())
        .await
        .unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    _node_b
        .connect_peer(make_multiaddr("127.0.0.1", 39104).unwrap())
        .await
        .unwrap();

    // Wait and check that no PeerConnected events arrive (wrong chain_id)
    let mut connected = false;
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(3);

    loop {
        tokio::select! {
            Some(event) = erx_a.recv() => {
                if matches!(event, NodeEvent::PeerConnected(_)) {
                    connected = true;
                    break;
                }
            }
            _ = tokio::time::sleep_until(deadline) => { break; }
        }
    }

    assert!(
        !connected,
        "Nodes with different chain_ids should NOT become verified peers"
    );
}
