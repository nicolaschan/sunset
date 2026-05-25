//! `Peer` is the host-agnostic "running sunset peer" entity.
//! Holds identity, store, sync engine, supervisor, and a registry of
//! open rooms. `Peer::open_room(name)` (added in Phase 5) returns an
//! `OpenRoom` handle.

mod open_room;

pub use open_room::OpenRoom;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use sunset_store::Store;
use sunset_sync::{IntentId, IntentSnapshot, PeerSupervisor, SyncEngine, Transport};

use crate::Identity;
use crate::crypto::room::RoomFingerprint;
use crate::signaling::MultiRoomSignaler;

pub struct Peer<St: Store + 'static, T: Transport + 'static> {
    identity: Identity,
    store: Arc<St>,
    engine: Rc<SyncEngine<St, T>>,
    supervisor: Rc<PeerSupervisor<St, T>>,
    /// Held across `open_room`'s await window so two concurrent
    /// `open_room("alpha")` calls coalesce on a single `RoomState`
    /// rather than racing through the idempotency check, both doing
    /// Argon2 work and both registering signalers (the second
    /// overwriting the first in the dispatcher).
    open_rooms: tokio::sync::Mutex<HashMap<RoomFingerprint, Weak<open_room::RoomState<St, T>>>>,
    pub(crate) rtc_signaler_dispatcher: Rc<MultiRoomSignaler>,
    /// Last name set via `set_self_name`. Applied to newly-opened
    /// rooms' publishers in `start_presence` so that a web client
    /// calling `set_self_name` from `ClientReady` (before any room is
    /// open) doesn't silently no-op. Empty string is stored as `None`.
    pending_self_name: RefCell<Option<String>>,
}

