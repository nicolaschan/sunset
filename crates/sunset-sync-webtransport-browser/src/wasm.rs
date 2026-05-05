//! Real wasm32 implementation of `RawTransport` over `web_sys::WebTransport`.
//!
//! Reliable channel: one persistent bidirectional stream opened during
//! `connect()`, framed with a 4-byte big-endian length prefix per
//! `SyncMessage`. Mirrors the native crate so the engine sees the same
//! semantics on every transport.
//!
//! Unreliable channel: WebTransport datagrams. Datagram payloads above
//! the user-agent's `maxDatagramSize` are silently truncated by some
//! browsers; we hard-cap at 1200 bytes (matches the native crate) and
//! return `Err` for oversize sends.

use std::cell::RefCell;
use std::rc::Rc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::channel::mpsc::{self, UnboundedReceiver};
use futures::future::poll_fn;
use futures::task::Poll;
use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    ReadableStreamDefaultReader, WebTransport, WebTransportBidirectionalStream, WebTransportHash,
    WebTransportOptions, WritableStreamDefaultWriter,
};

use sunset_sync::{Error, PeerAddr, RawConnection, RawTransport, Result};

const MAX_DATAGRAM_PAYLOAD: usize = 1200;
const MAX_RELIABLE_FRAME: usize = 16 * 1024 * 1024;

/// Browser WebTransport — dial-only (browsers can't accept inbound).
pub struct WebTransportRawTransport;

impl WebTransportRawTransport {
    pub fn dial_only() -> Self {
        Self
    }
}

#[async_trait(?Send)]
impl RawTransport for WebTransportRawTransport {
    type Connection = WebTransportRawConnection;

    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
        let parsed = parse_addr(&addr)?;

        // Build options: cert-sha256 hashes from the URL fragment turn
        // into `serverCertificateHashes`. Empty hashes → use the system
        // CA chain (production deployment).
        let options = WebTransportOptions::new();
        if !parsed.cert_hashes.is_empty() {
            let mut hashes: Vec<WebTransportHash> = Vec::with_capacity(parsed.cert_hashes.len());
            for h in &parsed.cert_hashes {
                let hash_obj = WebTransportHash::new();
                hash_obj.set_algorithm("sha-256");
                let view = Uint8Array::new_with_length(32);
                view.copy_from(h);
                hash_obj.set_value(&view);
                hashes.push(hash_obj);
            }
            options.set_server_certificate_hashes(hashes.as_slice());
        }

        let wt = WebTransport::new_with_options(parsed.https_url(), &options)
            .map_err(|e| Error::Transport(format!("WebTransport new: {e:?}")))?;

        // Wait for the .ready promise to resolve. If it rejects we
        // surface the error; if it resolves we proceed to open the
        // bidirectional reliable stream. The web-sys typed promise must
        // be cast to the untyped `js_sys::Promise` JsFuture accepts.
        let ready_typed = wt.ready();
        let ready: js_sys::Promise = ready_typed.unchecked_into();
        JsFuture::from(ready)
            .await
            .map_err(|e| Error::Transport(format!("WebTransport ready: {e:?}")))?;
        tracing::info!(
            url = %parsed.https_url(),
            "webtransport: session ready (browser)"
        );

        // Open the persistent bidi stream. Server-side mirrors with
        // `accept_bi`. Both sides must be set up before we hand back a
        // RawConnection — otherwise the first `send_reliable` lands on
        // a stream the server hasn't accepted yet.
        let bidi_promise: js_sys::Promise = wt.create_bidirectional_stream().unchecked_into();
        let bidi_js = JsFuture::from(bidi_promise)
            .await
            .map_err(|e| Error::Transport(format!("WebTransport open bidi: {e:?}")))?;
        let bidi: WebTransportBidirectionalStream = bidi_js
            .dyn_into()
            .map_err(|_| Error::Transport("WebTransport: bidi stream wrong type".into()))?;

        let writable = bidi.writable();
        let readable = bidi.readable();
        let bidi_writer = writable
            .get_writer()
            .map_err(|e| Error::Transport(format!("WebTransport bidi writer: {e:?}")))?;
        let readable_js: JsValue = readable.into();
        let bidi_reader_js = Reflect::get(&readable_js, &JsValue::from_str("getReader"))
            .and_then(|f| f.dyn_into::<js_sys::Function>())
            .and_then(|f| f.call0(&readable_js))
            .map_err(|e| Error::Transport(format!("WebTransport bidi reader: {e:?}")))?;
        let bidi_reader: ReadableStreamDefaultReader = bidi_reader_js
            .dyn_into()
            .map_err(|_| Error::Transport("WebTransport: bidi reader wrong type".into()))?;

