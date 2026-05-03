//! Membership + relay-status tracker. Platform-agnostic chat-semantics
//! layer over the engine's presence-namespace store events and
//! `EngineEvent::PeerAdded` / `PeerRemoved` stream. Hosts (web-wasm,
//! TUI, mod) plug in their own callback to render the member list and
//! relay-status indicator.
//!
//! Three input streams drive the tracker task:
//!
//! 1. local store events on `<room_fp>/presence/<peer_pubkey>` —
//!    application-level heartbeats published by `presence_publisher`.
//! 2. engine events (`PeerAdded` / `PeerRemoved` carrying a transport
//!    `kind`) — used to derive `connection_mode` (`direct` /
//!    `via_relay`) per peer.
//! 3. periodic refresh tick — catches Online↔Away↔Offline threshold
//!    crossings between heartbeats. Also doubles as the path that
//!    drains the cleared `last_signature` after a `(re-)register`-
//!    style API call (see `TrackerHandles::last_signature`).
//!
//! On each input, re-derives the member list and (if it differs from
//! the last fired signature) invokes the registered members callback.
//! The relay-status callback is fired separately on engine events.
//!
//! Callbacks are typed as `Box<dyn Fn>` so the same tracker drives
//! both wasm-bindgen `js_sys::Function` shims (in `sunset-web-wasm`)
//! and native client-surface callbacks (TUI, mod).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use bytes::Bytes;
use futures::channel::mpsc as fmpsc;
use futures::{FutureExt, StreamExt};

use sunset_store::{Filter, Replay, Store};
use sunset_sync::{EngineEvent, PeerId, TransportKind};

// Portable sleep: native uses tokio::time, wasm uses wasmtimer's
// setTimeout-backed drop-in.
#[cfg(not(target_arch = "wasm32"))]
use tokio::time::sleep;
#[cfg(target_arch = "wasm32")]
use wasmtimer::tokio::sleep;

/// Three-state presence bucket derived from heartbeat age.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presence {
    Online,
    Away,
    Offline,
}

impl Presence {
    pub fn as_str(self) -> &'static str {
        match self {
            Presence::Online => "online",
            Presence::Away => "away",
            Presence::Offline => "offline",
        }
    }
}

/// How the local node reaches a peer:
/// - `Self_`  — this peer is us.
/// - `Direct` — open `Secondary` transport (e.g. WebRTC datachannel).
/// - `ViaRelay` — known transport is `Primary` (relay WS), or the peer's
///   presence reached us via a relay we hold a Primary connection to.
/// - `Unknown` — no transport entry and no Primary connection at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionMode {
    Self_,
    Direct,
    ViaRelay,
    Unknown,
}

impl ConnectionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionMode::Self_ => "self",
            ConnectionMode::Direct => "direct",
            ConnectionMode::ViaRelay => "via_relay",
            ConnectionMode::Unknown => "unknown",
        }
    }
}

/// Derived per-member view. Hosts wrap this for their UI layer
/// (e.g. `MemberJs` in `sunset-web-wasm` is a wasm-bindgen wrapper
/// that exposes string fields and a `f64` heartbeat sentinel for JS).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub pubkey: Vec<u8>,
    pub presence: Presence,
    pub connection_mode: ConnectionMode,
    pub is_self: bool,
    /// Unix-ms timestamp of the last app-level presence heartbeat.
    /// `None` for self (we don't track our own presence) and for any
    /// peer we've heard nothing from. UI surfaces compute
    /// `now_ms - last_heartbeat_ms` for the "heard from N ago" string.
    pub last_heartbeat_ms: Option<u64>,
}

/// Stable per-member signature row used for debounce:
/// `(pubkey, presence, connection_mode)`. Excludes
/// `last_heartbeat_ms` so the tracker doesn't re-emit on every
/// heartbeat tick.
pub type MemberSig = Vec<(Vec<u8>, Presence, ConnectionMode)>;

/// Callback fired with the current rendered member list whenever it
/// changes (per `members_signature` debounce).
pub type MembersCallback = Box<dyn Fn(&[Member])>;

/// Callback fired with the current relay-status string whenever it
/// changes. Values: `"connecting"`, `"connected"`, `"disconnected"`,
/// `"error"`.
pub type RelayStatusCallback = Box<dyn Fn(&str)>;

