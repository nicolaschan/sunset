//! `Client`: high-level async API around a `sunset-core::Peer`.
//!
//! The UI and the integration tests both drive this type. It owns:
//!  - the `Peer` + `Engine` + `Supervisor` (constructed via
//!    `build::build_peer` and run on a `LocalSet`),
//!  - per-room snapshot state (`RoomView`) plus the `OpenRoom` handle
//!    that keeps the room's RoomState alive (its background tasks
//!    cancel on drop),
//!  - top-level state (`TopState`),
//!  - a `Notify` used to wake the UI loop on any state change.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use sunset_core::{Identity, OpenRoom};
use sunset_sync::Connectable;
use tokio::sync::Notify;

use crate::build::{BuiltPeer, CliEngine, CliPeer, CliSupervisor, build_peer};
use crate::resolver_adapter::ReqwestFetch;
use crate::view::{MemberRow, MessageLine, RelayRow, RoomView, TopState};

/// One open room: the snapshot view + the `OpenRoom` handle. Holding
/// the handle keeps the per-room background tasks (decode loop,
/// presence publisher, members tracker) alive.
struct Joined {
    view: Rc<RefCell<RoomView>>,
    _open_room: OpenRoom<sunset_store_memory::MemoryStore, crate::build::CliTransport>,
}

pub struct Client {
    pub identity: Identity,
    pub peer: Rc<CliPeer>,
    pub engine: Rc<CliEngine>,
    pub supervisor: Rc<CliSupervisor>,

    pub top: Rc<RefCell<TopState>>,
    rooms: Rc<RefCell<HashMap<String, Joined>>>,
    pub notify: Rc<Notify>,
}

impl Client {
    /// Constructs the Client, spawns engine + supervisor on the
    /// current `LocalSet`. Caller must already be inside a
    /// `LocalSet::run_until(...)`.
    pub fn start(identity: Identity) -> Rc<Self> {
        let BuiltPeer {
            peer,
            engine,
            supervisor,
            store: _store,
        } = build_peer(identity.clone());

        let engine_clone = engine.clone();
        tokio::task::spawn_local(async move {
            if let Err(e) = engine_clone.run().await {
                tracing::error!(error = %e, "engine exited");
            }
        });
        let sup_clone = supervisor.clone();
        tokio::task::spawn_local(async move { sup_clone.run().await });

        let identity_hex = hex::encode(identity.public().as_bytes());
        let top = Rc::new(RefCell::new(TopState {
            identity_hex,
            ..TopState::default()
        }));
        let rooms = Rc::new(RefCell::new(HashMap::new()));
        let notify = Rc::new(Notify::new());

        let me = Rc::new(Self {
            identity,
            peer,
            engine,
            supervisor,
            top,
            rooms,
            notify,
        });

        me.spawn_intent_subscriber();
        me
    }

    fn poke(&self) {
        self.notify.notify_one();
    }

    fn spawn_intent_subscriber(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        let peer = self.peer.clone();
        tokio::task::spawn_local(async move {
            let mut rx = peer.subscribe_intents().await;
            while let Some(snap) = rx.recv().await {
                let Some(strong) = weak.upgrade() else {
                    return;
                };
                strong.apply_intent_snapshot(snap);
            }
        });
    }

    fn apply_intent_snapshot(&self, snap: sunset_sync::IntentSnapshot) {
        let state = match snap.state {
            sunset_sync::IntentState::Connecting => "connecting",
            sunset_sync::IntentState::Connected => "connected",
            sunset_sync::IntentState::Backoff => "backoff",
            sunset_sync::IntentState::Cancelled => "cancelled",
        };
        let row = RelayRow {
            label: snap.label,
            state,
            last_rtt_ms: snap.last_rtt_ms,
        };
        let mut top = self.top.borrow_mut();
        if let Some(existing) = top.relays.iter_mut().find(|r| r.label == row.label) {
            *existing = row;
        } else {
            top.relays.push(row);
        }
        drop(top);
        self.poke();
    }

