//! Real wasm32 implementation of `RawTransport` over `web_sys::WebSocket`.

use std::cell::RefCell;
use std::rc::Rc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::channel::mpsc::{self, UnboundedReceiver};
use futures::future::poll_fn;
use futures::task::Poll;
use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use web_sys::{BinaryType, CloseEvent, Event, MessageEvent, WebSocket};

use sunset_sync::{Error, PeerAddr, RawConnection, RawTransport, Result};

/// Browser WebSocket transport. Dial-only — browsers can't accept inbound.
pub struct WebSocketRawTransport;

impl WebSocketRawTransport {
    /// The only constructor.
    pub fn dial_only() -> Self {
        Self
    }
}

#[async_trait(?Send)]
impl RawTransport for WebSocketRawTransport {
    type Connection = WebSocketRawConnection;

    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
        let url = parse_addr_url(&addr)?;

        // Construct the WebSocket; throws on bad URL.
        let ws = WebSocket::new(&url).map_err(|e| Error::Transport(format!("ws new: {:?}", e)))?;
        ws.set_binary_type(BinaryType::Arraybuffer);

        // Channels: open, error (one-shot), message (continuous), close
        // (one-shot post-open detection — see recv_reliable).
        let (open_tx, mut open_rx) = mpsc::unbounded::<()>();
        let (err_tx, mut err_rx) = mpsc::unbounded::<String>();
        let (msg_tx, msg_rx) = mpsc::unbounded::<Bytes>();
        let (close_tx, close_rx) = mpsc::unbounded::<()>();

        // Construct the closed flag before closures so on_close can clone it.
        let closed: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));

        // on_open
        let on_open: Closure<dyn FnMut(Event)> = Closure::new({
            let open_tx = open_tx.clone();
            move |_: Event| {
                let _ = open_tx.unbounded_send(());
            }
        });
        ws.set_onopen(Some(on_open.as_ref().unchecked_ref()));

        // on_message
        let on_message: Closure<dyn FnMut(MessageEvent)> = Closure::new({
            let msg_tx = msg_tx.clone();
            move |event: MessageEvent| {
                let data = event.data();
                if let Ok(buffer) = data.dyn_into::<ArrayBuffer>() {
                    let array = Uint8Array::new(&buffer);
                    let mut bytes = vec![0u8; array.length() as usize];
                    array.copy_to(&mut bytes);
                    let _ = msg_tx.unbounded_send(Bytes::from(bytes));
                }
                // Non-binary messages are silently dropped — sunset-sync
                // only sends binary frames, so a text frame is a protocol
                // error.
            }
        });
        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        // on_error
        let on_error: Closure<dyn FnMut(Event)> = Closure::new({
            let err_tx = err_tx.clone();
            move |event: Event| {
                let _ = err_tx.unbounded_send(format!("ws error: {:?}", event));
            }
        });
        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        // on_close — also flips `closed` so peer-initiated close is observable.
        let closed_for_on_close = closed.clone();
        let on_close: Closure<dyn FnMut(CloseEvent)> = Closure::new({
            let close_tx = close_tx.clone();
            move |_: CloseEvent| {
                *closed_for_on_close.borrow_mut() = true;
                let _ = close_tx.unbounded_send(());
            }
        });
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        // Wrap the WS + closures in the connection struct *before* the
        // open wait, so the Drop impl handles cleanup uniformly across
        // every err-path below. Without this, dropping the closures
        // while the JS WebSocket still has live `.on*` handlers
        // pointing at them causes wasm-bindgen to panic
        // ("closure invoked recursively or after being dropped") the
        // first time the dying socket fires another close/error event.
        let conn = WebSocketRawConnection {
            ws,
            rx: RefCell::new(msg_rx),
            // close_rx is wrapped `Option` so the connect-time wait
            // below can `take()` it — owning it for the duration of
            // the `select!` rather than holding a `RefMut` across
            // `.await`, which would leak the borrow if the future is
            // dropped mid-wait. Restored to `Some(_)` on open success
            // so `recv_reliable` can poll it later.
            close_rx: RefCell::new(None),
            closed,
            _on_open: on_open,
            _on_message: on_message,
            _on_error: on_error,
            _on_close: on_close,
        };

        // Wait for open OR error OR close (whichever fires first).
        // Use the locally-owned `close_rx`; on open success it gets
        // moved into `conn.close_rx` for `recv_reliable`'s use.
        let mut close_rx = close_rx;
        futures::select! {
            maybe_open = open_rx.next() => {
                if maybe_open.is_none() {
                    return Err(Error::Transport("ws open channel closed before open".into()));
                }
            }
            maybe_err = err_rx.next() => {
                return Err(Error::Transport(
                    maybe_err.unwrap_or_else(|| "ws unknown error".into()),
                ));
            }
            _ = close_rx.next() => {
                return Err(Error::Transport("ws closed before open".into()));
            }
        }
        *conn.close_rx.borrow_mut() = Some(close_rx);

        Ok(conn)
    }

    async fn accept(&self) -> Result<Self::Connection> {
        // Browsers can't accept inbound. Return a never-completing future
        // per the trait's documented contract for dial-only transports.
        std::future::pending::<()>().await;
        unreachable!();
    }
}

