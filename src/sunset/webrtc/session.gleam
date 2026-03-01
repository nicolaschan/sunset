import sunset/webrtc/debug
import sunset/webrtc/media
import sunset/webrtc/pc.{type PeerConnection, type Stream, type Track}
import sunset/webrtc/query
import sunset/webrtc/sdp
import sunset/webrtc/track

pub type Pending {
  Pending(pc: PeerConnection, local_track: Track, local_stream: Stream)
}

pub type Connection {
  Connection(pc: PeerConnection, local_track: Track, local_stream: Stream)
}

const ice_gathering_timeout_ms = 2000

fn try_async(
  invoke: fn(fn(Result(a, e)) -> Nil) -> Nil,
  on_error: fn(e) -> Nil,
  on_ok: fn(a) -> Nil,
) -> Nil {
  invoke(fn(result) {
    case result {
      Ok(value) -> on_ok(value)
      Error(err) -> on_error(err)
    }
  })
}

fn create_audio_pc(on_state_change: fn(String) -> Nil) -> PeerConnection {
  pc.create_pc(
    fn(_candidate) { Nil },
    on_state_change,
    fn() { Nil },
    fn(_track, remote_stream) {
      let _ = media.play_stream(remote_stream)
      Nil
    },
  )
}

pub fn offer(
  on_state_change: fn(String) -> Nil,
  callback: fn(Result(#(Pending, String), String)) -> Nil,
) -> Nil {
  let fail = fn(err) {
    debug.log("session", "offer failed: " <> err)
    callback(Error(err))
  }
  debug.log("session", "offer: getting user audio")
  use #(audio_track, audio_stream) <- try_async(media.get_user_audio, fail)
  debug.log("session", "offer: got audio, creating pc")
  let conn = create_audio_pc(on_state_change)
  let _ = track.add_track(conn, audio_track, audio_stream)
  debug.log("session", "offer: creating sdp offer")
  use _ <- try_async(sdp.create_offer(conn, _), fail)
  debug.log("session", "offer: waiting for ice gathering")
  use <- pc.wait_for_ice_gathering(conn, ice_gathering_timeout_ms)
  let sdp_json = query.get_local_description(conn)
  debug.log("session", "offer: complete")
  callback(Ok(#(Pending(conn, audio_track, audio_stream), sdp_json)))
}

pub fn accept_answer(
  pending: Pending,
  answer_json: String,
  callback: fn(Result(Connection, String)) -> Nil,
) -> Nil {
  let fail = fn(err) {
    debug.log("session", "accept_answer failed: " <> err)
    callback(Error(err))
  }
  debug.log("session", "accept_answer: setting remote description")
  use _ <- try_async(
    sdp.set_remote_description(pending.pc, answer_json, _),
    fail,
  )
  debug.log("session", "accept_answer: complete")
  callback(
    Ok(Connection(pending.pc, pending.local_track, pending.local_stream)),
  )
}

pub fn answer(
  offer_json: String,
  on_state_change: fn(String) -> Nil,
  callback: fn(Result(#(Connection, String), String)) -> Nil,
) -> Nil {
  let fail = fn(err) {
    debug.log("session", "answer failed: " <> err)
    callback(Error(err))
  }
  debug.log("session", "answer: getting user audio")
  use #(audio_track, audio_stream) <- try_async(media.get_user_audio, fail)
  debug.log("session", "answer: got audio, creating pc")
  let conn = create_audio_pc(on_state_change)
  let _ = track.add_track(conn, audio_track, audio_stream)
  debug.log("session", "answer: setting remote description")
  use _ <- try_async(sdp.set_remote_description(conn, offer_json, _), fail)
  debug.log("session", "answer: creating sdp answer")
  use _ <- try_async(sdp.create_answer(conn, _), fail)
  debug.log("session", "answer: waiting for ice gathering")
  use <- pc.wait_for_ice_gathering(conn, ice_gathering_timeout_ms)
  let sdp_json = query.get_local_description(conn)
  debug.log("session", "answer: complete")
  callback(Ok(#(Connection(conn, audio_track, audio_stream), sdp_json)))
}

pub fn close(conn: Connection) -> Nil {
  media.stop_track(conn.local_track)
  pc.close_pc(conn.pc)
}
