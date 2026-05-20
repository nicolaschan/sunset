//! Native fallback. Compiled on non-wasm targets so the workspace builds
//! without wasm tooling. Calls return `Error::Transport`.

pub struct WebTransportRawTransport;

impl WebTransportRawTransport {
    pub fn dial_only() -> Self {
        Self
    }
}

pub struct WebTransportRawConnection;

sunset_sync::native_stub_impls!(
    transport = WebTransportRawTransport,
    connection = WebTransportRawConnection,
    crate_name = "sunset-sync-webtransport-browser",
);
