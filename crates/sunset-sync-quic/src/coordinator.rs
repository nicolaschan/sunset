//! Per-peer probe loop driving the NAT holepunch. Resolves with the
//! first confirmed candidate addr (in either direction) or times out.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::mpsc;

use crate::socket::HolepunchSocket;
use crate::wire::{Probe, ProbeRole};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConfirmedPath {
    pub addr: SocketAddr,
}

#[derive(Debug, thiserror::Error)]
pub enum HolepunchError {
    #[error("holepunch: no candidate confirmed in {0:?}")]
    Timeout(Duration),
    #[error("holepunch: probe channel closed")]
    ProbeChannelClosed,
}

/// Drives the per-peer ping/pong exchange over a shared
/// [`HolepunchSocket`]. The first candidate (in either direction) to
/// produce a matching reply is returned.
pub struct HolepunchCoordinator {
    socket: Arc<HolepunchSocket>,
    session_id: [u8; 16],
    local_pk: [u8; 32],
    remote_pk: [u8; 32],
    remote_candidates: Vec<SocketAddr>,
    pending_nonces: HashSet<[u8; 16]>,
    probe_rx: mpsc::UnboundedReceiver<(SocketAddr, Bytes)>,
}

impl HolepunchCoordinator {
    pub fn new(
        socket: Arc<HolepunchSocket>,
        session_id: [u8; 16],
        local_pk: [u8; 32],
        remote_pk: [u8; 32],
        remote_candidates: Vec<SocketAddr>,
        probe_rx: mpsc::UnboundedReceiver<(SocketAddr, Bytes)>,
    ) -> Self {
        Self {
            socket,
            session_id,
            local_pk,
            remote_pk,
            remote_candidates,
            pending_nonces: HashSet::new(),
            probe_rx,
        }
    }

    /// Drive the probe loop. Resolves with the first confirmed path,
    /// or a [`HolepunchError::Timeout`] after `deadline`.
    pub async fn run(mut self, deadline: Duration) -> Result<ConfirmedPath, HolepunchError> {
        let mut probe_interval = tokio::time::interval(Duration::from_millis(250));
        let mut overall_deadline = Box::pin(tokio::time::sleep(deadline));
        let mut tick: u64 = 0;
        loop {
            tokio::select! {
                _ = probe_interval.tick() => {
                    tick = tick.wrapping_add(1);
                    let nonce = derive_nonce(&self.session_id, &self.local_pk, tick);
                    self.pending_nonces.insert(nonce);
                    let probe = Probe {
                        session_id: self.session_id,
                        role: ProbeRole::Ping,
                        sender_pk: self.local_pk,
                        nonce,
                    };
                    let wire = match probe.encode() {
                        Ok(w) => w,
                        Err(e) => {
                            tracing::warn!("probe encode: {e}");
                            continue;
                        }
                    };
                    for cand in &self.remote_candidates {
                        if let Err(e) = self.socket.try_send_probe(*cand, &wire) {
                            tracing::debug!("probe try_send_probe({cand}): {e}");
                        }
                    }
                }
                inbound = self.probe_rx.recv() => {
                    let (src, body) = inbound.ok_or(HolepunchError::ProbeChannelClosed)?;
                    let probe = match Probe::decode(&body) {
                        Ok(Some(p)) => p,
                        _ => continue,
                    };
                    if probe.session_id != self.session_id {
                        continue;
                    }
                    if probe.sender_pk != self.remote_pk {
                        continue;
                    }
                    match probe.role {
                        ProbeRole::Ping => {
                            let pong = Probe {
                                session_id: self.session_id,
                                role: ProbeRole::Pong,
                                sender_pk: self.local_pk,
                                nonce: probe.nonce,
                            };
                            let wire = match pong.encode() {
                                Ok(w) => w,
                                Err(e) => {
                                    tracing::warn!("pong encode: {e}");
                                    continue;
                                }
                            };
                            if let Err(e) = self.socket.try_send_probe(src, &wire) {
                                tracing::debug!("pong try_send_probe({src}): {e}");
                            }
                            return Ok(ConfirmedPath { addr: src });
                        }
                        ProbeRole::Pong => {
                            if self.pending_nonces.contains(&probe.nonce) {
                                return Ok(ConfirmedPath { addr: src });
                            }
                        }
                    }
                }
                _ = &mut overall_deadline => {
                    return Err(HolepunchError::Timeout(deadline));
                }
            }
        }
    }
}

