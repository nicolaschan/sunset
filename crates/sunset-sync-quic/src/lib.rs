//! sunset-sync-quic: NAT-hole-punched QUIC transport for sunset-sync.
//!
//! Implements [`sunset_sync::RawTransport`] using a shared
//! [`sunset_sync::Signaler`] (typically a `RelaySignaler` over a
//! `sunset-store`) to exchange candidate addresses out-of-band, then
//! probes for a working UDP path, then layers QUIC on top.
//!
//! See `docs/superpowers/specs/2026-05-12-sunset-sync-quic-design.md`.

mod cert;
mod connection;
mod coordinator;
mod discovery;
mod socket;
mod wire;

pub use connection::{QuicRawConnection, MAX_DATAGRAM_PAYLOAD};
