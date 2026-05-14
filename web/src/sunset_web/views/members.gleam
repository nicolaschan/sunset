//// Members rail (column 4) — online + offline member groupings, plus
//// the relays section (which used to live in the channels rail but
//// is conceptually about who/what we're connected to, not what
//// channels exist; same column as the member roster reads more
//// naturally).

import gleam/int
import gleam/list
import gleam/option.{type Option}
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Member, type Relay, Away, MutedP, OfflineP, Online, Speaking,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui
import sunset_web/views/relays as relays_view

pub fn view(
  palette p: Palette,
  members ms: List(Member),
  relays relays: List(Relay),
  on_open_status on_open: fn(domain.MemberId) -> msg,
  on_open_relay on_open_relay: fn(Float, Option(Float)) -> msg,
) -> Element(msg) {
  let in_call_others = ms |> list.filter(fn(m) { m.in_call && !m.you })
  let online_not_in_call =
    ms
    |> list.filter(fn(m) {
      !m.in_call
      && case m.status {
        OfflineP -> False
        _ -> True
      }
    })
  let offline_members =
    ms
    |> list.filter(fn(m) {
      case m.status {
        OfflineP -> True
        _ -> False
      }
    })

  let online_count =
    list.length(in_call_others) + list.length(online_not_in_call)
  let offline_count = list.length(offline_members)

  // `height: 100%` resolves correctly for both layouts: the drawer's
  // safe-area-padded content box on phone, and the desktop grid row
  // (sized to 100dvh by shell.desktop_view's `grid-template-rows`).
  // A bare 100dvh would overflow the drawer's clipping box on phone
  // PWA mode and cover the iOS home indicator.
  html.aside(
    [
      ui.css([
        #("height", "100%"),
        #("min-height", "0"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("background", p.surface),
        #("border-left", "1px solid " <> p.border),
        #("padding", "14px 8px"),
        #("overflow-y", "auto"),
      ]),
    ],
    list.flatten([
      [section_title(p, "Online — " <> int.to_string(online_count))],
      list.map(in_call_others, fn(m) { member_row(p, m, on_open, False) }),
      list.map(online_not_in_call, fn(m) { member_row(p, m, on_open, False) }),
      case offline_count {
        0 -> []
        _ ->
          list.flatten([
            [
              html.div(
                [
                  ui.css([
                    #("height", "12px"),
                  ]),
                ],
                [],
              ),
              section_title(p, "Offline — " <> int.to_string(offline_count)),
            ],
            list.map(offline_members, fn(m) { member_row(p, m, on_open, True) }),
          ])
      },
      // Relays show after the member groupings. Same column rather than
      // a separate panel because the user reads "who am I connected to"
      // top-to-bottom: members in the room, then the upstream relays
      // those members route through.
      case relays {
        [] -> []
        _ -> [relay_section(p, relays, on_open_relay)]
      },
    ]),
  )
}

/// "Relays" section header + rows, rendered into the members rail.
/// The wrapping div carries `data-testid="relays-section"` so e2e
/// tests can assert presence of the relay UI regardless of which rail
/// it lives in (the testid contract predates this move).
fn relay_section(
  p: Palette,
  relays: List(Relay),
  on_open_relay: fn(Float, Option(Float)) -> msg,
) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", "relays-section"),
      ui.css([
        #("display", "flex"),
        #("flex-direction", "column"),
      ]),
    ],
    [
      html.div([ui.css([#("height", "12px")])], []),
      section_title(p, "Relays — " <> int.to_string(list.length(relays))),
      html.div(
        [
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("gap", "1px"),
          ]),
        ],
        list.map(relays, fn(r) { relays_view.row(p, r, on_open_relay) }),
      ),
    ],
  )
}

fn section_title(p: Palette, label: String) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("padding", "4px 10px 8px 10px"),
        #("font-size", "13.125px"),
        #("font-weight", "600"),
        #("text-transform", "uppercase"),
        #("letter-spacing", "0.04em"),
        #("color", p.text_faint),
      ]),
    ],
    [html.text(label)],
  )
}

