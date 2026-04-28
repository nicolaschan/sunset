//! Browser WebRTC `RawTransport` implementation.
//!
//! Bytes pipe only — pair with `sunset_noise::NoiseTransport` for the
//! authenticated encrypted layer. Out-of-band SDP/ICE exchange flows
//! through a `Signaler` (typically a `RelaySignaler` over the existing
//! sunset-sync engine, with Noise_KK PFS encryption applied at that
//! layer).
//!
//! A single shared dispatcher task (started lazily on the first
//! `connect()` or `accept()` call) drains `signaler.recv()` and routes
//! each incoming `WebRtcSignalKind` either onto the inbound `Offer`
//! queue or onto the per-peer queue used by an in-progress handshake.

use std::cell::RefCell;
use std::collections::HashMap;
use std::pin::Pin;
use std::rc::Rc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::FutureExt;
use futures::channel::{mpsc, oneshot};
use futures::future::poll_fn;
use futures::stream::{Stream, StreamExt};
use futures::task::Poll;
use js_sys::{ArrayBuffer, Reflect, Uint8Array};
use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    MessageEvent, RtcConfiguration, RtcDataChannel, RtcDataChannelEvent, RtcDataChannelInit,
    RtcDataChannelType, RtcIceCandidate, RtcIceCandidateInit, RtcIceServer, RtcPeerConnection,
    RtcPeerConnectionIceEvent, RtcSdpType, RtcSessionDescriptionInit,
};

use sunset_store::VerifyingKey;
use sunset_sync::{
    Error, PeerAddr, PeerId, RawConnection, RawTransport, Result, SignalMessage, Signaler,
};

/// Wire shape for one signaling message between two peers. Postcard-encoded
/// inside `SignalMessage::payload`; the `Signaler` (e.g. `RelaySignaler`)
/// is responsible for the outer routing + any encryption (Noise_KK PFS).
#[derive(Serialize, Deserialize)]
enum WebRtcSignalKind {
    Offer(String),
    Answer(String),
    /// JSON-stringified RTCIceCandidate (per `candidate.toJSON()`).
    IceCandidate(String),
}

pub struct WebRtcRawTransport {
    signaler: Rc<dyn Signaler>,
    local_peer: PeerId,
    ice_urls: Vec<String>,
    inner: Rc<RefCell<Inner>>,
}

struct Inner {
    dispatcher_started: bool,
    /// In-progress handshakes' inbound queues, keyed by remote peer.
    /// Connect-side registers before sending Offer; accept-side registers
    /// after receiving Offer.
    per_peer: HashMap<PeerId, mpsc::UnboundedSender<WebRtcSignalKind>>,
    /// Drained by `accept()`. Each entry is (from_peer, offer_sdp).
    offers_tx: mpsc::UnboundedSender<(PeerId, String)>,
    offers_rx: Option<mpsc::UnboundedReceiver<(PeerId, String)>>,
}

impl WebRtcRawTransport {
    /// `ice_urls` should typically contain at least one STUN server,
    /// e.g. `["stun:stun.l.google.com:19302".into()]`.
    pub fn new(signaler: Rc<dyn Signaler>, local_peer: PeerId, ice_urls: Vec<String>) -> Self {
        let (offers_tx, offers_rx) = mpsc::unbounded::<(PeerId, String)>();
        Self {
            signaler,
            local_peer,
            ice_urls,
            inner: Rc::new(RefCell::new(Inner {
                dispatcher_started: false,
                per_peer: HashMap::new(),
                offers_tx,
                offers_rx: Some(offers_rx),
            })),
        }
    }

    /// Start the shared `signaler.recv()` drain task on first use.
    fn ensure_dispatcher(&self) {
        let mut inner = self.inner.borrow_mut();
        if inner.dispatcher_started {
            return;
        }
        inner.dispatcher_started = true;
        let signaler = self.signaler.clone();
        let inner_ref = self.inner.clone();
        spawn_local(async move {
            loop {
                let msg = match signaler.recv().await {
                    Ok(m) => m,
                    Err(_) => break,
                };
                let kind: WebRtcSignalKind = match postcard::from_bytes(&msg.payload) {
                    Ok(k) => k,
                    Err(_) => continue,
                };
                match kind {
                    WebRtcSignalKind::Offer(sdp) => {
                        let tx = inner_ref.borrow().offers_tx.clone();
                        let _ = tx.unbounded_send((msg.from, sdp));
                    }
                    other => {
                        let from = msg.from;
                        let target = inner_ref.borrow().per_peer.get(&from).cloned();
                        if let Some(tx) = target {
                            let _ = tx.unbounded_send(other);
                        }
                    }
                }
            }
        });
    }

