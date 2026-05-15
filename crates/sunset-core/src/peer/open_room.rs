//! `OpenRoom` is the per-room handle returned by `Peer::open_room`.
//! Method bodies (send_text, on_message, presence, etc.) are filled in
//! by Phase 5+ of the multi-room plan. This file currently only declares
//! the data shape so `Peer` can hold its registry of `Weak<RoomState>`.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::rc::{Rc, Weak};

use sunset_store::Store;
use sunset_sync::Transport;

use crate::ChannelLabel;
use crate::crypto::room::Room;
use crate::membership::TrackerHandles;
use crate::message::DecodedMessage;

pub(crate) type MessageCallback = Box<dyn Fn(&DecodedMessage, bool /* is_self */)>;
pub(crate) type ReceiptCallback =
    Box<dyn Fn(sunset_store::Hash, &crate::IdentityKey, &ChannelLabel, u64)>;
pub(crate) type ChannelsCallback = Box<dyn Fn(&[ChannelLabel])>;
pub(crate) type MessagesCallback = Box<dyn Fn(&[DecodedMessage])>;

/// Total-order key for the sorted Text-message index. Primary sort is
/// the sender-claimed `sent_at_ms` (so the timeline renders in author
/// chronology, not arrival order). Tie-break on `value_hash` because
/// it's content-addressed and unique — gives a deterministic order
/// even when two messages claim the same millisecond, so renders don't
/// flip-flop between sessions.
type MessageKey = (u64, sunset_store::Hash);

#[derive(Default)]
pub(crate) struct RoomCallbacks {
    pub(crate) on_message: Option<MessageCallback>,
    pub(crate) on_receipt: Option<ReceiptCallback>,
    pub(crate) on_channels: Option<ChannelsCallback>,
    pub(crate) on_messages: Option<MessagesCallback>,
}

impl RoomCallbacks {
    /// True iff no decode-loop-driven callbacks are registered. Used by
    /// the `on_*` registration methods to decide whether *this*
    /// registration should kick off the per-room decode loop. Adding a
    /// new callback slot only needs one update here.
    fn is_empty(&self) -> bool {
        self.on_message.is_none()
            && self.on_receipt.is_none()
            && self.on_channels.is_none()
            && self.on_messages.is_none()
    }
}

pub(crate) struct RoomState<St: Store + 'static, T: Transport + 'static> {
    pub(crate) room: Rc<Room>,
    pub(crate) peer_weak: Weak<super::Peer<St, T>>,
    pub(crate) presence_started: Cell<bool>,
    pub(crate) publisher: RefCell<Option<crate::membership::PublisherHandle>>,
    pub(crate) tracker_handles: Rc<TrackerHandles>,
    pub(crate) reaction_handles: crate::reactions::ReactionHandles,
    pub(crate) cancel_decode: Rc<Cell<bool>>,
    pub(crate) callbacks: Rc<RefCell<RoomCallbacks>>,
    /// Sorted set of channels observed in the decode loop. Always
    /// contains `ChannelLabel::default_general()` (seeded at room
    /// open). Mutations fire `on_channels` with the full sorted
    /// snapshot.
    pub(crate) observed_channels: Rc<RefCell<BTreeSet<ChannelLabel>>>,
    /// Sorted index of decoded Text messages by `(sent_at_ms,
    /// value_hash)`. Built up by `spawn_decode_loop` so all clients
    /// share one source of "messages in claimed-time order" without
    /// each having to sort. Only `Text` bodies land here; Receipts
    /// and Reactions are side data and stay out of the timeline.
    pub(crate) decoded_text_messages: Rc<RefCell<BTreeMap<MessageKey, DecodedMessage>>>,
}

pub struct OpenRoom<St: Store + 'static, T: Transport + 'static> {
    pub(crate) inner: Rc<RoomState<St, T>>,
}

impl<St: Store + 'static, T: Transport + 'static> OpenRoom<St, T> {
    pub fn fingerprint(&self) -> crate::crypto::room::RoomFingerprint {
        self.inner.room.fingerprint()
    }

    /// Return a reference-counted handle to the `Room` key material.
    pub fn room(&self) -> Rc<Room> {
        self.inner.room.clone()
    }

