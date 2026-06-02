//! `DynBus` — type-erased host-supplied message bus.
//!
//! The runtime takes an `Rc<dyn DynBus>` so it does not need to be
//! parameterised over `<S: Store, T: Transport>`. Browsers and native
//! hosts pass an `Rc<BusImpl<...>>` cast to `dyn DynBus`.

use bytes::Bytes;
use futures::stream::LocalBoxStream;
use tokio::sync::mpsc;

use sunset_core::bus::{
    EngineEvent, Filter, PeerId, SignedDatagram, SubscriptionPolicy, TransportKind,
};
use sunset_store::{ContentBlock, SignedKvEntry};

/// Type-erased `Bus`. `?Send` — single-threaded data plane.
#[async_trait::async_trait(?Send)]
pub trait DynBus {
    async fn publish_ephemeral(
        &self,
        name: Bytes,
        seq: u64,
        payload: Bytes,
    ) -> Result<(), Box<dyn std::error::Error>>;

    async fn publish_durable(
        &self,
        entry: SignedKvEntry,
        block: Option<ContentBlock>,
    ) -> Result<(), Box<dyn std::error::Error>>;

    async fn subscribe_prefix(
        &self,
        prefix: Bytes,
    ) -> Result<LocalBoxStream<'static, sunset_core::bus::BusEvent>, Box<dyn std::error::Error>>;

    /// Declare interest in `filter` from one specific `provider`.
    async fn subscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
        policy: SubscriptionPolicy,
    ) -> Result<(), Box<dyn std::error::Error>>;

    /// Withdraw a `subscribe_via(filter, provider)` interest. Idempotent.
    async fn unsubscribe_via(
        &self,
        filter: Filter,
        provider: PeerId,
    ) -> Result<(), Box<dyn std::error::Error>>;

    /// Snapshot the currently-connected peers with each peer's transport kind.
    async fn current_peers(&self) -> Vec<(PeerId, TransportKind)>;

    /// Subscribe to engine lifecycle events. Fresh receiver per call; no replay.
    async fn subscribe_engine_events(&self) -> mpsc::UnboundedReceiver<EngineEvent>;

    /// Open the in-process ephemeral channel for `filter` WITHOUT arming any
    /// remote interest (no `BroadcastIntent`).
    async fn subscribe_ephemeral_local(
        &self,
        filter: Filter,
    ) -> mpsc::UnboundedReceiver<SignedDatagram>;
}
