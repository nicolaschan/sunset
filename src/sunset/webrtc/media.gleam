import sunset/webrtc/pc.{type PlaybackHandle, type Stream, type Track}

@external(javascript, "./media.ffi.mjs", "get_user_audio")
pub fn get_user_audio(
  _callback: fn(Result(#(Track, Stream), String)) -> Nil,
) -> Nil {
  Nil
}

@external(javascript, "./media.ffi.mjs", "stop_track")
pub fn stop_track(_track: Track) -> Nil {
  Nil
}

@external(javascript, "./media.ffi.mjs", "play_stream")
pub fn play_stream(_stream: Stream) -> PlaybackHandle {
  panic as "play_stream is only available on JavaScript"
}

@external(javascript, "./media.ffi.mjs", "stop_playback")
pub fn stop_playback(_handle: PlaybackHandle) -> Nil {
  Nil
}
