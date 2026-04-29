//! Pub/sub abstraction over both durable (CRDT-replicated) and
//! ephemeral (real-time, fire-and-forget) message delivery. Same
//! filter system, same signing model; different persistence + transport.
//!
//! See `docs/superpowers/specs/2026-04-28-sunset-bus-pubsub-design.md`
//! for the architecture.

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::LocalBoxStream;

use sunset_store::{ContentBlock, Filter, SignedDatagram, SignedKvEntry};

use crate::error::Result;

/// A message delivered to a Bus subscriber. Tagged by delivery mode
/// so consumers can act differently (e.g. voice consumes Ephemeral,
/// chat consumes Durable).
#[derive(Clone, Debug)]
pub enum BusEvent {
    Durable {
        entry: SignedKvEntry,
        block: Option<ContentBlock>,
    },
    Ephemeral(SignedDatagram),
}

/// Unified pub/sub interface. `publish_durable` writes a signed KV
/// entry to the local store and lets the engine fan out via CRDT
/// replication. `publish_ephemeral` signs the payload, hands it to
/// the engine for unreliable fan-out, and dispatches a loopback copy
/// to local subscribers. `subscribe` opens a single stream that
/// merges both delivery modes.
#[async_trait(?Send)]
pub trait Bus {
    async fn publish_durable(
        &self,
        entry: SignedKvEntry,
        block: Option<ContentBlock>,
    ) -> Result<()>;

    async fn publish_ephemeral(&self, name: Bytes, payload: Bytes) -> Result<()>;

    async fn subscribe(&self, filter: Filter) -> Result<LocalBoxStream<'static, BusEvent>>;
}
