//! Discover a candidate set for a bound UDP socket:
//! * every local interface address (filtered for non-unspecified),
//!   stamped with the socket's bound port;
//! * the STUN-reflexive address for each provided STUN server (if any).

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};

use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use stunclient::StunClient;
use tokio::net::{lookup_host, UdpSocket};

/// Enumerate local-interface socket addrs stamped with `port`.
pub fn local_candidates(port: u16) -> Vec<SocketAddr> {
    let mut out = HashSet::new();
    let ifs = match NetworkInterface::show() {
        Ok(ifs) => ifs,
        Err(e) => {
            tracing::warn!("NetworkInterface::show: {e}");
            return vec![];
        }
    };
    for iface in ifs {
        for addr in iface.addr {
            let ip = match addr {
                Addr::V4(v4) => IpAddr::V4(v4.ip),
                Addr::V6(v6) => IpAddr::V6(v6.ip),
            };
            if ip.is_unspecified() {
                continue;
            }
            out.insert(SocketAddr::new(ip, port));
        }
    }
    out.into_iter().collect()
}

/// Best-effort STUN-reflexive address lookup over the bound socket.
/// Returns an empty `Vec` if all STUN servers fail or the list is empty.
pub async fn stun_candidates(socket: &UdpSocket, stun_servers: &[String]) -> Vec<SocketAddr> {
    let mut out = HashSet::new();
    for server in stun_servers {
        let resolved = match lookup_host(server).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("stun lookup_host({server}): {e}");
                continue;
            }
        };
        for addr in resolved {
            let client = StunClient::new(addr);
            match client.query_external_address_async(socket).await {
                Ok(reflexive) => {
                    out.insert(reflexive);
                }
                Err(e) => {
                    tracing::warn!("stun query {addr}: {e}");
                }
            }
        }
    }
    out.into_iter().collect()
}

/// Union of local + STUN-reflexive candidates for the given socket and
/// STUN list. Filters unspecified addresses.
pub async fn discover(socket: &UdpSocket, stun_servers: &[String]) -> Vec<SocketAddr> {
    let port = match socket.local_addr() {
        Ok(a) => a.port(),
        Err(_) => return vec![],
    };
    let mut out: HashSet<SocketAddr> = local_candidates(port).into_iter().collect();
    out.extend(stun_candidates(socket, stun_servers).await);
    out.retain(|s| !s.ip().is_unspecified());
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn local_candidates_include_loopback_with_port() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = socket.local_addr().unwrap().port();
        let cands = local_candidates(port);
        assert!(
            cands.iter().any(|s| s.ip().is_loopback() && s.port() == port),
            "expected loopback in {cands:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn discover_with_no_stun_returns_only_local() {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = socket.local_addr().unwrap().port();
        let cands = discover(&socket, &[]).await;
        assert!(
            cands.iter().any(|s| s.ip().is_loopback() && s.port() == port),
            "expected loopback in {cands:?}"
        );
    }
}