    /// Local identity's public key, if the parent `Peer` is still alive.
    /// Hosts use this to compute `is_self` for messages in
    /// `ordered_messages` without needing to thread the identity through
    /// their own state separately. `None` when the parent `Peer` has
    /// been dropped (the `Weak` upgrade failed).
    pub fn local_identity_key(&self) -> Option<crate::IdentityKey> {
        self.inner
            .peer_weak
            .upgrade()
            .map(|p| p.identity().public())
    }

    /// Convenience wrapper: sends a text message under the default
    /// `general` channel. New callers that want explicit channel
    /// routing should use [`OpenRoom::send_text_in_channel`].
    pub async fn send_text(
        &self,
        body: String,
        sent_at_ms: u64,
    ) -> crate::Result<sunset_store::Hash> {
        self.send_text_in_channel(ChannelLabel::default_general(), body, sent_at_ms)
            .await
    }

    /// Compose and insert a Text message under `channel`. Returns the
    /// composed entry's `value_hash`.
    pub async fn send_text_in_channel(
        &self,
        channel: ChannelLabel,
        body: String,
        sent_at_ms: u64,
    ) -> crate::Result<sunset_store::Hash> {
        self.send_post_in_channel(channel, body, Vec::new(), sent_at_ms)
            .await
    }

    /// Compose and insert a chat post (text + optional image attachments)
    /// under `channel`. An image-only post is fine — pass `body = ""`
    /// with a non-empty `images`. Returns the composed entry's
    /// `value_hash`.
    pub async fn send_post_in_channel(
        &self,
        channel: ChannelLabel,
        body: String,
        images: Vec<crate::ImageAttachment>,
        sent_at_ms: u64,
    ) -> crate::Result<sunset_store::Hash> {
        use crate::compose_post;
        use crate::message::PostPayload;
        use rand_chacha::ChaCha20Rng;
        use rand_core::SeedableRng;

        let peer = self
            .inner
            .peer_weak
            .upgrade()
            .ok_or_else(|| crate::Error::Other("peer dropped".into()))?;

        let mut rng = ChaCha20Rng::from_entropy();
        let composed = compose_post(
            peer.identity(),
            &self.inner.room,
            crate::V1_EPOCH_ID,
            sent_at_ms,
            channel,
            &PostPayload {
                text: &body,
                images: &images,
            },
            &mut rng,
        )?;

        let value_hash = composed.entry.value_hash;
        peer.store()
            .insert(composed.entry, Some(composed.block))
            .await
            .map_err(|e| crate::Error::Other(format!("store insert: {e}")))?;
        Ok(value_hash)
    }

    pub fn on_message<F: Fn(&DecodedMessage, bool) + 'static>(&self, cb: F) {
        let mut cbs = self.inner.callbacks.borrow_mut();
        let was_unregistered = cbs.is_empty();
        cbs.on_message = Some(Box::new(cb));
        drop(cbs);

