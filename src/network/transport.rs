// transport.rs - Sentrix — libp2p transport stack (TCP + Noise + Yamux)
//
// Builds a fully encrypted, multiplexed transport:
//   TCP  →  Noise XX (mutual auth + forward secrecy)  →  Yamux (stream muxing)

use libp2p::{
    core::{muxing::StreamMuxerBox, transport::Boxed, upgrade},
    identity::Keypair,
    noise,
    tcp,
    yamux,
    PeerId,
    Transport,
};
use crate::types::error::{SentrixError, SentrixResult};

/// Fully assembled, boxed transport: TCP + Noise + Yamux.
/// Ready to be handed to a libp2p `Swarm`.
pub type SentrixTransport = Boxed<(PeerId, StreamMuxerBox)>;

/// Build the Sentrix libp2p transport stack.
///
/// Stack layers:
/// - **TCP** (`libp2p::tcp::tokio`): OS-level reliable byte stream
/// - **Noise XX**: mutual peer authentication + encrypted channel (forward secrecy)
/// - **Yamux**: multiplexes multiple logical streams over a single TCP connection
///
/// The returned transport is boxed so callers do not need to name the full type.
pub fn build_transport(keypair: &Keypair) -> SentrixResult<SentrixTransport> {
    let noise_config = noise::Config::new(keypair)
        .map_err(|e| SentrixError::NetworkError(format!("noise init failed: {e}")))?;

    let transport = tcp::tokio::Transport::new(tcp::Config::default())
        .upgrade(upgrade::Version::V1)
        .authenticate(noise_config)
        .multiplex(yamux::Config::default())
        .boxed();

    Ok(transport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libp2p::identity;

    #[test]
    fn test_build_transport_succeeds() {
        let keypair = identity::Keypair::generate_ed25519();
        let result = build_transport(&keypair);
        assert!(result.is_ok(), "build_transport failed: {:?}", result.err());
    }

    #[test]
    fn test_build_transport_different_keys_ok() {
        // Each node generates its own identity — both should succeed
        let kp1 = identity::Keypair::generate_ed25519();
        let kp2 = identity::Keypair::generate_ed25519();
        assert!(build_transport(&kp1).is_ok());
        assert!(build_transport(&kp2).is_ok());
        // Different keypairs must produce different PeerIds
        let pid1 = libp2p::PeerId::from_public_key(&kp1.public());
        let pid2 = libp2p::PeerId::from_public_key(&kp2.public());
        assert_ne!(pid1, pid2);
    }
}
