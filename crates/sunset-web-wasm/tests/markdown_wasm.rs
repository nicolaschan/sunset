//! Round-trip `parse_markdown` through wasm-bindgen so a `serde` derive
//! drift in the AST shape is caught at CI time, not at first user load.

#![cfg(target_arch = "wasm32")]

use wasm_bindgen_test::wasm_bindgen_test;

wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_node_experimental);

#[wasm_bindgen_test]
fn parse_markdown_returns_some_value() {
    let value = sunset_web_wasm::parse_markdown("hello");
    assert!(!value.is_undefined());
    assert!(!value.is_null());
}

#[wasm_bindgen_test]
fn parse_markdown_round_trips_bold_text() {
    let value = sunset_web_wasm::parse_markdown("**hi**");
    let json = js_sys::JSON::stringify(&value)
        .expect("stringify")
        .as_string()
        .expect("stringify result");
    assert!(json.contains("\"Bold\""), "expected Bold variant in JSON, got: {json}");
    assert!(json.contains("\"hi\""), "expected payload, got: {json}");
}
