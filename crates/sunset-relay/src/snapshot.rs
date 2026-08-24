//! Engine-side construction of the identity descriptor served at `GET /`.
//!
//! Reads the live `Rc<SyncEngine>` and produces a `Send` POD the axum
//! side renders. Runs inside the LocalSet identity pump; never crosses
//! runtimes itself.

use std::rc::Rc;

use sunset_store::Store;
use sunset_sync::SyncEngine;

/// Send-only POD carrying everything the `/` descriptor reports.
#[derive(Clone, Debug)]
pub struct IdentitySnapshot {
    pub ed25519_public: [u8; 32],
    pub x25519_public: [u8; 32],
    pub dial_url: String,
    /// SHA-256 hex of the SPKI for the relay's self-signed WebTransport
    /// cert (the `serverCertificateHashes` value the browser pins).
    /// `None` when the relay didn't manage to bind its UDP listener.
    ///
    /// We deliberately ship *only the hash*, not a full URL — the relay
    /// has no reliable way to know its own public hostname (it could
    /// be behind any number of proxies, and binds `0.0.0.0` in
    /// production), so the resolver builds the WT URL from the
    /// user-typed authority instead.
    pub webtransport_cert_sha256: Option<String>,
    /// Running count of ephemeral datagrams this relay re-forwarded,
    /// summed over recipients. Server-side ground truth the voice
    /// relay-fallback e2e polls to confirm audio is crossing the relay
    /// (rising) vs. flowing direct (flat).
    pub ephemeral_forwarded: u64,
}

/// Generic over the store/transport so the relay's concrete
/// `SyncEngine<FsStore, InboundTransport>` and the test harness's
/// `SyncEngine<MemoryStore, TestTransport>` both satisfy it.
pub async fn build_identity_snapshot<S, T>(
    engine: &Rc<SyncEngine<S, T>>,
    ed25519_public: [u8; 32],
    x25519_public: [u8; 32],
    dial_url: &str,
    webtransport_cert_sha256: Option<&str>,
) -> IdentitySnapshot
where
    S: Store + 'static,
    T: sunset_sync::Transport + 'static,
    T::Connection: 'static,
{
    IdentitySnapshot {
        ed25519_public,
        x25519_public,
        dial_url: dial_url.to_owned(),
        webtransport_cert_sha256: webtransport_cert_sha256.map(str::to_owned),
        ephemeral_forwarded: engine.ephemeral_forwarded().await,
    }
}
