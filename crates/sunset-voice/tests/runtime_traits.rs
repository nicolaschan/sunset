use sunset_voice::runtime::{Dialer, FrameSink, PeerStateSink, VoicePeerState};
use sunset_sync::PeerId;
use std::cell::RefCell;
use std::rc::Rc;

struct RecordingDialer { calls: RefCell<Vec<PeerId>> }
#[async_trait::async_trait(?Send)]
impl Dialer for RecordingDialer {
    async fn ensure_direct(&self, peer: PeerId) {
        self.calls.borrow_mut().push(peer);
    }
}

struct RecordingFrameSink {
    delivered: RefCell<Vec<(PeerId, Vec<f32>)>>,
    dropped: RefCell<Vec<PeerId>>,
}
impl FrameSink for RecordingFrameSink {
    fn deliver(&self, peer: &PeerId, pcm: &[f32]) {
        self.delivered.borrow_mut().push((peer.clone(), pcm.to_vec()));
    }
    fn drop_peer(&self, peer: &PeerId) {
        self.dropped.borrow_mut().push(peer.clone());
    }
}

struct RecordingPeerStateSink { events: RefCell<Vec<VoicePeerState>> }
impl PeerStateSink for RecordingPeerStateSink {
    fn emit(&self, state: &VoicePeerState) {
        self.events.borrow_mut().push(state.clone());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn traits_are_object_safe_and_implementable() {
    let d: Rc<dyn Dialer> = Rc::new(RecordingDialer { calls: RefCell::new(vec![]) });
    let f: Rc<dyn FrameSink> = Rc::new(RecordingFrameSink {
        delivered: RefCell::new(vec![]),
        dropped: RefCell::new(vec![]),
    });
    let p: Rc<dyn PeerStateSink> = Rc::new(RecordingPeerStateSink { events: RefCell::new(vec![]) });

    let dummy = PeerId(sunset_store::VerifyingKey::new(bytes::Bytes::from_static(&[0u8; 32])));
    d.ensure_direct(dummy.clone()).await;
    f.deliver(&dummy, &[0.0_f32; 960]);
    f.drop_peer(&dummy);
    p.emit(&VoicePeerState { peer: dummy, in_call: true, talking: false, is_muted: false });
}
