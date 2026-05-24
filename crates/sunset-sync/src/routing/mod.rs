//! Cooperative-relay routing layer.
//!
//! See `docs/superpowers/specs/2026-05-23-cooperative-relay-design.md`
//! and `docs/superpowers/specs/2026-05-24-cooperative-relay-phase2-design.md`.
//!
//! This module is the substrate (wire types, naming, policy, pure
//! predicates). The receiver loop, provider loop, liveness, and
//! integration into the engine ship in follow-up plans.
//!
//! Note: the legacy single-key subscribe path at
//! `crate::reserved::SUBSCRIBE_NAME` (`_sunset-sync/subscribe`, no
//! suffix) is still live and used by the existing engine. It will be
//! retired when the receiver loop migrates call sites to the per-pair
//! `naming::SUBSCRIBE_PREFIX` (`_sunset-sync/subscribe/...`) keys
//! introduced here. The two key spaces are disjoint as exact names.

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
    subscription_name,
};
pub use policy::SubscriptionPolicy;
pub use routes::{BroadcastIntent, FilterHash, Outbound, OutboundKey, Routes, filter_hash};
pub use types::{LinkState, Neighbor, ProviderTick, SubscriptionEntry};