fn member_row(
  p: Palette,
  m: Member,
  on_open: fn(domain.MemberId) -> msg,
  dim: Bool,
) -> Element(msg) {
  // Universal presence semantics:
  //   * Speaking / Online → green (palette ok = palette live)
  //   * Away → amber (warn)
  //   * Muted / Offline → muted gray
  // No accent / sunset color in the presence dot — sunset is reserved
  // for branding / CTAs, and using it for status was making "online"
  // hard to recognize at a glance.
  let dot_color = case m.status {
    Speaking -> p.ok
    Online -> p.ok
    Away -> p.warn
    MutedP -> p.text_faint
    OfflineP -> p.text_faint
  }
  let opacity = case dim {
    True -> "0.55"
    False -> "1"
  }
  let weight = case m.status {
    Speaking -> "600"
    _ -> "400"
  }
  // Member name color matches the chat author color so a glance at
  // the right rail tells you what color each speaker's messages will
  // be down in the chat:
  //   * YOU → palette accent (same as own-message author name)
  //   * offline → muted gray (the section reads as inactive)
  //   * everyone else → stable per-author hue keyed off the display
  //     name (must match `main_panel.author_color`'s lookup key)
  let color = case m.you, m.status {
    True, _ -> p.accent
    False, OfflineP -> p.text_faint
    False, _ -> theme.hue_for_identity(p, m.name)
  }

  // Click anywhere on the row opens the per-peer status popover.
  // Self isn't actionable — no handler, no cursor change.
  let click_attrs = case m.you {
    True -> []
    False -> [
      event.on_click(on_open(m.id)),
      ui.css([#("cursor", "pointer")]),
    ]
  }

  html.div(
    list.append(
      [
        attribute.attribute("data-testid", "member-row"),
        attribute.attribute("data-member-id", member_id_str(m.id)),
        ui.css([
          #("display", "flex"),
          #("align-items", "center"),
          #("gap", "8px"),
          #("padding", "5px 10px"),
          #("opacity", opacity),
        ]),
      ],
      click_attrs,
    ),
    list.flatten([
      [
        // Offline members render a hollow ring rather than a filled
        // dot — same shape, but visually reads as "this is off" without
        // the user having to remember which gray means what.
        html.span(
          [
            ui.css(case m.status {
              OfflineP -> [
                #("width", "7px"),
                #("height", "7px"),
                #("border-radius", "999px"),
                #("background", "transparent"),
                #("border", "1.5px solid " <> dot_color),
                #("box-sizing", "border-box"),
                #("flex-shrink", "0"),
              ]
              _ -> [
                #("width", "7px"),
                #("height", "7px"),
                #("border-radius", "999px"),
                #("background", dot_color),
                #("flex-shrink", "0"),
              ]
            }),
          ],
          [],
        ),
        html.span(
          [
            ui.css([
              #("font-size", "16.25px"),
              #("font-weight", weight),
              #("color", color),
              #("flex", "1"),
              #("min-width", "0"),
              #("white-space", "nowrap"),
              #("overflow", "hidden"),
              #("text-overflow", "ellipsis"),
            ]),
          ],
          [html.text(m.name)],
        ),
        transport_icon(p, m.relay),
      ],
      [],
    ]),
  )
}

/// Return a small Unicode glyph indicating the transport route for this
/// peer. `↔` for direct WebRTC, `⤴` for via-relay; nothing for self or
/// unknown topologies (deferred multi-hop / bridge variants).
fn transport_icon(p: Palette, r: domain.RelayStatus) -> Element(msg) {
  let glyph = case r {
    domain.Direct -> "↔"
    domain.OneHop -> "⤴"
    _ -> ""
  }
  case glyph {
    "" -> element.fragment([])
    g ->
      html.span(
        [
          attribute.attribute("data-testid", "member-transport-icon"),
          ui.css([
            #("font-size", "12px"),
            #("color", p.text_faint),
            #("flex-shrink", "0"),
          ]),
        ],
        [html.text(g)],
      )
  }
}

fn member_id_str(id: domain.MemberId) -> String {
  let domain.MemberId(s) = id
  s
}
