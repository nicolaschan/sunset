//// Right-column message-details panel — replaces the members rail
//// when a message's info button is clicked.
////
//// Renders up to five sections:
////   • the quoted message body (always)
////   • sender / cryptographic provenance (when full details are known)
////   • delivery path (when full details are known)
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
import gleam/option.{type Option, None, Some}
import gleam/order
import gleam/string
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Member, type Message, type MessageDetails, type RelayStatus, Direct,
  HasDetails, MemberId, NoDetails, NoRelay, OneHop, SelfRelay, TwoHop, ViaPeer,
}
import sunset_web/sunset
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  message m: Message,
  receipts r: Dict(String, Int),
  reactions reactions: Dict(String, Dict(String, Int)),
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
          reactions_section(p, reactions, ms),
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
            html.text(sunset.short_pubkey(m.author_pubkey)),
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
  r: Dict(String, Int),
  ms: List(Member),
) -> Element(msg) {
  // Order: matches member-rail order so receipts read consistently
  // across panels. Pubkeys not in the member list (peer left, never
  // seen, etc.) get appended at the end.
  let from_members =
    list.filter_map(ms, fn(member) {
      let MemberId(pk) = member.id
      case dict.get(r, pk) {
        Ok(ts) -> Ok(#(pk, member.name, member.relay, ts))
        Error(_) -> Error(Nil)
      }
    })
  let known_pks = list.map(from_members, fn(t) { t.0 })
  let stragglers =
    dict.to_list(r)
    |> list.filter(fn(pair) { !list.contains(known_pks, pair.0) })
    |> list.map(fn(pair) {
      let #(pk, ts) = pair
      #(pk, pk, NoRelay, ts)
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
fn empty_state_text(m: Message) -> String {
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
  ms: List(Member),
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
            reaction_group(p, emoji, authors, ms)
          }),
        )
    },
  ])
}

fn reaction_group(
  p: Palette,
  emoji: String,
  authors: Dict(String, Int),
  ms: List(Member),
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
          reactor_row(p, author_hex, ts, ms)
        }),
      ),
    ],
  )
}

fn reactor_row(
  p: Palette,
  author_hex: String,
  sent_at_ms: Int,
  ms: List(Member),
) -> Element(msg) {
  // Member ids are short-pubkey (8 hex). Reaction author keys arrive
  // from the reactions tracker as full hex. Match by prefix so the
  // friendly name shows up when available; otherwise render the short
  // hex form as the identifier.
  let short = string.slice(author_hex, 0, 8)
  let display_name = case
    list.find(ms, fn(member) {
      let MemberId(pk) = member.id
      pk == short
    })
  {
    Ok(member) -> member.name
    Error(_) -> short
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
