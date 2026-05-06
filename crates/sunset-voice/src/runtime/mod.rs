//! Host-agnostic voice runtime.
//!
//! `VoiceRuntime` owns the protocol state (heartbeat + subscribe +
//! liveness + auto-connect + jitter buffer + mute/deafen). Hosts
//! provide three traits: `Dialer` (ensure direct WebRTC connection),
//! `FrameSink` (deliver decoded PCM to the audio output), and
//! `PeerStateSink` (receive `VoicePeerState` change events).
//!
//! `?Send` throughout — single-threaded, matches the project's WASM
//! constraint. Hosts spawn the returned futures with whatever
//! single-threaded local-spawn primitive they have
//! (`wasm_bindgen_futures::spawn_local` for browser, `LocalSet::spawn_local`
//! for native).

mod dyn_bus;
mod dyn_bus_impl;
mod state;
mod traits;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use rand_chacha::ChaCha20Rng;
use rand_core::SeedableRng;

use sunset_core::liveness::Liveness;
use sunset_core::{Identity, Room};

use crate::VoiceEncoder;

pub use dyn_bus::DynBus;
pub use traits::{Dialer, FrameSink, PeerStateSink, VoicePeerState};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);
const FRAME_STALE_AFTER: Duration = Duration::from_millis(1000);
const MEMBERSHIP_STALE_AFTER: Duration = Duration::from_secs(5);
const JITTER_MAX_DEPTH: usize = 8;
const JITTER_PUMP_INTERVAL: Duration = Duration::from_millis(20);
pub(crate) const VOICE_PRESENCE_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
pub(crate) const VOICE_PRESENCE_TTL: Duration = Duration::from_secs(6);

pub(crate) fn voice_presence_name(room_fp_hex: &str, sender_pk_hex: &str) -> bytes::Bytes {
    bytes::Bytes::from(format!("voice-presence/{room_fp_hex}/{sender_pk_hex}"))
}

pub(crate) fn voice_presence_prefix(room_fp_hex: &str) -> bytes::Bytes {
    bytes::Bytes::from(format!("voice-presence/{room_fp_hex}/"))
}

pub struct VoiceRuntime {
    inner: Rc<state::RuntimeInner>,
}

pub struct VoiceTasks {
    pub heartbeat: futures::future::LocalBoxFuture<'static, ()>,
    pub subscribe: futures::future::LocalBoxFuture<'static, ()>,
    pub combiner: futures::future::LocalBoxFuture<'static, ()>,
    pub auto_connect: futures::future::LocalBoxFuture<'static, ()>,
    pub jitter_pump: futures::future::LocalBoxFuture<'static, ()>,
    pub voice_presence_publisher: futures::future::LocalBoxFuture<'static, ()>,
}

impl VoiceRuntime {
    pub fn new(
        bus: Rc<dyn DynBus>,
        room: Rc<Room>,
        identity: Identity,
        dialer: Rc<dyn Dialer>,
        frame_sink: Rc<dyn FrameSink>,
        peer_state_sink: Rc<dyn PeerStateSink>,
    ) -> (Self, VoiceTasks) {
        let now_nanos = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        let frame_liveness = Liveness::new(FRAME_STALE_AFTER);
        let membership_liveness = Liveness::new(MEMBERSHIP_STALE_AFTER);

        let inner = Rc::new(state::RuntimeInner {
            identity,
            room,
            bus,
            dialer,
            frame_sink: RefCell::new(frame_sink),
            peer_state_sink,
            encoder: RefCell::new(
                VoiceEncoder::new().expect("opus encoder construction must succeed"),
            ),
            seq: RefCell::new(0),
            rng: RefCell::new(ChaCha20Rng::seed_from_u64(now_nanos)),
            muted: RefCell::new(false),
            deafened: RefCell::new(false),
            denoise: RefCell::new(true),
            denoisers: RefCell::new(Default::default()),
            frame_liveness,
            membership_liveness,
            jitter: RefCell::new(Default::default()),
            last_delivered: RefCell::new(Default::default()),
            auto_connect_state: RefCell::new(Default::default()),
            last_emitted: RefCell::new(Default::default()),
        });

        let tasks = VoiceTasks {
            heartbeat: heartbeat::spawn(Rc::downgrade(&inner)),
            subscribe: subscribe::spawn(Rc::downgrade(&inner)),
            combiner: combiner::spawn(Rc::downgrade(&inner)),
            auto_connect: auto_connect::spawn(Rc::downgrade(&inner)),
            jitter_pump: jitter::spawn(Rc::downgrade(&inner)),
            voice_presence_publisher: voice_presence_publisher::spawn(Rc::downgrade(&inner)),
        };

        (VoiceRuntime { inner }, tasks)
    }

