//! axum 0.7 integration for `sunset-sync-ws-native`.
//!
//! Behind the optional `axum` feature. Provides a WebSocket upgrade
//! handler that pushes already-upgraded sockets onto the channel that
//! `WebSocketRawTransport::serving()` drains.

use axum::extract::WebSocketUpgrade;
use axum::response::Response;
use tokio::sync::mpsc::UnboundedSender;

/// Convert an inbound axum WebSocket upgrade request into an upgraded
/// socket pushed onto `tx`. Use as the body of an axum route handler:
///
/// ```ignore
/// let (raw, ws_tx) = WebSocketRawTransport::serving();
/// let app = axum::Router::new().route(
///     "/",
///     axum::routing::get(move |ws| ws_handler(ws, ws_tx.clone())),
/// );
/// ```
///
/// The returned `Response` is axum's standard 101 Switching Protocols
/// answer; the upgrade itself completes inside axum's per-request task,
/// so slow upgrades don't block other requests.
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    tx: UnboundedSender<axum::extract::ws::WebSocket>,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        // Best-effort send. If the receiver is gone, the relay is shutting
        // down; the upgraded socket will close on drop.
        let _ = tx.send(socket);
    })
}
