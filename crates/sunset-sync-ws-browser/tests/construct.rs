//! Compile + construct check. Real WebSocket I/O is exercised in Plan E's
//! browser-side UI integration; this test only confirms the crate
//! compiles for the wasm32 target and the constructor produces a value
//! whose types fit the trait surface.

#![cfg(target_arch = "wasm32")]

use sunset_sync::RawTransport;
use sunset_sync_ws_browser::WebSocketRawTransport;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_node_experimental);

#[wasm_bindgen_test]
fn dial_only_constructs() {
    let t = WebSocketRawTransport::dial_only();
    let _: &dyn TraitMarker = &t;
}

trait TraitMarker {}
impl<T: RawTransport> TraitMarker for T {}
