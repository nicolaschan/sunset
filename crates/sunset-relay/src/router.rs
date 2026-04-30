//! HTTP/WS multiplexer for the relay's single TCP listener. Peeks the
//! incoming request to route it: `/dashboard` → status page; WS upgrade
//! → forward the TcpStream to the WebSocketRawTransport via a channel;
//! anything else → 404.

use std::rc::Rc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use crate::error::Result;
use crate::status::{self, StatusContext};

/// Maximum size of the request prologue we'll peek before deciding how
/// to route. WebSocket upgrade requests are typically <1 KiB.
const PEEK_BYTES: usize = 4096;
/// How long we'll wait for the client to send enough bytes to make a
/// routing decision.
const PEEK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
enum Route {
    Dashboard,
    WebSocket,
    NotFound,
}

/// Run the dispatcher loop on `listener`. New TcpStreams are routed:
///   * `GET /dashboard` (any version) → render dashboard inline.
///   * WS upgrade → send to `ws_tx`.
///   * other → 404 + close.
///
/// Returns only on fatal listener error; caller spawns + aborts.
pub(crate) async fn dispatch(
    listener: TcpListener,
    ws_tx: mpsc::Sender<TcpStream>,
    status_ctx: Rc<StatusContext>,
) -> Result<()> {
    loop {
        let (tcp, _peer) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "relay accept failed");
                continue;
            }
        };
        let ws_tx = ws_tx.clone();
        let status_ctx = status_ctx.clone();
        tokio::task::spawn_local(async move {
            handle_connection(tcp, ws_tx, status_ctx).await;
        });
    }
}

async fn handle_connection(
    mut tcp: TcpStream,
    ws_tx: mpsc::Sender<TcpStream>,
    status_ctx: Rc<StatusContext>,
) {
    let route = match peek_route(&mut tcp).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!(error = %e, "router peek failed");
            return;
        }
    };
    match route {
        Route::Dashboard => {
            if let Err(e) = status::serve_dashboard(tcp, status_ctx).await {
                tracing::debug!(error = %e, "dashboard render failed");
            }
        }
        Route::WebSocket => {
            // Forward the (already-classified) TcpStream to the WS transport.
            // tokio_tungstenite::accept_async will re-read the upgrade headers
            // because we used peek (no bytes consumed).
            if ws_tx.send(tcp).await.is_err() {
                tracing::warn!("ws dispatch channel closed; dropping connection");
            }
        }
        Route::NotFound => {
            let body = b"404 Not Found\n";
            let head = format!(
                "HTTP/1.1 404 Not Found\r\n\
                 Content-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\
                 \r\n",
                body.len()
            );
            let _ = tcp.write_all(head.as_bytes()).await;
            let _ = tcp.write_all(body).await;
            let _ = tcp.shutdown().await;
        }
    }
}

/// Peek (do not consume) up to PEEK_BYTES from `tcp` until we see the
/// request prologue, then classify.
async fn peek_route(tcp: &mut TcpStream) -> std::io::Result<Route> {
    let mut buf = vec![0u8; PEEK_BYTES];
    // peek() is best-effort; loop until we have enough to decide or
    // hit the limit / timeout.
    let mut have = 0usize;
    let read_result = tokio::time::timeout(PEEK_TIMEOUT, async {
        loop {
            match tcp.peek(&mut buf[..]).await {
                Ok(0) => return Ok::<usize, std::io::Error>(have),
                Ok(n) => {
                    have = n;
                    if buf[..have].windows(4).any(|w| w == b"\r\n\r\n") {
                        return Ok(have);
                    }
                    if have >= buf.len() {
                        return Ok(have);
                    }
                    // Yield briefly so the kernel can buffer more bytes.
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(e) => return Err(e),
            }
        }
    })
    .await;
    let have = match read_result {
        Ok(Ok(n)) => n,
        Ok(Err(e)) => return Err(e),
        Err(_) => return Ok(Route::NotFound), // timeout — no request prologue
    };
    Ok(classify(&buf[..have]))
}

/// Inspect the peeked bytes. Look at the request line + headers.
fn classify(prologue: &[u8]) -> Route {
    // Find end of request line.
    let line_end = match prologue.windows(2).position(|w| w == b"\r\n") {
        Some(p) => p,
        None => return Route::NotFound,
    };
    let request_line = match std::str::from_utf8(&prologue[..line_end]) {
        Ok(s) => s,
        Err(_) => return Route::NotFound,
    };
    let mut parts = request_line.split_ascii_whitespace();
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    if method != "GET" {
        return Route::NotFound;
    }
    // Strip query string for the path comparison.
    let path_only = path.split('?').next().unwrap_or(path);
    if path_only == "/dashboard" || path_only.starts_with("/dashboard/") {
        return Route::Dashboard;
    }
    // Check headers for `Upgrade: websocket` (case-insensitive).
    let headers_end = prologue
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or(prologue.len());
    let headers = match std::str::from_utf8(&prologue[line_end + 2..headers_end]) {
        Ok(s) => s,
        Err(_) => return Route::NotFound,
    };
    let is_ws_upgrade = headers.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        lower.starts_with("upgrade:") && lower.contains("websocket")
    });
    if is_ws_upgrade {
        Route::WebSocket
    } else {
        Route::NotFound
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_dashboard() {
        let req = b"GET /dashboard HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(matches!(classify(req), Route::Dashboard));
    }

    #[test]
    fn classify_dashboard_subpath() {
        let req = b"GET /dashboard/style.css HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(matches!(classify(req), Route::Dashboard));
    }

    #[test]
    fn classify_dashboard_with_query() {
        let req = b"GET /dashboard?refresh=1 HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(matches!(classify(req), Route::Dashboard));
    }

    #[test]
    fn classify_ws_upgrade() {
        let req =
            b"GET / HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
        assert!(matches!(classify(req), Route::WebSocket));
    }

    #[test]
    fn classify_ws_case_insensitive() {
        let req = b"GET / HTTP/1.1\r\nupgrade: WebSocket\r\n\r\n";
        assert!(matches!(classify(req), Route::WebSocket));
    }

    #[test]
    fn classify_other_path_with_no_upgrade_is_404() {
        let req = b"GET /random HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(matches!(classify(req), Route::NotFound));
    }

    #[test]
    fn classify_post_is_404() {
        let req = b"POST /dashboard HTTP/1.1\r\nContent-Length: 0\r\n\r\n";
        assert!(matches!(classify(req), Route::NotFound));
    }
}
