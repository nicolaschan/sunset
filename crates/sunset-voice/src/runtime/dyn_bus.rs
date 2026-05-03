//! `DynBus` — type-erased host-supplied message bus.
//!
//! The runtime takes an `Rc<dyn DynBus>` so it does not need to be
//! parameterised over `<S: Store, T: Transport>`. Browsers and native
//! hosts pass an `Rc<BusImpl<...>>` cast to `dyn DynBus`.

use bytes::Bytes;
use futures::stream::LocalBoxStream;
use sunset_store::{ContentBlock, SignedKvEntry};

/// Type-erased `Bus`. `?Send` — single-threaded data plane.
#[async_trait::async_trait(?Send)]
pub trait DynBus {
    async fn publish_ephemeral(
        &self,
        name: Bytes,
        payload: Bytes,
    ) -> Result<(), Box<dyn std::error::Error>>;

    async fn publish_durable(
        &self,
        entry: SignedKvEntry,
        block: Option<ContentBlock>,
    ) -> Result<(), Box<dyn std::error::Error>>;

    async fn subscribe_voice_prefix(
        &self,
        prefix: Bytes,
    ) -> Result<LocalBoxStream<'static, sunset_core::bus::BusEvent>, Box<dyn std::error::Error>>;

    async fn subscribe_prefix(
        &self,
        prefix: Bytes,
    ) -> Result<LocalBoxStream<'static, sunset_core::bus::BusEvent>, Box<dyn std::error::Error>>;
}
