// The two tiny multiaddr conveniences. `make_multiaddr` exists because
// our bootstrap-peer config still ships the legacy `host:port` shape
// that pre-libp2p Sentrix used; everything has to go through this on
// the way into the swarm. `extract_ip` pulls the v4 / v6 out of a
// `ConnectedPoint` so the rate limiter has something to key on.

use libp2p::core::ConnectedPoint;
use libp2p::core::multiaddr::{Multiaddr, Protocol};
use sentrix_primitives::error::{SentrixError, SentrixResult};
use std::net::IpAddr;

/// Build a `/ip4/<host>/tcp/<port>` multiaddr from a host string and port.
///
/// Used to convert legacy `host:port` bootstrap peers into the libp2p format.
pub fn make_multiaddr(host: &str, port: u16) -> SentrixResult<Multiaddr> {
    let s = format!("/ip4/{}/tcp/{}", host, port);
    s.parse::<Multiaddr>()
        .map_err(|e| SentrixError::NetworkError(format!("invalid multiaddr '{}': {}", s, e)))
}

/// Extract IP address from a libp2p `ConnectedPoint`.
pub(super) fn extract_ip(endpoint: &ConnectedPoint) -> Option<IpAddr> {
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
