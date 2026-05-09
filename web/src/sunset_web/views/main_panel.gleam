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

import gleam/dict.{type Dict}
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
  type ChannelId, type Member, type MessageView, type Reaction, Away, ChannelId,
  Direct, OfflineP, OneHop, Online, SelfRelay, Speaking,
}
import sunset_web/markdown
import sunset_web/theme.{type Palette}
import sunset_web/ui

const quick_reactions = ["🌅", "👍", "👀", "🔥", "🌙"]

pub fn view(
  palette p: Palette,
  viewport viewport: domain.Viewport,
  current_channel cur: ChannelId,
  messages ms: List(MessageView),
  draft draft: String,
  on_draft on_draft: fn(String) -> msg,
  on_submit on_submit: msg,
  noop noop: msg,
  on_shortcut on_shortcut: fn(String, String, String, Bool) -> msg,
  reacting_to reacting_to: Option(String),
  detail_msg_id detail_msg_id: Option(String),
  on_toggle_reaction_picker on_react_toggle: fn(String) -> msg,
  on_add_reaction on_add_reaction: fn(String, String) -> msg,
  on_open_full_picker on_open_full_picker: fn(String, Option(#(Float, Float))) ->
    msg,
  on_open_detail on_open_detail: fn(String) -> msg,
  receipts receipts: Dict(String, Dict(String, Int)),
  selected_msg_id selected_msg_id: Option(String),
  on_toggle_selected on_toggle_selected: fn(String) -> msg,
  is_spoiler_revealed is_revealed: fn(markdown.SpoilerKey) -> Bool,
  on_toggle_spoiler on_toggle_spoiler: fn(markdown.SpoilerKey) -> msg,
  // Live members list — used to color message author names by their
  // current connection state.
  members members: List(Member),
  // Slot rendered between the channel header and the message list.
  // Used by the phone shell for the in-call voice mini-bar — voice
  // chat is a per-channel concern, so the banner sits inside the
  // channel's column. Pass `element.fragment([])` for none.
  voice_minibar voice_minibar: Element(msg),
) -> Element(msg) {
  let ChannelId(channel_name) = cur
  // On phone the host (shell.phone_view) gives this column a flex slot
  // sized by the page's column layout; setting height: 100dvh here
  // would overflow that slot and clip the composer behind the iOS URL
  // bar. Use height: 100% to fill the slot. On desktop the column
  // lives directly under the grid and needs the dvh anchor itself.
  let height_props = case viewport {
    domain.Phone -> [#("height", "100%"), #("min-height", "0")]
    domain.Desktop -> [#("height", "100vh"), #("height", "100dvh")]
  }
  html.main(
    [
      ui.css(
        list.append(height_props, [
          #("display", "flex"),
          #("flex-direction", "column"),
          #("background", p.surface),
          #("min-width", "0"),
        ]),
      ),
    ],
    [
      channel_header(p, channel_name),
      voice_minibar,
      messages_list(
        p,
        viewport,
        ms,
        reacting_to,
        detail_msg_id,
        on_react_toggle,
        on_add_reaction,
        on_open_full_picker,
        on_open_detail,
        receipts,
        selected_msg_id,
        on_toggle_selected,
        is_revealed,
        on_toggle_spoiler,
        members,
      ),
      composer(
        p,
        viewport,
        channel_name,
        draft,
        on_draft,
        on_submit,
        noop,
        on_shortcut,
      ),
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
  ms: List(MessageView),
  reacting_to: Option(String),
  detail_msg_id: Option(String),
  on_react_toggle: fn(String) -> msg,
  on_add_reaction: fn(String, String) -> msg,
  on_open_full_picker: fn(String, Option(#(Float, Float))) -> msg,
  on_open_detail: fn(String) -> msg,
  receipts: Dict(String, Dict(String, Int)),
  selected_msg_id: Option(String),
  on_toggle_selected: fn(String) -> msg,
  is_revealed: fn(markdown.SpoilerKey) -> Bool,
  on_toggle_spoiler: fn(markdown.SpoilerKey) -> msg,
  members: List(Member),
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
      let selected = case selected_msg_id {
        Some(id) if id == m.id -> True
        _ -> False
      }
      message_view(
        p,
        viewport,
        m,
        grouped,
        i == last_seen_index,
        picker_open,
        detail_open,
        selected,
        on_react_toggle,
        on_add_reaction,
        on_open_full_picker,
        on_open_detail,
        on_toggle_selected,
        receipts,
        is_revealed,
        on_toggle_spoiler,
        author_color(p, m, members),
      )
    })

  html.div(
    [
      attribute.attribute("data-testid", "messages-list"),
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
    rendered,
  )
}

fn message_view(
  p: Palette,
  viewport: domain.Viewport,
  m: MessageView,
  grouped: Bool,
  show_read_marker: Bool,
  picker_open: Bool,
  detail_open: Bool,
  selected: Bool,
  on_react_toggle: fn(String) -> msg,
  on_add_reaction: fn(String, String) -> msg,
  on_open_full_picker: fn(String, Option(#(Float, Float))) -> msg,
  on_open_detail: fn(String) -> msg,
  on_toggle_selected: fn(String) -> msg,
  receipts: Dict(String, Dict(String, Int)),
  is_revealed: fn(markdown.SpoilerKey) -> Bool,
  on_toggle_spoiler: fn(markdown.SpoilerKey) -> msg,
  author_color: String,
) -> Element(msg) {
  let pending =
    m.you
    && {
      case dict.get(receipts, m.id) {
        Ok(d) -> dict.size(d) == 0
        Error(_) -> True
      }
    }
  let opacity = case pending {
    True -> "0.55"
    False -> "1"
  }
  let margin_top = case grouped {
    True -> "2px"
    False -> "10px"
  }

  let header = case grouped {
    True -> element.fragment([])
    False -> message_header(p, m, author_color)
  }

  // Row classes:
  //   * `is-active` whenever a per-message menu is up (reaction picker
  //     / details panel) — pins both the highlight backdrop and the
  //     hover-only action toolbar visible while the user is interacting
  //     with that menu, even after the cursor leaves the row.
  //   * `is-selected` mirrors `selected_msg_id`. Used by the touch
  //     stylesheet to keep the action toolbar visible on tap (since
  //     :hover doesn't fire on touch devices).
  // The hover background, edge-to-edge stretching, and active-state
  // backdrop are all driven by CSS rules in `shell.global_reset` —
  // inline styles can't express :hover, and keeping the rules in one
  // place makes the layout easier to reason about.
  let row_class = case picker_open || detail_open, selected {
    True, True -> "msg-row is-active is-selected"
    True, False -> "msg-row is-active"
    False, True -> "msg-row is-selected"
    False, False -> "msg-row"
  }

  // Stretch the row's inline-block-with-padding so its background can
  // bleed to the edges of the messages container. The horizontal
  // padding here mirrors the messages_list container's own horizontal
  // padding (16/20px desktop, 12px phone), and the matching negative
  // margin pulls the row outside that padding so the highlight reads
  // as full-bleed.
  let bleed_h = case viewport {
    domain.Phone -> "12px"
    domain.Desktop -> "20px"
  }

  // Wrap header + body + reactions in an inner clickable div so a
  // tap toggles selection. The actions_toolbar lives as a sibling of
  // this wrapper (still a child of .msg-row, absolutely positioned),
  // so clicks on action buttons bubble to .msg-row but never through
  // the body wrapper — the React/Info buttons can't accidentally
  // toggle the selection.
  //
  // The pending-state opacity is applied to the inner clickable wrapper
  // (header + body + reactions), NOT to .msg-row itself. `opacity < 1`
  // creates a CSS stacking context, and putting it on .msg-row would
  // trap the reaction picker's `z-index: 5` inside that row's context —
  // which would let the *next* row's body paint on top of the picker
  // (the picker is taller than a single-line row, so it spills into
  // the next row's bounds). Keeping .msg-row free of opacity lets the
  // picker's z-index escape to the messages_list stacking context, so
  // it sits above every following row regardless of those rows'
  // pending-state stacking contexts. The actions toolbar / picker
  // staying at full opacity is also a UX improvement: the controls a
  // user reaches for stay legible while the still-unacked body fades.
  html.div([], [
    html.div(
      [
        attribute.class(row_class),
        ui.css([
          #("position", "relative"),
          #("padding", "2px " <> bleed_h),
          #("margin-left", "-" <> bleed_h),
          #("margin-right", "-" <> bleed_h),
          // No bg transition: the .is-active / :hover background change
          // must apply synchronously so a test (or a screen reader, or
          // a user that takes a screenshot at the wrong moment) reads
          // the highlighted state immediately. A 120 ms ease-in raced
          // with `getComputedStyle` polling — the property's "current
          // value" at t=0 is the transition's *from* value (transparent),
          // not the *to* value, so the row would intermittently report
          // `rgba(0,0,0,0)` even with `is-active` already on it.
          #("margin-top", margin_top),
        ]),
      ],
      [
        html.div(
          [
            event.on_click(on_toggle_selected(m.id)),
            ui.css([
              #("display", "flex"),
              #("flex-direction", "column"),
              #("opacity", opacity),
              #("transition", "opacity 220ms ease"),
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
              [markdown.render(m.body, m.id, is_revealed, on_toggle_spoiler, p)],
            ),
            case m.reactions {
              [] -> element.fragment([])
              rs -> reactions_row(p, m.id, rs, on_add_reaction)
            },
          ],
        ),
        actions_toolbar(
          p,
          m,
          picker_open,
          on_react_toggle,
          on_add_reaction,
          on_open_detail,
        ),
        case viewport, picker_open {
          domain.Desktop, True ->
            reaction_picker(p, m.id, on_add_reaction, on_open_full_picker)
          _, _ -> element.fragment([])
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
  m: MessageView,
  picker_open: Bool,
  on_react_toggle: fn(String) -> msg,
  _on_add_reaction: fn(String, String) -> msg,
  on_open_detail: fn(String) -> msg,
) -> Element(msg) {
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
      // Info is always enabled — the panel still has useful content
      // (the message body and any read receipts) even when the row
      // doesn't carry full crypto provenance.
      action_button(
        p,
        "Message details",
        info_icon(),
        False,
        False,
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
  on_open_full_picker: fn(String, Option(#(Float, Float))) -> msg,
) -> Element(msg) {
  let quick_buttons =
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
    })
  let plus_button =
    html.button(
      [
        attribute.title("More reactions"),
        attribute.attribute("data-testid", "reaction-picker-more"),
        // Capture the click's `clientY` and the live viewport height
        // (`view.innerHeight`) so the desktop overlay can decide
        // whether there's more room above or below the trigger before
        // it positions itself. Both fields are read directly off the
        // MouseEvent — the `view` accessor is the WindowProxy that
        // owns the document, which is non-null for any UIEvent fired
        // from a click.
        event.on("click", {
          use cy <- decode.subfield(["clientY"], decode.float)
          use vh <- decode.subfield(["view", "innerHeight"], decode.float)
          decode.success(on_open_full_picker(msg_id, option.Some(#(cy, vh))))
        }),
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
          #("font-size", "18px"),
          #("color", p.text_muted),
          #("cursor", "pointer"),
          #("font-family", "inherit"),
        ]),
      ],
      [html.text("+")],
    )
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
    list.append(quick_buttons, [plus_button]),
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

fn message_header(
  p: Palette,
  m: MessageView,
  author_color: String,
) -> Element(msg) {
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
            attribute.attribute("data-testid", "message-author"),
            attribute.attribute("data-author", m.author),
            ui.css([
              #("font-weight", "600"),
              #("font-size", "16.25px"),
              #("color", author_color),
              #("cursor", "default"),
            ]),
          ],
          [html.text(m.author)],
        ),
      ],
      [],
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

fn reactions_row(
  p: Palette,
  message_id: String,
  rs: List(Reaction),
  on_toggle_reaction: fn(String, String) -> msg,
) -> Element(msg) {
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
    list.map(rs, fn(r) { reaction_pill(p, message_id, r, on_toggle_reaction) }),
  )
}

fn reaction_pill(
  p: Palette,
  message_id: String,
  r: Reaction,
  on_toggle_reaction: fn(String, String) -> msg,
) -> Element(msg) {
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
  let title = case r.by_you {
    True -> "Remove your " <> r.emoji <> " reaction"
    False -> "React with " <> r.emoji
  }
  // stop_propagation: the message body wrapper above us toggles row
  // selection on click, which a pill click should not trigger.
  html.button(
    [
      attribute.attribute("data-testid", "reaction-pill"),
      attribute.attribute("data-emoji", r.emoji),
      attribute.attribute("aria-pressed", case r.by_you {
        True -> "true"
        False -> "false"
      }),
      attribute.title(title),
      event.stop_propagation(
        event.on_click(on_toggle_reaction(message_id, r.emoji)),
      ),
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
        #("font-family", "inherit"),
        #("cursor", "pointer"),
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

fn composer(
  p: Palette,
  viewport: domain.Viewport,
  channel_name: String,
  draft: String,
  on_draft: fn(String) -> msg,
  on_submit: msg,
  noop: msg,
  on_shortcut: fn(String, String, String, Bool) -> msg,
) -> Element(msg) {
  // The composer's outer height drives the column-bottom seam shared
  // with the rooms-rail you_row and channels-rail self-bar (64px); the
  // empty-state container must stay at 64px (±1px) so the bottom seam
  // reads as one horizontal line. Vertical padding is explicit (not
  // derived from `align-items: center` against `min-height`) so a
  // multi-line draft keeps the same bottom gutter as a single line —
  // relying on the centering hack would collapse the gutter to zero
  // once the textarea grows past 64px.
  //
  // Math: inner content ≈ 42px (one-line textarea + 8px ×2 inner pad +
  // 1px ×2 inner border) + 1px outer border-top + 2 × v_pad. With
  // v_pad = 10.5px the empty composer is 64px exactly; multi-line
  // grows the container while preserving the same 10.5px gutter top
  // and bottom.
  let v_pad = "10.5px"
  let h_pad = case viewport {
    domain.Phone -> "12px"
    domain.Desktop -> "20px"
  }
  html.div(
    [
      ui.css([
        #("box-sizing", "border-box"),
        #("min-height", "64px"),
        #("flex-shrink", "0"),
        #("display", "flex"),
        #("align-items", "center"),
        #("padding", v_pad <> " " <> h_pad),
        // Add safe-area-inset on top of the v_pad gutter; older WebKits
        // can't safely embed `env()` in the `padding` shorthand, so we
        // override just the bottom side here.
        #(
          "padding-bottom",
          "calc(" <> v_pad <> " + env(safe-area-inset-bottom))",
        ),
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
          html.textarea(
            [
              attribute.id("composer-textarea"),
              attribute.autofocus(True),
              attribute.placeholder("Message #" <> channel_name),
              attribute.attribute("rows", "1"),
              event.on_input(on_draft),
              event.advanced("keydown", {
                use key <- decode.subfield(["key"], decode.string)
                use shift <- decode.subfield(["shiftKey"], decode.bool)
                use meta <- decode.subfield(["metaKey"], decode.bool)
                use ctrl <- decode.subfield(["ctrlKey"], decode.bool)
                let mod = meta || ctrl
                // For Enter (no shift): prevent default so the browser does not
                // insert a newline into the textarea before Lustre clears it.
                // For all other keys let the browser's default action proceed.
                decode.success(case key, shift, mod {
                  "Enter", False, _ ->
                    event.handler(
                      on_submit,
                      prevent_default: True,
                      stop_propagation: False,
                    )
                  "b", _, True ->
                    event.handler(
                      on_shortcut("**", "", "**", True),
                      prevent_default: False,
                      stop_propagation: False,
                    )
                  "B", _, True ->
                    event.handler(
                      on_shortcut("**", "", "**", True),
                      prevent_default: False,
                      stop_propagation: False,
                    )
                  "i", _, True ->
                    event.handler(
                      on_shortcut("*", "", "*", True),
                      prevent_default: False,
                      stop_propagation: False,
                    )
                  "I", _, True ->
                    event.handler(
                      on_shortcut("*", "", "*", True),
                      prevent_default: False,
                      stop_propagation: False,
                    )
                  "k", _, True ->
                    event.handler(
                      on_shortcut("[", "", "](url)", True),
                      prevent_default: False,
                      stop_propagation: False,
                    )
                  "K", _, True ->
                    event.handler(
                      on_shortcut("[", "", "](url)", True),
                      prevent_default: False,
                      stop_propagation: False,
                    )
                  _, _, _ ->
                    event.handler(
                      noop,
                      prevent_default: False,
                      stop_propagation: False,
                    )
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
                #("resize", "none"),
                #("overflow", "hidden"),
                #("padding", "0"),
                #("line-height", "1.4"),
              ]),
            ],
            draft,
          ),
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

/// Pick the color for a message author's name based on the matching
/// member's connection state. Lookup is by `m.author == member.name`
/// (both hold the resolved display name, or the short-pubkey fallback
/// when no name has been set).
///
/// Mapping:
///   * own messages → palette accent (so "you" stands out)
///   * online + direct WebRTC → palette ok (green; healthy mesh)
///   * online + via-relay → palette warn (amber; not direct)
///   * speaking → palette live (matches the voice-rail dot)
///   * away → palette warn
///   * offline → palette text_faint
///   * fallback (no member match yet) → palette text
fn author_color(p: Palette, m: MessageView, members: List(Member)) -> String {
  case m.you {
    True -> p.accent
    False ->
      case list.find(members, fn(mem) { mem.name == m.author }) {
        Error(_) -> p.text
        Ok(mem) -> color_for_member(p, mem)
      }
  }
}

fn color_for_member(p: Palette, mem: Member) -> String {
  case mem.status, mem.relay {
    OfflineP, _ -> p.text_faint
    Away, _ -> p.warn
    Speaking, _ -> p.live
    Online, Direct -> p.ok
    Online, OneHop -> p.warn
    Online, SelfRelay -> p.text
    _, _ -> p.text
  }
}

/// Index of the last own message that's been seen by anyone — that's
/// where the "read up to here" marker goes.
fn last_own_seen_index(ms: List(MessageView)) -> Int {
  do_last_own_seen(ms, 0, -1)
}

fn do_last_own_seen(ms: List(MessageView), i: Int, best: Int) -> Int {
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
