//// Right-column message-details panel — replaces the members rail
//// when a message's info button is clicked.
////
//// Renders up to four sections:
////   • the quoted message body (always)
////   • sender / cryptographic provenance (when full details are known)
////   • delivery path (when full details are known)
////   • read receipts (always; sourced from the live receipts dict so
////     even messages without crypto provenance show acks as they
////     arrive)
////
//// Closes via the X button in the top-right.

import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/set.{type Set}
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Member, type Message, type MessageDetails, type RelayStatus, BridgeRelay,
  Direct, HasDetails, MemberId, NoDetails, NoRelay, OneHop, SelfRelay, TwoHop,
  ViaPeer,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  message m: Message,
  receipts r: Set(String),
  members ms: List(Member),
  on_close on_close: msg,
) -> Element(msg) {
  let detail_section = case m.details {
    HasDetails(d) ->
      element.fragment([sender_section(p, d), delivery_section(p, d)])
    NoDetails -> element.fragment([])
  }
  html.aside(
    [
      attribute.attribute("data-testid", "details-panel"),
      ui.css([
        #("height", "100vh"),
        #("height", "100dvh"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("background", p.surface),
        #("border-left", "1px solid " <> p.border),
        #("overflow", "hidden"),
        #("min-width", "0"),
      ]),
    ],
    [
      header(p, on_close),
      html.div(
        [
          ui.css([
            #("flex", "1 1 auto"),
            #("min-height", "0"),
            #("overflow-y", "auto"),
            #("padding", "16px 18px 24px 18px"),
            #("display", "flex"),
            #("flex-direction", "column"),
            #("gap", "20px"),
          ]),
        ],
        [
          message_quote(p, m),
          detail_section,
          receipts_section(p, m, r, ms),
        ],
      ),
    ],
  )
}

fn header(p: Palette, on_close: msg) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("box-sizing", "border-box"),
        #("height", "60px"),
        #("flex-shrink", "0"),
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "0 16px"),
        #("border-bottom", "1px solid " <> p.border_soft),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("flex", "1"),
            #("min-width", "0"),
            #("font-weight", "600"),
            #("font-size", "16.875px"),
            #("color", p.text),
          ]),
        ],
        [html.text("Message details")],
      ),
      close_button(p, on_close),
    ],
  )
}

fn close_button(p: Palette, on_close: msg) -> Element(msg) {
  html.button(
    [
      attribute.title("Close details"),
      attribute.attribute("aria-label", "Close details"),
      attribute.attribute("data-testid", "details-close"),
      event.on_click(on_close),
      ui.css([
        // The shell's theme-toggle button is fixed at top:12px right:16px
        // with z-index 10; the details panel header's close button sits
        // in the same screen quadrant. Stack the close button above the
        // toggle while the panel is open so clicks land on close instead
        // of accidentally flipping the theme.
        #("position", "relative"),
        #("z-index", "30"),
        #("width", "28px"),
        #("height", "28px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("padding", "0"),
        #("border", "1px solid " <> p.border_soft),
        #("background", p.surface),
        #("color", p.text_muted),
        #("border-radius", "6px"),
        #("cursor", "pointer"),
        #("font-family", "inherit"),
      ]),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "svg",
        [
          attribute.attribute("width", "12"),
          attribute.attribute("height", "12"),
          attribute.attribute("viewBox", "0 0 12 12"),
          attribute.attribute("fill", "none"),
        ],
        [
          element.namespaced(
            "http://www.w3.org/2000/svg",
            "path",
            [
              attribute.attribute("d", "M3 3l6 6M9 3l-6 6"),
              attribute.attribute("stroke", "currentColor"),
              attribute.attribute("stroke-width", "1.5"),
              attribute.attribute("stroke-linecap", "round"),
            ],
            [],
          ),
        ],
      ),
    ],
  )
}

