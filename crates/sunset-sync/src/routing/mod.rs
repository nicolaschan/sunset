//! Cooperative-relay routing layer.
//!
//! See `docs/superpowers/specs/2026-05-23-cooperative-relay-design.md`.
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
pub mod naming;
pub mod policy;
pub mod types;

pub use coverage::covers;
pub use naming::{LINKS_NAME, PROVIDER_TICK_NAME, SUBSCRIBE_PREFIX, subscription_name};
pub use policy::SubscriptionPolicy;
pub use types::{LinkState, Neighbor, ProviderTick, SubscriptionEntry};
