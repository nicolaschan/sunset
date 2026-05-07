//! sunset-cli: native ratatui chat client.
//!
//! See `docs/superpowers/specs/2026-05-06-sunset-cli-design.md`.
//! v1 ships chat / rooms / peers / relay management. Voice is
//! deferred — see the spec's "Out of scope" section.

pub mod build;
pub mod client;
pub mod command;
pub mod dispatch;
pub mod identity;
pub mod resolver_adapter;
pub mod ui;
pub mod view;
