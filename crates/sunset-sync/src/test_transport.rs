//! In-memory `Transport` implementation for tests.
//!
//! `TestNetwork` is a registry that mediates between `TestTransport`s.
//! Each transport has a `PeerAddr`; calling `connect(addr)` looks up the
//! matching transport in the network and creates a paired `TestConnection`
//! on both sides. Both reliable and unreliable channels are real in-memory
//! pipes (separate `mpsc::unbounded_channel` pairs).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};

use crate::error::{Error, Result};
use crate::transport::{Transport, TransportConnection};
use crate::types::{PeerAddr, PeerId};

/// Type alias to satisfy clippy's type_complexity lint.
type InboxMap = Rc<RefCell<HashMap<PeerAddr, (PeerId, mpsc::UnboundedSender<ConnectRequest>)>>>;

/// Routing fabric shared by all `TestTransport`s in a test.
#[derive(Clone, Default)]
pub struct TestNetwork {
    /// Maps a peer's address to (peer_id, accept-queue sender). The peer_id is
    /// kept here so a connecting peer can learn the acceptor's identity
    /// before any application-layer handshake. This is a TEST-only convenience;
    /// production transports learn peer_id from the connection handshake.
    inboxes: InboxMap,
}

impl TestNetwork {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a `TestTransport` with the given identity and address.
    /// Registering the address on the network makes it `connect`able.
    pub fn transport(&self, peer_id: PeerId, addr: PeerAddr) -> TestTransport {
        let (tx, rx) = mpsc::unbounded_channel::<ConnectRequest>();
        self.inboxes
            .borrow_mut()
            .insert(addr.clone(), (peer_id.clone(), tx));
        TestTransport {
            peer_id,
            addr,
            net: self.clone(),
            accept_rx: Rc::new(RefCell::new(rx)),
        }
    }
}

/// A connect-request crossing the network from initiator to acceptor.
struct ConnectRequest {
    /// Initiator's identity.
    from_peer: PeerId,
    /// Channel pair to install on the acceptor's side.
    /// (acceptor will send via `tx_to_initiator`; receive via `rx_from_initiator`.)
    tx_to_initiator: mpsc::UnboundedSender<Bytes>,
    rx_from_initiator: mpsc::UnboundedReceiver<Bytes>,
    /// Parallel unreliable channel pair installed on the acceptor's side.
    tx_to_initiator_unrel: mpsc::UnboundedSender<Bytes>,
    rx_from_initiator_unrel: mpsc::UnboundedReceiver<Bytes>,
    /// Reply channel: acceptor signals "connection installed" so the
    /// initiator can complete `connect()`.
    ready: oneshot::Sender<()>,
}

#[derive(Clone)]
pub struct TestTransport {
    peer_id: PeerId,
    addr: PeerAddr,
    net: TestNetwork,
    accept_rx: Rc<RefCell<mpsc::UnboundedReceiver<ConnectRequest>>>,
}

impl TestTransport {
    pub fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }

    pub fn addr(&self) -> &PeerAddr {
        &self.addr
    }
}

#[async_trait(?Send)]
impl Transport for TestTransport {
    type Connection = TestConnection;

    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
        // Find the target's inbox AND identity.
        let (target_peer_id, inbox) = self
            .net
            .inboxes
            .borrow()
            .get(&addr)
            .cloned()
            .ok_or_else(|| Error::Transport(format!("no peer at {:?}", addr)))?;

        // Build the channel pair (reliable).
        let (tx_initiator_to_acceptor, rx_initiator_to_acceptor) =
            mpsc::unbounded_channel::<Bytes>();
        let (tx_acceptor_to_initiator, rx_acceptor_to_initiator) =
            mpsc::unbounded_channel::<Bytes>();
        // Build the channel pair (unreliable).
        let (tx_initiator_to_acceptor_unrel, rx_initiator_to_acceptor_unrel) =
            mpsc::unbounded_channel::<Bytes>();
        let (tx_acceptor_to_initiator_unrel, rx_acceptor_to_initiator_unrel) =
            mpsc::unbounded_channel::<Bytes>();
        let (ready_tx, ready_rx) = oneshot::channel::<()>();

        // Send the request to the acceptor side.
        inbox
            .send(ConnectRequest {
                from_peer: self.peer_id.clone(),
                // Acceptor uses tx_acceptor_to_initiator for its send;
                // rx_initiator_to_acceptor for its recv.
                tx_to_initiator: tx_acceptor_to_initiator,
                rx_from_initiator: rx_initiator_to_acceptor,
                tx_to_initiator_unrel: tx_acceptor_to_initiator_unrel,
                rx_from_initiator_unrel: rx_initiator_to_acceptor_unrel,
                ready: ready_tx,
            })
            .map_err(|_| Error::Transport("acceptor inbox closed".into()))?;

        // Wait for the acceptor to install its side.
        ready_rx
            .await
            .map_err(|_| Error::Transport("acceptor dropped without accepting".into()))?;