/// Slot type for `TrackerHandles::on_members` — a re-assignable, lazy-
/// initialized members callback shared between the tracker task and
/// the host's public API surface.
pub type MembersCallbackSlot = Rc<RefCell<Option<MembersCallback>>>;

/// Slot type for `TrackerHandles::on_relay_status`.
pub type RelayStatusCallbackSlot = Rc<RefCell<Option<RelayStatusCallback>>>;

/// Bucket a heartbeat age into Online / Away / Offline.
///
/// - `age_ms < interval_ms`         → Online
/// - `interval_ms ≤ age_ms < ttl_ms` → Away
/// - `age_ms ≥ ttl_ms`              → Offline (caller drops member from list)
pub fn presence_bucket(age_ms: u64, interval_ms: u64, ttl_ms: u64) -> Presence {
    if age_ms < interval_ms {
        Presence::Online
    } else if age_ms < ttl_ms {
        Presence::Away
    } else {
        Presence::Offline
    }
}

/// Pure derivation: given the current state, return the rendered
/// member list. Self is always present and always Online. Others
/// whose age exceeds `ttl_ms` are filtered out.
pub fn derive_members(
    now_ms: u64,
    interval_ms: u64,
    ttl_ms: u64,
    self_peer: &PeerId,
    presence_map: &HashMap<PeerId, u64>,
    peer_kinds: &HashMap<PeerId, TransportKind>,
) -> Vec<Member> {
    let mut out = Vec::new();
    out.push(Member {
        pubkey: self_peer.verifying_key().as_bytes().to_vec(),
        presence: Presence::Online,
        connection_mode: ConnectionMode::Self_,
        is_self: true,
        last_heartbeat_ms: None,
    });
    let mut others: Vec<(&PeerId, &u64)> = presence_map
        .iter()
        .filter(|(pk, _)| *pk != self_peer)
        .collect();
    others.sort_by(|(a, _), (b, _)| {
        a.verifying_key()
            .as_bytes()
            .cmp(b.verifying_key().as_bytes())
    });
    // V1 single-relay topology assumption: if we hold any Primary
    // transport (i.e. an open relay connection), then a peer for
    // whom we have no direct/relay transport entry yet — most
    // commonly because the relay forwarded their presence entry
    // without us building a transport to them — is reachable
    // "via_relay". Revisit when multi-relay/federated routing
    // lands: the right model then is to track which specific relay
    // forwarded each heartbeat and key the via_relay decision on
    // whether that relay is currently in `peer_kinds`.
    let any_relay = peer_kinds.values().any(|k| *k == TransportKind::Primary);
    for (pk, last_ms) in others {
        let age = now_ms.saturating_sub(*last_ms);
        let presence = presence_bucket(age, interval_ms, ttl_ms);
        if presence == Presence::Offline {
            continue;
        }
        let connection_mode = match peer_kinds.get(pk) {
            Some(TransportKind::Secondary) => ConnectionMode::Direct,
            Some(TransportKind::Primary) => ConnectionMode::ViaRelay,
            _ if any_relay => ConnectionMode::ViaRelay,
            _ => ConnectionMode::Unknown,
        };
        out.push(Member {
            pubkey: pk.verifying_key().as_bytes().to_vec(),
            presence,
            connection_mode,
            is_self: false,
            last_heartbeat_ms: Some(*last_ms),
        });
    }
    out
}

/// Stable shape signature used to debounce callbacks. The tracker
/// compares the current signature with the previously-emitted one
/// and only fires the callback if it changed.
pub fn members_signature(members: &[Member]) -> MemberSig {
    members
        .iter()
        .map(|m| (m.pubkey.clone(), m.presence, m.connection_mode))
        .collect()
}