impl<St, T> Peer<St, T>
where
    St: Store + 'static,
    T: Transport + 'static,
    T::Connection: 'static,
{
    pub fn new(
        identity: Identity,
        store: Arc<St>,
        engine: Rc<SyncEngine<St, T>>,
        supervisor: Rc<PeerSupervisor<St, T>>,
        rtc_signaler_dispatcher: Rc<MultiRoomSignaler>,
    ) -> Rc<Self> {
        Rc::new(Self {
            identity,
            store,
            engine,
            supervisor,
            open_rooms: tokio::sync::Mutex::new(HashMap::new()),
            rtc_signaler_dispatcher,
            pending_self_name: RefCell::new(None),
        })
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.identity.public().as_bytes()
    }

    pub async fn open_room(self: &Rc<Self>, room_name: &str) -> crate::Result<OpenRoom<St, T>> {
        // Open the Room (Argon2id derivation; expensive — ~tens to
        // hundreds of ms with production params). Done outside the
        // registry lock so concurrent opens of *different* rooms can
        // run their KDF in parallel (single-threaded WASM doesn't
        // actually parallelize, but this keeps native hosts honest).
        let room = Rc::new(crate::Room::open(room_name)?);
        let fp = room.fingerprint();

        // Hold the registry lock across the rest of the open: this is
        // the idempotency guarantee. Without it two concurrent
        // open_room("alpha") calls both pass the registry check, both
        // register signalers (second wins), and the first signaler's
        // spawn_dispatcher leaks. Cost: serializes opens of *different*
        // rooms too. Acceptable — opens are rare and the work is
        // bounded.
        let mut open_rooms = self.open_rooms.lock().await;

        // Idempotency check under lock.
        if let Some(weak) = open_rooms.get(&fp) {
            if let Some(strong) = weak.upgrade() {
                return Ok(OpenRoom { inner: strong });
            }
        }

        // Build a fresh per-room signaler and register it with the
        // dispatcher.
        let signaler: Rc<crate::signaling::RelaySignaler<St>> =
            crate::signaling::RelaySignaler::new(self.identity.clone(), fp.to_hex(), &self.store);
        self.rtc_signaler_dispatcher.register(fp, signaler.clone());

        // Publish the room subscription via the high-level subscribe
        // API (records a BroadcastIntent + auto-resubscribes on
        // PeerHello, so we don't need an explicit renewal loop here).
        let filter = crate::filters::room_filter(&room);
        self.engine
            .subscribe(
                filter,
                sunset_sync::routing::SubscriptionPolicy::store_data(),
            )
            .await
            .map_err(|e| crate::Error::Other(format!("subscribe: {e}")))?;

        // The per-room signaler doesn't need a strong ref on RoomState:
        // RelaySignaler::new spawned its dispatcher task with its own
        // strong Rc, and dispatcher.register stored another in the
        // dispatcher's HashMap. RoomState::drop's `unregister` call
        // drops the latter; the dispatcher task keeps the signaler
        // alive until its store-subscribe stream ends. The local
        // `signaler` binding falls out of scope here without any
        // further wiring.

        // Spawn the per-room reaction tracker. It subscribes to
        // <room_fp>/msg/, decodes Reaction entries, applies LWW per
        // (author, target, emoji), and fires on_reactions_changed
        // (registered via OpenRoom::on_reactions_changed) per
        // debounced per-target snapshot change.
        let reaction_handles = crate::reactions::ReactionHandles::default();
        crate::reactions::spawn_reaction_tracker(
            self.store.clone(),
            (*room).clone(),
            fp.to_hex(),
            reaction_handles.clone(),
        );

        // Seed observed_channels with the default `general` channel so
        // hosts that read it (or register on_channels_changed) before
        // any traffic arrives still see ["general"]. The decode loop
        // expands this set as new channels are observed.
        let mut chans = std::collections::BTreeSet::new();
        chans.insert(crate::ChannelLabel::default_general());
        let observed_channels = Rc::new(std::cell::RefCell::new(chans));

        // `cancel_decode` is consumed by the decode loop spawned in
        // `OpenRoom::spawn_decode_loop`; `RoomState::drop` flips it to
        // signal shutdown.
        let state = Rc::new(open_room::RoomState {
            room,
            peer_weak: Rc::downgrade(self),
            presence_started: std::cell::Cell::new(false),
            publisher: std::cell::RefCell::new(None),
            tracker_handles: Rc::new(crate::membership::TrackerHandles::new()),
            reaction_handles,
            cancel_decode: Rc::new(std::cell::Cell::new(false)),
            callbacks: Rc::new(std::cell::RefCell::new(open_room::RoomCallbacks::default())),
            observed_channels,
            decoded_text_messages: Rc::new(std::cell::RefCell::new(
                std::collections::BTreeMap::new(),
            )),
        });

        open_rooms.insert(fp, Rc::downgrade(&state));
        Ok(OpenRoom { inner: state })
    }

    /// Register a durable connection intent. Returns the supervisor's
    /// `IntentId` once the intent is recorded (one cmd-channel
    /// round-trip; does NOT wait for the first connection). Transient
    /// failures (resolver fetch, dial, Hello) never bubble out — the
    /// supervisor retries with exponential backoff. The only `Err` is
    /// `ResolveErr::Parse` for typed garbage that the resolver can
    /// never make sense of.
    ///
    /// To observe live state for the returned intent, call
    /// `on_intent_changed`.
    pub async fn add_relay(
        &self,
        connectable: sunset_sync::Connectable,
    ) -> Result<IntentId, sunset_sync::ResolveErr> {
        self.supervisor.add(connectable).await
    }

    /// Snapshot every registered intent's current state. Used by hosts
    /// on first paint, before the live `on_intent_changed` stream
    /// arrives.
    pub async fn intents(&self) -> Vec<IntentSnapshot> {
        self.supervisor.snapshot().await
    }

    /// Subscribe to per-intent state transitions. The returned receiver
    /// is fed the current snapshot of every existing intent on
    /// subscribe (so late subscribers don't miss state), then every
    /// change after that.
    pub async fn subscribe_intents(&self) -> tokio::sync::mpsc::UnboundedReceiver<IntentSnapshot> {
        self.supervisor.subscribe_intents().await
    }

    /// Update the display name carried in every open room's presence
    /// heartbeats. Caches the name so rooms opened after this call
    /// also pick it up via `start_presence`. Silently skips rooms
    /// whose `OpenRoom` has been dropped (the corresponding
    /// `Weak<RoomState>` upgrade fails). Silently skips rooms that
    /// have not called `start_presence` yet.
    pub fn set_self_name(&self, name: &str) {
        // Cache unconditionally — this is the whole point: a web client
        // calls set_self_name from ClientReady before any room is open,
        // and start_presence must pick it up later.
        *self.pending_self_name.borrow_mut() = if name.is_empty() {
            None
        } else {
            Some(name.to_owned())
        };

        let rooms = match self.open_rooms.try_lock() {
            Ok(g) => g,
            Err(_) => {
                // Lock is held by an in-flight open_room; the new room
                // will inherit the cached name via start_presence.
                tracing::debug!("set_self_name: open_rooms lock contended; cache written");
                return;
            }
        };
        for weak in rooms.values() {
            if let Some(state) = weak.upgrade() {
                if let Some(p) = state.publisher.borrow().as_ref() {
                    p.update_name(name);
                }
            }
        }
    }

    /// Returns the last name set via `set_self_name`, or `None` if
    /// no name has been set (or the last name was empty).
    pub(crate) fn cached_self_name(&self) -> Option<String> {
        self.pending_self_name.borrow().clone()
    }

    // Accessor methods consumed by Phase 5+ (open_room, send_text, etc.).
    pub(crate) fn identity(&self) -> &Identity {
        &self.identity
    }

    pub(crate) fn store(&self) -> &Arc<St> {
        &self.store
    }

    pub(crate) fn engine(&self) -> &Rc<SyncEngine<St, T>> {
        &self.engine
    }

    /// Public accessor for the engine. Used by e2e test diagnostics
    /// (e.g. `voice_engine_connected_peers` in sunset-web-wasm) to read
    /// connected-peer state for layered failure analysis. Gated behind
    /// `test-hooks` so the engine surface stays out of the production
    /// API; production callers go through `Peer`'s curated methods.
    #[cfg(feature = "test-hooks")]
    pub fn engine_handle(&self) -> &Rc<SyncEngine<St, T>> {
        &self.engine
    }

    pub(crate) fn supervisor(&self) -> &Rc<PeerSupervisor<St, T>> {
        &self.supervisor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Ed25519Verifier;
    use sunset_store_memory::MemoryStore;

    fn ident(seed: u8) -> Identity {
        Identity::from_secret_bytes(&[seed; 32])
    }

    #[tokio::test(flavor = "current_thread")]
    async fn peer_new_exposes_public_key_and_no_intents() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(7)).await;
                assert_eq!(peer.public_key().len(), 32);
                // Fresh peer: no intents registered.
                assert!(peer.intents().await.is_empty());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn add_relay_records_intent_and_returns_id() {
        // The supervisor is the source of truth for relay connection
        // state. `add_relay` returns once the intent is recorded (one
        // cmd-channel round-trip); transient failures (NopTransport's
        // immediate Transport("nop")) don't bubble out — they leave
        // the intent in Backoff. We assert the intent is registered
        // and visible via `intents()`.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(8)).await;
                assert!(peer.intents().await.is_empty());

                let id = peer
                    .add_relay(sunset_sync::Connectable::Direct(
                        sunset_sync::PeerAddr::new(bytes::Bytes::from_static(
                            b"wss://nowhere.invalid",
                        )),
                    ))
                    .await
                    .expect("add_relay records intent without bubbling transient errors");

                let snaps = peer.intents().await;
                assert_eq!(snaps.len(), 1);
                assert_eq!(snaps[0].id, id);
                assert_eq!(snaps[0].label, "wss://nowhere.invalid");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_text_inserts_a_text_entry() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(10)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                let now_ms = 1_700_000_000_000u64;
                let value_hash = room
                    .send_text("hello world".to_owned(), now_ms)
                    .await
                    .expect("send_text");

                // The store should now hold the content block under that hash.
                use sunset_store::Store as _;
                let block = peer
                    .store()
                    .get_content(&value_hash)
                    .await
                    .expect("get_content");
                assert!(block.is_some(), "content block missing");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn open_room_twice_returns_same_state() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(9)).await;
                let r1 = peer.open_room("alpha").await.expect("open_room r1");
                let r2 = peer.open_room("alpha").await.expect("open_room r2");
                assert_eq!(r1.fingerprint(), r2.fingerprint());
                // Internal: both handles share the same Rc<RoomState>.
                assert!(Rc::ptr_eq(&r1.inner, &r2.inner));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_open_room_for_same_name_coalesces() {
        // Race window check: two parallel open_room("alpha") calls must
        // return handles to the same RoomState. Without serialization
        // they'd both pass the idempotency check, both do Argon2 work,
        // both register signalers (second overwrites the first in the
        // dispatcher), leaking the first signaler's spawn_dispatcher.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(20)).await;
                let (r1, r2) = futures::join!(peer.open_room("alpha"), peer.open_room("alpha"));
                let r1 = r1.expect("open_room r1");
                let r2 = r2.expect("open_room r2");
                assert!(
                    Rc::ptr_eq(&r1.inner, &r2.inner),
                    "concurrent open_room must coalesce to one RoomState"
                );
                // The dispatcher should hold exactly one signaler for this fp,
                // not two.
                assert_eq!(peer.rtc_signaler_dispatcher.len(), 1);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_message_fires_for_self_send() {
        use std::cell::RefCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(11)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                let received: Rc<RefCell<Vec<(String, bool)>>> = Rc::new(RefCell::new(Vec::new()));
                let received_clone = received.clone();
                room.on_message(move |decoded, is_self| {
                    if let crate::MessageBody::Text { text, .. } = &decoded.body {
                        received_clone.borrow_mut().push((text.clone(), is_self));
                    }
                });

                let _ = room
                    .send_text("hello self".to_owned(), 1_700_000_000_000)
                    .await
                    .expect("send_text");

                // Yield repeatedly so the decode loop's spawn_local runs.
                for _ in 0..50 {
                    tokio::task::yield_now().await;
                }

                let got = received.borrow().clone();
                assert_eq!(got, vec![("hello self".to_owned(), true)]);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auto_ack_writes_a_receipt_for_inbound_text_from_peer() {
        // When a Text from a different identity lands in the store, the
        // decode loop must auto-write a Receipt back so the original
        // sender's UI can flip out of "pending" / gray. Without this,
        // delivery confirmations are silently broken (web/e2e/receipts.spec.js).
        use rand_core::SeedableRng;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(30)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                // Register a no-op on_message so the decode loop spawns.
                room.on_message(|_, _| {});

                // Compose a Text from a DIFFERENT identity (peer doesn't
                // ack its own messages) and insert into the store.
                let other_identity = ident(31);
                let mut rng = rand_chacha::ChaCha20Rng::from_seed([7; 32]);
                let composed = crate::compose_message(
                    &other_identity,
                    &room.inner.room,
                    crate::V1_EPOCH_ID,
                    1_700_000_000_000,
                    crate::ChannelLabel::default_general(),
                    crate::MessageBody::text("from-other"),
                    &mut rng,
                )
                .expect("compose_message");
                let text_value_hash = composed.entry.value_hash;
                use sunset_store::Store as _;
                peer.store()
                    .insert(composed.entry, Some(composed.block))
                    .await
                    .expect("insert text");

                // Yield so the decode loop runs the auto-ack.
                for _ in 0..50 {
                    tokio::task::yield_now().await;
                }

                // The store should now hold a Receipt entry authored by
                // OUR identity (not other_identity) referencing the
                // text's value_hash. Locate it via room_messages_filter
                // — Receipts share the `<fp>/msg/` namespace.
                use futures::StreamExt;
                use sunset_store::Replay;
                let filter = crate::filters::room_messages_filter(&room.inner.room);
                let mut sub = peer
                    .store()
                    .subscribe(filter, Replay::All)
                    .await
                    .expect("subscribe");

                let mut found_receipt = false;
                let our_pubkey = peer.identity().public();
                // Consume the replayed entries (text + receipt) and look
                // for our auto-ack Receipt.
                for _ in 0..5 {
                    let ev = match tokio::time::timeout(
                        std::time::Duration::from_millis(200),
                        sub.next(),
                    )
                    .await
                    {
                        Ok(Some(ev)) => ev,
                        _ => break,
                    };
                    let entry = match ev {
                        Ok(sunset_store::Event::Inserted(e)) => e,
                        _ => continue,
                    };
                    let block = peer
                        .store()
                        .get_content(&entry.value_hash)
                        .await
                        .expect("get_content")
                        .expect("block");
                    let decoded =
                        crate::decode_message(&room.inner.room, &entry, &block).expect("decode");
                    if let crate::MessageBody::Receipt { for_value_hash } = decoded.body {
                        if for_value_hash == text_value_hash && decoded.author_key == our_pubkey {
                            found_receipt = true;
                            break;
                        }
                    }
                }
                assert!(
                    found_receipt,
                    "expected an auto-ack Receipt by our identity for the inbound Text"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn send_text_in_channel_round_trips_channel() {
        // The channel passed to send_text_in_channel must round-trip
        // through the AEAD/sig envelope and surface in
        // `decoded.channel` on the on_message callback.
        use std::cell::RefCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(50)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                let received: Rc<RefCell<Vec<(String, String)>>> =
                    Rc::new(RefCell::new(Vec::new()));
                let received_cb = received.clone();
                room.on_message(move |decoded, _is_self| {
                    if let crate::MessageBody::Text { text, .. } = &decoded.body {
                        received_cb
                            .borrow_mut()
                            .push((decoded.channel.as_str().to_owned(), text.clone()));
                    }
                });

                let _ = room
                    .send_text_in_channel(
                        crate::ChannelLabel::try_new("links").expect("links label"),
                        "hi".to_owned(),
                        1,
                    )
                    .await
                    .expect("send_text_in_channel");

                for _ in 0..50 {
                    if !received.borrow().is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                let got = received.borrow().clone();
                assert_eq!(got, vec![("links".to_owned(), "hi".to_owned())]);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observed_channels_starts_with_default() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(51)).await;
                let room = peer.open_room("alpha").await.expect("open_room");
                let chans: Vec<String> = room
                    .observed_channels()
                    .iter()
                    .map(|c| c.as_str().to_owned())
                    .collect();
                assert_eq!(chans, vec!["general".to_owned()]);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_channels_changed_fires_with_default_immediately() {
        // Registering on_channels_changed should fire once with the
        // current snapshot (which always includes "general"), so a
        // host that registers late doesn't sit empty waiting for the
        // next channel-creating message.
        use std::cell::RefCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(52)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                let snapshots: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
                let snapshots_cb = snapshots.clone();
                room.on_channels_changed(move |chans| {
                    snapshots_cb
                        .borrow_mut()
                        .push(chans.iter().map(|c| c.as_str().to_owned()).collect());
                });

                // Don't yield — the immediate snapshot should already
                // be there synchronously.
                let got = snapshots.borrow().clone();
                assert_eq!(got, vec![vec!["general".to_owned()]]);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_channels_changed_callback_can_re_register_without_panicking() {
        // Regression test for the re-entrancy footgun in
        // OpenRoom::on_channels_changed: the immediate-fire path used
        // to invoke the user callback while a `RefCell` borrow on
        // RoomState::callbacks was still live, so a callback that
        // synchronously called back into any `on_*` registration would
        // panic with `BorrowMutError`. Construct that exact scenario:
        // from inside the on_channels_changed callback, register
        // on_message via a second `OpenRoom` handle built off the same
        // inner `Rc<RoomState>`. The fix boxes the callback first and
        // fires it before any borrow is held; without the fix, this
        // test would panic.
        use std::cell::RefCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(45)).await;
                let room = peer.open_room("alpha").await.expect("open_room");
                // OpenRoom isn't Clone; rebuild a handle off the same
                // inner Rc so the callback can call on_message on it.
                let room_for_cb = OpenRoom {
                    inner: room.inner.clone(),
                };
                let nested_registered: Rc<RefCell<bool>> = Rc::new(RefCell::new(false));
                let nested_registered_cb = nested_registered.clone();
                room.on_channels_changed(move |_chans| {
                    // Register on_message from inside the on_channels
                    // immediate-fire callback. Pre-fix: panics with
                    // BorrowMutError. Post-fix: succeeds.
                    room_for_cb.on_message(|_, _| {});
                    *nested_registered_cb.borrow_mut() = true;
                });
                assert!(
                    *nested_registered.borrow(),
                    "nested on_message registration must run inside the immediate-fire callback"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observed_channels_includes_new_channel_after_send() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(53)).await;
                let room = peer.open_room("alpha").await.expect("open_room");
                // Register a no-op on_message so the decode loop spawns and
                // observes the channel from the inserted Text.
                room.on_message(|_, _| {});

                let _ = room
                    .send_text_in_channel(
                        crate::ChannelLabel::try_new("links").expect("links label"),
                        "hi".to_owned(),
                        1,
                    )
                    .await
                    .expect("send_text_in_channel");

                // Poll observed_channels until "links" appears.
                let mut got: Vec<String> = Vec::new();
                for _ in 0..50 {
                    got = room
                        .observed_channels()
                        .iter()
                        .map(|c| c.as_str().to_owned())
                        .collect();
                    if got.contains(&"links".to_owned()) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                // BTreeSet sorts: "general" < "links".
                assert_eq!(got, vec!["general".to_owned(), "links".to_owned()]);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_channels_changed_fires_when_new_channel_arrives() {
        use std::cell::RefCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(54)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                let snapshots: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
                let snapshots_cb = snapshots.clone();
                room.on_channels_changed(move |chans| {
                    snapshots_cb
                        .borrow_mut()
                        .push(chans.iter().map(|c| c.as_str().to_owned()).collect());
                });

                let _ = room
                    .send_text_in_channel(
                        crate::ChannelLabel::try_new("links").expect("links label"),
                        "hi".to_owned(),
                        1,
                    )
                    .await
                    .expect("send_text_in_channel");

                // Wait for a second snapshot containing "links".
                for _ in 0..50 {
                    let has_links = snapshots
                        .borrow()
                        .iter()
                        .any(|s| s.contains(&"links".to_owned()));
                    if has_links {
                        break;
                    }
                    tokio::task::yield_now().await;
                }

                let got = snapshots.borrow().clone();
                // First snapshot is the immediate-on-register one
                // (default only). Some snapshot after that contains
                // the newly observed "links" channel alongside
                // "general" (sorted).
                assert!(
                    got.first() == Some(&vec!["general".to_owned()]),
                    "first snapshot should be the default-only snapshot, got {got:?}"
                );
                assert!(
                    got.iter()
                        .any(|s| s == &vec!["general".to_owned(), "links".to_owned()]),
                    "expected a snapshot of [general, links], got {got:?}"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auto_ack_receipt_inherits_target_channel() {
        // Drive the auto-ack helper directly with a non-default
        // channel; decode the resulting Receipt and assert its
        // `channel` matches what we passed in. The decode loop's
        // auto-ack path always passes the target Text's channel
        // through to `send_receipt`, so this exercises the contract.
        use rand_core::SeedableRng;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(55)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                let target: sunset_store::Hash = blake3::hash(b"target-text").into();
                let mut rng = rand_chacha::ChaCha20Rng::from_seed([42; 32]);
                let channel = crate::ChannelLabel::try_new("links").expect("links label");

                open_room::send_receipt(
                    peer.store(),
                    &room.inner.room,
                    peer.identity(),
                    target,
                    channel,
                    &mut rng,
                )
                .await;

                // Find the inserted Receipt and decode it.
                use futures::StreamExt;
                use sunset_store::{Replay, Store as _};
                let filter = crate::filters::room_messages_filter(&room.inner.room);
                let mut sub = peer
                    .store()
                    .subscribe(filter, Replay::All)
                    .await
                    .expect("subscribe");
                let ev = tokio::time::timeout(std::time::Duration::from_millis(200), sub.next())
                    .await
                    .expect("no event within 200ms")
                    .expect("subscription closed");
                let entry = match ev.expect("event err") {
                    sunset_store::Event::Inserted(e) => e,
                    other => panic!("unexpected event: {other:?}"),
                };
                let block = peer
                    .store()
                    .get_content(&entry.value_hash)
                    .await
                    .expect("get_content")
                    .expect("block");
                let decoded =
                    crate::decode_message(&room.inner.room, &entry, &block).expect("decode");
                assert!(matches!(decoded.body, crate::MessageBody::Receipt { .. }));
                assert_eq!(decoded.channel.as_str(), "links");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordered_messages_sorts_by_claimed_time_regardless_of_arrival_order() {
        // Insert two Text entries whose store-arrival order is reversed
        // relative to their sender-claimed `sent_at_ms`. The
        // `ordered_messages` snapshot must return them in claimed-time
        // order — that's the whole contract.
        use rand_core::SeedableRng;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(60)).await;
                let room = peer.open_room("alpha").await.expect("open_room");
                // Spawn the decode loop so the BTreeMap fills up as
                // entries land.
                room.on_message(|_, _| {});

                let other = ident(61);
                let mut rng = rand_chacha::ChaCha20Rng::from_seed([7; 32]);
                // Compose two texts: one claimed at t=100 (older), one at
                // t=200 (newer). Insert *the newer one first*.
                let older = crate::compose_text(
                    &other,
                    &room.inner.room,
                    crate::V1_EPOCH_ID,
                    100,
                    crate::ChannelLabel::default_general(),
                    "older",
                    &mut rng,
                )
                .expect("compose older");
                let newer = crate::compose_text(
                    &other,
                    &room.inner.room,
                    crate::V1_EPOCH_ID,
                    200,
                    crate::ChannelLabel::default_general(),
                    "newer",
                    &mut rng,
                )
                .expect("compose newer");

                use sunset_store::Store as _;
                peer.store()
                    .insert(newer.entry, Some(newer.block))
                    .await
                    .expect("insert newer first");
                peer.store()
                    .insert(older.entry, Some(older.block))
                    .await
                    .expect("insert older second");

                // Wait for both to land in the snapshot.
                let mut snap: Vec<crate::DecodedMessage> = Vec::new();
                for _ in 0..100 {
                    snap = room.ordered_messages();
                    if snap.len() >= 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                let texts: Vec<&str> = snap
                    .iter()
                    .filter_map(|m| match &m.body {
                        crate::MessageBody::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(
                    texts,
                    vec!["older", "newer"],
                    "ordered_messages must sort by sender-claimed sent_at_ms"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordered_messages_breaks_ties_deterministically() {
        // Two texts claiming the exact same `sent_at_ms` must appear in
        // a deterministic order so the UI doesn't flip-flop between
        // renders. Tie-break is on `value_hash` (content-addressed and
        // unique).
        use rand_core::SeedableRng;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(62)).await;
                let room = peer.open_room("alpha").await.expect("open_room");
                room.on_message(|_, _| {});

                let other_a = ident(63);
                let other_b = ident(64);
                let mut rng = rand_chacha::ChaCha20Rng::from_seed([13; 32]);
                let m_a = crate::compose_text(
                    &other_a,
                    &room.inner.room,
                    crate::V1_EPOCH_ID,
                    1000,
                    crate::ChannelLabel::default_general(),
                    "from-a",
                    &mut rng,
                )
                .expect("compose a");
                let m_b = crate::compose_text(
                    &other_b,
                    &room.inner.room,
                    crate::V1_EPOCH_ID,
                    1000,
                    crate::ChannelLabel::default_general(),
                    "from-b",
                    &mut rng,
                )
                .expect("compose b");

                use sunset_store::Store as _;
                // Insertion order #1: a, b
                peer.store()
                    .insert(m_a.entry.clone(), Some(m_a.block.clone()))
                    .await
                    .expect("insert a");
                peer.store()
                    .insert(m_b.entry.clone(), Some(m_b.block.clone()))
                    .await
                    .expect("insert b");

                let mut order1: Vec<sunset_store::Hash> = Vec::new();
                for _ in 0..100 {
                    order1 = room
                        .ordered_messages()
                        .iter()
                        .map(|m| m.value_hash)
                        .collect();
                    if order1.len() >= 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert_eq!(order1.len(), 2);

                // Now build a *second* peer and insert in reverse order
                // (b, a). The resulting snapshot order must match the
                // first peer's — i.e. the order doesn't depend on
                // arrival order when sent_at_ms ties.
                let peer2 = helpers::mk_peer(ident(65)).await;
                let room2 = peer2.open_room("alpha").await.expect("open_room2");
                room2.on_message(|_, _| {});
                peer2
                    .store()
                    .insert(m_b.entry, Some(m_b.block))
                    .await
                    .expect("insert b first on peer2");
                peer2
                    .store()
                    .insert(m_a.entry, Some(m_a.block))
                    .await
                    .expect("insert a second on peer2");
                let mut order2: Vec<sunset_store::Hash> = Vec::new();
                for _ in 0..100 {
                    order2 = room2
                        .ordered_messages()
                        .iter()
                        .map(|m| m.value_hash)
                        .collect();
                    if order2.len() >= 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert_eq!(order1, order2, "tie-breaking must be deterministic");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ordered_messages_excludes_receipts_and_reactions() {
        // The snapshot is the rendered-message timeline. Receipts and
        // Reactions are side data — they must not appear.
        use rand_core::SeedableRng;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(66)).await;
                let room = peer.open_room("alpha").await.expect("open_room");
                room.on_message(|_, _| {});

                let other = ident(67);
                let mut rng = rand_chacha::ChaCha20Rng::from_seed([5; 32]);
                let text = crate::compose_text(
                    &other,
                    &room.inner.room,
                    crate::V1_EPOCH_ID,
                    1,
                    crate::ChannelLabel::default_general(),
                    "real-text",
                    &mut rng,
                )
                .expect("compose text");
                let text_hash = text.entry.value_hash;
                let receipt = crate::compose_receipt(
                    &other,
                    &room.inner.room,
                    crate::V1_EPOCH_ID,
                    2,
                    crate::ChannelLabel::default_general(),
                    text_hash,
                    &mut rng,
                )
                .expect("compose receipt");
                let reaction = crate::compose_reaction(
                    &other,
                    &room.inner.room,
                    crate::V1_EPOCH_ID,
                    3,
                    crate::ChannelLabel::default_general(),
                    &crate::ReactionPayload {
                        for_value_hash: text_hash,
                        emoji: "👍",
                        action: crate::ReactionAction::Add,
                    },
                    &mut rng,
                )
                .expect("compose reaction");

                use sunset_store::Store as _;
                peer.store()
                    .insert(text.entry, Some(text.block))
                    .await
                    .expect("insert text");
                peer.store()
                    .insert(receipt.entry, Some(receipt.block))
                    .await
                    .expect("insert receipt");
                peer.store()
                    .insert(reaction.entry, Some(reaction.block))
                    .await
                    .expect("insert reaction");

                let mut snap = Vec::new();
                for _ in 0..100 {
                    snap = room.ordered_messages();
                    if !snap.is_empty() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                let bodies: Vec<&str> = snap
                    .iter()
                    .filter_map(|m| match &m.body {
                        crate::MessageBody::Text { text, .. } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                assert_eq!(bodies, vec!["real-text"]);
                // Yield more to give receipt + reaction a chance to be
                // misclassified (they shouldn't be).
                for _ in 0..50 {
                    tokio::task::yield_now().await;
                }
                let bodies_after: Vec<crate::MessageBody> = room
                    .ordered_messages()
                    .into_iter()
                    .map(|m| m.body)
                    .collect();
                assert_eq!(
                    bodies_after.len(),
                    1,
                    "snapshot must contain only Text bodies, got {bodies_after:?}"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_messages_changed_fires_with_empty_snapshot_immediately() {
        // Mirror on_channels_changed: registering must fire once with
        // the current (possibly empty) snapshot so a late-registering
        // host doesn't sit empty waiting for the next message.
        use std::cell::RefCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(68)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                let snapshots: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
                let snapshots_cb = snapshots.clone();
                room.on_messages_changed(move |msgs| {
                    snapshots_cb.borrow_mut().push(msgs.len());
                });

                assert_eq!(snapshots.borrow().clone(), vec![0]);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_messages_changed_fires_sorted_snapshot_on_out_of_order_arrival() {
        // Insert (newer, older) order; the callback must eventually fire
        // a snapshot containing [older, newer] in claimed-time order.
        use rand_core::SeedableRng;
        use std::cell::RefCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(69)).await;
                let room = peer.open_room("alpha").await.expect("open_room");
                let snapshots: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
                let snapshots_cb = snapshots.clone();
                room.on_messages_changed(move |msgs| {
                    let texts: Vec<String> = msgs
                        .iter()
                        .filter_map(|m| match &m.body {
                            crate::MessageBody::Text { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .collect();
                    snapshots_cb.borrow_mut().push(texts);
                });

                let other = ident(70);
                let mut rng = rand_chacha::ChaCha20Rng::from_seed([9; 32]);
                let older = crate::compose_text(
                    &other,
                    &room.inner.room,
                    crate::V1_EPOCH_ID,
                    100,
                    crate::ChannelLabel::default_general(),
                    "older",
                    &mut rng,
                )
                .expect("compose older");
                let newer = crate::compose_text(
                    &other,
                    &room.inner.room,
                    crate::V1_EPOCH_ID,
                    200,
                    crate::ChannelLabel::default_general(),
                    "newer",
                    &mut rng,
                )
                .expect("compose newer");

                use sunset_store::Store as _;
                peer.store()
                    .insert(newer.entry, Some(newer.block))
                    .await
                    .expect("insert newer first");
                peer.store()
                    .insert(older.entry, Some(older.block))
                    .await
                    .expect("insert older second");

                // Wait for a snapshot containing both texts in
                // claimed-time order.
                let target = vec!["older".to_owned(), "newer".to_owned()];
                let mut saw_target = false;
                for _ in 0..200 {
                    if snapshots.borrow().iter().any(|s| s == &target) {
                        saw_target = true;
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert!(
                    saw_target,
                    "expected a snapshot of [older, newer]; observed {:?}",
                    snapshots.borrow()
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_receipt_fires_for_inserted_receipt() {
        use rand_core::SeedableRng;
        use std::cell::RefCell;
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(12)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                let received: Rc<RefCell<Vec<(sunset_store::Hash, u64)>>> =
                    Rc::new(RefCell::new(Vec::new()));
                let received_clone = received.clone();
                // Register a no-op on_message so the decode loop spawns even
                // though we only care about receipts here. (The loop spawns on
                // first on_message OR on_receipt registration — either works.)
                room.on_message(|_, _| {});
                room.on_receipt(
                    move |for_hash,
                          _from: &crate::IdentityKey,
                          _channel: &crate::ChannelLabel,
                          sent_at_ms| {
                        received_clone.borrow_mut().push((for_hash, sent_at_ms));
                    },
                );

                // Compose+insert a Receipt referencing some target hash.
                let target: sunset_store::Hash = blake3::hash(b"target").into();
                let mut rng = rand_chacha::ChaCha20Rng::from_seed([42; 32]);
                let composed = crate::compose_receipt(
                    peer.identity(),
                    &room.inner.room,
                    0,
                    1_700_000_000_000,
                    crate::ChannelLabel::default_general(),
                    target,
                    &mut rng,
                )
                .expect("compose_receipt");
                use sunset_store::Store as _;
                peer.store()
                    .insert(composed.entry, Some(composed.block))
                    .await
                    .expect("insert receipt");

                for _ in 0..50 {
                    tokio::task::yield_now().await;
                }
                assert_eq!(received.borrow().clone(), vec![(target, 1_700_000_000_000)]);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn start_presence_publishes_a_heartbeat_entry() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(13)).await;
                let room = peer.open_room("alpha").await.expect("open_room");
                let my_hex = hex::encode(peer.public_key());

                room.start_presence(50, 1000, 100).await;

                // Wait for the publisher's first iteration.
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;

                use futures::StreamExt;
                use sunset_store::{Filter, Replay, Store as _};
                let presence_filter = Filter::NamePrefix(bytes::Bytes::from(format!(
                    "{}/presence/{}",
                    room.fingerprint().to_hex(),
                    my_hex,
                )));
                let mut sub = peer
                    .store()
                    .subscribe(presence_filter, Replay::All)
                    .await
                    .expect("subscribe");
                let ev = tokio::time::timeout(std::time::Duration::from_millis(500), sub.next())
                    .await
                    .expect("no presence entry within 500ms")
                    .expect("subscription closed");
                assert!(matches!(ev, Ok(sunset_store::Event::Inserted(_))));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_self_name_updates_every_open_room() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(40)).await;
                let alpha = peer.open_room("alpha").await.expect("open_room alpha");
                let beta = peer.open_room("beta").await.expect("open_room beta");
                alpha.start_presence(50, 1000, 100).await;
                beta.start_presence(50, 1000, 100).await;

                peer.set_self_name("alice");

                // After the immediate republish, both rooms' presence bodies
                // should decode to name = Some("alice").
                let pk_hex = hex::encode(peer.public_key());
                for (room_fp_hex, label) in [
                    (alpha.fingerprint().to_hex(), "alpha"),
                    (beta.fingerprint().to_hex(), "beta"),
                ] {
                    let key = format!("{room_fp_hex}/presence/{pk_hex}");
                    let store = peer.store().clone();
                    // Wait up to 1s for the body name to flip to Some("alice").
                    let mut found = false;
                    for _ in 0..100 {
                        if let Some(body) = read_presence_body(&store, &key).await {
                            if body.name == Some("alice".to_owned()) {
                                found = true;
                                break;
                            }
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                    assert!(found, "body name never became Some(alice) for room {label}");
                }
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn set_self_name_before_open_room_persists_via_pending_cache() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let peer = helpers::mk_peer(ident(41)).await;
                // CRITICAL ORDER: set the name BEFORE opening the room.
                peer.set_self_name("alice");
                let alpha = peer.open_room("alpha").await.expect("open_room alpha");
                alpha.start_presence(50, 1000, 100).await;

                let room_fp_hex = alpha.fingerprint().to_hex();
                let pk_hex = hex::encode(peer.public_key());
                let key = format!("{room_fp_hex}/presence/{pk_hex}");
                let store = peer.store().clone();

                let mut found = false;
                for _ in 0..100 {
                    if let Some(body) = read_presence_body(&store, &key).await {
                        if body.name == Some("alice".to_owned()) {
                            found = true;
                            break;
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                assert!(
                    found,
                    "presence body never picked up alice from pending cache"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn decode_loop_cancels_on_open_room_drop() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(14)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                // Drop the OpenRoom handle; verify cancel was set.
                let cancel = room.inner.cancel_decode.clone();
                drop(room);
                // The Drop impl on RoomState fires cancel_decode = true.
                // Yield so the decode loop notices (we don't actually assert
                // its termination here — just that the cancel signal is set,
                // which structurally guarantees its exit).
                tokio::task::yield_now().await;
                assert!(
                    cancel.get(),
                    "cancel_decode should be set after OpenRoom drop"
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn room_drop_unsubscribes_engine_broadcast_intent() {
        // Regression: `Peer::open_room` calls
        // `engine.subscribe(room_filter, ...)` which records a
        // `BroadcastIntent` in the engine's routing state. Pre-fix,
        // `RoomState::drop` tore down only `cancel_decode` and the
        // signaler dispatcher — it never called the matching
        // `engine.unsubscribe(filter)`, so every open/close cycle
        // leaked a permanent broadcast intent. The PeerHello
        // auto-resubscriber replayed those intents on every new
        // connection, growing SubscriptionEntry traffic linearly with
        // session-time room reopens. After the fix, dropping the
        // `OpenRoom` must remove the broadcast intent.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(80)).await;
                let room = peer.open_room("alpha").await.expect("open_room");
                let filter = crate::filters::room_filter(&room.inner.room);

                // Pre-drop: the broadcast intent for the room filter
                // is recorded in engine routing state.
                let pre = peer.engine().broadcast_intent_filters_snapshot().await;
                assert!(
                    pre.iter().any(|f| f == &filter),
                    "expected open_room to record a BroadcastIntent for room_filter; \
                     snapshot pre-drop = {pre:?}",
                );

                drop(room);

                // Drop kicks the unsubscribe off via spawn_local; the
                // engine command channel round-trip then needs the
                // engine task to pump it. Poll a few times so the
                // chain (spawn_local task -> cmd_tx -> engine run-loop
                // -> remove intent) can complete.
                let mut post: Vec<sunset_store::Filter> = Vec::new();
                for _ in 0..200 {
                    post = peer.engine().broadcast_intent_filters_snapshot().await;
                    if !post.iter().any(|f| f == &filter) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert!(
                    !post.iter().any(|f| f == &filter),
                    "RoomState::drop must unsubscribe the room filter; \
                     snapshot post-drop still contains it: {post:?}",
                );
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn peer_connection_mode_reads_from_tracker() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(15)).await;
                let room = peer.open_room("alpha").await.expect("open_room");
                let bogus_pk = [9u8; 32];
                // Without start_presence, peer_kinds is empty → "unknown"
                assert_eq!(room.peer_connection_mode(bogus_pk), "unknown");

                // Inject a kind manually (test-only, via the tracker handle).
                use sunset_sync::{PeerId, TransportKind};
                let pk = PeerId(sunset_store::VerifyingKey::new(
                    bytes::Bytes::copy_from_slice(&bogus_pk),
                ));
                room.inner
                    .tracker_handles
                    .peer_kinds
                    .borrow_mut()
                    .insert(pk, TransportKind::Secondary);
                assert_eq!(room.peer_connection_mode(bogus_pk), "direct");
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn on_members_changed_clears_last_signature() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(16)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                // Seed last_signature with something non-empty so we can verify
                // the registration clears it. MemberSig = Vec<(Vec<u8>, Presence, ConnectionMode, Option<String>)>
                room.inner
                    .tracker_handles
                    .last_signature
                    .borrow_mut()
                    .push((
                        vec![1, 2, 3],
                        crate::membership::Presence::Online,
                        crate::membership::ConnectionMode::Direct,
                        None,
                    ));
                assert!(
                    !room
                        .inner
                        .tracker_handles
                        .last_signature
                        .borrow()
                        .is_empty()
                );

                room.on_members_changed(|_| {});
                assert!(
                    room.inner
                        .tracker_handles
                        .last_signature
                        .borrow()
                        .is_empty(),
                    "last_signature should be cleared after on_members_changed registration"
                );
            })
            .await;
    }

    /// `connect_direct` should register a supervisor intent (visible
    /// via `Peer::intents()`) and return its `IntentId` so
    /// session-scoped callers (e.g. the voice runtime) can later
    /// `cancel_direct` it.
    #[tokio::test(flavor = "current_thread")]
    async fn connect_direct_returns_intent_id_and_registers_intent() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(60)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                let other_pk = [42u8; 32];
                let id = room
                    .connect_direct(other_pk)
                    .await
                    .expect("connect_direct returns IntentId");

                // The supervisor should now have one intent (the direct
                // one) and its label should be the canonical
                // `webrtc://<pk>#x25519=<x>` URL.
                let snaps = peer.intents().await;
                assert_eq!(snaps.len(), 1);
                assert_eq!(snaps[0].id, id);
                assert!(
                    snaps[0].label.starts_with("webrtc://"),
                    "expected webrtc:// label, got {}",
                    snaps[0].label
                );
            })
            .await;
    }

    /// `cancel_direct` should drop the supervisor intent so a follow-up
    /// `connect_direct` for the same peer creates a *fresh* intent
    /// rather than deduping against the cancelled one.
    ///
    /// This is the load-bearing invariant for the voice
    /// leave-and-rejoin path: the WebDialer drops on `voice_stop` and
    /// runs `cancel_direct` for every intent it accumulated; the next
    /// `voice_start` then dials cleanly instead of short-circuiting
    /// against a stale `Connected` / `Backoff` intent whose underlying
    /// WebRTC connection died silently during the gap.
    #[tokio::test(flavor = "current_thread")]
    async fn connect_direct_dedupes_until_cancelled_then_fresh_intent() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(61)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                let other_pk = [42u8; 32];
                let id_1 = room.connect_direct(other_pk).await.expect("first dial");

                // Repeat call dedupes (supervisor returns existing id).
                let id_again = room.connect_direct(other_pk).await.expect("dedup dial");
                assert_eq!(
                    id_again, id_1,
                    "supervisor.add should dedupe by Connectable identity"
                );
                assert_eq!(peer.intents().await.len(), 1);

                // Cancel: the intent disappears from snapshots.
                room.cancel_direct(id_1).await;
                assert!(
                    peer.intents().await.iter().all(|s| s.id != id_1),
                    "cancelled intent should be removed from snapshot"
                );

                // Fresh dial after cancel: a NEW IntentId is allocated.
                let id_2 = room
                    .connect_direct(other_pk)
                    .await
                    .expect("fresh dial after cancel");
                assert_ne!(
                    id_2, id_1,
                    "after cancel_direct the dedup slate is clear so a fresh \
                     connect_direct must allocate a new IntentId"
                );
            })
            .await;
    }

    /// `cancel_direct` is a no-op for an `IntentId` that doesn't exist
    /// (already-cancelled, or never registered). This is the
    /// safety net for the WebDialer's `Drop`-side cleanup: if voice
    /// drops before the supervisor processes the original Add (rare),
    /// the cleanup must not panic.
    #[tokio::test(flavor = "current_thread")]
    async fn cancel_direct_is_noop_for_unknown_intent() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let peer = helpers::mk_peer(ident(62)).await;
                let room = peer.open_room("alpha").await.expect("open_room");

                // Cancel an IntentId we never registered — must complete
                // cleanly without panic, without error, without state
                // leaking onto the supervisor.
                room.cancel_direct(99_999).await;
                assert!(peer.intents().await.is_empty());
            })
            .await;
    }

    /// Read the current `PresenceBody` for an exact store key.
    /// Returns `None` if no entry is present yet.
    async fn read_presence_body(
        store: &std::sync::Arc<MemoryStore>,
        name: &str,
    ) -> Option<crate::membership::PresenceBody> {
        use bytes::Bytes;
        use futures::StreamExt;
        use sunset_store::{Filter, Replay, Store as _};
        let mut sub = store
            .subscribe(Filter::Namespace(Bytes::from(name.to_owned())), Replay::All)
            .await
            .ok()?;
        let ev = sub.next().await?.ok()?;
        let entry = match ev {
            sunset_store::Event::Inserted(e) => e,
            sunset_store::Event::Replaced { new, .. } => new,
            _ => return None,
        };
        let block = store.get_content(&entry.value_hash).await.ok()??;
        postcard::from_bytes(&block.data).ok()
    }

    pub(super) mod helpers {
        use super::*;
        use async_trait::async_trait;
        use bytes::Bytes;
        use sunset_sync::types::PeerAddr;
        use sunset_sync::{
            BackoffPolicy, PeerId, SyncConfig, SyncEngine, Transport, TransportConnection,
            TransportKind,
        };

        /// Stub transport for unit tests that don't exercise the network.
        pub(crate) struct NopTransport;

        #[async_trait(?Send)]
        impl Transport for NopTransport {
            type Connection = NopConnection;

            async fn connect(&self, _addr: PeerAddr) -> sunset_sync::Result<Self::Connection> {
                Err(sunset_sync::Error::Transport("nop".into()))
            }

            async fn accept(&self) -> sunset_sync::Result<Self::Connection> {
                std::future::pending().await
            }
        }

        pub(crate) struct NopConnection;

        #[async_trait(?Send)]
        impl TransportConnection for NopConnection {
            async fn send_reliable(&self, _bytes: Bytes) -> sunset_sync::Result<()> {
                Ok(())
            }

            async fn recv_reliable(&self) -> sunset_sync::Result<Bytes> {
                std::future::pending().await
            }

            async fn send_unreliable(&self, _bytes: Bytes) -> sunset_sync::Result<()> {
                Ok(())
            }

            async fn recv_unreliable(&self) -> sunset_sync::Result<Bytes> {
                std::future::pending().await
            }

            fn peer_id(&self) -> PeerId {
                // Unreachable in tests since connect() always errors and accept()
                // never resolves, but we need a valid impl.
                PeerId(sunset_store::VerifyingKey::new(Bytes::from(vec![0u8; 32])))
            }

            fn kind(&self) -> TransportKind {
                TransportKind::Unknown
            }

            async fn close(&self) -> sunset_sync::Result<()> {
                Ok(())
            }
        }

        pub(crate) async fn mk_peer(identity: Identity) -> Rc<Peer<MemoryStore, NopTransport>> {
            let store = Arc::new(MemoryStore::new(Arc::new(Ed25519Verifier)));
            let signer: Arc<dyn sunset_sync::Signer> = Arc::new(identity.clone());
            let local_peer = PeerId(identity.store_verifying_key());
            let engine = Rc::new(SyncEngine::new(
                store.clone(),
                NopTransport,
                SyncConfig::default(),
                local_peer,
                signer,
            ));
            sunset_sync::spawn::spawn_local({
                let e = engine.clone();
                async move {
                    let _ = e.run().await;
                }
            });
            let supervisor = PeerSupervisor::new(engine.clone(), BackoffPolicy::default());
            sunset_sync::spawn::spawn_local({
                let s = supervisor.clone();
                async move { s.run().await }
            });
            let dispatcher = MultiRoomSignaler::new();
            Peer::new(identity, store, engine, supervisor, dispatcher)
        }
    }
}
