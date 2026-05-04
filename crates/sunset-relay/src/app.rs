//! axum app + handlers for the relay's HTTP/WS endpoints.
//!
//! The app holds only `Send` state: the WS upgrade sender and the
//! engine-command sender. All engine reads go through `RelayCommand`.

use axum::Router;
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::{mpsc, oneshot};

use sunset_sync_ws_native::axum_integration::ws_handler;

use crate::bridge::RelayCommand;
use crate::render::{render_dashboard, render_identity};

#[derive(Clone)]
pub struct AppState {
    /// Sends already-upgraded axum WebSockets to the engine-side
    /// `WebSocketRawTransport::serving()` channel.
    pub ws_tx: mpsc::UnboundedSender<axum::extract::ws::WebSocket>,
    /// Sends commands (snapshot, identity) to the engine-side command pump.
    pub cmd_tx: mpsc::UnboundedSender<RelayCommand>,
}

pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/dashboard", get(dashboard_handler))
        .route("/", get(root_handler))
        .with_state(state)
}

async fn dashboard_handler(State(state): State<AppState>) -> Response {
    let (reply, rx) = oneshot::channel();
    if state.cmd_tx.send(RelayCommand::Snapshot { reply }).is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "engine unavailable\n").into_response();
    }
    let snap = match rx.await {
        Ok(s) => s,
        Err(_) => {
            return (StatusCode::SERVICE_UNAVAILABLE, "engine unavailable\n").into_response();
        }
    };
    let body = render_dashboard(&snap);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "text/plain; charset=utf-8".parse().unwrap(),
    );
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    (StatusCode::OK, headers, body).into_response()
}

/// Either a WebSocket upgrade (engine path) or the JSON identity descriptor
/// for browsers/clients that GET / without an Upgrade header.
///
/// Every JSON response — success AND the early-503 paths — sets
/// `Access-Control-Allow-Origin: *`. Without it, browsers from a
/// different origin (e.g. `https://sunset.chat` fetching from
/// `https://relay.sunset.chat`) CORS-block 5xx responses, so the
/// resolver upstream sees a generic network error instead of a clean
/// `status 503`. That difference is invisible to the supervisor's
/// retry logic but extremely confusing in browser console logs.
async fn root_handler(
    State(state): State<AppState>,
    upgrade: Option<WebSocketUpgrade>,
) -> Response {
    if let Some(ws) = upgrade {
        return ws_handler(ws, state.ws_tx).await;
    }

    fn cors_503(reason: &'static str) -> Response {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
        headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        (StatusCode::SERVICE_UNAVAILABLE, headers, reason).into_response()
    }

    let (reply, rx) = oneshot::channel();
    if state.cmd_tx.send(RelayCommand::Identity { reply }).is_err() {
        return cors_503("engine unavailable: cmd_tx closed\n");
    }
    let snap = match rx.await {
        Ok(s) => s,
        Err(_) => return cors_503("engine unavailable: reply dropped\n"),
    };
    let body = render_identity(&snap);
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        "application/json; charset=utf-8".parse().unwrap(),
    );
    headers.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*".parse().unwrap());
    (StatusCode::OK, headers, body).into_response()
}
