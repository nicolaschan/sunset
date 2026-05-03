//! `OpenRoom` is the per-room handle returned by `Peer::open_room`.
//! Method bodies (send_text, on_message, presence, etc.) are filled in
//! by Phase 5+ of the multi-room plan. This file currently only declares
//! the data shape so `Peer` can hold its registry of `Weak<RoomState>`.

use std::cell::Cell;
use std::rc::{Rc, Weak};

use sunset_sync::Transport;
use sunset_store::Store;

use crate::crypto::room::Room;
use crate::membership::TrackerHandles;
use crate::signaling::RelaySignaler;

// Fields are placeholder data shape for Phase 5+. All will be read by
// open_room, send_text, on_message, presence, etc. that arrive later.
#[allow(dead_code)]
pub(crate) struct RoomState<St: Store + 'static, T: Transport + 'static> {
    pub(crate) room: Rc<Room>,
    pub(crate) peer_weak: Weak<super::Peer<St, T>>,
    pub(crate) presence_started: Cell<bool>,
    pub(crate) tracker_handles: Rc<TrackerHandles>,
    pub(crate) signaler: Rc<RelaySignaler<St>>,
    pub(crate) cancel_decode: Rc<Cell<bool>>,
}

// `inner` will be accessed by Phase 5+ methods added to `OpenRoom`.
#[allow(dead_code)]
pub struct OpenRoom<St: Store + 'static, T: Transport + 'static> {
    pub(crate) inner: Rc<RoomState<St, T>>,
}

impl<St: Store + 'static, T: Transport + 'static> Drop for RoomState<St, T> {
    fn drop(&mut self) {
        self.cancel_decode.set(true);
        if let Some(peer) = self.peer_weak.upgrade() {
            peer.rtc_signaler_dispatcher.unregister(&self.room.fingerprint());
        }
    }
}
