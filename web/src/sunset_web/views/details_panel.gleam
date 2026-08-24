//// Right-column message-details panel — replaces the members rail
//// when a message's info button is clicked.
////
//// Renders three sections:
////   • the quoted message body
////   • delivery acknowledgements — peers whose delivery receipt for
////     this message has landed locally, each stamped with the unix-ms
////     when that peer composed the receipt
////   • reactions — per-emoji breakdown of who reacted and when
////
//// "Delivered" rather than "Read" because what we surface is a
//// best-effort, automatic ack the recipient writes when the encrypted
//// payload decodes locally; it doesn't claim the user has actually
//// looked at the message yet.
////
//// Closes via the X button in the top-right.

import gleam/dict.{type Dict}
import gleam/int
import gleam/list
import gleam/order
import gleam/string
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Member, type MessageView, type RelayStatus, Direct, MemberId, NoRelay,
  OneHop, SelfRelay, TwoHop, ViaPeer,
}
import sunset_web/sunset
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  message m: MessageView,
  receipts r: Dict(String, Int),
  reactions reactions: Dict(String, Dict(String, Int)),
  members ms: List(Member),
  name_map nm: Dict(String, String),
  on_close on_close: msg,
) -> Element(msg) {
  // Sized by the right-rail flex-column wrapper in sunset_web (which
  // also pins the self/settings row beneath). `flex: 1; min-height: 0`
  // here lets the panel fill the column above the row while its inner
  // body scrolls.
  html.aside(
    [
      attribute.attribute("data-testid", "details-panel"),
      ui.css([
        #("flex", "1 1 auto"),
        #("min-height", "0"),
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
          receipts_section(p, m, r, ms, nm),
          reactions_section(p, reactions, nm),
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

fn message_quote(p: Palette, m: MessageView) -> Element(msg) {
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

fn receipts_section(
  p: Palette,
  m: MessageView,
  r: Dict(String, Int),
  ms: List(Member),
  nm: Dict(String, String),
) -> Element(msg) {
  // Order: matches member-rail order so receipts read consistently
  // across panels. Pubkeys not in the member list (peer left, never
  // seen, etc.) get appended at the end.
  //
  // Receipts are keyed by full-hex pubkey. Member ids are short-pubkey
  // (8 hex chars). Match by comparing the first 8 chars of the receipt
  // key against the member's short id.
  let from_members =
    list.filter_map(ms, fn(member) {
      let MemberId(short) = member.id
      case
        list.find_map(dict.to_list(r), fn(pair) {
          let #(full_hex, ts) = pair
          case string.slice(full_hex, 0, 8) == short {
            True -> Ok(#(full_hex, ts))
            False -> Error(Nil)
          }
        })
      {
        Ok(#(full_hex, ts)) -> {
          let name = case dict.get(nm, full_hex) {
            Ok(n) -> n
            Error(_) -> member.name
          }
          Ok(#(full_hex, name, member.relay, ts))
        }
        Error(_) -> Error(Nil)
      }
    })
  let known_hexes = list.map(from_members, fn(t) { t.0 })
  let stragglers =
    dict.to_list(r)
    |> list.filter(fn(pair) { !list.contains(known_hexes, pair.0) })
    |> list.map(fn(pair) {
      let #(full_hex, ts) = pair
      let name = case dict.get(nm, full_hex) {
        Ok(n) -> n
        Error(_) -> short_pubkey_from_hex(full_hex)
      }
      #(full_hex, name, NoRelay, ts)
    })
  let rows = list.append(from_members, stragglers)

  section(p, "Delivered to", [
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
            let #(pk, name, relay, ts) = row
            receipt_row(p, pk, name, relay, ts)
          }),
        )
    },
  ])
}

/// Receipts only flow back for our own outgoing messages — peers don't
/// emit acks for messages they sent. Tell the reader which case applies
/// instead of just "no acks yet" everywhere.
fn empty_state_text(m: MessageView) -> String {
  case m.you {
    True -> "No deliveries yet."
    False -> "Receipts are only tracked for messages you sent."
  }
}

fn receipt_row(
  p: Palette,
  _pk: String,
  name: String,
  relay: RelayStatus,
  delivered_at_ms: Int,
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
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("align-items", "baseline"),
            #("gap", "10px"),
            #("white-space", "nowrap"),
          ]),
        ],
        [
          html.span(
            [
              ui.css([
                #("font-size", "12.5px"),
                #("color", p.text),
                #("font-variant-numeric", "tabular-nums"),
              ]),
            ],
            [html.text(sunset.format_time_ms_exact(delivered_at_ms))],
          ),
          html.span(
            [
              ui.css([
                #("font-size", "12.5px"),
                #("color", p.text_muted),
              ]),
            ],
            [html.text(relay_label(relay))],
          ),
        ],
      ),
    ],
  )
}

