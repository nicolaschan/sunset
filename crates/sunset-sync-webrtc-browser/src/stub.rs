//! Native fallback. Compiled on non-wasm targets so the workspace builds
//! without wasm tooling. Calls return `Error::Transport`.

use std::rc::Rc;

use sunset_sync::{PeerId, Signaler};

pub struct WebRtcRawTransport {
    _signaler: Rc<dyn Signaler>,
    _local_peer: PeerId,
    _ice_urls: Vec<String>,
}

impl WebRtcRawTransport {
    pub fn new(signaler: Rc<dyn Signaler>, local_peer: PeerId, ice_urls: Vec<String>) -> Self {
        Self {
            _signaler: signaler,
            _local_peer: local_peer,
            _ice_urls: ice_urls,
        }
    }
}

pub struct WebRtcRawConnection;

sunset_sync::native_stub_impls!(
    transport = WebRtcRawTransport,
    connection = WebRtcRawConnection,
    crate_name = "sunset-sync-webrtc-browser",
);