    fn register_peer(&self, remote: PeerId) -> mpsc::UnboundedReceiver<WebRtcSignalKind> {
        let (tx, rx) = mpsc::unbounded::<WebRtcSignalKind>();
        self.inner.borrow_mut().per_peer.insert(remote, tx);
        rx
    }

    fn unregister_peer(&self, remote: &PeerId) {
        self.inner.borrow_mut().per_peer.remove(remote);
    }
}

#[async_trait(?Send)]
impl RawTransport for WebRtcRawTransport {
    type Connection = WebRtcRawConnection;

    async fn connect(&self, addr: PeerAddr) -> Result<Self::Connection> {
        self.ensure_dispatcher();

        let remote_peer = parse_addr_peer_id(&addr)?;
        let mut peer_in_rx = self.register_peer(remote_peer.clone());

        let pc = build_peer_connection(&self.ice_urls)?;
        let dc_init = RtcDataChannelInit::new();
        let dc = pc.create_data_channel_with_data_channel_dict("sunset-sync", &dc_init);
        dc.set_binary_type(RtcDataChannelType::Arraybuffer);

        let (ice_tx, ice_rx) = mpsc::unbounded::<String>();
        let (open_tx, open_rx) = oneshot::channel::<()>();
        let (msg_tx, msg_rx) = mpsc::unbounded::<Bytes>();

        let on_ice = make_ice_closure(ice_tx);
        pc.set_onicecandidate(Some(on_ice.as_ref().unchecked_ref()));
        let on_open = make_open_closure(open_tx);
        dc.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        let on_msg = make_msg_closure(msg_tx);
        dc.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));

        // Create offer + setLocalDescription.
        let offer = JsFuture::from(pc.create_offer())
            .await
            .map_err(|e| Error::Transport(format!("createOffer: {e:?}")))?;
        let sdp = sdp_from_session_description(&offer, "offer")?;
        let sd = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
        sd.set_sdp(&sdp);
        JsFuture::from(pc.set_local_description(&sd))
            .await
            .map_err(|e| Error::Transport(format!("setLocalDescription: {e:?}")))?;

        // Send the Offer.
        send_signal(
            &*self.signaler,
            self.local_peer.clone(),
            remote_peer.clone(),
            0,
            &WebRtcSignalKind::Offer(sdp),
        )
        .await?;

        // Spawn local-ICE forwarder.
        spawn_ice_forwarder(
            self.signaler.clone(),
            self.local_peer.clone(),
            remote_peer.clone(),
            ice_rx,
        );

        // Drive inbound (Answer + ICE) until the datachannel opens.
        let mut got_answer = false;
        let open_fut = open_rx.fuse();
        futures::pin_mut!(open_fut);
        loop {
            futures::select! {
                _ = open_fut.as_mut() => break,
                opt = peer_in_rx.next().fuse() => {
                    let kind = opt.ok_or_else(|| {
                        Error::Transport("signaling closed before open".into())
                    })?;
                    match kind {
                        WebRtcSignalKind::Answer(sdp) if !got_answer => {
                            let sd = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                            sd.set_sdp(&sdp);
                            JsFuture::from(pc.set_remote_description(&sd)).await.map_err(|e| {
                                Error::Transport(format!("setRemoteDescription: {e:?}"))
                            })?;
                            got_answer = true;
                        }
                        WebRtcSignalKind::IceCandidate(json) => {
                            add_remote_ice(&pc, &json).await?;
                        }
                        WebRtcSignalKind::Offer(_) | WebRtcSignalKind::Answer(_) => {
                            // Glare or duplicate — ignore.
                        }
                    }
                }
            }
        }

        self.unregister_peer(&remote_peer);

        Ok(WebRtcRawConnection {
            _pc: pc,
            dc,
            rx: RefCell::new(msg_rx),
            _on_ice: on_ice,
            _on_open: Some(on_open),
            _on_msg: Some(on_msg),
            _on_dc: None,
        })
    }

    async fn accept(&self) -> Result<Self::Connection> {
        self.ensure_dispatcher();

        // Take the offers_rx the first time accept() is called; the
        // engine calls accept() in a loop, so put it back when done.
        let mut offers_rx = self
            .inner
            .borrow_mut()
            .offers_rx
            .take()
            .ok_or_else(|| Error::Transport("accept already in progress".into()))?;
        let result = self.accept_one(&mut offers_rx).await;
        self.inner.borrow_mut().offers_rx = Some(offers_rx);
        result
    }
}