    pub async fn add_relay(&self, url: String) -> Result<(), String> {
        let fetch: Rc<dyn sunset_relay_resolver::HttpFetch> = Rc::new(ReqwestFetch::default());
        let connectable = Connectable::Resolving { input: url, fetch };
        self.peer
            .add_relay(connectable)
            .await
            .map_err(|e| format!("{e}"))
            .map(|_| ())
    }

    pub fn set_self_name(&self, name: &str) {
        self.peer.set_self_name(name);
        let mut top = self.top.borrow_mut();
        top.self_name = if name.is_empty() {
            None
        } else {
            Some(name.to_owned())
        };
        drop(top);
        self.poke();
    }

    pub async fn join_room(self: &Rc<Self>, name: &str) -> Result<(), String> {
        if self.rooms.borrow().contains_key(name) {
            self.set_active(name);
            return Ok(());
        }
        let open_room = self.peer.open_room(name).await.map_err(|e| format!("{e}"))?;

        let view = Rc::new(RefCell::new(RoomView {
            name: name.to_owned(),
            messages: Vec::new(),
            members: Vec::new(),
        }));

        {
            let view = view.clone();
            let notify = self.notify.clone();
            open_room.on_message(move |decoded, is_self| {
                if let sunset_core::MessageBody::Text(t) = &decoded.body {
                    let line = MessageLine {
                        author_pubkey: decoded.author_key.as_bytes(),
                        author_name: None,
                        body: t.clone(),
                        sent_at_ms: decoded.sent_at_ms,
                        is_self,
                    };
                    view.borrow_mut().messages.push(line);
                    notify.notify_one();
                }
            });
        }
        {
            let view = view.clone();
            let notify = self.notify.clone();
            open_room.on_members_changed(move |members| {
                let rows: Vec<MemberRow> = members
                    .iter()
                    .map(|m| {
                        let mut pk = [0u8; 32];
                        let len = m.pubkey.len().min(32);
                        pk[..len].copy_from_slice(&m.pubkey[..len]);
                        MemberRow {
                            pubkey: pk,
                            name: m.name.clone(),
                            connection_mode: m.connection_mode.as_str(),
                            presence: m.presence.as_str(),
                        }
                    })
                    .collect();
                view.borrow_mut().members = rows;
                notify.notify_one();
            });
        }

        open_room.start_presence(2_000, 6_000, 1_000).await;

        self.rooms.borrow_mut().insert(
            name.to_owned(),
            Joined {
                view,
                _open_room: open_room,
            },
        );
        self.set_active(name);

        Ok(())
    }

    pub fn set_active(&self, name: &str) {
        let mut top = self.top.borrow_mut();
        top.active_room = Some(name.to_owned());
        if !top.open_rooms.iter().any(|r| r == name) {
            top.open_rooms.push(name.to_owned());
        }
        drop(top);
        self.poke();
    }

    pub fn leave_room(&self, name: &str) {
        self.rooms.borrow_mut().remove(name);
        let mut top = self.top.borrow_mut();
        top.open_rooms.retain(|r| r != name);
        if top.active_room.as_deref() == Some(name) {
            top.active_room = top.open_rooms.last().cloned();
        }
        drop(top);
        self.poke();
    }

    /// Send a chat text into the active room. No-op if no active room.
    pub async fn send_text(&self, body: String) -> Result<(), String> {
        let active = self.top.borrow().active_room.clone();
        let Some(name) = active else {
            return Ok(());
        };
        let open_room = self
            .peer
            .open_room(&name)
            .await
            .map_err(|e| format!("{e}"))?;
        let now_ms = chrono::Utc::now().timestamp_millis().max(0) as u64;
        open_room
            .send_text(body, now_ms)
            .await
            .map_err(|e| format!("{e}"))?;
        Ok(())
    }

    pub fn snapshot_top(&self) -> TopState {
        self.top.borrow().clone()
    }

    pub fn snapshot_room(&self, name: &str) -> Option<RoomView> {
        self.rooms
            .borrow()
            .get(name)
            .map(|j| j.view.borrow().clone())
    }

    pub fn append_system(&self, line: String) {
        self.top.borrow_mut().system_log.push(line);
        self.poke();
    }
}
