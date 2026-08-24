//! axum app + handlers for the relay's HTTP/WS endpoints.
//!
//! The app holds only `Send` state: the WS upgrade sender and the
//! identity-request sender. The engine itself is `?Send`, so every read
//! of it goes through the LocalSet-side identity pump.

use axum::Router;
use axum::extract::{State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tokio::sync::{mpsc, oneshot};

use sunset_sync_ws_native::axum_integration::ws_handler;

use crate::snapshot::IdentitySnapshot;

/// One in-flight descriptor request: the engine-side pump answers by
/// sending a freshly built snapshot back down this channel.
pub type IdentityRequest = oneshot::Sender<IdentitySnapshot>;

#[derive(Clone)]
pub struct AppState {
    /// Sends already-upgraded axum WebSockets to the engine-side
    /// `WebSocketRawTransport::serving()` channel.
    pub ws_tx: mpsc::UnboundedSender<axum::extract::ws::WebSocket>,
    /// Asks the engine-side pump for a fresh identity snapshot.
    pub identity_tx: mpsc::UnboundedSender<IdentityRequest>,
}

pub fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/", get(root_handler))
        .with_state(state)
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
    if state.identity_tx.send(reply).is_err() {
        return cors_503("engine unavailable: identity channel closed\n");
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

/// JSON identity. Hex-only field values, no escaping needed.
///
/// The optional `webtransport_cert_sha256` field appears only when the
/// relay successfully bound a UDP listener and generated its self-signed
/// WT cert. Old clients (which don't know about the field) keep
/// working off `address` (the legacy WS URL). Newer clients use this
/// hash to pin the relay's WT cert in `serverCertificateHashes`, and
/// build the actual WT URL themselves from the hostname they used to
/// reach the descriptor — see
/// `sunset_relay_resolver::Resolver::resolve_with_fallback`.
fn render_identity(snap: &IdentitySnapshot) -> String {
    let mut out = String::from("{");
    out.push_str(&format!(
        "\"ed25519\":\"{}\",",
        hex::encode(snap.ed25519_public)
    ));
    out.push_str(&format!(
        "\"x25519\":\"{}\",",
        hex::encode(snap.x25519_public)
    ));
    out.push_str(&format!("\"address\":\"{}\"", snap.dial_url));
    if let Some(cert_hex) = &snap.webtransport_cert_sha256 {
        out.push_str(&format!(",\"webtransport_cert_sha256\":\"{cert_hex}\""));
    }
    out.push_str(&format!(
        ",\"ephemeral_forwarded\":{}",
        snap.ephemeral_forwarded
    ));
    out.push_str("}\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_json_has_expected_shape() {
        let snap = IdentitySnapshot {
            ed25519_public: [0xab; 32],
            x25519_public: [0xcd; 32],
            dial_url: "ws://relay.example:8443".into(),
            webtransport_cert_sha256: None,
            ephemeral_forwarded: 0,
        };
        let json = render_identity(&snap);
        assert_eq!(
            json,
            "{\"ed25519\":\"abababababababababababababababababababababababababababababababab\",\
             \"x25519\":\"cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd\",\
             \"address\":\"ws://relay.example:8443\",\
             \"ephemeral_forwarded\":0}\n"
        );
    }

    #[test]
    fn identity_json_carries_ephemeral_forwarded_count() {
        let snap = IdentitySnapshot {
            ed25519_public: [0; 32],
            x25519_public: [0; 32],
            dial_url: "ws://relay.example:8443".into(),
            webtransport_cert_sha256: None,
            ephemeral_forwarded: 42,
        };
        let json = render_identity(&snap);
        assert!(
            json.contains("\"ephemeral_forwarded\":42"),
            "forward counter missing/wrong in: {json}"
        );
    }

    #[test]
    fn identity_json_includes_webtransport_cert_when_present() {
        let cert_hex = "ee".repeat(32);
        let snap = IdentitySnapshot {
            ed25519_public: [0xab; 32],
            x25519_public: [0xcd; 32],
            dial_url: "ws://relay.example:8443".into(),
            webtransport_cert_sha256: Some(cert_hex.clone()),
            ephemeral_forwarded: 0,
        };
        let json = render_identity(&snap);
        assert!(
            json.contains(&format!("\"webtransport_cert_sha256\":\"{cert_hex}\"")),
            "missing wt cert field in: {json}"
        );
        // Crucially: the JSON must NOT carry a fully-formed URL — the
        // descriptor's job is identity material only; the resolver
        // builds the URL from user-typed authority.
        assert!(
            !json.contains("webtransport_address"),
            "descriptor must not ship a URL, only the cert hash: {json}"
        );
    }
}
