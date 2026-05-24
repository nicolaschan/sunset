//! Cooperative-relay routing layer.
//!
//! See `docs/superpowers/specs/2026-05-23-cooperative-relay-design.md`.
//!
//! This module is the substrate (wire types, naming, policy, pure
//! predicates). The receiver loop, provider loop, liveness, and
//! integration into the engine ship in follow-up plans.

pub mod naming;
pub mod policy;
pub mod types;

pub use naming::{LINKS_NAME, PROVIDER_TICK_NAME, SUBSCRIBE_PREFIX, subscription_name};
pub use policy::SubscriptionPolicy;
pub use types::{LinkState, Neighbor, ProviderTick, SubscriptionEntry};
