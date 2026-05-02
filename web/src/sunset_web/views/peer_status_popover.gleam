//// Floating popover that opens when the user clicks a member row.
////
//// Shows three lines:
////   * Transport label ("Direct (WebRTC)" or "Via relay" or "Self" / "Unknown")
////   * Time since last app-level presence heartbeat (humanized)
////   * Short pubkey (first 4 + last 4 hex bytes)
////
//// Anchored at a fixed position over the chat shell to match the
//// existing voice_popover convention. Two placements: Floating (desktop)
//// and InSheet (mobile bottom sheet).

import gleam/int
import gleam/option
import gleam/string
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Member, Direct, NoRelay, OneHop, SelfRelay,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub type Placement {
  Floating
  InSheet
}

pub fn view(
  palette p: Palette,
  member m: Member,
  now_ms now: Int,
  placement placement: Placement,
  on_close on_close: msg,
) -> Element(msg) {
  let body =
    html.div(
      [
        ui.css([
          #("display", "flex"),
          #("flex-direction", "column"),
          #("gap", "10px"),
          #("padding", "14px 16px"),
        ]),
      ],
      [
        header(p, m.name, on_close),
        row(p, transport_label(m.relay)),
        row(p, "heard from " <> humanize_age(now, m.last_heartbeat_ms)),
        row_mono(p, short_pubkey_display(m)),
      ],
    )

  case placement {
    Floating ->
      html.div(
        [
          attribute.attribute("data-testid", "peer-status-popover"),
          ui.css([
            #("position", "fixed"),
            #("top", "120px"),
            #("right", "260px"),
            #("width", "260px"),
            #("background", p.surface),
            #("color", p.text),
            #("border", "1px solid " <> p.border),
            #("border-radius", "10px"),
            #("box-shadow", p.shadow_lg),
            #("z-index", "20"),
          ]),
        ],
        [body],
      )
    InSheet ->
      html.div(
        [
          attribute.attribute("data-testid", "peer-status-popover"),
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("background", p.surface),
            #("color", p.text),
          ]),
        ],
        [body],
      )
  }
}

fn header(p: Palette, name: String, on_close: msg) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", "space-between"),
        #("gap", "8px"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("font-weight", "600"),
            #("font-size", "16px"),
            #("color", p.text),
            #("white-space", "nowrap"),
            #("overflow", "hidden"),
            #("text-overflow", "ellipsis"),
          ]),
        ],
        [html.text(name)],
      ),
      html.button(
        [
          attribute.attribute("data-testid", "peer-status-popover-close"),
          event.on_click(on_close),
          ui.css([
            #("background", "transparent"),
            #("border", "none"),
            #("color", p.text_faint),
            #("cursor", "pointer"),
            #("font-size", "16px"),
            #("padding", "0 4px"),
          ]),
        ],
        [html.text("×")],
      ),
    ],
  )
}

fn row(p: Palette, text: String) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("font-size", "14px"),
        #("color", p.text_muted),
      ]),
    ],
    [html.text(text)],
  )
}

fn row_mono(p: Palette, text: String) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("font-family", "monospace"),
        #("font-size", "13px"),
        #("color", p.text_faint),
      ]),
    ],
    [html.text(text)],
  )
}

/// Map domain.RelayStatus → user-facing label. Exhaustive on the v1 set.
pub fn transport_label(r: domain.RelayStatus) -> String {
  case r {
    Direct -> "Direct (WebRTC)"
    OneHop -> "Via relay"
    SelfRelay -> "Self"
    NoRelay -> "Unknown"
    _ -> "Unknown"
  }
}

/// Render age "heard from …": "just now" / "Ns ago" / "Nm ago" / "Nh ago" / "never".
pub fn humanize_age(now_ms: Int, last_ms: option.Option(Int)) -> String {
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

/// First 4 + last 4 hex bytes of the pubkey (derived from MemberId in v1
/// where the id IS the short pubkey hex). For v1 the MemberId already
/// holds the short pubkey string, so we just truncate/format.
pub fn short_pubkey_display(m: Member) -> String {
  let domain.MemberId(s) = m.id
  case string.length(s) {
    n if n <= 16 -> s
    _ -> string.slice(s, 0, 8) <> "…" <> string.slice(s, string.length(s) - 8, 8)
  }
}