        // First on_message / on_receipt / on_channels_changed call
        // kicks off the decode loop.
        if was_unregistered {
            self.spawn_decode_loop();
        }
    }

    pub fn on_receipt<
        F: Fn(sunset_store::Hash, &crate::IdentityKey, &ChannelLabel, u64) + 'static,
    >(
        &self,
        cb: F,
    ) {
        let mut cbs = self.inner.callbacks.borrow_mut();
        let was_unregistered = cbs.is_empty();
        cbs.on_receipt = Some(Box::new(cb));
        drop(cbs);
        if was_unregistered {
            self.spawn_decode_loop();
        }
    }

    /// Register a callback that fires whenever the set of observed
    /// channels in this room grows. The callback is fired *once
    /// immediately* with the current sorted snapshot (so a host that
    /// registers late doesn't sit on a quiet stream waiting for the
    /// next message), then again every time a newly-observed channel
    /// is added by the decode loop.
    ///
    /// The set always contains `ChannelLabel::default_general()`.
    pub fn on_channels_changed<F: Fn(&[ChannelLabel]) + 'static>(&self, cb: F) {
        // Box the callback once so we can both invoke it now and stash
        // it for later. Fire immediately with the current snapshot
        // *before any borrow is held* on `self.inner.callbacks`, so a
        // user callback that synchronously re-registers any `on_*`
        // handler can't deadlock on `RoomState`'s `RefCell` with a
        // `BorrowMutError`.
        let boxed: ChannelsCallback = Box::new(cb);
        let snap = self.observed_channels();
        boxed(&snap);

        let mut cbs = self.inner.callbacks.borrow_mut();
        let was_unregistered = cbs.is_empty();
        cbs.on_channels = Some(boxed);
        drop(cbs);

        if was_unregistered {
            self.spawn_decode_loop();
        }
    }

    /// Sorted snapshot of channels observed in this room so far.
    /// Always contains `ChannelLabel::default_general()`.
    pub fn observed_channels(&self) -> Vec<ChannelLabel> {
        self.inner
            .observed_channels
            .borrow()
            .iter()
            .cloned()
            .collect()
    }

    /// Snapshot of every decoded Text message in this room, sorted by
    /// sender-claimed `sent_at_ms` ascending, tie-broken on
    /// `value_hash`. Receipts and Reactions are not included — the
    /// timeline only renders Text bodies. Built up by the decode loop;
    /// callers that register `on_message`, `on_receipt`,
    /// `on_channels_changed`, or `on_messages_changed` start the loop
    /// and the index fills up as entries land in the store.
    pub fn ordered_messages(&self) -> Vec<DecodedMessage> {
        self.inner
            .decoded_text_messages
            .borrow()
            .values()
            .cloned()
            .collect()
    }

    /// Register a callback fired with the current sorted-by-claimed-time
    /// Text snapshot (immediately on register, then again after every
    /// change). Lets thin clients drop their own ordering logic and
    /// just render what arrives. Mirrors `on_channels_changed`'s
    /// register-late-get-current-state shape, including the
    /// re-entrancy guarantee (the immediate fire happens before any
    /// borrow on `callbacks` is held, so a callback that synchronously
    /// re-registers any `on_*` handler doesn't panic with
    /// `BorrowMutError`).
    pub fn on_messages_changed<F: Fn(&[DecodedMessage]) + 'static>(&self, cb: F) {
        let boxed: MessagesCallback = Box::new(cb);
        let snap = self.ordered_messages();
        boxed(&snap);

        let mut cbs = self.inner.callbacks.borrow_mut();
        let was_unregistered = cbs.is_empty();
        cbs.on_messages = Some(boxed);
        drop(cbs);

        if was_unregistered {
            self.spawn_decode_loop();
        }
    }

    fn spawn_decode_loop(&self) {
        let inner = self.inner.clone();
        let peer = match inner.peer_weak.upgrade() {
            Some(p) => p,
            None => return,
        };
        let store = peer.store().clone();
        let identity = peer.identity().clone();
        let identity_pub = identity.public();
        let room = inner.room.clone();
        let cancel = inner.cancel_decode.clone();
        let callbacks = inner.callbacks.clone();

        sunset_sync::spawn::spawn_local(async move {
            use futures::StreamExt;
            use rand_chacha::ChaCha20Rng;
            use rand_core::SeedableRng;
            use std::collections::HashSet;
            use sunset_store::{Event, Replay};

            // Session-only dedup: which Text value-hashes have we
            // already auto-acked since this decode loop started?
            // `Replay::All` redelivers them on subscribe; without this
            // we'd write a fresh Receipt every time. Cross-session
            // dedup is out of scope for v1.
            let mut acked: HashSet<sunset_store::Hash> = HashSet::new();
            let mut rng = ChaCha20Rng::from_entropy();

            let filter = crate::filters::room_messages_filter(&room);
            let mut events = match store.subscribe(filter, Replay::All).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("OpenRoom decode subscribe: {e}");
                    return;
                }
            };
            while let Some(ev) = events.next().await {
                if cancel.get() {
                    return;
                }
                let entry = match ev {
                    Ok(Event::Inserted(e)) => e,
                    Ok(Event::Replaced { new, .. }) => new,
                    Ok(_) => continue,
                    Err(e) => {
                        tracing::error!("OpenRoom decode event: {e}");
                        continue;
                    }
                };
                let block = match store.get_content(&entry.value_hash).await {
                    Ok(Some(b)) => b,
                    Ok(None) => continue,
                    Err(e) => {
                        tracing::error!("OpenRoom decode get_content: {e}");
                        continue;
                    }
                };
                let decoded = match crate::message::decode_message(&room, &entry, &block) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::error!("OpenRoom decode_message: {e}");
                        continue;
                    }
                };

                let is_self = decoded.author_key == identity_pub;
                // For Text messages: insert into the sorted index
                // *before* firing callbacks so a synchronous handler
                // that reads `ordered_messages()` sees the new entry.
                // Detect novelty here too — Replay::All re-emits an
                // entry whose key is already in the index, and we
                // want to skip re-firing `on_messages_changed` for
                // duplicates so subscribers don't see a stream of
                // identical snapshots.
                let messages_changed = if let crate::MessageBody::Text { .. } = &decoded.body {
                    let key = (decoded.sent_at_ms, decoded.value_hash);
                    let mut map = inner.decoded_text_messages.borrow_mut();
                    if let std::collections::btree_map::Entry::Vacant(e) = map.entry(key) {
                        e.insert(decoded.clone());
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };

                {
                    let cbs = callbacks.borrow();
                    match &decoded.body {
                        crate::MessageBody::Text { .. } => {
                            if let Some(cb) = cbs.on_message.as_ref() {
                                cb(&decoded, is_self);
                            }
                        }
                        crate::MessageBody::Receipt { for_value_hash } => {
                            if let Some(cb) = cbs.on_receipt.as_ref() {
                                cb(
                                    *for_value_hash,
                                    &decoded.author_key,
                                    &decoded.channel,
                                    decoded.sent_at_ms,
                                );
                            }
                        }
                        // Reactions are handled by ReactionTracker
                        // (spawned separately, see sunset-core::reactions).
                        // The per-room decode loop has nothing to do here.
                        crate::MessageBody::Reaction { .. } => {}
                    }
                }

                // Fire on_messages_changed with the current sorted
                // snapshot iff this iteration actually grew the index.
                // Borrowing the BTreeMap to collect the snapshot is
                // done before the callback is invoked so a handler
                // that calls `ordered_messages()` doesn't deadlock on
                // `RefCell`.
                if messages_changed {
                    let snap: Vec<DecodedMessage> = inner
                        .decoded_text_messages
                        .borrow()
                        .values()
                        .cloned()
                        .collect();
                    if let Some(cb) = callbacks.borrow().on_messages.as_ref() {
                        cb(&snap);
                    }
                }

                // Live channels rail: every successfully decoded
                // message — Text, Receipt, or Reaction — contributes
                // its channel. New channels fire on_channels with the
                // full sorted snapshot.
                let inserted = inner
                    .observed_channels
                    .borrow_mut()
                    .insert(decoded.channel.clone());
                if inserted {
                    let snap: Vec<_> = inner.observed_channels.borrow().iter().cloned().collect();
                    if let Some(cb) = callbacks.borrow().on_channels.as_ref() {
                        cb(&snap);
                    }
                }

                // Auto-ack: when a Text from another peer lands, write
                // a Receipt back so the sender's UI can flip out of
                // "pending". Skip self-Texts (no point acking your own)
                // and dedupe per-session against Replay::All re-emits.
                // The receipt inherits the target Text's channel so a
                // sender filtering its UI to e.g. #links sees the ack
                // arrive in the same channel.
                if let crate::MessageBody::Text { .. } = &decoded.body {
                    if !is_self && acked.insert(entry.value_hash) {
                        send_receipt(
                            &store,
                            &room,
                            &identity,
                            entry.value_hash,
                            decoded.channel.clone(),
                            &mut rng,
                        )
                        .await;
                    }
                }
            }
        });
    }

    pub async fn start_presence(&self, interval_ms: u64, ttl_ms: u64, refresh_ms: u64) {
        if self.inner.presence_started.replace(true) {
            return;
        }
        let peer = match self.inner.peer_weak.upgrade() {
            Some(p) => p,
            None => return,
        };
        let room_fp_hex = self.inner.room.fingerprint().to_hex();
        let local_peer = sunset_sync::PeerId(peer.identity().store_verifying_key());

        let publisher = crate::membership::spawn_publisher(
            peer.identity().clone(),
            room_fp_hex.clone(),
            peer.store().clone(),
            interval_ms,
            ttl_ms,
        );
        *self.inner.publisher.borrow_mut() = Some(publisher);

        // Apply any name cached before this room was opened (e.g. set
        // via Peer::set_self_name in ClientReady before RoomOpened).
        if let Some(name) = peer.cached_self_name() {
            if let Some(p) = self.inner.publisher.borrow().as_ref() {
                p.update_name(&name);
            }
        }

        let engine_events = peer.engine().subscribe_engine_events().await;
        let snapshot = peer.engine().current_peers().await;
        {
            let mut peer_kinds = self.inner.tracker_handles.peer_kinds.borrow_mut();
            for (pk, kind) in snapshot {
                peer_kinds.insert(pk, kind);
            }
        }

        crate::membership::spawn_tracker(
            peer.store().clone(),
            engine_events,
            local_peer,
            crate::membership::PresenceConfig {
                room_fp_hex,
                interval_ms,
                ttl_ms,
                refresh_ms,
            },
            (*self.inner.tracker_handles).clone(),
        );
    }

    /// Update the display name carried in this room's presence
    /// heartbeats. No-op until `start_presence` has been called.
    pub fn set_self_name(&self, name: &str) {
        if let Some(p) = self.inner.publisher.borrow().as_ref() {
            p.update_name(name);
        }
    }

    /// Register a durable supervisor intent for a direct WebRTC
    /// connection to `peer_pubkey`. Returns the `IntentId` so the caller
    /// can later [`cancel_direct`](Self::cancel_direct) it.
    ///
    /// Intents are deduplicated by `Connectable` (i.e. the resolved
    /// `webrtc://<pk>#x25519=<x>` address): calling `connect_direct`
    /// twice for the same peer returns the same `IntentId` without
    /// kicking a fresh dial. Session-scoped callers (e.g. the voice
    /// runtime) must pair `connect_direct` with `cancel_direct` so a
    /// post-stop rejoin doesn't dedupe against a stale intent whose
    /// underlying WebRTC connection silently died — the supervisor
    /// would otherwise keep the orphaned intent in `Connected` /
    /// `Backoff` until heartbeat timeout (45 s default), during which
    /// a fresh `connect_direct` call would be a no-op.
    pub async fn connect_direct(
        &self,
        peer_pubkey: [u8; 32],
    ) -> crate::Result<sunset_sync::IntentId> {
        let peer = self
            .inner
            .peer_weak
            .upgrade()
            .ok_or_else(|| crate::Error::Other("peer dropped".into()))?;
        let x_pub = sunset_noise::ed25519_public_to_x25519(&peer_pubkey)
            .map_err(|e| crate::Error::Other(format!("x25519 derive: {e}")))?;
        let addr_str = format!(
            "webrtc://{}#x25519={}",
            hex::encode(peer_pubkey),
            hex::encode(x_pub)
        );
        let addr = sunset_sync::PeerAddr::new(bytes::Bytes::from(addr_str));
        let id = peer
            .supervisor()
            .add(sunset_sync::Connectable::Direct(addr))
            .await
            .map_err(|e| crate::Error::Other(format!("connect_direct: {e}")))?;
        Ok(id)
    }

    /// Cancel a direct-connection intent registered via
    /// [`connect_direct`](Self::connect_direct). Tears down the
    /// supervisor's intent (and the underlying engine peer if it was
    /// connected) so a subsequent `connect_direct` for the same peer
    /// starts a fresh dial rather than deduplicating against a stale
    /// `Connected` / `Backoff` state. No-op if the intent has already
    /// been removed or the parent `Peer` has been dropped.
    pub async fn cancel_direct(&self, intent_id: sunset_sync::IntentId) {
        if let Some(peer) = self.inner.peer_weak.upgrade() {
            peer.supervisor().remove(intent_id).await;
        }
    }

    pub fn peer_connection_mode(&self, peer_pubkey: [u8; 32]) -> &'static str {
        use sunset_sync::TransportKind;
        let peer_id = sunset_sync::PeerId(sunset_store::VerifyingKey::new(
            bytes::Bytes::copy_from_slice(&peer_pubkey),
        ));
        match self.inner.tracker_handles.peer_kinds.borrow().get(&peer_id) {
            Some(TransportKind::Secondary) => "direct",
            Some(TransportKind::Primary) => "via_relay",
            _ => "unknown",
        }
    }

    pub fn on_members_changed<F: Fn(&[crate::membership::Member]) + 'static>(&self, cb: F) {
        *self.inner.tracker_handles.on_members.borrow_mut() = Some(Box::new(cb));
        // Match Client::on_members_changed: clear last_signature so the next
        // refresh tick fires the callback with the current snapshot.
        self.inner
            .tracker_handles
            .last_signature
            .borrow_mut()
            .clear();
    }

    /// Register a callback that fires whenever the per-target reaction
    /// snapshot for any message in this room changes. The callback
    /// receives the target message's `value_hash` and the new
    /// `ReactionSnapshot` (emoji → set of author identity keys).
    /// Mirrors `on_members_changed`'s "register late, get current
    /// state" behaviour by clearing per-target debounce signatures so
    /// the next event refires the snapshot.
    pub fn on_reactions_changed<
        F: Fn(&sunset_store::Hash, &crate::ChannelLabel, &crate::reactions::ReactionSnapshot)
            + 'static,
    >(
        &self,
        cb: F,
    ) {
        *self
            .inner
            .reaction_handles
            .on_reactions_changed
            .borrow_mut() = Some(Box::new(cb));
        self.inner
            .reaction_handles
            .last_target_signatures
            .borrow_mut()
            .clear();
    }

    /// Convenience wrapper: sends a reaction under the default
    /// `general` channel. New callers that want explicit channel
    /// routing should use [`OpenRoom::send_reaction_in_channel`]. The
    /// reaction tracker (spawned in `Peer::open_room`) picks it up via
    /// its `<room_fp>/msg/` subscription and dispatches a snapshot
    /// change to `on_reactions_changed`. `action` is "add" or "remove".
    pub async fn send_reaction(
        &self,
        target: sunset_store::Hash,
        emoji: String,
        action: crate::ReactionAction,
        sent_at_ms: u64,
    ) -> crate::Result<()> {
        self.send_reaction_in_channel(
            ChannelLabel::default_general(),
            target,
            emoji,
            action,
            sent_at_ms,
        )
        .await
    }

    /// Compose and insert a Reaction entry under `channel`. Reactions
    /// inherit the channel of the message they target — callers of
    /// this method are expected to pass the same channel as the
    /// target Text. See [`OpenRoom::send_reaction`] for the
    /// channel-defaulted convenience wrapper.
    pub async fn send_reaction_in_channel(
        &self,
        channel: ChannelLabel,
        target: sunset_store::Hash,
        emoji: String,
        action: crate::ReactionAction,
        sent_at_ms: u64,
    ) -> crate::Result<()> {
        use rand_chacha::ChaCha20Rng;
        use rand_core::SeedableRng;

        let peer = self
            .inner
            .peer_weak
            .upgrade()
            .ok_or_else(|| crate::Error::Other("peer dropped".into()))?;

        let mut rng = ChaCha20Rng::from_entropy();
        let composed = crate::compose_reaction(
            peer.identity(),
            &self.inner.room,
            crate::V1_EPOCH_ID,
            sent_at_ms,
            channel,
            &crate::ReactionPayload {
                for_value_hash: target,
                emoji: &emoji,
                action,
            },
            &mut rng,
        )?;
        peer.store()
            .insert(composed.entry, Some(composed.block))
            .await
            .map_err(|e| crate::Error::Other(format!("store insert: {e}")))?;
        Ok(())
    }
}

