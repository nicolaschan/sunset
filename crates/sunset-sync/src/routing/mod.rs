//! Cooperative-relay routing layer.
//!
//! See `docs/superpowers/specs/2026-05-23-cooperative-relay-design.md`.
//!
//! This module is the substrate (wire types, naming, policy, pure
//! predicates). The receiver loop, provider loop, liveness, and
//! integration into the engine ship in follow-up plans.

pub mod types;

pub use types::{LinkState, Neighbor, ProviderTick, SubscriptionEntry};