fn reactions_section(
  p: Palette,
  reactions: Dict(String, Dict(String, Int)),
  nm: Dict(String, String),
) -> Element(msg) {
  let entries =
    dict.to_list(reactions)
    |> list.filter(fn(pair) { dict.size(pair.1) > 0 })
    // Stable-ish ordering: emoji asc. The engine uses LWW by
    // `(sent_at_ms, value_hash)`, but at the panel level we just want a
    // deterministic listing.
    |> list.sort(fn(a, b) { string.compare(a.0, b.0) })

  section(p, "Reactions", [
    case entries {
      [] ->
        html.div(
          [
            ui.css([
              #("color", p.text_faint),
              #("font-size", "13.75px"),
              #("font-style", "italic"),
            ]),
          ],
          [html.text("No reactions yet.")],
        )
      _ ->
        html.div(
          [
            ui.css([
              #("display", "flex"),
              #("flex-direction", "column"),
              #("gap", "10px"),
            ]),
          ],
          list.map(entries, fn(pair) {
            let #(emoji, authors) = pair
            reaction_group(p, emoji, authors, nm)
          }),
        )
    },
  ])
}

fn reaction_group(
  p: Palette,
  emoji: String,
  authors: Dict(String, Int),
  nm: Dict(String, String),
) -> Element(msg) {
  // Sort reactors oldest-first so the list reads as a chronological
  // story of who reacted when. Within equal timestamps fall back to
  // pubkey for determinism.
  let sorted =
    dict.to_list(authors)
    |> list.sort(fn(a, b) {
      case int.compare(a.1, b.1) {
        order.Eq -> string.compare(a.0, b.0)
        other -> other
      }
    })
  html.div(
    [
      attribute.attribute("data-testid", "reaction-group"),
      ui.css([
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "4px"),
        #("padding", "8px 10px"),
        #("border", "1px solid " <> p.border_soft),
        #("border-radius", "8px"),
        #("background", p.surface_alt),
      ]),
    ],
    [
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("align-items", "baseline"),
            #("gap", "8px"),
          ]),
        ],
        [
          html.span(
            [ui.css([#("font-size", "18.75px"), #("line-height", "1")])],
            [html.text(emoji)],
          ),
          html.span(
            [
              ui.css([
                #("font-size", "12.5px"),
                #("color", p.text_faint),
                #("font-variant-numeric", "tabular-nums"),
              ]),
            ],
            [html.text(reactor_count_label(list.length(sorted)))],
          ),
        ],
      ),
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("gap", "2px"),
          ]),
        ],
        list.map(sorted, fn(pair) {
          let #(author_hex, ts) = pair
          reactor_row(p, author_hex, ts, nm)
        }),
      ),
    ],
  )
}

fn reactor_row(
  p: Palette,
  author_hex: String,
  sent_at_ms: Int,
  nm: Dict(String, String),
) -> Element(msg) {
  // Look up the reactor's chosen display name from name_map (keyed by
  // full hex). Fall back to the 8-char hex prefix when no name has been
  // observed — covers offline reactors and peers who haven't set a name.
  let display_name = case dict.get(nm, author_hex) {
    Ok(name) -> name
    Error(_) -> short_pubkey_from_hex(author_hex)
  }
  html.div(
    [
      attribute.attribute("data-testid", "reactor-row"),
      ui.css([
        #("display", "flex"),
        #("align-items", "baseline"),
        #("justify-content", "space-between"),
        #("gap", "8px"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("font-family", theme.font_mono),
            #("font-size", "13.75px"),
            #("color", p.text),
            #("overflow", "hidden"),
            #("text-overflow", "ellipsis"),
          ]),
        ],
        [html.text(display_name)],
      ),
      html.span(
        [
          ui.css([
            #("font-size", "12.5px"),
            #("color", p.text_muted),
            #("white-space", "nowrap"),
            #("font-variant-numeric", "tabular-nums"),
          ]),
        ],
        [html.text(sunset.format_time_ms_exact(sent_at_ms))],
      ),
    ],
  )
}

/// Returns the first 8 hex characters of a full-hex pubkey string.
/// Used as a fallback display name when no chosen name is in the map.
/// Matches what short_pubkey() returns for the first 4 bytes of a key.
fn short_pubkey_from_hex(hex: String) -> String {
  string.slice(hex, 0, 8)
}

fn reactor_count_label(n: Int) -> String {
  case n {
    1 -> "1 reactor"
    _ -> int.to_string(n) <> " reactors"
  }
}

fn relay_label(r: RelayStatus) -> String {
  case r {
    Direct -> "direct"
    OneHop -> "1-hop"
    TwoHop -> "2-hop"
    ViaPeer(name) -> "via " <> name
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
