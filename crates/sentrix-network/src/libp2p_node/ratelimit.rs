// Per-IP connection rate limiter that the libp2p node consults whenever
// a new TCP handshake arrives. Two pools: a sliding window of recent
// connection counts, and a temporary ban list. An IP that opens more
// than MAX_CONN_PER_IP connections inside RATE_LIMIT_WINDOW_SECS lands
// in the ban list and gets ignored for BAN_DURATION_SECS before its
// counters reset.
//
// Why the limit had to be raised to 20: an operator may legitimately
// run multiple validator processes behind a single public IP (NAT,
// shared host, or a co-tenanted block-producer setup). A rolling
// deploy on such a host triggers ~3 reconnects per validator, so a
// single deploy window can produce 15 connection attempts from one
// IP in a few seconds. The previous lower cap was banning legitimate
// peers in that shape.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use super::{BAN_DURATION_SECS, MAX_CONN_PER_IP, RATE_LIMIT_WINDOW_SECS};

pub(super) struct IpRateLimiter {
    /// Connection count + window start per IP.
    counts: HashMap<IpAddr, (u32, Instant)>,
    /// Banned IPs with ban start time.
    bans: HashMap<IpAddr, Instant>,
}

impl IpRateLimiter {
    pub(super) fn new() -> Self {
        Self {
            counts: HashMap::new(),
            bans: HashMap::new(),
        }
    }

    /// Check if an IP is allowed to connect. Returns `false` if banned or rate-limited.
    pub(super) fn check_and_track(&mut self, ip: IpAddr) -> bool {
        // Check active ban
        if let Some(ban_time) = self.bans.get(&ip) {
            if ban_time.elapsed() < Duration::from_secs(BAN_DURATION_SECS) {
                return false;
            }
            // Ban expired
            self.bans.remove(&ip);
        }

        // Track connection rate
        let now = Instant::now();
        let entry = self.counts.entry(ip).or_insert((0, now));
        if entry.1.elapsed() > Duration::from_secs(RATE_LIMIT_WINDOW_SECS) {
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

    /// How many IPs are currently being tracked. Test-only — production
    /// code shouldn't care about the internal map size.
    #[cfg(test)]
    pub(super) fn tracked_count(&self) -> usize {
        self.counts.len()
    }

    /// Prune stale entries to prevent unbounded growth.
    pub(super) fn prune_stale(&mut self) {
        let window = Duration::from_secs(RATE_LIMIT_WINDOW_SECS);
        let ban_dur = Duration::from_secs(BAN_DURATION_SECS);
        self.counts.retain(|_, (_, start)| start.elapsed() < window);
        self.bans.retain(|_, start| start.elapsed() < ban_dur);
    }
}