impl<St: Store + 'static, T: Transport + 'static> Drop for RoomState<St, T> {
    fn drop(&mut self) {
        self.cancel_decode.set(true);
        if let Some(peer) = self.peer_weak.upgrade() {
            peer.rtc_signaler_dispatcher
                .unregister(&self.room.fingerprint());
        }
    }
}

/// Compose and insert a delivery Receipt acknowledging
/// `for_value_hash` (the value_hash of the original Text). Used by the
/// auto-ack path in `spawn_decode_loop`; the `channel` argument is
/// the target Text's channel so the receipt rides in the same channel
/// as the message it acknowledges. Errors are logged via `tracing`
/// and swallowed — receipts are best-effort; failing to ack is not
/// fatal.
pub(crate) async fn send_receipt<St: Store + 'static>(
    store: &std::sync::Arc<St>,
    room: &Room,
    identity: &crate::Identity,
    for_value_hash: sunset_store::Hash,
    channel: ChannelLabel,
    rng: &mut rand_chacha::ChaCha20Rng,
) {
    let now_ms = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let composed = match crate::compose_receipt(
        identity,
        room,
        crate::V1_EPOCH_ID,
        now_ms,
        channel,
        for_value_hash,
        rng,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("compose_receipt failed: {e}");
            return;
        }
    };
    if let Err(e) = store.insert(composed.entry, Some(composed.block)).await {
        tracing::error!("store.insert(receipt) failed: {e}");
    }
}