        // Datagram readable + writable.
        let datagrams = wt.datagrams();
        let dgram_writable = datagrams.writable();
        let dgram_writer = dgram_writable
            .get_writer()
            .map_err(|e| Error::Transport(format!("WebTransport dgram writer: {e:?}")))?;
        let dgram_readable = datagrams.readable();
        let dgram_readable_js: JsValue = dgram_readable.into();
        let dgram_reader_js = Reflect::get(&dgram_readable_js, &JsValue::from_str("getReader"))
            .and_then(|f| f.dyn_into::<js_sys::Function>())
            .and_then(|f| f.call0(&dgram_readable_js))
            .map_err(|e| Error::Transport(format!("WebTransport dgram reader: {e:?}")))?;
        let dgram_reader: ReadableStreamDefaultReader = dgram_reader_js
            .dyn_into()
            .map_err(|_| Error::Transport("WebTransport: dgram reader wrong type".into()))?;

        // Close watcher: the WT `closed` promise resolves when the
        // session shuts down (either side). We pipe a one-shot mpsc so
        // `recv_*` can short-circuit on close, mirroring the
        // ws-browser crate's discipline.
        let (close_tx, close_rx) = mpsc::unbounded::<()>();
        let closed_flag = Rc::new(RefCell::new(false));
        // The typed Promise<WebTransportCloseInfo> casts cleanly to
        // js_sys::Promise — both are #[repr(transparent)] over JsValue
        // at the wasm-bindgen layer.
        let closed_promise: js_sys::Promise = wt.closed().unchecked_into();
        {
            let close_tx = close_tx.clone();
            let closed_flag = closed_flag.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let _ = JsFuture::from(closed_promise).await;
                *closed_flag.borrow_mut() = true;
                let _ = close_tx.unbounded_send(());
            });
        }

        // Reliable read pump: getReader().read() yields chunks of the
        // bidi readable stream. We feed an unbounded channel of Bytes
        // so `recv_reliable` can poll a length-prefix-framed reader.
        let (read_tx, read_rx) = mpsc::unbounded::<Bytes>();
        {
            let reader = bidi_reader.clone();
            let close_tx = close_tx.clone();
            wasm_bindgen_futures::spawn_local(async move {
                loop {
                    let result = match JsFuture::from(reader.read()).await {
                        Ok(v) => v,
                        Err(_) => {
                            let _ = close_tx.unbounded_send(());
                            break;
                        }
                    };
                    let done = Reflect::get(&result, &JsValue::from_str("done"))
                        .ok()
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if done {
                        let _ = close_tx.unbounded_send(());
                        break;
                    }
                    let value = match Reflect::get(&result, &JsValue::from_str("value")) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if let Ok(arr) = value.dyn_into::<Uint8Array>() {
                        let mut buf = vec![0u8; arr.length() as usize];
                        arr.copy_to(&mut buf);
                        if read_tx.unbounded_send(Bytes::from(buf)).is_err() {
                            break;
                        }
                    }
                }
            });
        }

        // Datagram read pump.
        let (dg_read_tx, dg_read_rx) = mpsc::unbounded::<Bytes>();
        {
            let reader = dgram_reader.clone();
            wasm_bindgen_futures::spawn_local(async move {
                loop {
                    let result = match JsFuture::from(reader.read()).await {
                        Ok(v) => v,
                        Err(_) => break,
                    };
                    let done = Reflect::get(&result, &JsValue::from_str("done"))
                        .ok()
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if done {
                        break;
                    }
                    let value = match Reflect::get(&result, &JsValue::from_str("value")) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if let Ok(arr) = value.dyn_into::<Uint8Array>() {
                        let mut buf = vec![0u8; arr.length() as usize];
                        arr.copy_to(&mut buf);
                        if dg_read_tx.unbounded_send(Bytes::from(buf)).is_err() {
                            break;
                        }
                    }
                }
            });
        }

        Ok(WebTransportRawConnection {
            wt,
            bidi_writer,
            dgram_writer,
            read_buf: RefCell::new(BytesReadBuf {
                inbox: read_rx,
                buffer: Vec::new(),
            }),
            dg_read_rx: RefCell::new(dg_read_rx),
            close_rx: RefCell::new(Some(close_rx)),
            closed: closed_flag,
        })
    }

    async fn accept(&self) -> Result<Self::Connection> {
        std::future::pending::<()>().await;
        unreachable!();
    }
}

