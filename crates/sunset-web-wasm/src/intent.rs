//! JS-facing intent snapshot.

use wasm_bindgen::prelude::*;

use sunset_sync::{IntentSnapshot, IntentState, TransportKind};

/// Maps `sunset_sync::IntentSnapshot` into a JS-friendly object.
/// `IntentId` is `u64` in Rust → `BigInt` in JS via wasm-bindgen, so
/// we narrow to `f64` (safe up to 2^53; the supervisor's monotonic
/// counter never gets near that in any realistic session).
#[wasm_bindgen]
pub struct IntentSnapshotJs {
    pub id: f64,
    #[wasm_bindgen(getter_with_clone)]
    pub state: String,
    #[wasm_bindgen(getter_with_clone)]
    pub label: String,
    #[wasm_bindgen(getter_with_clone)]
    pub peer_pubkey: Option<Vec<u8>>,
    #[wasm_bindgen(getter_with_clone)]
    pub kind: Option<String>,
    pub attempt: u32,
}

impl From<&IntentSnapshot> for IntentSnapshotJs {
    fn from(s: &IntentSnapshot) -> Self {
        Self {
            id: s.id as f64,
            state: match s.state {
                IntentState::Connecting => "connecting",
                IntentState::Connected => "connected",
                IntentState::Backoff => "backoff",
                IntentState::Cancelled => "cancelled",
            }
            .into(),
            label: s.label.clone(),
            peer_pubkey: s.peer_id.as_ref().map(|p| p.0.as_bytes().to_vec()),
            kind: s.kind.map(|k| match k {
                TransportKind::Primary => "primary".to_owned(),
                TransportKind::Secondary => "secondary".to_owned(),
                TransportKind::Unknown => "unknown".to_owned(),
            }),
            attempt: s.attempt,
        }
    }
}