fn message_quote(p: Palette, m: Message) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("padding", "10px 12px"),
        #("background", p.surface_alt),
        #("border", "1px solid " <> p.border_soft),
        #("border-radius", "8px"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "4px"),
      ]),
    ],
    [
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("gap", "8px"),
            #("align-items", "baseline"),
          ]),
        ],
        [
          html.span([ui.css([#("font-weight", "600"), #("color", p.text)])], [
            html.text(m.author),
          ]),
          html.span(
            [ui.css([#("color", p.text_faint), #("font-size", "13.125px")])],
            [html.text(m.time)],
          ),
        ],
      ),
      html.div(
        [
          ui.css([
            #("color", p.text_muted),
            #("font-size", "15.625px"),
            #("white-space", "pre-wrap"),
            #("word-break", "break-word"),
          ]),
        ],
        [html.text(m.body)],
      ),
    ],
  )
}

fn sender_section(p: Palette, d: MessageDetails) -> Element(msg) {
  let badge = case d.verified {
    True -> verified_badge(p)
    False -> unverified_badge(p)
  }
  section(p, "Sender", [
    kv_row(p, "From", mono(p, d.sender), Some(badge)),
    kv_row(p, "Message ID", mono(p, d.message_id), None),
    kv_row(p, "Prev", mono(p, d.prev_id), None),
    kv_row(p, "Signature", mono(p, d.signature), None),
  ])
}

fn delivery_section(p: Palette, d: MessageDetails) -> Element(msg) {
  section(p, "Delivery", [
    kv_row(p, "Sent", html.text(d.sent_at), None),
    kv_row(p, "Delivered", html.text(d.delivered_at), None),
    html.div(
      [
        ui.css([
          #("display", "flex"),
          #("flex-direction", "column"),
          #("gap", "4px"),
          #("margin-top", "2px"),
        ]),
      ],
      [
        kv_label(p, "Path"),
        hops_chain(p, d.hops),
      ],
    ),
  ])
}

fn hops_chain(p: Palette, hops: List(String)) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("flex-wrap", "wrap"),
        #("align-items", "center"),
        #("gap", "4px"),
        #("font-size", "13.75px"),
        #("color", p.text),
      ]),
    ],
    list.intersperse(list.map(hops, fn(name) { hop_chip(p, name) }), arrow(p)),
  )
}

fn hop_chip(p: Palette, name: String) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("padding", "2px 8px"),
        #("background", p.accent_soft),
        #("color", p.accent_deep),
        #("border-radius", "999px"),
        #("font-weight", "500"),
        #("font-size", "13.125px"),
        #("white-space", "nowrap"),
      ]),
    ],
    [html.text(name)],
  )
}

