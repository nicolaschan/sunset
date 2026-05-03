//! WASM bundle: sunset-core + sunset-store-memory + sunset-sync +
//! sunset-noise + sunset-sync-ws-browser, exposed to JS as a `Client` class.
//!
//! See `docs/superpowers/specs/2026-04-27-sunset-web-e2e-design.md`.

#[cfg(target_arch = "wasm32")]
mod client;
#[cfg(target_arch = "wasm32")]
mod markdown;
#[cfg(target_arch = "wasm32")]
mod identity;
#[cfg(target_arch = "wasm32")]
mod members;
#[cfg(target_arch = "wasm32")]
mod messages;
#[cfg(target_arch = "wasm32")]
mod presence_publisher;
#[cfg(target_arch = "wasm32")]
mod reactions;
#[cfg(target_arch = "wasm32")]
mod relay_signaler;
#[cfg(target_arch = "wasm32")]
pub(crate) mod resolver_adapter;
#[cfg(target_arch = "wasm32")]
mod voice;

#[cfg(target_arch = "wasm32")]
pub use client::Client;
#[cfg(target_arch = "wasm32")]
pub use markdown::parse_markdown;
#[cfg(target_arch = "wasm32")]
pub use members::MemberJs;
#[cfg(target_arch = "wasm32")]
pub use messages::IncomingMessage;
#[cfg(target_arch = "wasm32")]
pub use relay_signaler::{RelaySignaler, signaling_filter};

#[cfg(not(target_arch = "wasm32"))]
pub struct Client;
#[cfg(not(target_arch = "wasm32"))]
pub struct IncomingMessage;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn __sunset_web_wasm_start() {
    let mut config = wasm_tracing::WasmLayerConfig::default();
    config.set_max_level(tracing::Level::INFO);
    // The Result is `Err` only if a global subscriber was already set,
    // which can't happen here: this function is the sole #[wasm_bindgen(start)]
    // entrypoint and runs exactly once per module load.
    let _ = wasm_tracing::set_as_global_default_with_config(config);
}
