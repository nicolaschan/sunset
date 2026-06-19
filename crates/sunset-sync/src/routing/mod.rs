//! Cooperative-relay routing layer.
//!
//! See `docs/superpowers/specs/2026-05-23-cooperative-relay-design.md`
//! and `docs/superpowers/specs/2026-05-24-cooperative-relay-phase2-design.md`.
//!
//! Engine-facing layout:
//!
//! - **Wire types** (`types`): `SubscriptionEntry`, `LinkState`,
//!   `Neighbor`, `ProviderTick`.
//! - **Per-pair naming** (`naming`): `SUBSCRIBE_PREFIX`,
//!   `subscription_name`, `is_subscription_name`,
//!   `decode_filter_hash_from_name`, plus the well-known
//!   `LINKS_NAME` / `PROVIDER_TICK_NAME` keys.
//! - **Subscription policy** (`policy`): `SubscriptionPolicy` with
//!   `store_data()` / `relay_broad()` constructors driving entry TTL
//!   and refresh interval, plus the `relay_broad_filter()` helper.
//! - **Coverage predicate** (`coverage`): `covers(superset, subset)`.
//! - **Receiver-side state** (`routes`): `Routes`, `OutboundKey`,
//!   `Outbound`, `BroadcastIntent`, `FilterHash`,
//!   `FILTER_HASH_HEX_LEN`, `filter_hash`.
//! - **Forwarding decision** (`forward`): `forward_targets` and the
//!   `PeerInterests` trait.
//!
//! `SyncEngine::subscribe` / `subscribe_via` / `unsubscribe` /
//! `unsubscribe_via` publish per-pair `SUBSCRIBE_PREFIX` entries via
//! these helpers; `handle_local_store_event` dispatches inbound
//! subscription entries; the routing tick calls
//! `Routes::due_for_refresh` to republish expiring subscriptions; and
//! the `PeerHello` handler replays every live `BroadcastIntent` to a
//! newly-connected peer so reconnect re-establishes coverage without
//! waiting for the next refresh.

pub mod coverage;
pub mod forward;
pub mod naming;
pub mod policy;
pub mod routes;
pub mod types;

pub use coverage::covers;
pub use forward::{PeerInterests, forward_targets};
pub use naming::{
    LINKS_NAME, PROVIDER_TICK_NAME, SUBSCRIBE_PREFIX, decode_filter_hash_from_name,
    decode_provider_from_name, is_subscription_name, subscription_name,
};
pub use policy::{SubscriptionPolicy, relay_broad_filter};
pub use routes::{
    BroadcastIntent, FILTER_HASH_HEX_LEN, FilterHash, Outbound, OutboundKey, Routes, filter_hash,
};
pub use types::{LinkState, Neighbor, ProviderTick, SubscriptionEntry};