fn arrow(p: Palette) -> Element(msg) {
  html.span([ui.css([#("color", p.text_faint), #("font-size", "11.5px")])], [
    html.text("→"),
  ])
}

fn receipts_section(
  p: Palette,
  m: Message,
  r: Set(String),
  ms: List(Member),
) -> Element(msg) {
  // Order: matches member-rail order so receipts read consistently
  // across panels. Pubkeys not in the member list (peer left, never
  // seen, etc.) get appended at the end.
  let from_members =
    list.filter_map(ms, fn(member) {
      let MemberId(pk) = member.id
      case set.contains(r, pk) {
        True -> Ok(#(pk, member.name, member.relay))
        False -> Error(Nil)
      }
    })
  let known_pks =
    list.fold(from_members, set.new(), fn(acc, t) { set.insert(acc, t.0) })
  let stragglers =
    set.to_list(set.difference(r, known_pks))
    |> list.map(fn(pk) { #(pk, pk, NoRelay) })
  let rows = list.append(from_members, stragglers)

  section(p, "Read by", [
    case rows {
      [] ->
        html.div(
          [
            ui.css([
              #("color", p.text_faint),
              #("font-size", "13.75px"),
              #("font-style", "italic"),
            ]),
          ],
          [html.text(empty_state_text(m))],
        )
      _ ->
        html.div(
          [
            ui.css([
              #("display", "flex"),
              #("flex-direction", "column"),
              #("gap", "8px"),
            ]),
          ],
          list.map(rows, fn(row) {
            let #(pk, name, relay) = row
            receipt_row(p, pk, name, relay)
          }),
        )
    },
  ])
}

/// Receipts only flow back for our own outgoing messages — peers don't
/// emit acks for messages they sent. Tell the reader which case applies
/// instead of just "no reads yet" everywhere.
fn empty_state_text(m: Message) -> String {
  case m.you {
    True -> "No reads yet."
    False -> "Receipts are only tracked for messages you sent."
  }
}

fn receipt_row(
  p: Palette,
  _pk: String,
  name: String,
  relay: RelayStatus,
) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", "receipt-row"),
      ui.css([
        #("display", "flex"),
        #("align-items", "baseline"),
        #("justify-content", "space-between"),
        #("gap", "8px"),
        #("padding", "6px 8px"),
        #("border", "1px solid " <> p.border_soft),
        #("border-radius", "6px"),
        #("background", p.surface_alt),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("font-weight", "600"),
            #("color", p.text),
            #("font-family", theme.font_mono),
            #("font-size", "13.75px"),
            #("overflow", "hidden"),
            #("text-overflow", "ellipsis"),
          ]),
        ],
        [html.text(name)],
      ),
      html.span(
        [
          ui.css([
            #("font-size", "12.5px"),
            #("color", p.text_muted),
            #("white-space", "nowrap"),
          ]),
        ],
        [html.text(relay_label(relay))],
      ),
    ],
  )
}

fn relay_label(r: RelayStatus) -> String {
  case r {
    Direct -> "direct"
    OneHop -> "1-hop"
    TwoHop -> "2-hop"
    ViaPeer(name) -> "via " <> name
    BridgeRelay -> "bridge"
    SelfRelay -> "self"
    NoRelay -> "—"
  }
}

fn section(p: Palette, title: String, rows: List(Element(msg))) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "8px"),
      ]),
    ],
    [
      html.div(
        [
          ui.css([
            #("font-size", "13.125px"),
            #("font-weight", "600"),
            #("text-transform", "uppercase"),
            #("letter-spacing", "0.05em"),
            #("color", p.text_faint),
          ]),
        ],
        [html.text(title)],
      ),
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("gap", "6px"),
          ]),
        ],
        rows,
      ),
    ],
  )
}

fn kv_row(
  p: Palette,
  label: String,
  value: Element(msg),
  trailing: Option(Element(msg)),
) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "baseline"),
        #("gap", "8px"),
        #("flex-wrap", "wrap"),
      ]),
    ],
    [
      kv_label(p, label),
      html.span([ui.css([#("flex", "1"), #("min-width", "0")])], [value]),
      case trailing {
        Some(el) -> el
        None -> element.fragment([])
      },
    ],
  )
}

fn kv_label(p: Palette, label: String) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("font-size", "12.5px"),
        #("color", p.text_faint),
        #("text-transform", "uppercase"),
        #("letter-spacing", "0.04em"),
        #("font-weight", "600"),
        #("min-width", "70px"),
      ]),
    ],
    [html.text(label)],
  )
}

fn mono(p: Palette, s: String) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("font-family", theme.font_mono),
        #("font-size", "13.125px"),
        #("color", p.text),
        #("word-break", "break-all"),
      ]),
    ],
    [html.text(s)],
  )
}

fn verified_badge(p: Palette) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("padding", "1px 7px"),
        #("border-radius", "999px"),
        #("background", p.ok_soft),
        #("color", p.ok),
        #("font-size", "11.5px"),
        #("font-weight", "600"),
        #("letter-spacing", "0.02em"),
      ]),
    ],
    [html.text("✓ verified")],
  )
}

fn unverified_badge(p: Palette) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("padding", "1px 7px"),
        #("border-radius", "999px"),
        #("background", p.warn_soft),
        #("color", p.warn),
        #("font-size", "11.5px"),
        #("font-weight", "600"),
      ]),
    ],
    [html.text("unverified")],
  )
}
