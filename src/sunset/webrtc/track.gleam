import sunset/webrtc/pc.{
  type PeerConnection, type Sender, type Stream, type Track,
}

@external(javascript, "./track.ffi.mjs", "add_track")
pub fn add_track(_pc: PeerConnection, _track: Track, _stream: Stream) -> Sender {
  panic as "add_track is only available on JavaScript"
}

@external(javascript, "./track.ffi.mjs", "remove_track")
pub fn remove_track(_pc: PeerConnection, _sender: Sender) -> Nil {
  Nil
}