/// Shared mutable state between the tracker task and the host's public
/// API. Cloneable so the host (e.g. `Client`) can keep its own handle
/// alongside the spawned task's. All fields are `Rc`-backed so updates
/// from either side are immediately visible to the other.
#[derive(Clone, Default)]
pub struct TrackerHandles {
    pub on_members: MembersCallbackSlot,
    pub on_relay_status: RelayStatusCallbackSlot,
    pub last_relay_status: Rc<RefCell<String>>,
    pub peer_kinds: Rc<RefCell<HashMap<PeerId, TransportKind>>>,
    /// Per-member signature of the most recent fire. Lives on the handle
    /// (not as a tracker-task local) so a host's `on_members_changed`-
    /// style API can clear it on re-registration: the next `maybe_fire`
    /// then sees signature ≠ stored signature and fires the callback
    /// with the current state. Without this, a callback registered after
    /// the system has stabilized would never see anything until the next
    /// presence transition (which can be never if heartbeats land before
    /// the refresh tick crosses `interval_ms`).
    pub last_signature: Rc<RefCell<MemberSig>>,
}

impl TrackerHandles {
    pub fn new(initial_relay_status: &str) -> Self {
        Self {
            on_members: Rc::new(RefCell::new(None)),
            on_relay_status: Rc::new(RefCell::new(None)),
            last_relay_status: Rc::new(RefCell::new(initial_relay_status.to_owned())),
            peer_kinds: Rc::new(RefCell::new(HashMap::new())),
            last_signature: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

/// Spawn the tracker. Runs forever (host-process / page lifetime).
///
/// `store` carries the presence entries (subscribe-namespace + chat +
/// presence; the tracker filters to `<room_fp>/presence/`).
/// `engine_events` is the consumer half of
/// `SyncEngine::subscribe_engine_events()`. `room_fp_hex` keys the
/// presence-namespace prefix. `interval_ms` / `ttl_ms` thresholds are
/// passed through to `presence_bucket`; `refresh_ms` is the inter-tick
/// period of the catch-up timer that handles Online↔Away↔Offline
/// transitions between heartbeats.
#[allow(clippy::too_many_arguments)]
pub fn spawn_tracker<S: Store + 'static>(
    store: std::sync::Arc<S>,
    mut engine_events: tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
    self_peer: PeerId,
    room_fp_hex: String,
    interval_ms: u64,
    ttl_ms: u64,
    refresh_ms: u64,
    handles: TrackerHandles,
) {
    // Periodic refresh ticker as a separate task. Pushes a unit into
    // `refresh_rx` every `refresh_ms`. The main select loop only
    // deals with channels — no inline `sleep().fuse()` pinning
    // gymnastics required.
    let (refresh_tx, refresh_rx) = fmpsc::unbounded::<()>();
    sunset_sync::spawn::spawn_local(async move {
        loop {
            sleep(Duration::from_millis(refresh_ms)).await;
            if refresh_tx.unbounded_send(()).is_err() {
                break;
            }
        }
    });

    sunset_sync::spawn::spawn_local(async move {
        let presence_filter = Filter::NamePrefix(Bytes::from(format!("{room_fp_hex}/presence/")));
        let presence_sub = match store.subscribe(presence_filter, Replay::All).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "presence subscribe failed");
                return;
            }
        };
        let mut presence_sub = presence_sub.fuse();
        let mut refresh_rx = refresh_rx.fuse();
        let presence_map: Rc<RefCell<HashMap<PeerId, u64>>> = Rc::new(RefCell::new(HashMap::new()));
        let prefix = format!("{room_fp_hex}/presence/");

        loop {
            futures::select! {
                ev = presence_sub.next() => {
                    let Some(ev) = ev else { break };
                    let entry = match ev {
                        Ok(sunset_store::Event::Inserted(e)) => e,
                        Ok(sunset_store::Event::Replaced { new, .. }) => new,
                        Ok(_) => continue,
                        Err(e) => {
                            tracing::warn!(error = %e, "presence event error");
                            continue;
                        }
                    };
                    let Some(pk) = parse_presence_pk(&entry.name, &prefix) else { continue };
                    presence_map.borrow_mut().insert(pk, entry.priority);
                    maybe_fire_members(
                        now_ms(),
                        interval_ms,
                        ttl_ms,
                        &self_peer,
                        &presence_map.borrow(),
                        &handles,
                    );
                }
                ev = recv_engine(&mut engine_events).fuse() => {
                    let Some(ev) = ev else { break };
                    handle_engine_event(&handles, &ev);
                    maybe_fire_relay_status(&handles);
                    maybe_fire_members(
                        now_ms(),
                        interval_ms,
                        ttl_ms,
                        &self_peer,
                        &presence_map.borrow(),
                        &handles,
                    );
                }
                _ = refresh_rx.next() => {
                    // Periodic re-derive (catches Online↔Away threshold crossings,
                    // and is the path that fires the callback for free shortly after
                    // a host's `on_members_changed` clears `last_signature`).
                    maybe_fire_members(
                        now_ms(),
                        interval_ms,
                        ttl_ms,
                        &self_peer,
                        &presence_map.borrow(),
                        &handles,
                    );
                }
            }
        }
    });
}

