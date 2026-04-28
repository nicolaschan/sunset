//// Main column (column 3): channel header, messages, composer.
////
//// Hover state on each message row reveals a small toolbar with two
//// icon buttons:
////   • react — opens a 5-emoji quick-picker that toggles the user's
////     reaction on the message.
////   • info — opens the message-details side panel (in the right
////     column, replacing the members rail).
////
//// Image attachments are still deferred to a later plan.

import gleam/dynamic/decode
import gleam/int
import gleam/list
import gleam/option.{type Option, Some}
import gleam/string
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type ChannelId, type Message, type Reaction, ChannelId, HasBridge, HasDetails,
  Minecraft, NoBridge, NoDetails,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui

const quick_reactions = ["🌅", "👍", "👀", "🔥", "🌙"]

pub fn view(
  palette p: Palette,
  viewport viewport: domain.Viewport,
  current_channel cur: ChannelId,
  messages ms: List(Message),
  draft draft: String,
  on_draft on_draft: fn(String) -> msg,
  on_submit on_submit: msg,
  noop noop: msg,
  reacting_to reacting_to: Option(String),
  detail_msg_id detail_msg_id: Option(String),
  on_toggle_reaction_picker on_react_toggle: fn(String) -> msg,
  on_add_reaction on_add_reaction: fn(String, String) -> msg,
  on_open_detail on_open_detail: fn(String) -> msg,
) -> Element(msg) {
  let ChannelId(channel_name) = cur
  html.main(
    [
      ui.css([
        #("height", "100vh"),
        #("height", "100dvh"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("background", p.surface),
        #("min-width", "0"),
      ]),
    ],
    [
      channel_header(p, channel_name),
      messages_list(
        p,
        viewport,
        ms,
        reacting_to,
        detail_msg_id,
        on_react_toggle,
        on_add_reaction,
        on_open_detail,
      ),
      composer(p, viewport, channel_name, draft, on_draft, on_submit, noop),
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

fn messages_list(
  p: Palette,
  viewport: domain.Viewport,
  ms: List(Message),
  reacting_to: Option(String),
  detail_msg_id: Option(String),
  on_react_toggle: fn(String) -> msg,
  on_add_reaction: fn(String, String) -> msg,
  on_open_detail: fn(String) -> msg,
) -> Element(msg) {
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
      let picker_open = case reacting_to {
        Some(id) if id == m.id -> True
        _ -> False
      }
      let detail_open = case detail_msg_id {
        Some(id) if id == m.id -> True
        _ -> False
      }
      message_view(
        p,
        m,
        grouped,
        i == last_seen_index,
        picker_open,
        detail_open,
        on_react_toggle,
        on_add_reaction,
        on_open_detail,
      )
    })

  html.div(
    [
      attribute.class("scroll-area"),
      ui.css([
        #("flex", "1 1 auto"),
        #("overflow-y", "auto"),
        #("padding", case viewport {
          domain.Phone -> "12px 12px"
          domain.Desktop -> "16px 20px"
        }),
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
  picker_open: Bool,
  detail_open: Bool,
  on_react_toggle: fn(String) -> msg,
  on_add_reaction: fn(String, String) -> msg,
  on_open_detail: fn(String) -> msg,
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

  let bg = case picker_open || detail_open {
    True -> p.surface_alt
    False -> "transparent"
  }
  let row_class = case detail_open {
    True -> "msg-row is-active"
    False -> "msg-row"
  }

  html.div([], [
    html.div(
      [
        attribute.class(row_class),
        ui.css([
          #("position", "relative"),
          #("padding", "2px 8px"),
          #("border-radius", "6px"),
          #("opacity", opacity),
          #("margin-top", margin_top),
          #("background", bg),
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
        actions_toolbar(
          p,
          m,
          picker_open,
          on_react_toggle,
          on_add_reaction,
          on_open_detail,
        ),
        case picker_open {
          True -> reaction_picker(p, m.id, on_add_reaction)
          False -> element.fragment([])
        },
      ],
    ),
    case show_read_marker {
      True -> read_marker(p, m.seen_by)
      False -> element.fragment([])
    },
  ])
}

/// Floating toolbar in the top-right of each message row. Two
/// icon-only buttons: react (opens the emoji picker) and info (opens
/// the message-details side panel). Hidden by default; revealed on
/// hover via the .msg-row CSS rule in shell.gleam.
fn actions_toolbar(
  p: Palette,
  m: Message,
  picker_open: Bool,
  on_react_toggle: fn(String) -> msg,
  _on_add_reaction: fn(String, String) -> msg,
  on_open_detail: fn(String) -> msg,
) -> Element(msg) {
  let info_disabled = case m.details {
    HasDetails(_) -> False
    NoDetails -> True
  }
  html.div(
    [
      attribute.class("msg-actions"),
      ui.css([
        #("position", "absolute"),
        #("top", "-12px"),
        #("right", "12px"),
        #("display", "inline-flex"),
        #("gap", "2px"),
        #("padding", "2px"),
        #("background", p.surface),
        #("border", "1px solid " <> p.border),
        #("border-radius", "6px"),
        #("box-shadow", p.shadow),
      ]),
    ],
    [
      action_button(
        p,
        "React",
        smiley_icon(),
        picker_open,
        False,
        on_react_toggle(m.id),
      ),
      action_button(
        p,
        "Message details",
        info_icon(),
        False,
        info_disabled,
        on_open_detail(m.id),
      ),
    ],
  )
}

fn action_button(
  p: Palette,
  title: String,
  icon: Element(msg),
  active: Bool,
  disabled: Bool,
  click: msg,
) -> Element(msg) {
  let bg = case active {
    True -> p.accent_soft
    False -> "transparent"
  }
  let color = case active {
    True -> p.accent_deep
    False -> p.text_muted
  }
  let cursor = case disabled {
    True -> "not-allowed"
    False -> "pointer"
  }
  let opacity = case disabled {
    True -> "0.4"
    False -> "1"
  }
  html.button(
    [
      attribute.title(title),
      attribute.attribute("aria-label", title),
      event.on_click(click),
      attribute.disabled(disabled),
      ui.css([
        #("width", "26px"),
        #("height", "26px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("padding", "0"),
        #("border", "none"),
        #("background", bg),
        #("color", color),
        #("border-radius", "4px"),
        #("cursor", cursor),
        #("opacity", opacity),
        #("font-family", "inherit"),
      ]),
    ],
    [icon],
  )
}

fn reaction_picker(
  p: Palette,
  msg_id: String,
  on_add_reaction: fn(String, String) -> msg,
) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", "reaction-picker"),
      ui.css([
        #("position", "absolute"),
        #("top", "18px"),
        #("right", "12px"),
        #("display", "inline-flex"),
        #("gap", "2px"),
        #("padding", "4px"),
        #("background", p.surface),
        #("border", "1px solid " <> p.border),
        #("border-radius", "999px"),
        #("box-shadow", p.shadow_lg),
        #("z-index", "5"),
      ]),
    ],
    list.map(quick_reactions, fn(emoji) {
      html.button(
        [
          attribute.title("React with " <> emoji),
          event.on_click(on_add_reaction(msg_id, emoji)),
          ui.css([
            #("width", "32px"),
            #("height", "32px"),
            #("display", "inline-flex"),
            #("align-items", "center"),
            #("justify-content", "center"),
            #("padding", "0"),
            #("border", "none"),
            #("background", "transparent"),
            #("border-radius", "999px"),
            #("cursor", "pointer"),
            #("font-size", "18px"),
            #("font-family", "inherit"),
          ]),
        ],
        [html.text(emoji)],
      )
    }),
  )
}

fn smiley_icon() -> Element(msg) {
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "14"),
      attribute.attribute("height", "14"),
      attribute.attribute("viewBox", "0 0 16 16"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "circle",
        [
          attribute.attribute("cx", "8"),
          attribute.attribute("cy", "8"),
          attribute.attribute("r", "5.5"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M5.8 9.5c.6.7 1.4 1 2.2 1s1.6-.3 2.2-1"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "circle",
        [
          attribute.attribute("cx", "6.2"),
          attribute.attribute("cy", "6.7"),
          attribute.attribute("r", "0.8"),
          attribute.attribute("fill", "currentColor"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "circle",
        [
          attribute.attribute("cx", "9.8"),
          attribute.attribute("cy", "6.7"),
          attribute.attribute("r", "0.8"),
          attribute.attribute("fill", "currentColor"),
        ],
        [],
      ),
    ],
  )
}

fn info_icon() -> Element(msg) {
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "14"),
      attribute.attribute("height", "14"),
      attribute.attribute("viewBox", "0 0 16 16"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "circle",
        [
          attribute.attribute("cx", "8"),
          attribute.attribute("cy", "8"),
          attribute.attribute("r", "5.5"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M8 11V7M8 5.2v.05"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.6"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
    ],
  )
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
        #("margin-bottom", "4px"),
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
  viewport: domain.Viewport,
  channel_name: String,
  draft: String,
  on_draft: fn(String) -> msg,
  on_submit: msg,
  noop: msg,
) -> Element(msg) {
  // Fixed 64px height with a 1px border-top — the rooms-rail you_row
  // and channels-rail self-bar share the same shape so the three
  // column-bottom rows align on the same horizontal seam.
  html.div(
    [
      ui.css([
        #("box-sizing", "border-box"),
        #("min-height", "64px"),
        #("flex-shrink", "0"),
        #("display", "flex"),
        #("align-items", "center"),
        #("padding", case viewport {
          domain.Phone -> "0 12px"
          domain.Desktop -> "0 20px"
        }),
        #("padding-bottom", "env(safe-area-inset-bottom)"),
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
            event.on("keydown", {
              use key <- decode.subfield(["key"], decode.string)
              decode.success(case key {
                "Enter" -> on_submit
                _ -> noop
              })
            }),
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