pub struct WebTransportRawConnection {
    /// Held alive so the WT session stays open. Drop tears the session
    /// down, which the close-watcher spawn_local task observes and
    /// short-circuits any in-flight `recv_*` via `close_rx`.
    wt: WebTransport,
    bidi_writer: WritableStreamDefaultWriter,
    dgram_writer: WritableStreamDefaultWriter,
    read_buf: RefCell<BytesReadBuf>,
    dg_read_rx: RefCell<UnboundedReceiver<Bytes>>,
    /// Wrapped `Option` so `recv_reliable` can take ownership for the
    /// duration of a poll without holding a `RefMut` across an await
    /// suspension point. Mirrors `sunset-sync-ws-browser`.
    close_rx: RefCell<Option<UnboundedReceiver<()>>>,
    closed: Rc<RefCell<bool>>,
}

/// Read-side scratch space: an mpsc receiver of Bytes chunks (the WT
/// stream may chop bytes up arbitrarily) plus a contiguous buffer we
/// consume from to satisfy length-prefix-framed reads.
struct BytesReadBuf {
    inbox: UnboundedReceiver<Bytes>,
    buffer: Vec<u8>,
}

impl WebTransportRawConnection {
    /// Pull `n` bytes off the read buffer, blocking until enough have
    /// arrived or the stream closes. Mirrors `RecvStream::read_exact`
    /// in the native crate.
    async fn read_exact(&self, n: usize) -> Result<Vec<u8>> {
        loop {
            {
                let mut rb = self.read_buf.borrow_mut();
                if rb.buffer.len() >= n {
                    let out = rb.buffer.drain(..n).collect();
                    return Ok(out);
                }
            }
            // Need more bytes. Poll the inbox while watching close.
            let chunk = poll_fn(|cx| {
                {
                    let mut close_rx_opt = self.close_rx.borrow_mut();
                    if let Some(close_rx) = close_rx_opt.as_mut()
                        && futures::Stream::poll_next(std::pin::Pin::new(close_rx), cx).is_ready()
                    {
                        return Poll::Ready(Err(Error::Transport("wt closed".into())));
                    }
                }
                let mut rb = self.read_buf.borrow_mut();
                match futures::Stream::poll_next(std::pin::Pin::new(&mut rb.inbox), cx) {
                    Poll::Ready(Some(b)) => Poll::Ready(Ok(b)),
                    Poll::Ready(None) => Poll::Ready(Err(Error::Transport("wt closed".into()))),
                    Poll::Pending => Poll::Pending,
                }
            })
            .await?;
            self.read_buf.borrow_mut().buffer.extend_from_slice(&chunk);
        }
    }
}