/// Strip the `#x25519=...` fragment that the Noise wrapper above us
/// consumes; pass the rest to `WebSocket::new()`.
fn parse_addr_url(addr: &PeerAddr) -> Result<String> {
    let s = std::str::from_utf8(addr.as_bytes())
        .map_err(|e| Error::Transport(format!("addr not utf-8: {e}")))?;
    let no_frag = s.split('#').next().unwrap_or(s);
    Ok(no_frag.to_owned())
}

/// Browser WebSocket connection. Bridges the JS-callback model to an
/// async channel-based API compatible with `RawConnection`.
pub struct WebSocketRawConnection {
    ws: WebSocket,
    rx: RefCell<UnboundedReceiver<Bytes>>,
    /// Signaled by `on_close` so `recv_reliable` can return Err the
    /// moment the JS WebSocket reaches CLOSED, without waiting for
    /// the next outbound write to fail. Without this, disconnect
    /// detection is gated on the heartbeat tick (15s default), and
    /// even then can take an extra tick because sends to a CLOSING
    /// (not yet CLOSED) socket are silently dropped by the JS API.
    ///
    /// Wrapped in `Option` because `connect()` temporarily takes
    /// ownership during its open/err/close `select!` (so the
    /// connect-time wait doesn't hold a `RefMut` across `.await`)
    /// and puts it back on open success. After `connect()` returns
    /// `Ok`, this is always `Some(_)`.
    close_rx: RefCell<Option<UnboundedReceiver<()>>>,
    /// True once close() has been called locally or peer initiated a close.
    closed: Rc<RefCell<bool>>,

    // Hold JS-side closures alive while the WebSocket exists. Dropping
    // these while `ws` is still receiving callbacks would cause UB —
    // see `Drop` impl below, which detaches the handlers first.
    _on_open: Closure<dyn FnMut(Event)>,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_error: Closure<dyn FnMut(Event)>,
    _on_close: Closure<dyn FnMut(CloseEvent)>,
}

impl Drop for WebSocketRawConnection {
    fn drop(&mut self) {
        // Detach JS handlers BEFORE the Closure fields drop. A live
        // close/error event firing after the Closure is freed panics
        // wasm-bindgen with "closure invoked recursively or after
        // being dropped" — and once that throws, subsequent wasm
        // calls (including any reconnect attempt) trip the same
        // poisoned state. Clearing the .on* properties first means
        // any in-flight events have nowhere to go.
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onerror(None);
        self.ws.set_onclose(None);
        // Best-effort close — no-op if the socket already
        // CLOSING/CLOSED. We don't surface the error: nothing
        // useful to do in Drop.
        let _ = self.ws.close();
    }
}

#[async_trait(?Send)]
impl RawConnection for WebSocketRawConnection {
    async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
        if *self.closed.borrow() {
            return Err(Error::Transport("ws closed".into()));
        }
        self.ws
            .send_with_u8_array(&bytes)
            .map_err(|e| Error::Transport(format!("ws send: {:?}", e)))
    }

    async fn recv_reliable(&self) -> Result<Bytes> {
        // Poll close before the message stream so a peer-initiated
        // close (or local ws.close()) gets propagated to the engine
        // immediately on the on_close callback rather than waiting for
        // the next outbound write to fail. Both polls register wakers
        // for `cx`, so whichever fires first wakes this task.
        // RefCell borrows are held only within the poll closure and
        // released before any suspension point.
        poll_fn(|cx| {
            {
                let mut close_rx_opt = self.close_rx.borrow_mut();
                let close_rx = close_rx_opt
                    .as_mut()
                    .expect("close_rx is Some after connect()");
                if futures::Stream::poll_next(std::pin::Pin::new(close_rx), cx).is_ready() {
                    return Poll::Ready(Err(Error::Transport("ws closed".into())));
                }
            }
            let mut rx = self.rx.borrow_mut();
            match futures::Stream::poll_next(std::pin::Pin::new(&mut *rx), cx) {
                Poll::Ready(Some(bytes)) => Poll::Ready(Ok(bytes)),
                Poll::Ready(None) => Poll::Ready(Err(Error::Transport("ws closed".into()))),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }

    async fn send_unreliable(&self, _: Bytes) -> Result<()> {
        Err(Error::Transport(
            "websocket: unreliable channel unsupported".into(),
        ))
    }

    async fn recv_unreliable(&self) -> Result<Bytes> {
        Err(Error::Transport(
            "websocket: unreliable channel unsupported".into(),
        ))
    }

    async fn close(&self) -> Result<()> {
        if *self.closed.borrow() {
            return Ok(());
        }
        *self.closed.borrow_mut() = true;
        self.ws
            .close()
            .map_err(|e| Error::Transport(format!("ws close: {:?}", e)))
    }
}
