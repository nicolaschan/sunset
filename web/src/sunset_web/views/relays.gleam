//// Relays UI: rail-section list + click-through popover (desktop
//// floating / phone bottom sheet). This file currently exposes the
//// pure helpers and a from_intent / relays_for_view derivation. The
//// view functions land in subsequent changes.

import gleam/dict.{type Dict}
import gleam/int
import gleam/list
import gleam/option.{type Option}
import gleam/order
import gleam/string
import gleam/uri
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Relay, type RelayConnState, Relay, RelayBackoff, RelayCancelled,
  RelayConnected, RelayConnecting,
}
import sunset_web/sunset.{type IntentSnapshot}
import sunset_web/theme.{type Palette}
import sunset_web/ui

/// True when `label` is a relay (not a direct WebRTC peer).
/// Connectable::Direct(webrtc://…) carries that scheme on its label;
/// every other shape (Resolving inputs like "relay.sunset.chat" or
/// Direct(wss://…) URLs from ?relay=) is a relay.
pub fn is_relay_label(label: String) -> Bool {
  !string.starts_with(label, "webrtc://")
}

/// Best-effort hostname extraction. When `label` looks like a URL
/// (contains `://`), use gleam/uri to extract host[:port]. When it's
/// a bare hostname (typical for Resolving inputs), return it
/// unchanged. Returns `label` on parse failure — defensive fallback so
/// a malformed label never crashes the rail.
pub fn parse_host(label: String) -> String {
  case string.contains(label, "://") {
    False -> label
    True ->
      case uri.parse(label) {
        Ok(u) ->
          case u.host {
            option.Some(h) ->
              case u.port {
                option.Some(p) -> h <> ":" <> int.to_string(p)
                option.None -> h
              }
            option.None -> label
          }
        Error(_) -> label
      }
  }
}

/// Map JS-side intent state string to the typed enum. Unknown
/// strings fall back to `RelayBackoff` so the row stays visible in
/// some recoverable state rather than being silently dropped.
pub fn parse_state(s: String) -> RelayConnState {
  case s {
    "connected" -> RelayConnected
    "connecting" -> RelayConnecting
    "backoff" -> RelayBackoff
    "cancelled" -> RelayCancelled
    _ -> RelayBackoff
  }
}

/// User-facing label for the connection state. For Backoff with a
/// non-zero attempt counter, includes the attempt number.
pub fn format_status(state: RelayConnState, attempt: Int) -> String {
  case state, attempt {
    RelayConnected, _ -> "Connected"
    RelayConnecting, _ -> "Connecting"
    RelayBackoff, 0 -> "Backoff"
    RelayBackoff, n -> "Backoff (attempt " <> int.to_string(n) <> ")"
    RelayCancelled, _ -> "Cancelled"
  }
}

/// "RTT 42 ms" / "RTT —".
pub fn format_rtt(last_rtt_ms: Option(Int)) -> String {
  case last_rtt_ms {
    option.Some(n) -> "RTT " <> int.to_string(n) <> " ms"
    option.None -> "RTT —"
  }
}

/// Render age "heard from …": "just now" / "Ns ago" / "Nm ago" /
/// "Nh ago" / "never". Mirrors `peer_status_popover.humanize_age`;
/// kept duplicated rather than pre-extracting a shared helper —
/// extract when a third caller appears.
pub fn humanize_age(now_ms: Int, last_ms: Option(Int)) -> String {
  case last_ms {
    option.None -> "never"
    option.Some(t) -> {
      let age_ms = case now_ms - t {
        n if n < 0 -> 0
        n -> n
      }
      let age_s = age_ms / 1000
      case age_s {
        s if s < 1 -> "just now"
        s if s < 60 -> int.to_string(s) <> "s ago"
        s if s < 3600 -> int.to_string(s / 60) <> "m ago"
        s -> int.to_string(s / 3600) <> "h ago"
      }
    }
  }
}

/// Format a hex pubkey as "first8…last8" (8 chars on each side).
/// Strings of length ≤ 16 are returned unchanged.
pub fn short_peer_id(hex: String) -> String {
  case string.length(hex) {
    n if n <= 16 -> hex
    n -> string.slice(hex, 0, 8) <> "…" <> string.slice(hex, n - 8, 8)
  }
}