/// Public re-evaluation entry point used by hosts after seeding
/// `peer_kinds` from the engine snapshot. Fires the relay-status
/// callback if `peer_kinds` now resolves to a state different from
/// `last_relay_status`. Idempotent: if nothing changed, no-op.
pub fn fire_relay_status_now(handles: &TrackerHandles) {
    maybe_fire_relay_status(handles);
}

fn now_ms() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn recv_engine(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<EngineEvent>,
) -> Option<EngineEvent> {
    rx.recv().await
}

fn parse_presence_pk(name: &[u8], prefix: &str) -> Option<PeerId> {
    let s = std::str::from_utf8(name).ok()?;
    let suffix = s.strip_prefix(prefix)?;
    let bytes = hex::decode(suffix).ok()?;
    Some(PeerId(sunset_store::VerifyingKey::new(Bytes::from(bytes))))
}

fn handle_engine_event(handles: &TrackerHandles, ev: &EngineEvent) {
    match ev {
        EngineEvent::PeerAdded { peer_id, kind } => {
            handles
                .peer_kinds
                .borrow_mut()
                .insert(peer_id.clone(), *kind);
        }
        EngineEvent::PeerRemoved { peer_id } => {
            handles.peer_kinds.borrow_mut().remove(peer_id);
        }
    }
}

fn derive_relay_status(peer_kinds: &HashMap<PeerId, TransportKind>, prior: &str) -> String {
    // Sticky "connecting"/"error" states are owned by the host (set
    // at add-relay call time). We only flip between "connected" and
    // "disconnected" based on whether any Primary connection exists.
    if prior == "connecting" || prior == "error" {
        return prior.to_owned();
    }
    if peer_kinds.values().any(|k| *k == TransportKind::Primary) {
        "connected".to_owned()
    } else {
        "disconnected".to_owned()
    }
}

fn maybe_fire_relay_status(handles: &TrackerHandles) {
    let prior = handles.last_relay_status.borrow().clone();
    let next = derive_relay_status(&handles.peer_kinds.borrow(), &prior);
    if next != prior {
        *handles.last_relay_status.borrow_mut() = next.clone();
        if let Some(cb) = handles.on_relay_status.borrow().as_ref() {
            cb(&next);
        }
    }
}

