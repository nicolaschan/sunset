//! Noise IK handshake + post-handshake transport encryption.
//!
//! Wraps any `sunset_sync::RawTransport` with the
//! `Noise_IK_25519_XChaChaPoly_BLAKE2b` pattern via `snow`.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use snow::{Builder, HandshakeState, TransportState};
use tokio::sync::Mutex;

use sunset_store::VerifyingKey;
use sunset_sync::{
    PeerAddr, PeerId, RawConnection, RawTransport, Result as SyncResult, Transport,
    TransportConnection,
};

use crate::error::{Error, Result};
use crate::identity::{NoiseIdentity, ed25519_seed_to_x25519_secret};
use crate::pattern::NOISE_PATTERN;

/// A `Transport` decorator that runs the Noise IK handshake on each
/// connection and exposes the result as an authenticated, encrypted
/// `TransportConnection`.
pub struct NoiseTransport<R: RawTransport> {
    raw: R,
    local: Arc<dyn NoiseIdentity>,
}

impl<R: RawTransport> NoiseTransport<R> {
    pub fn new(raw: R, local: Arc<dyn NoiseIdentity>) -> Self {
        Self { raw, local }
    }
}

#[async_trait(?Send)]
impl<R: RawTransport> Transport for NoiseTransport<R>
where
    R::Connection: 'static,
{
    type Connection = NoiseConnection<R::Connection>;

    async fn connect(&self, addr: PeerAddr) -> SyncResult<Self::Connection> {
        let remote_x25519 =
            parse_addr_x25519(&addr).map_err(|e| sunset_sync::Error::Transport(format!("{e}")))?;
        let raw = self.raw.connect(addr).await?;
        do_handshake_initiator(raw, self.local.clone(), remote_x25519)
            .await
            .map_err(|e| sunset_sync::Error::Transport(format!("noise initiator: {e}")))
    }

    async fn accept(&self) -> SyncResult<Self::Connection> {
        let raw = self.raw.accept().await?;
        do_handshake_responder(raw, self.local.clone())
            .await
            .map_err(|e| sunset_sync::Error::Transport(format!("noise responder: {e}")))
    }
}

/// Parse `wss://host:port#x25519=<hex>` (or ws://, etc.) and return the
/// X25519 pubkey. The fragment is the contractual home for the responder's
/// expected static pubkey under the Noise IK pattern.
fn parse_addr_x25519(addr: &PeerAddr) -> Result<[u8; 32]> {
    let s =
        std::str::from_utf8(addr.as_bytes()).map_err(|e| Error::Addr(format!("not utf-8: {e}")))?;
    let (_url, fragment) = s
        .split_once('#')
        .ok_or_else(|| Error::MissingStaticPubkey(format!("address has no fragment: {s}")))?;
    let pair = fragment.strip_prefix("x25519=").ok_or_else(|| {
        Error::MissingStaticPubkey(format!("fragment is not `x25519=…`: {fragment}"))
    })?;
    let bytes = hex::decode(pair)
        .map_err(|e| Error::MissingStaticPubkey(format!("hex decode failed: {e}")))?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map_err(|_| Error::MissingStaticPubkey(format!("expected 32 bytes, got {}", bytes.len())))
}

async fn do_handshake_initiator<C: RawConnection + 'static>(
    raw: C,
    local: Arc<dyn NoiseIdentity>,
    remote_x25519: [u8; 32],
) -> Result<NoiseConnection<C>> {
    let seed = local.ed25519_secret_seed();
    let local_x25519_secret = ed25519_seed_to_x25519_secret(&seed);

    let mut hs: HandshakeState = Builder::new(
        NOISE_PATTERN
            .parse()
            .map_err(|e| Error::Snow(format!("{e:?}")))?,
    )
    .local_private_key(&local_x25519_secret[..])?
    .remote_public_key(&remote_x25519)?
    .build_initiator()?;

    // IK message 1: e, es, s, ss
    let mut buf = vec![0u8; 1024];
    let n = hs.write_message(&[], &mut buf)?;
    raw.send_reliable(Bytes::copy_from_slice(&buf[..n])).await?;

    // IK message 2: e, ee, se
    let response = raw.recv_reliable().await?;
    let mut payload = vec![0u8; 1024];
    hs.read_message(&response, &mut payload)?;

    let transport: TransportState = hs.into_transport_mode()?;
    let remote_static = transport
        .get_remote_static()
        .ok_or_else(|| Error::Snow("no remote static".into()))?;
    let remote_static_x25519: [u8; 32] = remote_static
        .try_into()
        .map_err(|_| Error::Snow("remote static wrong length".into()))?;

    if remote_static_x25519 != remote_x25519 {
        return Err(Error::Snow(
            "remote static does not match PeerAddr expected pubkey".into(),
        ));
    }

    let peer_id = PeerId(VerifyingKey::new(Bytes::copy_from_slice(
        &remote_static_x25519,
    )));

    Ok(NoiseConnection {
        raw,
        state: Arc::new(Mutex::new(transport)),
        peer_id,
    })
}

