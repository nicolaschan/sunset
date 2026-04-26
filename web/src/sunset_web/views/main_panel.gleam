//// Main column (column 3): channel header, messages, composer.
////
//// Image attachments and the message-details panel are deferred to a
//// later plan. Reactions render statically (no picker).

import gleam/int
import gleam/list
import gleam/string
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type ChannelId, type Message, type Reaction, ChannelId, HasBridge, Minecraft,
  NoBridge,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  current_channel cur: ChannelId,
  messages ms: List(Message),
  draft draft: String,
  on_draft on_draft: fn(String) -> msg,
) -> Element(msg) {
  let ChannelId(channel_name) = cur
  html.main(
    [
      ui.css([
        #("height", "100vh"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("background", p.surface),
        #("min-width", "0"),
      ]),
    ],
    [
      channel_header(p, channel_name),
      messages_list(p, ms),
      composer(p, channel_name, draft, on_draft),
    ],
  )
}

fn channel_header(p: Palette, name: String) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("box-sizing", "border-box"),
        #("height", "60px"),
        #("flex-shrink", "0"),
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "0 24px"),
        #("border-bottom", "1px solid " <> p.border_soft),
      ]),
    ],
    [
      html.span([ui.css([#("color", p.text_faint)])], [html.text("#")]),
      html.span(
        [
          ui.css([
            #("font-weight", "600"),
            #("font-size", "18.75px"),
            #("color", p.text),
          ]),
        ],
        [html.text(name)],
      ),
    ],
  )
}

fn messages_list(p: Palette, ms: List(Message)) -> Element(msg) {
  let last_seen_index = last_own_seen_index(ms)
  // Pair each message with its index AND its predecessor's author (for grouping).
  let rendered =
    ms
    |> list.index_map(fn(m, i) {
      let prev_author = case i {
        0 -> ""
        _ ->
          case list.first(list.drop(ms, i - 1)) {
            Ok(prev) -> prev.author
            Error(_) -> ""
          }
      }
      let grouped = i > 0 && prev_author == m.author
      message_view(p, m, grouped, i == last_seen_index)
    })

  html.div(
    [
      ui.css([
        #("flex", "1 1 auto"),
        #("overflow-y", "auto"),
        #("padding", "16px 20px"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("gap", "0"),
      ]),
    ],
    list.append(rendered, [typing_indicator(p)]),
  )
}

fn message_view(
  p: Palette,
  m: Message,
  grouped: Bool,
  show_read_marker: Bool,
) -> Element(msg) {
  let opacity = case m.pending {
    True -> "0.55"
    False -> "1"
  }
  let margin_top = case grouped {
    True -> "2px"
    False -> "10px"
  }

  let header = case grouped {
    True -> element.fragment([])
    False -> message_header(p, m)
  }

  html.div([], [
    html.div(
      [
        ui.css([
          #("padding", "2px 8px"),
          #("border-radius", "6px"),
          #("opacity", opacity),
          #("margin-top", margin_top),
        ]),
      ],
      [
        header,
        html.div(
          [
            ui.css([
              #("font-size", "16.875px"),
              #("color", p.text),
              #("white-space", "pre-wrap"),
              #("word-break", "break-word"),
            ]),
          ],
          [html.text(m.body)],
        ),
        case m.reactions {
          [] -> element.fragment([])
          rs -> reactions_row(p, rs)
        },
      ],
    ),
    case show_read_marker {
      True -> read_marker(p, m.seen_by)
      False -> element.fragment([])
    },
  ])
}

fn message_header(p: Palette, m: Message) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "baseline"),
        #("gap", "8px"),
        #("margin-bottom", "2px"),
      ]),
    ],
    list.flatten([
      [
        html.span(
          [
            ui.css([
              #("font-weight", "600"),
              #("font-size", "16.25px"),
              #("color", p.text),
              #("cursor", "default"),
            ]),
          ],
          [html.text(m.author)],
        ),
      ],
      case m.bridge {
        HasBridge(Minecraft) -> [bridge_tag(p, "⛏ minecraft")]
        NoBridge -> []
      },
      case m.you {
        True -> [you_tag(p)]
        False -> []
      },
      [
        html.span(
          [
            ui.css([
              #("font-size", "13.125px"),
              #("color", p.text_faint),
              #("white-space", "nowrap"),
            ]),
          ],
          [html.text(m.time)],
        ),
      ],
      case m.pending {
        True -> [
          html.span(
            [
              ui.css([
                #("font-size", "13.125px"),
                #("color", p.warn),
                #("font-style", "italic"),
              ]),
            ],
            [html.text("sending…")],
          ),
        ]
        False -> []
      },
    ]),
  )
}

fn reactions_row(p: Palette, rs: List(Reaction)) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("flex-wrap", "wrap"),
        #("gap", "4px"),
        #("margin-top", "4px"),
      ]),
    ],
    list.map(rs, fn(r) { reaction_pill(p, r) }),
  )
}

fn reaction_pill(p: Palette, r: Reaction) -> Element(msg) {
  let bg = case r.by_you {
    True -> p.accent_soft
    False -> p.surface_alt
  }
  let color = case r.by_you {
    True -> p.accent_deep
    False -> p.text_muted
  }
  let border = case r.by_you {
    True -> p.accent
    False -> p.border_soft
  }
  html.span(
    [
      ui.css([
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("gap", "4px"),
        #("padding", "1px 8px"),
        #("border-radius", "999px"),
        #("background", bg),
        #("color", color),
        #("border", "1px solid " <> border),
        #("font-size", "13.75px"),
      ]),
    ],
    [
      html.text(r.emoji),
      html.span([], [html.text(int.to_string(r.count))]),
    ],
  )
}