impl WebRtcRawTransport {
    async fn accept_one(
        &self,
        offers_rx: &mut mpsc::UnboundedReceiver<(PeerId, String)>,
    ) -> Result<WebRtcRawConnection> {
        let (from_peer, offer_sdp) = offers_rx
            .next()
            .await
            .ok_or_else(|| Error::Transport("offers channel closed".into()))?;

        let mut peer_in_rx = self.register_peer(from_peer.clone());

        let pc = build_peer_connection(&self.ice_urls)?;
        let (ice_tx, ice_rx) = mpsc::unbounded::<String>();
        let (open_tx, open_rx) = oneshot::channel::<()>();
        let (msg_tx, msg_rx) = mpsc::unbounded::<Bytes>();
        let (dc_tx, dc_rx) = oneshot::channel::<RtcDataChannel>();

        let on_ice = make_ice_closure(ice_tx);
        pc.set_onicecandidate(Some(on_ice.as_ref().unchecked_ref()));

        // When the inbound datachannel arrives, attach onmessage + onopen
        // and forward the channel to dc_rx.
        let dc_tx_cell = Rc::new(RefCell::new(Some(dc_tx)));
        let open_tx_cell = Rc::new(RefCell::new(Some(open_tx)));
        let msg_tx_for_dc = msg_tx;
        let on_dc =
            Closure::<dyn FnMut(RtcDataChannelEvent)>::new(move |ev: RtcDataChannelEvent| {
                let dc = ev.channel();
                dc.set_binary_type(RtcDataChannelType::Arraybuffer);

                let on_open = make_open_closure_from_cell(open_tx_cell.clone());
                dc.set_onopen(Some(on_open.as_ref().unchecked_ref()));
                // The closure is only ever fired once; once it has fired and
                // signaled `open_tx`, dropping it is safe. To keep it alive
                // until then, we leak it. Acceptable for v1 (page lifetime).
                on_open.forget();

                let on_msg = make_msg_closure(msg_tx_for_dc.clone());
                dc.set_onmessage(Some(on_msg.as_ref().unchecked_ref()));
                // The msg closure must outlive the datachannel itself, which
                // outlives the connection it's stored on. Page-lifetime leak
                // is acceptable here for v1; revisit if cleanup becomes
                // important.
                on_msg.forget();

                if let Some(tx) = dc_tx_cell.borrow_mut().take() {
                    let _ = tx.send(dc);
                }
            });
        pc.set_ondatachannel(Some(on_dc.as_ref().unchecked_ref()));

        let sd = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
        sd.set_sdp(&offer_sdp);
        JsFuture::from(pc.set_remote_description(&sd))
            .await
            .map_err(|e| Error::Transport(format!("setRemoteDescription offer: {e:?}")))?;

        let answer = JsFuture::from(pc.create_answer())
            .await
            .map_err(|e| Error::Transport(format!("createAnswer: {e:?}")))?;
        let sdp = sdp_from_session_description(&answer, "answer")?;
        let sd = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
        sd.set_sdp(&sdp);
        JsFuture::from(pc.set_local_description(&sd))
            .await
            .map_err(|e| Error::Transport(format!("setLocalDescription answer: {e:?}")))?;

        send_signal(
            &*self.signaler,
            self.local_peer.clone(),
            from_peer.clone(),
            0,
            &WebRtcSignalKind::Answer(sdp),
        )
        .await?;

        spawn_ice_forwarder(
            self.signaler.clone(),
            self.local_peer.clone(),
            from_peer.clone(),
            ice_rx,
        );

        // Drain inbound ICE candidates while waiting for ondatachannel,
        // then for dc.open. The dispatcher routes any remote ICE here.
        let dc_fut = dc_rx.fuse();
        let open_fut = open_rx.fuse();
        futures::pin_mut!(dc_fut, open_fut);
        let mut dc_opt: Option<RtcDataChannel> = None;
        loop {
            futures::select! {
                got = dc_fut.as_mut() => {
                    dc_opt = Some(got.map_err(|_| {
                        Error::Transport("peer connection dropped before ondatachannel".into())
                    })?);
                }
                _ = open_fut.as_mut() => {
                    if dc_opt.is_some() {
                        break;
                    }
                }
                opt = peer_in_rx.next().fuse() => {
                    let kind = opt.ok_or_else(|| {
                        Error::Transport("signaling closed mid-handshake".into())
                    })?;
                    if let WebRtcSignalKind::IceCandidate(json) = kind {
                        add_remote_ice(&pc, &json).await?;
                    }
                }
            }
        }

        self.unregister_peer(&from_peer);

        let dc = dc_opt.ok_or_else(|| Error::Transport("no inbound datachannel".into()))?;
        Ok(WebRtcRawConnection {
            _pc: pc,
            dc,
            rx: RefCell::new(msg_rx),
            _on_ice: on_ice,
            _on_open: None,
            _on_msg: None,
            _on_dc: Some(on_dc),
        })
    }
}

