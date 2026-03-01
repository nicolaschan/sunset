import gleam/int
import gleam/order
import gleam/string
import sunset/webrtc/debug
import sunset/webrtc/session.{type Connection}

pub type Channel {
  Channel(
    send: fn(String, fn(Result(Nil, String)) -> Nil) -> Nil,
    on_receive: fn(fn(String) -> Nil) -> Nil,
  )
}

const syn_retry_delays = [500, 1500, 3500]

pub fn connect(
  channel: Channel,
  set_timeout: fn(fn() -> Nil, Int) -> Nil,
  on_state_change: fn(String) -> Nil,
  callback: fn(Result(Connection, String)) -> Nil,
) -> Nil {
  let my_nonce = int.random(1_000_000_000)
  let nonce_str = int.to_string(my_nonce)
  debug.log("call", "connect nonce=" <> nonce_str)
  let fail = fn(err) {
    debug.log("call", "failed: " <> err)
    callback(Error(err))
  }
  let syn_msg = "syn:" <> nonce_str

  channel.on_receive(handle_waiting_for_syn(
    channel,
    set_timeout,
    my_nonce,
    on_state_change,
    callback,
    _,
  ))

  send_with_retries(channel, set_timeout, syn_msg, syn_retry_delays, fail)
}

fn send_with_retries(
  channel: Channel,
  set_timeout: fn(fn() -> Nil, Int) -> Nil,
  msg: String,
  delays: List(Int),
  fail: fn(String) -> Nil,
) -> Nil {
  use _ <- try_async(channel.send(msg, _), fail)
  schedule_retries(channel, set_timeout, msg, delays)
}

fn schedule_retries(
  channel: Channel,
  set_timeout: fn(fn() -> Nil, Int) -> Nil,
  msg: String,
  delays: List(Int),
) -> Nil {
  case delays {
    [] -> Nil
    [delay, ..rest] -> {
      set_timeout(
        fn() {
          channel.send(msg, fn(_) { Nil })
          schedule_retries(channel, set_timeout, msg, rest)
        },
        delay,
      )
    }
  }
}

fn handle_waiting_for_syn(
  channel: Channel,
  set_timeout: fn(fn() -> Nil, Int) -> Nil,
  my_nonce: Int,
  on_state_change: fn(String) -> Nil,
  callback: fn(Result(Connection, String)) -> Nil,
  msg: String,
) -> Nil {
  let fail = fn(err) {
    debug.log("call", "failed: " <> err)
    callback(Error(err))
  }
  case parse_syn(msg) {
    Ok(remote_nonce) -> {
      debug.log(
        "call",
        "got syn remote="
          <> int.to_string(remote_nonce)
          <> " my="
          <> int.to_string(my_nonce),
      )
      let ack_msg =
        "ack:" <> int.to_string(my_nonce) <> ":" <> int.to_string(remote_nonce)
      channel.on_receive(handle_waiting_for_ack(
        channel,
        set_timeout,
        my_nonce,
        remote_nonce,
        on_state_change,
        callback,
        _,
      ))
      use _ <- try_async(channel.send(ack_msg, _), fail)
      debug.log("call", "sent ack: " <> ack_msg)
      Nil
    }
    Error(_) ->
      case parse_ack(msg) {
        Ok(#(remote_nonce, ack_mine)) if ack_mine == my_nonce -> {
          debug.log(
            "call",
            "waiting_for_syn got early ack remote="
              <> int.to_string(remote_nonce)
              <> " my="
              <> int.to_string(my_nonce),
          )
          let ack_msg =
            "ack:"
            <> int.to_string(my_nonce)
            <> ":"
            <> int.to_string(remote_nonce)
          channel.send(ack_msg, fn(_) { Nil })
          resolve_role(
            channel,
            set_timeout,
            my_nonce,
            remote_nonce,
            on_state_change,
            callback,
          )
        }
        _ -> {
          debug.log("call", "waiting_for_syn got non-syn: " <> msg)
          Nil
        }
      }
  }
}

fn handle_waiting_for_ack(
  channel: Channel,
  set_timeout: fn(fn() -> Nil, Int) -> Nil,
  my_nonce: Int,
  remote_nonce: Int,
  on_state_change: fn(String) -> Nil,
  callback: fn(Result(Connection, String)) -> Nil,
  msg: String,
) -> Nil {
  let fail = fn(err) {
    debug.log("call", "failed: " <> err)
    callback(Error(err))
  }
  case parse_syn(msg) {
    Ok(new_remote_nonce) -> {
      debug.log(
        "call",
        "waiting_for_ack got new syn remote=" <> int.to_string(new_remote_nonce),
      )
      let ack_msg =
        "ack:"
        <> int.to_string(my_nonce)
        <> ":"
        <> int.to_string(new_remote_nonce)
      channel.on_receive(handle_waiting_for_ack(
        channel,
        set_timeout,
        my_nonce,
        new_remote_nonce,
        on_state_change,
        callback,
        _,
      ))
      use _ <- try_async(channel.send(ack_msg, _), fail)
      Nil
    }
    Error(_) ->
      case parse_ack(msg) {
        Error(_) -> {
          debug.log("call", "waiting_for_ack got unknown: " <> msg)
          Nil
        }
        Ok(#(ack_remote, ack_mine)) -> {
          debug.log(
            "call",
            "got ack remote="
              <> int.to_string(ack_remote)
              <> " mine="
              <> int.to_string(ack_mine)
              <> " expected_my="
              <> int.to_string(my_nonce)
              <> " expected_remote="
              <> int.to_string(remote_nonce),
          )
          case ack_mine == my_nonce && ack_remote == remote_nonce {
            False -> {
              debug.log("call", "ack nonce mismatch, ignoring")
              Nil
            }
            True ->
              resolve_role(
                channel,
                set_timeout,
                my_nonce,
                remote_nonce,
                on_state_change,
                callback,
              )
          }
        }
      }
  }
}