fn read_marker(p: Palette, seen_by: Int) -> Element(msg) {
  let label = case seen_by {
    0 -> ""
    1 -> "read by 1"
    n -> "read by " <> int.to_string(n)
  }
  case string.is_empty(label) {
    True -> element.fragment([])
    False ->
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("align-items", "center"),
            #("gap", "10px"),
            #("padding", "6px 8px"),
            #("font-size", "13.125px"),
            #("color", p.text_faint),
          ]),
        ],
        [
          html.span(
            [
              ui.css([
                #("flex", "1"),
                #("height", "1px"),
                #("background", p.border_soft),
              ]),
            ],
            [],
          ),
          html.span([], [html.text("↑ " <> label)]),
          html.span(
            [
              ui.css([
                #("flex", "1"),
                #("height", "1px"),
                #("background", p.border_soft),
              ]),
            ],
            [],
          ),
        ],
      )
  }
}

fn typing_indicator(p: Palette) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("padding", "8px 8px 0 8px"),
        #("font-size", "14.375px"),
        #("color", p.text_faint),
        #("font-style", "italic"),
      ]),
    ],
    [html.text("noor is typing…")],
  )
}

fn composer(
  p: Palette,
  channel_name: String,
  draft: String,
  on_draft: fn(String) -> msg,
) -> Element(msg) {
  // Fixed 64px height with a 1px border-top — the rooms-rail you_row
  // and channels-rail self-bar share the same shape so the three
  // column-bottom rows align on the same horizontal seam.
  html.div(
    [
      ui.css([
        #("box-sizing", "border-box"),
        #("height", "64px"),
        #("flex-shrink", "0"),
        #("display", "flex"),
        #("align-items", "center"),
        #("padding", "0 20px"),
        #("border-top", "1px solid " <> p.border_soft),
      ]),
    ],
    [
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("align-items", "center"),
            #("gap", "8px"),
            #("padding", "8px 10px"),
            #("flex", "1"),
            #("background", p.surface_alt),
            #("border", "1px solid " <> p.border),
            #("border-radius", "8px"),
          ]),
        ],
        [
          attach_button(p),
          html.input([
            attribute.value(draft),
            attribute.placeholder("Message #" <> channel_name),
            event.on_input(on_draft),
            ui.css([
              #("flex", "1"),
              #("border", "none"),
              #("background", "transparent"),
              #("font-family", "inherit"),
              #("font-size", "16.25px"),
              #("color", p.text),
              #("outline", "none"),
            ]),
          ]),
          html.span(
            [
              ui.css([
                #("font-family", theme.font_mono),
                #("font-size", "13.125px"),
                #("color", p.text_faint),
              ]),
            ],
            [html.text("↵ send")],
          ),
        ],
      ),
    ],
  )
}

fn attach_button(p: Palette) -> Element(msg) {
  html.button(
    [
      attribute.title("Attach image"),
      ui.css([
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("width", "24px"),
        #("height", "24px"),
        #("border", "none"),
        #("background", "transparent"),
        #("color", p.text_faint),
        #("cursor", "pointer"),
        #("padding", "0"),
        #("border-radius", "4px"),
      ]),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "svg",
        [
          attribute.attribute("width", "16"),
          attribute.attribute("height", "16"),
          attribute.attribute("viewBox", "0 0 16 16"),
          attribute.attribute("fill", "none"),
        ],
        [
          element.namespaced(
            "http://www.w3.org/2000/svg",
            "rect",
            [
              attribute.attribute("x", "2.5"),
              attribute.attribute("y", "3"),
              attribute.attribute("width", "11"),
              attribute.attribute("height", "10"),
              attribute.attribute("rx", "1.5"),
              attribute.attribute("stroke", "currentColor"),
              attribute.attribute("stroke-width", "1.3"),
            ],
            [],
          ),
          element.namespaced(
            "http://www.w3.org/2000/svg",
            "circle",
            [
              attribute.attribute("cx", "6"),
              attribute.attribute("cy", "7"),
              attribute.attribute("r", "1.2"),
              attribute.attribute("fill", "currentColor"),
            ],
            [],
          ),
          element.namespaced(
            "http://www.w3.org/2000/svg",
            "path",
            [
              attribute.attribute("d", "M3 11l3-3 4 4 3-2"),
              attribute.attribute("stroke", "currentColor"),
              attribute.attribute("stroke-width", "1.3"),
              attribute.attribute("stroke-linejoin", "round"),
            ],
            [],
          ),
        ],
      ),
    ],
  )
}

fn bridge_tag(p: Palette, label: String) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("padding", "1px 5px"),
        #("border-radius", "3px"),
        #("background", p.accent_soft),
        #("color", p.accent_deep),
        #("font-size", "13.125px"),
        #("font-weight", "500"),
      ]),
    ],
    [html.text(label)],
  )
}

fn you_tag(p: Palette) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("padding", "1px 4px"),
        #("border-radius", "3px"),
        #("background", p.surface_alt),
        #("color", p.text_faint),
        #("font-size", "11.875px"),
        #("font-weight", "500"),
        #("letter-spacing", "0.02em"),
        #("text-transform", "uppercase"),
      ]),
    ],
    [html.text("you")],
  )
}

/// Index of the last own message that's been seen by anyone — that's
/// where the "read up to here" marker goes.
fn last_own_seen_index(ms: List(Message)) -> Int {
  do_last_own_seen(ms, 0, -1)
}

fn do_last_own_seen(ms: List(Message), i: Int, best: Int) -> Int {
  case ms {
    [] -> best
    [m, ..rest] -> {
      let new_best = case m.you && m.seen_by > 0 {
        True -> i
        False -> best
      }
      do_last_own_seen(rest, i + 1, new_best)
    }
  }
}