pub struct WebRtcRawConnection {
    _pc: RtcPeerConnection,
    dc: RtcDataChannel,
    rx: RefCell<mpsc::UnboundedReceiver<Bytes>>,
    _on_ice: Closure<dyn FnMut(RtcPeerConnectionIceEvent)>,
    /// Connect side keeps these on the connection. Accept side leaks them
    /// inside the `ondatachannel` handler (page lifetime), so these are
    /// `None` on the accept side.
    _on_open: Option<Closure<dyn FnMut(JsValue)>>,
    _on_msg: Option<Closure<dyn FnMut(MessageEvent)>>,
    /// Only set on the accept side.
    _on_dc: Option<Closure<dyn FnMut(RtcDataChannelEvent)>>,
}

#[async_trait(?Send)]
impl RawConnection for WebRtcRawConnection {
    async fn send_reliable(&self, bytes: Bytes) -> Result<()> {
        self.dc
            .send_with_u8_array(&bytes)
            .map_err(|e| Error::Transport(format!("dc.send: {e:?}")))
    }

    async fn recv_reliable(&self) -> Result<Bytes> {
        poll_fn(|cx| {
            let mut rx = self.rx.borrow_mut();
            match Stream::poll_next(Pin::new(&mut *rx), cx) {
                Poll::Ready(Some(b)) => Poll::Ready(Ok(b)),
                Poll::Ready(None) => Poll::Ready(Err(Error::Transport("dc closed".into()))),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }

    async fn send_unreliable(&self, _: Bytes) -> Result<()> {
        Err(Error::Transport(
            "webrtc: unreliable channel not implemented in v1".into(),
        ))
    }

    async fn recv_unreliable(&self) -> Result<Bytes> {
        Err(Error::Transport(
            "webrtc: unreliable channel not implemented in v1".into(),
        ))
    }

    async fn close(&self) -> Result<()> {
        self.dc.close();
        Ok(())
    }
}

// -----------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------

/// Parse `webrtc://<hex-pubkey>`.
fn parse_addr_peer_id(addr: &PeerAddr) -> Result<PeerId> {
    let s = std::str::from_utf8(addr.as_bytes())
        .map_err(|e| Error::Transport(format!("addr not utf-8: {e}")))?;
    let suffix = s
        .strip_prefix("webrtc://")
        .ok_or_else(|| Error::Transport(format!("addr not webrtc://: {s}")))?;
    let bytes =
        hex::decode(suffix).map_err(|e| Error::Transport(format!("hex decode failed: {e}")))?;
    Ok(PeerId(VerifyingKey::new(Bytes::from(bytes))))
}

fn build_peer_connection(ice_urls: &[String]) -> Result<RtcPeerConnection> {
    let config = RtcConfiguration::new();
    let servers = js_sys::Array::new();
    for url in ice_urls {
        let s = RtcIceServer::new();
        let urls = js_sys::Array::new();
        urls.push(&JsValue::from_str(url));
        Reflect::set(&s, &JsValue::from_str("urls"), &urls)
            .map_err(|e| Error::Transport(format!("set urls: {e:?}")))?;
        servers.push(&s);
    }
    config.set_ice_servers(&servers);
    RtcPeerConnection::new_with_configuration(&config)
        .map_err(|e| Error::Transport(format!("RtcPeerConnection: {e:?}")))
}

fn sdp_from_session_description(desc: &JsValue, kind: &str) -> Result<String> {
    Reflect::get(desc, &JsValue::from_str("sdp"))
        .ok()
        .and_then(|v| v.as_string())
        .ok_or_else(|| Error::Transport(format!("{kind}.sdp missing")))
}

fn make_ice_closure(
    tx: mpsc::UnboundedSender<String>,
) -> Closure<dyn FnMut(RtcPeerConnectionIceEvent)> {
    Closure::<dyn FnMut(RtcPeerConnectionIceEvent)>::new(move |ev: RtcPeerConnectionIceEvent| {
        if let Some(c) = ev.candidate() {
            let cand_str = js_sys::JSON::stringify(&c.to_json())
                .ok()
                .and_then(|s| s.as_string())
                .unwrap_or_default();
            let _ = tx.unbounded_send(cand_str);
        }
    })
}

fn make_open_closure(tx: oneshot::Sender<()>) -> Closure<dyn FnMut(JsValue)> {
    let cell = Rc::new(RefCell::new(Some(tx)));
    Closure::<dyn FnMut(JsValue)>::new(move |_| {
        if let Some(tx) = cell.borrow_mut().take() {
            let _ = tx.send(());
        }
    })
}

fn make_open_closure_from_cell(
    cell: Rc<RefCell<Option<oneshot::Sender<()>>>>,
) -> Closure<dyn FnMut(JsValue)> {
    Closure::<dyn FnMut(JsValue)>::new(move |_| {
        if let Some(tx) = cell.borrow_mut().take() {
            let _ = tx.send(());
        }
    })
}

fn make_msg_closure(tx: mpsc::UnboundedSender<Bytes>) -> Closure<dyn FnMut(MessageEvent)> {
    Closure::<dyn FnMut(MessageEvent)>::new(move |ev: MessageEvent| {
        let data = ev.data();
        if let Ok(buf) = data.dyn_into::<ArrayBuffer>() {
            let arr = Uint8Array::new(&buf);
            let mut bytes = vec![0u8; arr.length() as usize];
            arr.copy_to(&mut bytes);
            let _ = tx.unbounded_send(Bytes::from(bytes));
        }
    })
}

async fn send_signal(
    signaler: &dyn Signaler,
    from: PeerId,
    to: PeerId,
    seq: u64,
    kind: &WebRtcSignalKind,
) -> Result<()> {
    let payload =
        postcard::to_stdvec(kind).map_err(|e| Error::Transport(format!("postcard: {e}")))?;
    signaler
        .send(SignalMessage {
            from,
            to,
            seq,
            payload: Bytes::from(payload),
        })
        .await
}

fn spawn_ice_forwarder(
    signaler: Rc<dyn Signaler>,
    from: PeerId,
    to: PeerId,
    mut ice_rx: mpsc::UnboundedReceiver<String>,
) {
    spawn_local(async move {
        let mut seq: u64 = 1;
        while let Some(cand) = ice_rx.next().await {
            let kind = WebRtcSignalKind::IceCandidate(cand);
            let payload = match postcard::to_stdvec(&kind) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let _ = signaler
                .send(SignalMessage {
                    from: from.clone(),
                    to: to.clone(),
                    seq,
                    payload: Bytes::from(payload),
                })
                .await;
            seq += 1;
        }
    });
}

async fn add_remote_ice(pc: &RtcPeerConnection, cand_json: &str) -> Result<()> {
    let parsed = js_sys::JSON::parse(cand_json)
        .map_err(|e| Error::Transport(format!("ice json parse: {e:?}")))?;
    let candidate_str = Reflect::get(&parsed, &JsValue::from_str("candidate"))
        .ok()
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    let init = RtcIceCandidateInit::new(&candidate_str);
    if let Some(mid) = Reflect::get(&parsed, &JsValue::from_str("sdpMid"))
        .ok()
        .and_then(|v| v.as_string())
    {
        init.set_sdp_mid(Some(&mid));
    }
    if let Some(line_idx) = Reflect::get(&parsed, &JsValue::from_str("sdpMLineIndex"))
        .ok()
        .and_then(|v| v.as_f64())
    {
        init.set_sdp_m_line_index(Some(line_idx as u16));
    }
    let cand = RtcIceCandidate::new(&init)
        .map_err(|e| Error::Transport(format!("RtcIceCandidate: {e:?}")))?;
    JsFuture::from(pc.add_ice_candidate_with_opt_rtc_ice_candidate(Some(&cand)))
        .await
        .map_err(|e| Error::Transport(format!("addIceCandidate: {e:?}")))?;
    Ok(())
}
