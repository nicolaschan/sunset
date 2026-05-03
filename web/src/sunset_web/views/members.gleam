//// Members rail (column 4) — online + offline groupings, status dots,
//// no avatars. Routing-detail-on-hover is deferred.

import gleam/int
import gleam/list
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type Member, Away, MutedP, OfflineP, Online, Speaking,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  members ms: List(Member),
  on_open_status on_open: fn(domain.MemberId) -> msg,
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

  html.aside(
    [
      ui.css([
        #("height", "100vh"),
        #("height", "100dvh"),
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
    ]),
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
  let dot_color = case m.status {
    Speaking -> p.live
    Online -> p.live
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
  let color = case m.status {
    Speaking -> p.text
    _ -> p.text_muted
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
        html.span(
          [
            ui.css([
              #("width", "7px"),
              #("height", "7px"),
              #("border-radius", "999px"),
              #("background", dot_color),
              #("flex-shrink", "0"),
            ]),
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