fn maybe_fire_members(
    now_ms: u64,
    interval_ms: u64,
    ttl_ms: u64,
    self_peer: &PeerId,
    presence_map: &HashMap<PeerId, u64>,
    handles: &TrackerHandles,
) {
    let members = derive_members(
        now_ms,
        interval_ms,
        ttl_ms,
        self_peer,
        presence_map,
        &handles.peer_kinds.borrow(),
    );
    let sig = members_signature(&members);
    if sig == *handles.last_signature.borrow() {
        return;
    }
    *handles.last_signature.borrow_mut() = sig;
    if let Some(cb) = handles.on_members.borrow().as_ref() {
        cb(&members);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use sunset_store::VerifyingKey;

    fn pk(b: u8) -> PeerId {
        PeerId(VerifyingKey::new(Bytes::copy_from_slice(&[b; 32])))
    }

    #[test]
    fn presence_bucket_thresholds() {
        assert_eq!(presence_bucket(0, 1000, 3000), Presence::Online);
        assert_eq!(presence_bucket(999, 1000, 3000), Presence::Online);
        assert_eq!(presence_bucket(1000, 1000, 3000), Presence::Away);
        assert_eq!(presence_bucket(2999, 1000, 3000), Presence::Away);
        assert_eq!(presence_bucket(3000, 1000, 3000), Presence::Offline);
        assert_eq!(presence_bucket(10_000, 1000, 3000), Presence::Offline);
    }

    #[test]
    fn derive_members_self_only_when_no_peers() {
        let me = pk(1);
        let presence = HashMap::new();
        let kinds = HashMap::new();
        let out = derive_members(0, 1000, 3000, &me, &presence, &kinds);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_self);
        assert_eq!(out[0].presence, Presence::Online);
        assert_eq!(out[0].connection_mode, ConnectionMode::Self_);
    }

    #[test]
    fn derive_members_skips_offline_peers() {
        let me = pk(1);
        let bob = pk(2);
        let mut presence = HashMap::new();
        presence.insert(bob.clone(), 0u64);
        let kinds = HashMap::new();
        // bob's heartbeat is 5s old but ttl is 3s → Offline → dropped.
        let out = derive_members(5000, 1000, 3000, &me, &presence, &kinds);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_self);
    }

    #[test]
    fn derive_members_maps_kinds_to_modes() {
        let me = pk(1);
        let bob = pk(2);
        let carol = pk(3);
        let dave = pk(4);
        let mut presence = HashMap::new();
        presence.insert(bob.clone(), 100);
        presence.insert(carol.clone(), 100);
        presence.insert(dave.clone(), 100);
        let mut kinds = HashMap::new();
        kinds.insert(bob.clone(), TransportKind::Primary);
        kinds.insert(carol.clone(), TransportKind::Secondary);
        // dave: no kind, but a Primary (bob) exists → dave's presence was
        // forwarded by the relay → "via_relay".
        let out = derive_members(200, 1000, 3000, &me, &presence, &kinds);
        assert_eq!(out.len(), 4);
        let modes: Vec<ConnectionMode> = out.iter().map(|m| m.connection_mode).collect();
        assert_eq!(
            modes,
            vec![
                ConnectionMode::Self_,
                ConnectionMode::ViaRelay,
                ConnectionMode::Direct,
                ConnectionMode::ViaRelay,
            ]
        );
    }

    #[test]
    fn derive_members_unknown_when_no_relay_no_kind() {
        let me = pk(1);
        let dave = pk(4);
        let mut presence = HashMap::new();
        presence.insert(dave.clone(), 100);
        // No Primary in peer_kinds → dave's presence has no traceable
        // route → "unknown".
        let kinds = HashMap::new();
        let out = derive_members(200, 1000, 3000, &me, &presence, &kinds);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].connection_mode, ConnectionMode::Unknown);
    }

    #[test]
    fn members_signature_changes_on_presence_change() {
        let me = pk(1);
        let bob = pk(2);
        let mut presence = HashMap::new();
        presence.insert(bob.clone(), 0);
        let kinds = HashMap::new();

        let s1 = members_signature(&derive_members(500, 1000, 3000, &me, &presence, &kinds));
        let s2 = members_signature(&derive_members(1500, 1000, 3000, &me, &presence, &kinds));
        assert_ne!(s1, s2, "Online → Away should change signature");
    }

    #[test]
    fn derive_members_includes_last_heartbeat_for_others() {
        let me = pk(0);
        let bob = pk(1);
        let mut presence = HashMap::new();
        presence.insert(bob.clone(), 12_345_u64);
        let kinds = HashMap::new();
        let out = derive_members(20_000, 30_000, 60_000, &me, &presence, &kinds);
        assert!(out[0].is_self);
        assert_eq!(out[0].last_heartbeat_ms, None);
        assert!(!out[1].is_self);
        assert_eq!(out[1].last_heartbeat_ms, Some(12_345_u64));
    }

    #[test]
    fn signature_ignores_heartbeat_timestamp() {
        // Two member lists differing ONLY in last_heartbeat_ms must
        // produce equal signatures, otherwise the membership tracker
        // would re-emit on every age tick.
        let m1 = Member {
            pubkey: vec![1; 32],
            presence: Presence::Online,
            connection_mode: ConnectionMode::ViaRelay,
            is_self: false,
            last_heartbeat_ms: Some(100),
        };
        let m2 = Member {
            pubkey: vec![1; 32],
            presence: Presence::Online,
            connection_mode: ConnectionMode::ViaRelay,
            is_self: false,
            last_heartbeat_ms: Some(200),
        };
        assert_eq!(members_signature(&[m1]), members_signature(&[m2]));
    }
}
