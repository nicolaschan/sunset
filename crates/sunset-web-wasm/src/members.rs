//! Wasm-bindgen wrapper for `sunset_core::membership::Member`.
//!
//! The data model + reducers (presence bucketing, list derivation,
//! signature-debounce shape) live in `sunset_core::membership` so
//! native client surfaces (TUI, mod) share the same logic. This file
//! is just the JS-facing flavor: string fields and an `f64` heartbeat
//! sentinel that round-trips cleanly across `wasm-bindgen`.

use wasm_bindgen::prelude::*;

use sunset_core::membership::Member;

/// JS-exported per-member view consumed by the Gleam UI.
#[wasm_bindgen]
pub struct MemberJs {
    pub(crate) pubkey: Vec<u8>,
    pub(crate) presence: String,
    pub(crate) connection_mode: String,
    pub(crate) is_self: bool,
    /// Unix-ms timestamp of the last app-level presence heartbeat we
    /// observed for this peer. `None` for self (we don't track our own
    /// presence) and for any peer we've heard nothing from. The Gleam
    /// popover computes age = now_ms - last_heartbeat_ms.
    pub(crate) last_heartbeat_ms: Option<u64>,
    pub(crate) name: Option<String>,
}

impl From<&Member> for MemberJs {
    fn from(m: &Member) -> Self {
        MemberJs {
            pubkey: m.pubkey.clone(),
            presence: m.presence.as_str().to_owned(),
            connection_mode: m.connection_mode.as_str().to_owned(),
            is_self: m.is_self,
            last_heartbeat_ms: m.last_heartbeat_ms,
            name: m.name.clone(),
        }
    }
}

#[wasm_bindgen]
impl MemberJs {
    #[wasm_bindgen(getter)]
    pub fn pubkey(&self) -> Vec<u8> {
        self.pubkey.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn presence(&self) -> String {
        self.presence.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn connection_mode(&self) -> String {
        self.connection_mode.clone()
    }
    #[wasm_bindgen(getter)]
    pub fn is_self(&self) -> bool {
        self.is_self
    }
    /// Heartbeat timestamp as `f64` (JS Number), or `-1` for "no
    /// heartbeat observed" (i.e. self, or a peer we've heard nothing
    /// from). We expose `f64` rather than `Option<u64>` because
    /// wasm-bindgen serializes `Option<u64>` as `bigint | undefined`
    /// in JS — the BigInt half then doesn't mix with regular Number
    /// arithmetic on the Gleam side. `f64` round-trips unix-ms
    /// exactly out to ~285616 AD, which is plenty for our purposes.
    #[wasm_bindgen(getter)]
    pub fn last_heartbeat_ms(&self) -> f64 {
        match self.last_heartbeat_ms {
            Some(ms) => ms as f64,
            None => -1.0,
        }
    }
    /// Display name claimed by this peer in their most recent presence
    /// heartbeat. wasm-bindgen exposes Option<String> as `string |
    /// undefined` on the JS side. `undefined` ⇒ peer hasn't set a
    /// name; UI falls back to short_pubkey rendering.
    #[wasm_bindgen(getter)]
    pub fn name(&self) -> Option<String> {
        self.name.clone()
    }
}