fn resolve_role(
  channel: Channel,
  set_timeout: fn(fn() -> Nil, Int) -> Nil,
  my_nonce: Int,
  remote_nonce: Int,
  on_state_change: fn(String) -> Nil,
  callback: fn(Result(Connection, String)) -> Nil,
) -> Nil {
  let role = case int.compare(my_nonce, remote_nonce) {
    order.Eq -> "restart"
    order.Lt -> "offerer"
    order.Gt -> "answerer"
  }
  debug.log("call", "role=" <> role)
  case int.compare(my_nonce, remote_nonce) {
    order.Eq -> connect(channel, set_timeout, on_state_change, callback)
    order.Lt -> do_offer(channel, on_state_change, callback)
    order.Gt -> do_answer(channel, on_state_change, callback)
  }
}

fn do_offer(
  channel: Channel,
  on_state_change: fn(String) -> Nil,
  callback: fn(Result(Connection, String)) -> Nil,
) -> Nil {
  let fail = fn(err) {
    debug.log("call", "offer failed: " <> err)
    callback(Error(err))
  }
  debug.log("call", "creating offer...")
  use result <- session.offer(on_state_change)
  case result {
    Error(err) -> fail(err)
    Ok(#(pending, sdp)) -> {
      debug.log("call", "offer created, sending sdp")
      let offer_msg = "offer:" <> sdp
      channel.on_receive(handle_waiting_for_answer(pending, callback, _))
      use _ <- try_async(channel.send(offer_msg, _), fail)
      debug.log("call", "offer sent, waiting for answer")
      Nil
    }
  }
}

fn handle_waiting_for_answer(
  pending: session.Pending,
  callback: fn(Result(Connection, String)) -> Nil,
  msg: String,
) -> Nil {
  case parse_payload(msg, "answer") {
    Error(_) -> {
      debug.log("call", "waiting_for_answer got non-answer: " <> msg)
      Nil
    }
    Ok(answer_sdp) -> {
      debug.log("call", "got answer, accepting")
      session.accept_answer(pending, answer_sdp, callback)
    }
  }
}

fn do_answer(
  channel: Channel,
  on_state_change: fn(String) -> Nil,
  callback: fn(Result(Connection, String)) -> Nil,
) -> Nil {
  debug.log("call", "waiting for offer (answerer role)")
  channel.on_receive(handle_waiting_for_offer(
    channel,
    on_state_change,
    callback,
    _,
  ))
}

fn handle_waiting_for_offer(
  channel: Channel,
  on_state_change: fn(String) -> Nil,
  callback: fn(Result(Connection, String)) -> Nil,
  msg: String,
) -> Nil {
  let fail = fn(err) {
    debug.log("call", "answer failed: " <> err)
    callback(Error(err))
  }
  case parse_payload(msg, "offer") {
    Error(_) -> {
      debug.log("call", "waiting_for_offer got non-offer: " <> msg)
      Nil
    }
    Ok(offer_sdp) -> {
      debug.log("call", "got offer, creating answer")
      use result <- session.answer(offer_sdp, on_state_change)
      case result {
        Error(err) -> fail(err)
        Ok(#(conn, answer_sdp)) -> {
          debug.log("call", "answer created, sending")
          let answer_msg = "answer:" <> answer_sdp
          use _ <- try_async(channel.send(answer_msg, _), fail)
          debug.log("call", "answer sent, connection complete")
          callback(Ok(conn))
        }
      }
    }
  }
}

pub fn hangup(conn: Connection) -> Nil {
  session.close(conn)
}

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

fn parse_syn(msg: String) -> Result(Int, Nil) {
  case parse_payload(msg, "syn") {
    Error(_) -> Error(Nil)
    Ok(nonce_str) -> int.parse(nonce_str)
  }
}

fn parse_ack(msg: String) -> Result(#(Int, Int), Nil) {
  case parse_payload(msg, "ack") {
    Error(_) -> Error(Nil)
    Ok(rest) ->
      case string.split_once(rest, ":") {
        Error(_) -> Error(Nil)
        Ok(#(a, b)) ->
          case int.parse(a), int.parse(b) {
            Ok(na), Ok(nb) -> Ok(#(na, nb))
            _, _ -> Error(Nil)
          }
      }
  }
}

fn parse_payload(msg: String, tag: String) -> Result(String, Nil) {
  case string.split_once(msg, ":") {
    Ok(#(prefix, rest)) if prefix == tag -> Ok(rest)
    _ -> Error(Nil)
  }
}
