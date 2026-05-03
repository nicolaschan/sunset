//! `Dialer` impl that wraps `OpenRoom::connect_direct`.
//!
//! The voice subsystem calls `ensure_direct` when it sees a peer's
//! first heartbeat. The actual WebRTC negotiation is handled by
//! `PeerSupervisor`; this is just the "kick off the dial" call.

use std::rc::Rc;

use async_trait::async_trait;

use sunset_sync::PeerId;
use sunset_voice::Dialer;

use crate::room_handle::OpenRoomT;

pub(crate) struct WebDialer {
    pub open_room: Rc<OpenRoomT>,
}

#[async_trait(?Send)]
impl Dialer for WebDialer {
    async fn ensure_direct(&self, peer: PeerId) {
        let pk_bytes = peer.0.as_bytes();
        let arr: [u8; 32] = match pk_bytes.try_into() {
            Ok(a) => a,
            Err(_) => {
                tracing::warn!("WebDialer: peer public key is not 32 bytes, skipping dial");
                return;
            }
        };
        if let Err(e) = self.open_room.connect_direct(arr).await {
            tracing::warn!(error = %e, "voice ensure_direct failed");
        }
    }
}