fn derive_nonce(session_id: &[u8; 16], local_pk: &[u8; 32], tick: u64) -> [u8; 16] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(session_id);
    h.update(local_pk);
    h.update(tick.to_le_bytes());
    let digest: [u8; 32] = h.finalize().into();
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

#[cfg(test)]
mod tests {
    use std::io::IoSliceMut;

    use super::*;
    use quinn::udp::RecvMeta;
    use quinn::AsyncUdpSocket;

    /// Build a HolepunchSocket on 127.0.0.1 with its own probe channel,
    /// plus a tokio task that drives poll_recv to route probes.
    async fn make_socket() -> (
        Arc<HolepunchSocket>,
        SocketAddr,
        mpsc::UnboundedReceiver<(SocketAddr, Bytes)>,
        tokio::task::JoinHandle<()>,
    ) {
        let std_udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let local_addr = std_udp.local_addr().unwrap();
        let (probe_tx, probe_rx) = mpsc::unbounded_channel();
        let sock = Arc::new(HolepunchSocket::new(std_udp, probe_tx).unwrap());
        let sock_pump = Arc::clone(&sock);
        let pump = tokio::spawn(async move {
            let mut storage = vec![0u8; 2048];
            loop {
                let mut bufs = [IoSliceMut::new(&mut storage)];
                let mut meta = [RecvMeta::default()];
                let r =
                    std::future::poll_fn(|cx| sock_pump.poll_recv(cx, &mut bufs, &mut meta)).await;
                if r.is_err() {
                    return;
                }
            }
        });
        (sock, local_addr, probe_rx, pump)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ping_pong_resolves_confirmed_path_via_loopback() {
        let (a_sock, a_addr, a_rx, a_pump) = make_socket().await;
        let (b_sock, b_addr, b_rx, b_pump) = make_socket().await;

        let session = [9u8; 16];
        let a_pk = [1u8; 32];
        let b_pk = [2u8; 32];
        let a = HolepunchCoordinator::new(a_sock, session, a_pk, b_pk, vec![b_addr], a_rx);
        let b = HolepunchCoordinator::new(b_sock, session, b_pk, a_pk, vec![a_addr], b_rx);

        let (a_res, b_res) = tokio::join!(
            a.run(Duration::from_secs(3)),
            b.run(Duration::from_secs(3))
        );
        let a_path = a_res.expect("a path");
        let b_path = b_res.expect("b path");
        assert_eq!(a_path.addr, b_addr);
        assert_eq!(b_path.addr, a_addr);

        a_pump.abort();
        b_pump.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn timeout_when_no_remote_candidate_responds() {
        let (sock, _, _rx, pump) = make_socket().await;
        // The receiver above never fires because no peer responds.
        // We need a new (unused) rx for the coordinator, since the
        // pump-driven one routes to its own queue.
        let (_unused_tx, rx) = mpsc::unbounded_channel();
        // Reachable but ignored: a port we know is closed on loopback.
        let blackhole: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let coord = HolepunchCoordinator::new(sock, [0u8; 16], [0u8; 32], [0u8; 32], vec![blackhole], rx);
        let err = coord
            .run(Duration::from_millis(800))
            .await
            .expect_err("expected timeout");
        assert!(matches!(err, HolepunchError::Timeout(_)));
        pump.abort();
    }
}