        // Initiator's connection: send via tx_initiator_to_acceptor, recv via rx_acceptor_to_initiator.
        // peer_id is the acceptor's identity (we looked it up above).
        Ok(TestConnection::new(
            target_peer_id,
            tx_initiator_to_acceptor,
            rx_acceptor_to_initiator,
            tx_initiator_to_acceptor_unrel,
            rx_acceptor_to_initiator_unrel,
        ))
    }

    // `borrow_mut()` is held across `.await` intentionally: the entire transport
    // runs on a single-threaded (?Send) executor, so no concurrent borrow can
    // occur while the future is suspended.
    #[allow(clippy::await_holding_refcell_ref)]
    async fn accept(&self) -> Result<Self::Connection> {
        let req = self
            .accept_rx
            .borrow_mut()
            .recv()
            .await
            .ok_or_else(|| Error::Transport("transport closed".into()))?;
        // Install our side and signal ready.
        let _ = req.ready.send(());
        Ok(TestConnection::new(
            req.from_peer,
            req.tx_to_initiator,
            req.rx_from_initiator,
            req.tx_to_initiator_unrel,
            req.rx_from_initiator_unrel,
        ))
    }
}

#[derive(Debug)]
pub struct TestConnection {
    peer_id: PeerId,
    tx: mpsc::UnboundedSender<Bytes>,
    rx: RefCell<mpsc::UnboundedReceiver<Bytes>>,
    tx_unrel: mpsc::UnboundedSender<Bytes>,
    rx_unrel: RefCell<mpsc::UnboundedReceiver<Bytes>>,
}

impl TestConnection {
    fn new(
        peer_id: PeerId,
        tx: mpsc::UnboundedSender<Bytes>,
        rx: mpsc::UnboundedReceiver<Bytes>,
        tx_unrel: mpsc::UnboundedSender<Bytes>,
        rx_unrel: mpsc::UnboundedReceiver<Bytes>,
    ) -> Self {
        Self {
            peer_id,
            tx,
            rx: RefCell::new(rx),
            tx_unrel,
            rx_unrel: RefCell::new(rx_unrel),
        }
    }
}

#[async_trait(?Send)]
impl TransportConnection for TestConnection {
    async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
        self.tx
            .send(bytes)
            .map_err(|_| Error::Transport("connection closed".into()))
    }

    // `borrow_mut()` is held across `.await` intentionally: single-threaded
    // (?Send) executor means no concurrent borrow can occur while suspended.
    #[allow(clippy::await_holding_refcell_ref)]
    async fn recv_reliable(&self) -> Result<Bytes> {
        self.rx
            .borrow_mut()
            .recv()
            .await
            .ok_or_else(|| Error::Transport("connection closed".into()))
    }

    async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
        self.tx_unrel
            .send(bytes)
            .map_err(|_| Error::Transport("connection closed".into()))
    }

    // `borrow_mut()` is held across `.await` intentionally: single-threaded
    // (?Send) executor means no concurrent borrow can occur while suspended.
    #[allow(clippy::await_holding_refcell_ref)]
    async fn recv_unreliable(&self) -> Result<Bytes> {
        self.rx_unrel
            .borrow_mut()
            .recv()
            .await
            .ok_or_else(|| Error::Transport("connection closed".into()))
    }

    fn peer_id(&self) -> PeerId {
        self.peer_id.clone()
    }

    async fn close(&self) -> Result<()> {
        // Drops on Drop; nothing to do explicitly. The next send/recv on
        // the other end will yield `connection closed`.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn vk(b: &[u8]) -> VerifyingKey {
        VerifyingKey::new(Bytes::copy_from_slice(b))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pair_can_send_and_recv() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice_addr = PeerAddr::new("alice");
                let bob_addr = PeerAddr::new("bob");
                let alice = net.transport(PeerId(vk(b"alice")), alice_addr.clone());
                let bob = net.transport(PeerId(vk(b"bob")), bob_addr.clone());

                let bob_accept =
                    tokio::task::spawn_local(async move { bob.accept().await.unwrap() });

                let alice_conn = alice.connect(bob_addr).await.unwrap();
                let bob_conn = bob_accept.await.unwrap();

                alice_conn
                    .send_reliable(Bytes::from_static(b"hello"))
                    .await
                    .unwrap();
                let got = bob_conn.recv_reliable().await.unwrap();
                assert_eq!(got, Bytes::from_static(b"hello"));

                bob_conn
                    .send_reliable(Bytes::from_static(b"world"))
                    .await
                    .unwrap();
                let got = alice_conn.recv_reliable().await.unwrap();
                assert_eq!(got, Bytes::from_static(b"world"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pair_can_send_and_recv_unreliable() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let net = TestNetwork::new();
                let alice_addr = PeerAddr::new("alice");
                let bob_addr = PeerAddr::new("bob");
                let alice = net.transport(PeerId(vk(b"alice")), alice_addr.clone());
                let bob = net.transport(PeerId(vk(b"bob")), bob_addr.clone());

                let bob_accept =
                    tokio::task::spawn_local(async move { bob.accept().await.unwrap() });

                let alice_conn = alice.connect(bob_addr).await.unwrap();
                let bob_conn = bob_accept.await.unwrap();

                alice_conn
                    .send_unreliable(Bytes::from_static(b"datagram"))
                    .await
                    .unwrap();
                let got = bob_conn.recv_unreliable().await.unwrap();
                assert_eq!(got, Bytes::from_static(b"datagram"));

                bob_conn
                    .send_unreliable(Bytes::from_static(b"reply"))
                    .await
                    .unwrap();
                let got = alice_conn.recv_unreliable().await.unwrap();
                assert_eq!(got, Bytes::from_static(b"reply"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn connect_to_unknown_addr_errors() {
        let net = TestNetwork::new();
        let alice = net.transport(PeerId(vk(b"alice")), PeerAddr::new("alice"));
        let err = alice.connect(PeerAddr::new("nobody")).await.unwrap_err();
        assert!(matches!(err, Error::Transport(_)));
    }
}