#[async_trait(?Send)]
impl RawConnection for WebTransportRawConnection {
    async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
        if *self.closed.borrow() {
            return Err(Error::Transport("wt closed".into()));
        }
        if bytes.len() > MAX_RELIABLE_FRAME {
            return Err(Error::Transport(format!(
                "wt send_reliable: frame too large ({} > {MAX_RELIABLE_FRAME})",
                bytes.len()
            )));
        }
        let len = u32::try_from(bytes.len())
            .map_err(|_| Error::Transport("wt send_reliable: len > u32::MAX".into()))?;
        let mut framed = Vec::with_capacity(4 + bytes.len());
        framed.extend_from_slice(&len.to_be_bytes());
        framed.extend_from_slice(&bytes);
        let chunk = Uint8Array::new_with_length(framed.len() as u32);
        chunk.copy_from(&framed);
        let promise = self.bidi_writer.write_with_chunk(&chunk);
        JsFuture::from(promise)
            .await
            .map_err(|e| Error::Transport(format!("wt send_reliable: {e:?}")))?;
        Ok(())
    }

    async fn recv_reliable(&self) -> Result<Bytes> {
        let len_buf = self.read_exact(4).await?;
        let len = u32::from_be_bytes([len_buf[0], len_buf[1], len_buf[2], len_buf[3]]) as usize;
        if len > MAX_RELIABLE_FRAME {
            return Err(Error::Transport(format!(
                "wt recv_reliable: frame too large ({len} > {MAX_RELIABLE_FRAME})"
            )));
        }
        let body = self.read_exact(len).await?;
        Ok(Bytes::from(body))
    }

    async fn send_unreliable(&self, bytes: Bytes) -> Result<()> {
        if *self.closed.borrow() {
            return Err(Error::Transport("wt closed".into()));
        }
        if bytes.len() > MAX_DATAGRAM_PAYLOAD {
            return Err(Error::Transport(format!(
                "wt send_unreliable: payload too large ({} > {MAX_DATAGRAM_PAYLOAD})",
                bytes.len()
            )));
        }
        let chunk = Uint8Array::new_with_length(bytes.len() as u32);
        chunk.copy_from(&bytes);
        let promise = self.dgram_writer.write_with_chunk(&chunk);
        JsFuture::from(promise)
            .await
            .map_err(|e| Error::Transport(format!("wt send_unreliable: {e:?}")))?;
        Ok(())
    }

    async fn recv_unreliable(&self) -> Result<Bytes> {
        poll_fn(|cx| {
            {
                let mut close_rx_opt = self.close_rx.borrow_mut();
                if let Some(close_rx) = close_rx_opt.as_mut()
                    && futures::Stream::poll_next(std::pin::Pin::new(close_rx), cx).is_ready()
                {
                    return Poll::Ready(Err(Error::Transport("wt closed".into())));
                }
            }
            let mut rx = self.dg_read_rx.borrow_mut();
            match futures::Stream::poll_next(std::pin::Pin::new(&mut *rx), cx) {
                Poll::Ready(Some(b)) => Poll::Ready(Ok(b)),
                Poll::Ready(None) => Poll::Ready(Err(Error::Transport("wt closed".into()))),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }

    async fn close(&self) -> Result<()> {
        if *self.closed.borrow() {
            return Ok(());
        }
        *self.closed.borrow_mut() = true;
        // Best-effort: close the WT session; the close-watcher
        // spawn_local task observes the resulting `closed` promise
        // resolution and pumps `close_rx` so any in-flight `recv_*`
        // unblocks with a Transport error.
        self.wt.close();
        Ok(())
    }
}

// --- helpers ---

struct ParsedBrowserAddr {
    https_url: String,
    cert_hashes: Vec<[u8; 32]>,
}

impl ParsedBrowserAddr {
    fn https_url(&self) -> &str {
        &self.https_url
    }
}

fn parse_addr(addr: &PeerAddr) -> Result<ParsedBrowserAddr> {
    let s = std::str::from_utf8(addr.as_bytes())
        .map_err(|e| Error::Transport(format!("wt addr not utf-8: {e}")))?;
    let (head, fragment) = match s.split_once('#') {
        Some((h, f)) => (h, Some(f)),
        None => (s, None),
    };
    let authority = if let Some(rest) = head.strip_prefix("wt://") {
        rest
    } else if let Some(rest) = head.strip_prefix("wts://") {
        rest
    } else {
        return Err(Error::Transport(format!(
            "wt addr: unsupported scheme in {head}"
        )));
    };
    if authority.is_empty() {
        return Err(Error::Transport(format!("wt addr: empty authority in {s}")));
    }
    let mut cert_hashes = Vec::new();
    if let Some(fragment) = fragment {
        for part in fragment.split('&') {
            if let Some(hex) = part.strip_prefix("cert-sha256=") {
                if hex.len() != 64 {
                    return Err(Error::Transport(format!(
                        "wt addr: cert-sha256 not 64 hex chars (got {})",
                        hex.len()
                    )));
                }
                let mut bytes = [0u8; 32];
                for (i, b) in bytes.iter_mut().enumerate() {
                    let pair = &hex.as_bytes()[i * 2..i * 2 + 2];
                    let pair_str = std::str::from_utf8(pair)
                        .map_err(|_| Error::Transport("wt addr: cert-sha256 non-utf8".into()))?;
                    *b = u8::from_str_radix(pair_str, 16).map_err(|e| {
                        Error::Transport(format!("wt addr: cert-sha256 bad hex: {e}"))
                    })?;
                }
                cert_hashes.push(bytes);
            }
        }
    }
    Ok(ParsedBrowserAddr {
        https_url: format!("https://{authority}"),
        cert_hashes,
    })
}
