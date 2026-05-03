//! sunset-sync: peer-to-peer replication of sunset-store data over a pluggable
//! transport.
//!
//! See `docs/superpowers/specs/2026-04-25-sunset-store-and-sync-design.md` for design.

mod connectable;
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
pub mod spawning_acceptor;
pub mod subscription_registry;
pub mod supervisor;
pub mod transport;
pub mod types;

#[cfg(feature = "test-helpers")]
pub mod test_transport;

#[cfg(test)]
mod test_fixtures;

pub use connectable::{Connectable, ResolveErr};
pub use engine::{EngineEvent, SyncEngine};
pub use error::{Error, Result};
pub use message::{DigestRange, SyncMessage};
pub use multi_transport::{MultiConnection, MultiTransport};
pub use signaler::{SignalMessage, Signaler};
pub use signer::Signer;
pub use spawning_acceptor::SpawningAcceptor;
pub use supervisor::{BackoffPolicy, IntentId, IntentSnapshot, IntentState, PeerSupervisor};
pub use transport::{RawConnection, RawTransport, Transport, TransportConnection, TransportKind};
pub use types::{PeerAddr, PeerId, SyncConfig, TrustSet};