pub async fn do_handshake_responder<C: RawConnection + 'static>(
    raw: C,
    local: Arc<dyn NoiseIdentity>,
) -> Result<NoiseConnection<C>> {
    let seed = local.ed25519_secret_seed();
    let local_x25519_secret = ed25519_seed_to_x25519_secret(&seed);

    let mut hs: HandshakeState = Builder::new(
        NOISE_PATTERN
            .parse()
            .map_err(|e| Error::Snow(format!("{e:?}")))?,
    )
    .local_private_key(&local_x25519_secret[..])?
    .build_responder()?;

    // IK message 1
    let msg1 = raw.recv_reliable().await?;
    let mut payload = vec![0u8; 1024];
    hs.read_message(&msg1, &mut payload)?;

    // IK message 2
    let mut buf = vec![0u8; 1024];
    let n = hs.write_message(&[], &mut buf)?;
    raw.send_reliable(Bytes::copy_from_slice(&buf[..n])).await?;

    let transport: TransportState = hs.into_transport_mode()?;
    let remote_static = transport
        .get_remote_static()
        .ok_or_else(|| Error::Snow("no remote static".into()))?;
    let remote_static_x25519: [u8; 32] = remote_static
        .try_into()
        .map_err(|_| Error::Snow("remote static wrong length".into()))?;

    let peer_id = PeerId(VerifyingKey::new(Bytes::copy_from_slice(
        &remote_static_x25519,
    )));

    Ok(NoiseConnection {
        raw,
        state: Arc::new(Mutex::new(transport)),
        peer_id,
    })
}

/// Authenticated, encrypted connection. `send_reliable`/`recv_reliable`
/// transparently encrypt/decrypt via the Noise transport state.
pub struct NoiseConnection<C: RawConnection> {
    raw: C,
    state: Arc<Mutex<TransportState>>,
    peer_id: PeerId,
}

