//// Members rail (column 4) — online + offline groupings, status dots,
//// no avatars. Routing-detail-on-hover is deferred.

import gleam/int
import gleam/list
import lustre/element.{type Element}
import lustre/element/html
import sunset_web/domain.{
  type Member, Away, HasBridge, MutedP, NoBridge, OfflineP, Online, Speaking,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  members ms: List(Member),
) -> Element(msg) {
  let in_call_others =
    ms |> list.filter(fn(m) { m.in_call && !m.you })
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
      list.map(in_call_others, fn(m) { member_row(p, m, False) }),
      list.map(online_not_in_call, fn(m) { member_row(p, m, False) }),
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
              section_title(
                p,
                "Offline — " <> int.to_string(offline_count),
              ),
            ],
            list.map(offline_members, fn(m) { member_row(p, m, True) }),
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
        #("font-size", "10.5px"),
        #("font-weight", "600"),
        #("text-transform", "uppercase"),
        #("letter-spacing", "0.04em"),
        #("color", p.text_faint),
      ]),
    ],
    [html.text(label)],
  )
}

fn member_row(p: Palette, m: Member, dim: Bool) -> Element(msg) {
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
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "5px 10px"),
        #("opacity", opacity),
      ]),
    ],
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
              #("font-size", "13px"),
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
      ],
      case m.bridge {
        HasBridge(_) -> [bridge_tag(p)]
        NoBridge -> []
      },
    ]),
  )
}

fn bridge_tag(p: Palette) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("padding", "1px 5px"),
        #("border-radius", "3px"),
        #("background", p.accent_soft),
        #("color", p.accent_deep),
        #("font-size", "10px"),
        #("font-weight", "500"),
      ]),
    ],
    [html.text("⛏")],
  )
}