    /// Capture-path entry. Encodes one frame, encrypts, publishes via
    /// `Bus::publish_ephemeral`. Drops the frame silently if `muted`.
    pub fn send_pcm(&self, pcm: &[f32]) {
        if *self.inner.muted.borrow() {
            return;
        }
        if pcm.len() != crate::FRAME_SAMPLES {
            return;
        }

        let inner = self.inner.clone();
        let pcm = pcm.to_vec();
        // Spawn the publish — Bus::publish_ephemeral is async. We
        // can't .await synchronously here.
        spawn_local(async move {
            let encoded = match inner.encoder.borrow_mut().encode(&pcm) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(error = %e, "encode failed");
                    return;
                }
            };
            let now_ms = web_time::SystemTime::now()
                .duration_since(web_time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let seq = {
                let mut s = inner.seq.borrow_mut();
                let v = *s;
                *s = s.saturating_add(1);
                v
            };
            let pkt = crate::packet::VoicePacket::Frame {
                codec_id: crate::CODEC_ID.to_string(),
                seq,
                sender_time_ms: now_ms,
                payload: encoded,
            };
            let public = inner.identity.public();
            let ev = match crate::packet::encrypt(
                &inner.room,
                0,
                &public,
                &pkt,
                &mut *inner.rng.borrow_mut(),
            ) {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "encrypt failed");
                    return;
                }
            };
            let payload = match postcard::to_stdvec(&ev) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(error = %e, "postcard encode failed");
                    return;
                }
            };
            let room_fp = inner.room.fingerprint().to_hex();
            let sender_pk = hex::encode(inner.identity.store_verifying_key().as_bytes());
            let name = bytes::Bytes::from(format!("voice/{room_fp}/{sender_pk}"));
            let _ = inner
                .bus
                .publish_ephemeral(name, bytes::Bytes::from(payload))
                .await;
        });
    }

    pub fn set_muted(&self, muted: bool) {
        *self.inner.muted.borrow_mut() = muted;
    }

    pub fn set_deafened(&self, deafened: bool) {
        *self.inner.deafened.borrow_mut() = deafened;
    }

    /// Toggle receiver-side RNNoise denoising. Defaults to `true` (on)
    /// at runtime construction. Off bypasses the denoiser entirely;
    /// previously-built per-peer state is kept so re-enabling resumes
    /// where it left off rather than starting from scratch.
    pub fn set_denoise(&self, denoise: bool) {
        *self.inner.denoise.borrow_mut() = denoise;
    }

    /// Stop the runtime. Dropping `self` has the same effect — all tasks
    /// observe the `Weak` upgrade failure and exit cleanly.
    pub fn stop(self) {
        drop(self);
    }

    /// Read mute state. Gated behind `test-hooks`; production code
    /// reads `inner.muted` directly.
    #[cfg(feature = "test-hooks")]
    pub fn is_muted(&self) -> bool {
        *self.inner.muted.borrow()
    }

    /// Read denoise toggle state. Gated behind `test-hooks`; production
    /// code reads `inner.denoise` directly.
    #[cfg(feature = "test-hooks")]
    pub fn is_denoise_enabled(&self) -> bool {
        *self.inner.denoise.borrow()
    }

    /// Test-only: report jitter buffer depth for a peer.
    #[cfg(feature = "test-hooks")]
    pub fn test_jitter_len(&self, peer: &sunset_sync::PeerId) -> usize {
        self.inner
            .jitter
            .borrow()
            .get(peer)
            .map(|q| q.len())
            .unwrap_or(0)
    }

    /// Test-only: push a PCM frame directly into the jitter buffer.
    #[cfg(feature = "test-hooks")]
    pub fn test_push_frame(&self, peer: sunset_sync::PeerId, pcm: Vec<f32>) {
        self.inner
            .jitter
            .borrow_mut()
            .entry(peer)
            .or_default()
            .push_back(pcm);
    }

    /// Test-only: swap the `FrameSink` with a new implementation (e.g.
    /// a recording wrapper).
    #[cfg(feature = "test-hooks")]
    pub fn set_frame_sink(&self, new_sink: Rc<dyn traits::FrameSink>) {
        *self.inner.frame_sink.borrow_mut() = new_sink;
    }

    /// Test-only: snapshot the last emitted `VoicePeerState` for every
    /// peer that has been observed.
    #[cfg(feature = "test-hooks")]
    pub fn snapshot_states(&self) -> Vec<traits::VoicePeerState> {
        self.inner
            .last_emitted
            .borrow()
            .iter()
            .map(|(peer, s)| traits::VoicePeerState {
                peer: peer.clone(),
                in_call: s.in_call,
                talking: s.talking,
                is_muted: s.is_muted,
            })
            .collect()
    }

    /// Test-only: peers observed by the auto-connect FSM (i.e. seen via
    /// the durable `voice-presence/...` subscription). Useful for
    /// distinguishing R1 (presence never reached) from R2 (presence
    /// reached but auto-connect didn't act).
    #[cfg(feature = "test-hooks")]
    pub fn auto_connect_peers(&self) -> Vec<sunset_sync::PeerId> {
        self.inner
            .auto_connect_state
            .borrow()
            .keys()
            .cloned()
            .collect()
    }

    /// Test-only: peers for which the subscribe loop has decoded at
    /// least one inbound voice payload (Frame or Heartbeat). Frames go
    /// to `jitter`; Heartbeats land in `last_emitted`. The union of
    /// these two maps' keys tells us which peers the receiver has
    /// actually heard from over the WebRTC datachannel.
    #[cfg(feature = "test-hooks")]
    pub fn observed_voice_peers(&self) -> Vec<sunset_sync::PeerId> {
        let mut peers: std::collections::HashSet<sunset_sync::PeerId> =
            self.inner.jitter.borrow().keys().cloned().collect();
        for k in self.inner.last_emitted.borrow().keys() {
            peers.insert(k.clone());
        }
        peers.into_iter().collect()
    }

    /// Test-only: depth of each per-peer jitter buffer. Useful for
    /// distinguishing R4 (frames flowing) from R5 (combiner not
    /// observing them).
    #[cfg(feature = "test-hooks")]
    pub fn jitter_depths(&self) -> Vec<(sunset_sync::PeerId, usize)> {
        self.inner
            .jitter
            .borrow()
            .iter()
            .map(|(p, q)| (p.clone(), q.len()))
            .collect()
    }

    /// Test-only: return clones of the frame and membership `Liveness`
    /// arcs so tests can inject synthetic observations (e.g. to wake the
    /// combiner task for in-flight cancellation verification).
    #[cfg(feature = "test-hooks")]
    pub fn test_liveness(
        &self,
    ) -> (
        std::sync::Arc<sunset_core::liveness::Liveness>,
        std::sync::Arc<sunset_core::liveness::Liveness>,
    ) {
        (
            self.inner.frame_liveness.clone(),
            self.inner.membership_liveness.clone(),
        )
    }
}

#[cfg(target_arch = "wasm32")]
fn spawn_local<F: std::future::Future<Output = ()> + 'static>(f: F) {
    wasm_bindgen_futures::spawn_local(f);
}
#[cfg(not(target_arch = "wasm32"))]
fn spawn_local<F: std::future::Future<Output = ()> + 'static>(f: F) {
    tokio::task::spawn_local(f);
}

mod auto_connect;
mod combiner;
mod heartbeat;
mod jitter;
mod subscribe;
mod voice_presence_publisher;
