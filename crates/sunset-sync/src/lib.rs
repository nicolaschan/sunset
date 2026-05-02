//! sunset-sync: peer-to-peer replication of sunset-store data over a pluggable
//! transport.
//!
//! See `docs/superpowers/specs/2026-04-25-sunset-store-and-sync-design.md` for design.
//!
//! ## Inbound handshake concurrency
//!
//! Transports that accept inbound peers (TCP listener, WebRTC
//! offers, etc.) can use [`crate::spawn_accept_worker`] to run handshakes
//! concurrently with a per-task timeout and a semaphore-bounded
//! inflight cap. See `accept_worker.rs` for the abstraction; the
//! relay (`sunset-relay`) and the browser WebRTC transport
//! (`sunset-sync-webrtc-browser`) are the two reference adopters.

pub mod accept_worker;
pub mod digest;
pub mod engine;
pub mod error;
pub mod message;
pub mod multi_transport;
pub mod peer;
pub mod reserved;
pub mod signaler;
pub mod signer;
pub mod spawn;
pub mod subscription_registry;
pub mod supervisor;
pub mod transport;
pub mod types;

#[cfg(feature = "test-helpers")]
pub mod test_transport;

#[cfg(test)]
mod test_fixtures;

pub use crate::accept_worker::spawn_accept_worker;
pub use engine::{EngineEvent, SyncEngine};
pub use error::{Error, Result};
pub use message::{DigestRange, SyncMessage};
pub use multi_transport::{MultiConnection, MultiTransport};
pub use signaler::{SignalMessage, Signaler};
pub use signer::Signer;
pub use supervisor::{BackoffPolicy, IntentSnapshot, IntentState, PeerSupervisor};
pub use transport::{RawConnection, RawTransport, Transport, TransportConnection, TransportKind};
pub use types::{PeerAddr, PeerId, SyncConfig, TrustSet};
