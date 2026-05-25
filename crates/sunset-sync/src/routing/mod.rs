//! Cooperative-relay routing layer.
//!
//! See `docs/superpowers/specs/2026-05-23-cooperative-relay-design.md`
//! and `docs/superpowers/specs/2026-05-24-cooperative-relay-phase2-design.md`.
//!
//! This module is the substrate (wire types, naming, policy, pure
//! predicates). The receiver loop, provider loop, liveness, and
//! integration into the engine ship in follow-up plans.
//!
//! Phase 2 retired the legacy single-key subscribe path
//! (`_sunset-sync/subscribe` with no suffix); the engine now uses the
//! per-pair `naming::SUBSCRIBE_PREFIX` (`_sunset-sync/subscribe/<filter-hash>/<provider>`)
//! keys exclusively.

pub mod coverage;
pub mod forward;
pub mod naming;
pub mod policy;
pub mod routes;
pub mod types;

pub use coverage::covers;
pub use forward::forward_targets;
pub use naming::{
    LINKS_NAME, PROVIDER_TICK_NAME, SUBSCRIBE_PREFIX, decode_filter_hash_from_name,
    is_subscription_name, subscription_name,
};
pub use policy::{SubscriptionPolicy, relay_broad_filter};
pub use routes::{
    BroadcastIntent, FILTER_HASH_HEX_LEN, FilterHash, Outbound, OutboundKey, Routes, filter_hash,
};
pub use types::{LinkState, Neighbor, ProviderTick, SubscriptionEntry};