#[async_trait(?Send)]
impl<C: RawConnection> TransportConnection for NoiseConnection<C> {
    async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
        let mut buf = vec![0u8; bytes.len() + 16];
        let n = {
            let mut state = self.state.lock().await;
            state
                .write_message(&bytes, &mut buf)
                .map_err(|e| sunset_sync::Error::Transport(format!("noise encrypt: {e:?}")))?
        };
        self.raw
            .send_reliable(Bytes::copy_from_slice(&buf[..n]))
            .await
    }

    async fn recv_reliable(&self) -> SyncResult<Bytes> {
        let ct = self.raw.recv_reliable().await?;
        let mut pt = vec![0u8; ct.len()];
        let n = {
            let mut state = self.state.lock().await;
            state
                .read_message(&ct, &mut pt)
                .map_err(|e| sunset_sync::Error::Transport(format!("noise decrypt: {e:?}")))?
        };
        Ok(Bytes::copy_from_slice(&pt[..n]))
    }

    async fn send_unreliable(&self, bytes: Bytes) -> SyncResult<()> {
        self.raw.send_unreliable(bytes).await
    }

    async fn recv_unreliable(&self) -> SyncResult<Bytes> {
        self.raw.recv_unreliable().await
    }

    fn peer_id(&self) -> PeerId {
        self.peer_id.clone()
    }

    async fn close(&self) -> SyncResult<()> {
        self.raw.close().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use bytes::Bytes;
    use tokio::sync::mpsc;
    use zeroize::Zeroizing;

    struct PipeRawConnection {
        tx: tokio::sync::Mutex<mpsc::UnboundedSender<Bytes>>,
        rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<Bytes>>,
    }

    #[async_trait(?Send)]
    impl RawConnection for PipeRawConnection {
        async fn send_reliable(&self, bytes: Bytes) -> SyncResult<()> {
            self.tx
                .lock()
                .await
                .send(bytes)
                .map_err(|_| sunset_sync::Error::Transport("pipe closed".into()))
        }
        async fn recv_reliable(&self) -> SyncResult<Bytes> {
            self.rx
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| sunset_sync::Error::Transport("pipe closed".into()))
        }
        async fn send_unreliable(&self, _: Bytes) -> SyncResult<()> {
            Err(sunset_sync::Error::Transport("unsupported".into()))
        }
        async fn recv_unreliable(&self) -> SyncResult<Bytes> {
            Err(sunset_sync::Error::Transport("unsupported".into()))
        }
        async fn close(&self) -> SyncResult<()> {
            Ok(())
        }
    }

    fn make_pipe_pair() -> (PipeRawConnection, PipeRawConnection) {
        let (a_to_b_tx, a_to_b_rx) = mpsc::unbounded_channel::<Bytes>();
        let (b_to_a_tx, b_to_a_rx) = mpsc::unbounded_channel::<Bytes>();
        (
            PipeRawConnection {
                tx: tokio::sync::Mutex::new(a_to_b_tx),
                rx: tokio::sync::Mutex::new(b_to_a_rx),
            },
            PipeRawConnection {
                tx: tokio::sync::Mutex::new(b_to_a_tx),
                rx: tokio::sync::Mutex::new(a_to_b_rx),
            },
        )
    }

    struct StaticIdentity {
        seed: [u8; 32],
    }
    impl NoiseIdentity for StaticIdentity {
        fn ed25519_public(&self) -> [u8; 32] {
            use ed25519_dalek::SigningKey;
            SigningKey::from_bytes(&self.seed)
                .verifying_key()
                .to_bytes()
        }
        fn ed25519_secret_seed(&self) -> Zeroizing<[u8; 32]> {
            Zeroizing::new(self.seed)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn noise_handshake_roundtrip() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let alice = Arc::new(StaticIdentity { seed: [1u8; 32] });
                let bob = Arc::new(StaticIdentity { seed: [2u8; 32] });

                let (a_pipe, b_pipe) = make_pipe_pair();

                // Compute bob's X25519 PUBLIC key from his secret.
                let bob_x25519_secret = ed25519_seed_to_x25519_secret(&bob.seed);
                use curve25519_dalek::{MontgomeryPoint, scalar::Scalar};
                let bob_x25519_pub: [u8; 32] = {
                    let scalar = Scalar::from_bytes_mod_order(*bob_x25519_secret);
                    MontgomeryPoint::mul_base(&scalar).to_bytes()
                };

                let alice_handle = tokio::task::spawn_local({
                    let alice_id = alice.clone();
                    async move { do_handshake_initiator(a_pipe, alice_id, bob_x25519_pub).await }
                });
                let bob_handle = tokio::task::spawn_local({
                    let bob_id = bob.clone();
                    async move { do_handshake_responder(b_pipe, bob_id).await }
                });

                let alice_conn = alice_handle.await.unwrap().expect("alice handshake");
                let bob_conn = bob_handle.await.unwrap().expect("bob handshake");

                alice_conn
                    .send_reliable(Bytes::from_static(b"hello bob"))
                    .await
                    .unwrap();
                let received = bob_conn.recv_reliable().await.unwrap();
                assert_eq!(received.as_ref(), b"hello bob");
            })
            .await;
    }

    #[test]
    fn parse_addr_extracts_x25519_fragment() {
        let bytes = b"wss://relay.example.com:443#x25519=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let addr = PeerAddr::new(Bytes::copy_from_slice(bytes));
        let key = parse_addr_x25519(&addr).unwrap();
        assert_eq!(key.len(), 32);
        assert_eq!(key[0], 0x01);
        assert_eq!(key[31], 0xef);
    }

    #[test]
    fn parse_addr_rejects_missing_fragment() {
        let bytes = b"wss://relay.example.com:443/";
        let addr = PeerAddr::new(Bytes::copy_from_slice(bytes));
        let err = parse_addr_x25519(&addr).unwrap_err();
        assert!(matches!(err, Error::MissingStaticPubkey(_)));
    }
}