/// Build a domain.Relay from a sunset.IntentSnapshot. Pure projection.
pub fn from_intent(snap: IntentSnapshot) -> Relay {
  let peer_id_short =
    snap.peer_pubkey
    |> option.map(fn(bits) { short_peer_id(sunset.bits_to_hex(bits)) })
  Relay(
    id: snap.id,
    host: parse_host(snap.label),
    raw_label: snap.label,
    state: parse_state(snap.state),
    attempt: snap.attempt,
    peer_id_short: peer_id_short,
    last_pong_at_ms: snap.last_pong_at_ms,
    last_rtt_ms: snap.last_rtt_ms,
  )
}

/// Filter `intents` to relays only and project to view-models.
/// Stable order: ascending by IntentId.
pub fn relays_for_view(
  intents: Dict(Float, IntentSnapshot),
) -> List(Relay) {
  intents
  |> dict.values()
  |> list.filter(fn(s) { is_relay_label(s.label) })
  |> list.sort(fn(a, b) {
    case a.id <. b.id, a.id >. b.id {
      True, _ -> order.Lt
      _, True -> order.Gt
      _, _ -> order.Eq
    }
  })
  |> list.map(from_intent)
}

pub fn rail_section(
  palette p: Palette,
  relays rs: List(Relay),
  on_open on_open: fn(Float) -> msg,
) -> Element(msg) {
  case rs {
    [] -> element.fragment([])
    _ ->
      html.div(
        [
          attribute.attribute("data-testid", "relays-section"),
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("gap", "4px"),
          ]),
        ],
        [
          html.div(
            [
              ui.css([
                #("padding", "0 12px 4px 12px"),
                #("font-size", "13.125px"),
                #("color", p.text_faint),
                #("text-transform", "uppercase"),
                #("letter-spacing", "0.04em"),
              ]),
            ],
            [html.text("Relays")],
          ),
          html.div(
            [
              ui.css([
                #("display", "flex"),
                #("flex-direction", "column"),
                #("gap", "1px"),
              ]),
            ],
            list.map(rs, fn(r) { rail_row(p, r, on_open) }),
          ),
        ],
      )
  }
}

fn rail_row(
  p: Palette,
  r: Relay,
  on_open: fn(Float) -> msg,
) -> Element(msg) {
  html.button(
    [
      attribute.attribute("data-testid", "relay-row"),
      attribute.attribute("data-relay-host", r.host),
      attribute.attribute("data-relay-state", state_attr(r.state)),
      event.on_click(on_open(r.id)),
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "6px 12px"),
        #("border", "none"),
        #("background", "transparent"),
        #("color", p.text_muted),
        #("font-family", "inherit"),
        #("font-size", "16.25px"),
        #("text-align", "left"),
        #("cursor", "pointer"),
        #("border-radius", "6px"),
      ]),
    ],
    [
      conn_dot(p, r.state),
      html.span(
        [
          ui.css([
            #("flex", "1"),
            #("min-width", "0"),
            #("white-space", "nowrap"),
            #("overflow", "hidden"),
            #("text-overflow", "ellipsis"),
          ]),
        ],
        [html.text(r.host)],
      ),
    ],
  )
}

fn conn_dot(p: Palette, s: RelayConnState) -> Element(msg) {
  let c = case s {
    RelayConnected -> p.live
    RelayConnecting -> p.warn
    RelayBackoff -> p.warn
    RelayCancelled -> p.text_faint
  }
  html.span(
    [
      ui.css([
        #("width", "7px"),
        #("height", "7px"),
        #("border-radius", "999px"),
        #("background", c),
        #("display", "inline-block"),
        #("flex-shrink", "0"),
      ]),
    ],
    [],
  )
}

fn state_attr(s: RelayConnState) -> String {
  case s {
    RelayConnected -> "connected"
    RelayConnecting -> "connecting"
    RelayBackoff -> "backoff"
    RelayCancelled -> "cancelled"
  }
}
