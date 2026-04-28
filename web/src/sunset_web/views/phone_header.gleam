//// 56px sticky header for phone layout. Three slots: hamburger
//// (opens channels drawer), room title (room name + connection dot),
//// members icon (opens members drawer). Padding-top consumes safe-area
//// inset for notch / dynamic island.

import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{
  type ConnStatus, type Room, Connected, Offline, Reconnecting,
}
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  room r: Room,
  on_open_channels on_open_channels: msg,
  on_open_members on_open_members: msg,
) -> Element(msg) {
  html.header(
    [
      attribute.attribute("data-testid", "phone-header"),
      ui.css([
        #("position", "sticky"),
        #("top", "0"),
        #("z-index", "10"),
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("box-sizing", "border-box"),
        #("height", "calc(56px + env(safe-area-inset-top))"),
        #("padding", "env(safe-area-inset-top) 12px 0 12px"),
        #("background", p.surface),
        #("border-bottom", "1px solid " <> p.border),
        #("flex-shrink", "0"),
      ]),
    ],
    [
      icon_button(
        p,
        on_open_channels,
        "phone-rooms-toggle",
        "Open channels",
        hamburger_icon(),
      ),
      title(p, r),
      icon_button(
        p,
        on_open_members,
        "phone-members-toggle",
        "Open members",
        members_icon(),
      ),
    ],
  )
}

fn title(p: Palette, r: Room) -> Element(msg) {
  html.div(
    [
      ui.css([
        #("flex", "1"),
        #("min-width", "0"),
        #("display", "flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("gap", "6px"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("font-weight", "600"),
            #("font-size", "16.875px"),
            #("color", p.text),
            #("white-space", "nowrap"),
            #("overflow", "hidden"),
            #("text-overflow", "ellipsis"),
            #("max-width", "100%"),
          ]),
        ],
        [html.text(r.name)],
      ),
      conn_dot(p, r.status),
    ],
  )
}

fn icon_button(
  p: Palette,
  on_click: msg,
  test_id: String,
  label: String,
  icon: Element(msg),
) -> Element(msg) {
  html.button(
    [
      attribute.attribute("data-testid", test_id),
      attribute.attribute("aria-label", label),
      attribute.title(label),
      event.on_click(on_click),
      ui.css([
        #("width", "40px"),
        #("height", "40px"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("justify-content", "center"),
        #("padding", "0"),
        #("border", "none"),
        #("background", "transparent"),
        #("color", p.text),
        #("border-radius", "8px"),
        #("cursor", "pointer"),
        #("font-family", "inherit"),
        #("flex-shrink", "0"),
      ]),
    ],
    [icon],
  )
}

fn conn_dot(p: Palette, status: ConnStatus) -> Element(msg) {
  let c = case status {
    Connected -> p.live
    Reconnecting -> p.warn
    Offline -> p.text_faint
  }
  html.span(
    [
      ui.css([
        #("width", "8px"),
        #("height", "8px"),
        #("border-radius", "999px"),
        #("background", c),
        #("flex-shrink", "0"),
      ]),
    ],
    [],
  )
}

fn hamburger_icon() -> Element(msg) {
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "20"),
      attribute.attribute("height", "20"),
      attribute.attribute("viewBox", "0 0 20 20"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M4 6h12M4 10h12M4 14h12"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.6"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
    ],
  )
}

fn members_icon() -> Element(msg) {
  element.namespaced(
    "http://www.w3.org/2000/svg",
    "svg",
    [
      attribute.attribute("width", "20"),
      attribute.attribute("height", "20"),
      attribute.attribute("viewBox", "0 0 20 20"),
      attribute.attribute("fill", "none"),
    ],
    [
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "circle",
        [
          attribute.attribute("cx", "8"),
          attribute.attribute("cy", "8"),
          attribute.attribute("r", "3"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M2 17c0-3 2.7-5 6-5s6 2 6 5"),
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
          attribute.attribute("cx", "14"),
          attribute.attribute("cy", "8"),
          attribute.attribute("r", "2.4"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.2"),
          attribute.attribute("opacity", "0.7"),
        ],
        [],
      ),
    ],
  )
}
