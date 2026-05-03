//! Compile + construct check for the JS bridge. Real e2e is the Playwright
//! test in web/e2e/two_browser_chat.spec.js (Task 11).

#![cfg(target_arch = "wasm32")]

use sunset_web_wasm::Client;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_node_experimental);

#[wasm_bindgen_test]
fn client_constructs() {
    let seed = [42u8; 32];
    let client = Client::new(&seed).expect("Client::new");
    let pk = client.public_key();
    assert_eq!(pk.len(), 32);
    let status = client.relay_status();
    assert_eq!(status, "disconnected");
}
