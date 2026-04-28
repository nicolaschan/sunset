//! Compile + construct check. Real WebRTC I/O is exercised by the
//! Playwright kill-relay e2e test; this test only confirms the crate
//! compiles for the wasm32 target and the constructor produces a value
//! whose types fit the `RawTransport` trait surface.

#![cfg(target_arch = "wasm32")]

use std::rc::Rc;

use async_trait::async_trait;
use bytes::Bytes;
use sunset_store::VerifyingKey;
use sunset_sync::{PeerId, RawTransport, Result, SignalMessage, Signaler};
use sunset_sync_webrtc_browser::WebRtcRawTransport;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_node_experimental);

struct StubSignaler;

#[async_trait(?Send)]
impl Signaler for StubSignaler {
    async fn send(&self, _: SignalMessage) -> Result<()> {
        Ok(())
    }
    async fn recv(&self) -> Result<SignalMessage> {
        std::future::pending::<()>().await;
        unreachable!()
    }
}

#[wasm_bindgen_test]
fn webrtc_transport_constructs() {
    let signaler: Rc<dyn Signaler> = Rc::new(StubSignaler);
    let local = PeerId(VerifyingKey::new(Bytes::from_static(&[1u8; 32])));
    let t = WebRtcRawTransport::new(signaler, local, vec!["stun:stun.l.google.com:19302".into()]);
    let _: &dyn TraitMarker = &t;
}

trait TraitMarker {}
impl<T: RawTransport> TraitMarker for T {}
