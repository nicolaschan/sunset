//! Native fallback. Compiled on non-wasm targets so the workspace builds
//! without wasm tooling. Calls return `Error::Transport`.

pub struct WebSocketRawTransport;

impl WebSocketRawTransport {
    pub fn dial_only() -> Self {
        Self
    }
}

pub struct WebSocketRawConnection;

sunset_sync::native_stub_impls!(
    transport = WebSocketRawTransport,
    connection = WebSocketRawConnection,
);
