# Mobile-friendly frontend — design

**Status:** draft
**Date:** 2026-04-27
**Surface:** `web/` (Gleam + Lustre)

## Goal

Make the existing `web/` frontend usable on phones with feature parity to
desktop. Today the shell is pinned to a `position: fixed; inset: 0`
4-column grid with hard-coded column widths (260/54px, 230px, 1fr,
220/320px) and a fixed-position voice popover (`top: 120px; left:
540px`). Below ~900px the columns squeeze and the popover lands
off-screen. We want a phone-first layout below a single 768px
breakpoint that feels native: drawers for navigation, bottom sheets for
inspectors, no iOS auto-zoom on inputs, real safe-area handling,
keyboard-aware viewport, and parity with every desktop interaction
(including drag-drop room reorder).

## Non-goals

- Native iOS / Android wrappers. The target is mobile *web* only —
  Mobile Safari on iOS and Chrome on Android.
- Tablet-specific layout. iPads and similar fall in the desktop bucket
  (≥768px wide). A future plan can add a tablet tier if it earns its
  keep.
- Edge-swipe gestures, pinch-zoom, or other gesture handling beyond
  what HTML / `pointer*` events give us for free. Header icons drive
  drawer open/close.
- Reworking the desktop layout. Desktop renders today's 4-column
  shell unchanged; mobile branches off at the shell level.

## Decisions

| Decision | Choice |
|---|---|
| Layout strategy | Drawer-style overlays on phone; columns on desktop |
| Breakpoint | Two-tier at 768px (`max-width: 767px` is phone) |
| Right-side panels on phone | Members → right drawer · Message details → bottom sheet |
| Voice popover on phone | Bottom sheet |
| Landing page on phone | Full-screen takeover (drop centered-card framing) |
| Drawer affordances | Header icons only; no swipe gestures |
| Channels on phone | Two drawers; tapping room name in the channels drawer swaps to the rooms drawer |
| Self-controls during a call | Floating mini-bar (PiP-style) in the chat view |
| Long-press shortcut to leave call | Dropped — open the voice sheet to leave |

## Architecture

### Implementation strategy

A single shell with **width-aware model + structural branching**:

- A new `viewport: Viewport (Phone | Desktop)` field in `Model`,
  initialised from `window.matchMedia("(max-width: 767px)").matches` in
  `init`, and updated via a FFI subscription
  (`MediaQueryList.addEventListener("change")`) that dispatches
  `Msg::ViewportChanged(Viewport)`.
- `shell.view` branches on `viewport` and renders one of two layouts.
  Desktop is today's 4-column grid, structurally untouched. Phone is
  the new header + chat + drawers + sheet + mini-bar layout.
- Drawer/sheet open state lives in the model and only affects rendering
  on phone. Drawer/sheet elements are *always present in the DOM*,
  positioned offscreen via `transform: translateX(-100%)` (or
  `translateY(100%)` for sheets) when closed. This keeps CSS
  transitions cheap and avoids remount thrash on close.
- Column views (`rooms`, `channels`, `main_panel`, `members`,
  `details_panel`, `voice_popover`) stay viewport-agnostic. The shell
  hands them the right wrapper.

A pure-CSS responsive approach was rejected: Lustre's `ui.css([...])`
emits `style="..."` attributes that always win against stylesheet
rules without `!important`, and drawer state is structural anyway
(needs to be in the model), so CSS would only handle the cosmetic
parts. Two completely separate top-level views were rejected as
overkill — the column views are already reusable.

### State shape

New domain types in `domain.gleam`:

```gleam
pub type Viewport {
  Phone
  Desktop
}

pub type Drawer {
  RoomsDrawer
  ChannelsDrawer
  MembersDrawer
}

pub type Sheet {
  DetailsSheet(message_id: String)
  VoiceSheet(member_name: String)
}
```

`Model` additions in `sunset_web.gleam`:

- `viewport: Viewport`
- `drawer: Option(Drawer)` — currently open drawer (phone only;
  desktop ignores). Only one drawer is visible at a time. Navigation
  from channels to rooms is modeled as a *swap*: tapping the room
  title in the channels drawer dispatches `OpenDrawer(RoomsDrawer)`,
  which replaces the field's value. Visually, the channels drawer
  slides off-left as the rooms drawer slides on-left
  (cross-transition, both share the same left-side anchor). Tapping
  the rooms-drawer backdrop sets the field to `None` and closes back
  to chat — there's no "back to channels" affordance in v1; the user
  reopens the channels drawer from the header if needed. This
  simplifies the model (`Option(Drawer)` instead of a stack) at the
  cost of one extra tap when the user wants to return to channels
  after opening rooms.
- `sheet: Option(Sheet)` — replaces `detail_msg_id: Option(String)`
  and `voice_popover: Option(String)`. The merge enforces mutual
  exclusion (only one bottom sheet on phone at a time). On desktop the
  same field drives both today's right-rail details panel *and* the
  floating voice popover; opening one while the other is showing
  closes the other. This is a small behaviour change on desktop but
  matches what was already racy.

`Msg` additions:

- `ViewportChanged(Viewport)`
- `OpenDrawer(Drawer)`, `CloseDrawer`

Existing `OpenDetail`, `CloseDetail`, `OpenVoicePopover`,
`CloseVoicePopover`, `SetMemberVolume`, `ToggleMemberDenoise`,
`ToggleMemberDeafen`, `ResetMemberVoice` remain — their handlers
update the new `sheet` field instead of the dropped fields. Call sites
in `channels.gleam`, `main_panel.gleam`, etc. don't change.

### FFI additions in `storage.gleam`

```gleam
@external(javascript, "./storage.ffi.mjs", "isPhoneViewport")
pub fn is_phone_viewport() -> Bool

@external(javascript, "./storage.ffi.mjs", "onViewportChange")
pub fn on_viewport_change(cb: fn(Bool) -> Nil) -> Nil
```

JS side: `matchMedia("(max-width: 767px)")`, plus an `addEventListener`
on the `MediaQueryList`. The callback receives a `Bool` (`true` =
phone) which `init`'s effect translates into a `ViewportChanged` msg.

## Phone layout

```
┌─────────────────────────────────────┐
│ HEADER (56px)                       │  hamburger · room name · members
├─────────────────────────────────────┤
│                                     │
│  CHAT VIEW (1fr)                    │  main_panel.view
│                                     │
├─────────────────────────────────────┤
│ COMPOSER (auto)                     │
└─────────────────────────────────────┘
+ floating voice mini-bar (when in call)
+ drawers (offscreen via transform)
+ bottom sheet (offscreen via transform)
```

### Header (`views/phone_header.gleam`, new)

56px tall, sticky at top. Three slots:

- **Left:** hamburger icon, `data-testid="phone-rooms-toggle"`,
  dispatches `OpenDrawer(ChannelsDrawer)` (the channels drawer is the
  primary nav surface; rooms is reachable from inside it).
- **Center:** current room name + connection dot. Truncates with
  ellipsis on narrow screens.
- **Right:** members icon, `data-testid="phone-members-toggle"`,
  dispatches `OpenDrawer(MembersDrawer)`.

Padding-top uses `env(safe-area-inset-top)` to clear notch / dynamic
island.

### Chat view

Renders `main_panel.view` unchanged structurally. Two phone-specific
adjustments via the global stylesheet (gated on `@media (hover:
none)`):

- `.msg-row .msg-actions` — `opacity: 1` always (no hover on touch).
- Padding tightens (`12px` instead of `16px`) to recover horizontal
  space.

### Composer

Existing structure. Phone overrides:

- `<input>` / `<textarea>` get `font-size: 16px` minimum (iOS
  no-zoom).
- Wrapper uses `padding-bottom: max(env(safe-area-inset-bottom), 8px)`
  to clear the home-indicator on iPhone.

### Drawers

Two new modules, plus the channels drawer reusing the channels view:

- `views/drawer.gleam` — `pub fn view(palette, open, side, on_close,
  content) -> Element(msg)`. Owns the wrapper `<aside>` with
  `transform`, the `transition: transform 220ms ease`, and the
  backdrop.
- Sides: `Left` and `Right`. The drawer wrapper renders at 84% of
  viewport width, capped at 320px.

**Channels drawer (`Drawer = ChannelsDrawer`):** wraps
`channels.view`. The room name in `channels.view`'s header becomes
tappable on phone — dispatches `OpenDrawer(RoomsDrawer)` which slides
the rooms drawer in on top.

**Rooms drawer (`Drawer = RoomsDrawer`):** wraps `rooms.view`. Slides
in from the left, replacing the channels drawer (cross-transition
from the same anchor). Selecting a room dispatches `JoinRoom` *and*
`CloseDrawer` so the drawer closes and the user lands in chat for the
chosen room. The footer hosts the theme toggle on phone (relocated
from the desktop's fixed bottom-right pill).

**Members drawer (`Drawer = MembersDrawer`):** wraps `members.view`.
Slides in from the right.

Backdrop tap dispatches `CloseDrawer`, returning the user to the
chat view regardless of which drawer was open.

### Bottom sheet (`views/bottom_sheet.gleam`, new)

`pub fn view(palette, open, on_close, content) -> Element(msg)`. Rises
from `bottom: 0`, full width, `max-height: 75dvh`, with a 4px
drag-handle visual at the top edge (purely cosmetic — no actual swipe
gesture in v1). Tap-backdrop dismisses.

Hosts:

- **Details sheet** when `sheet = Some(DetailsSheet(id))` —
  `details_panel.view` content.
- **Voice sheet** when `sheet = Some(VoiceSheet(name))` —
  `voice_popover.view` content. The current `voice_popover.view` hard-codes
  `position: fixed`, `top: 120px`, `left: 540px`, `width: 320px`.
  Implementation refactors it to take a `placement: Floating |
  InSheet` parameter (or two thin wrappers) so the same content
  renders as the desktop floating popover *or* as full-width sheet
  content, without duplicating the body markup.
- **Reaction picker on phone** — when the picker is anchored to a
  message on phone, the picker mounts as a sheet rather than a
  positioned overlay, since the desktop overlay anchoring relies on
  hover row geometry that touch can't reproduce reliably. The picker
  doesn't fold into the `Sheet` ADT (it's a different state path —
  `reacting_to`), but it reuses the same `bottom_sheet.view`
  primitive.

### Voice mini-bar (`views/voice_minibar.gleam`, new)

Renders only when `viewport == Phone` *and* the user is in a call.
Floating pill, `position: fixed; right: 12px; bottom: calc(env(safe-area-inset-bottom) + 76px)`
(clears composer + safe-area). Shows live dot + channel name + mic
icon. Tapping dispatches `OpenVoicePopover("you")`, which puts the
voice sheet in front of the user no matter which drawer was last
open.

The desktop self-controls bar at the bottom of the channels column is
*hidden* on phone (its mute/headphones/leave functions live in the
voice sheet; the mini-bar is the always-visible affordance).

### Landing page on phone

`views/landing.gleam` reads `viewport` and renders an alternate
phone layout: edge-to-edge content, larger hero text, the input
expands to full width minus 24px gutters, the join button is full-width
under the input rather than to the right.

## Mobile-specific concerns

### iOS no-zoom on input focus

Inputs and textareas must use `font-size >= 16px` on phone, otherwise
iOS auto-zooms when focused. The composer and rooms-search currently
inherit 16.875px (passes), but the landing-input and any future
inputs need to be audited. Phone-only stylesheet rule:

```css
@media (max-width: 767px) {
  input, textarea, select { font-size: 16px; }
}
```

`<meta name="viewport">` updates to:

```html
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover, interactive-widget=resizes-content">
```

`viewport-fit=cover` enables `env(safe-area-inset-*)`.
`interactive-widget=resizes-content` is the modern replacement for
`user-scalable=no` and instructs iOS/Android to resize the layout
viewport (not just visual viewport) when the keyboard appears, so the
composer doesn't get covered.

### Dynamic viewport units

Replace `100vh` with `100dvh` everywhere in `shell.gleam` and column
wrappers, with a `100vh` fallback for older Safari:

```css
height: 100vh;
height: 100dvh;
```

### Safe-area insets

- Phone header — `padding-top: env(safe-area-inset-top)`
- Composer wrapper — `padding-bottom: max(env(safe-area-inset-bottom), 8px)`
- Voice mini-bar — `bottom: calc(env(safe-area-inset-bottom) + 76px)`
- Bottom-sheet content — `padding-bottom: env(safe-area-inset-bottom)`

### Overscroll containment

`overscroll-behavior: contain` on the chat-messages scroll container
and each drawer body. Stops page-bounce from leaking up to the
document level (which would flash a rubber-band on the address bar).

### Touch drag-drop for room reorder

The existing HTML5 drag events (`dragstart`/`dragover`/`drop`) don't
fire reliably on touch. We add a parallel `pointer*` path in
`rooms.gleam`:

1. `pointerdown` on a room row: if `event.pointerType === "touch"`,
   start a 400ms hold timer; movement before the timer fires aborts.
2. Timer fires: enter drag-mode. Dispatch `DragRoomStart(name)` (same
   Msg the desktop dragstart fires today).
3. `pointermove` while in drag-mode: hit-test against `.room-row`
   elements (via `document.elementFromPoint`); dispatch
   `DragRoomOver(target_name)` when it changes.
4. `pointerup` while in drag-mode: dispatch `DropRoomOn(target_name)`,
   then `DragRoomEnd`.
5. `pointercancel` or scroll-while-holding: cancel the timer / exit
   drag-mode without dispatching.

The desktop drag handlers stay unchanged. Mouse pointers don't go
through the long-press path (the timer guard checks `pointerType`).
A small `touch_drag.gleam` module (with paired `touch_drag.ffi.mjs`)
exposes `attach(callbacks)` that wires the timer + hit-test against
DOM elements carrying `data-room-row` markers. `rooms.view` calls
`attach` once via `effect.from` after the rail mounts; the JS side
holds the timer state and dispatches the existing reorder Msgs back
through Lustre.

### Hover affordances on phone

Global stylesheet `@media (hover: none)` block:

```css
@media (hover: none) {
  .msg-row .msg-actions,
  .room-row .room-delete {
    opacity: 1;
    pointer-events: auto;
  }
}
```

### Theme toggle relocation

On phone the desktop's fixed bottom-right pill collides with the voice
mini-bar. The theme toggle moves into the rooms-drawer footer on
phone. Desktop is unchanged.

## Component change list

| File | Change |
|---|---|
| `domain.gleam` | Add `Viewport`, `Drawer`, `Sheet` types |
| `sunset_web.gleam` | Add `viewport`/`drawer`/`sheet` to `Model`; merge `detail_msg_id` + `voice_popover` into `sheet`; new `ViewportChanged`/`OpenDrawer`/`CloseDrawer` `Msg`s; `init` reads viewport, subscribes to changes; `view` passes `viewport` to `shell.view` |
| `storage.gleam` | Add `is_phone_viewport()` + `on_viewport_change(cb)` FFI |
| `storage.ffi.mjs` | Add `isPhoneViewport` + `onViewportChange` JS helpers |
| `views/shell.gleam` | Branch on `viewport`: `desktop_view` (current grid, untouched) and `phone_view` (header + chat + drawers + sheet + mini-bar); add `100dvh`, `@media (hover: none)` rules to global stylesheet |
| `views/drawer.gleam` | New — reusable side-drawer primitive |
| `views/bottom_sheet.gleam` | New — reusable bottom-sheet primitive |
| `views/phone_header.gleam` | New — 56px header with toggles + room title |
| `views/voice_minibar.gleam` | New — floating pill rendered in chat view when in call |
| `views/touch_drag.gleam` + `touch_drag.ffi.mjs` | New — long-press + hit-test helper exposing `attach(callbacks)` |
| `views/rooms.gleam` | Hide collapse button on phone; theme toggle slot in footer; call `touch_drag.attach` from a mount effect |
| `views/voice_popover.gleam` | Add `placement: Floating \| InSheet` parameter so the same content renders as desktop popover or full-width sheet |
| `views/channels.gleam` | Room title becomes tappable on phone (dispatches `OpenDrawer(RoomsDrawer)`); hide self-controls bar on phone |
| `views/main_panel.gleam` | Reduce padding on phone; reaction picker uses bottom-sheet primitive on phone |
| `views/landing.gleam` | Branch on viewport: full-screen takeover layout on phone |
| `index.html` | Update `<meta viewport>` content |

## Testing

### Playwright project matrix

`playwright.config.js` gains a second project:

```js
projects: [
  { name: "chromium", use: { ...devices["Desktop Chrome"] } },
  { name: "mobile-chrome", use: { ...devices["Pixel 7"] } },
]
```

The existing 34-test suite runs against both projects. Tests that
currently click an element behind a drawer on phone need light tweaks:
the test helpers gain a `openMembersDrawer(page)` /
`openChannelsDrawer(page)` utility that only opens on phone (no-op on
desktop), and existing tests call it before clicking.

### New phone-only tests in `e2e/mobile.spec.js`

Gated to mobile project via `test.skip(({ project }) => project.name
!== "mobile-chrome")`:

- Drawer opens via header icons; closes on backdrop tap.
- Stacked drawers: open channels → tap room name → rooms drawer
  covers it → tap a room → both close, chat shows new room.
- Members drawer hosts members list; closes on backdrop tap.
- Bottom sheet opens for message details (info button on a delivered
  message); content matches desktop details panel.
- Bottom sheet opens for voice popover (tap an in-call peer in
  channels drawer); volume slider and denoise toggle work as on
  desktop.
- Voice mini-bar appears in chat view while in call; tapping opens
  self voice sheet.
- Touch drag-drop reorder: simulate `pointerdown` → wait 400ms →
  `pointermove` to a different row → `pointerup` → assert order
  changed.
- Composer input has `font-size >= 16px` (no-zoom regression).
- Header is sticky at top during chat scroll.
- `100dvh` shell never causes document overflow with keyboard up
  (use `page.evaluate(() => window.visualViewport.height)` against
  shell height).

### Manual verification (called out as pre-merge gate)

Headless Chromium can't fully reproduce Mobile Safari's
collapsing-url-bar, dynamic island, or keyboard behavior. Before
merging, manually verify on a real iOS device:

- No zoom-in when focusing any input.
- Composer doesn't get hidden by the keyboard.
- Safe-area insets render correctly (no content under home-indicator
  / notch).
- `100dvh` shell doesn't overflow as the url bar collapses on scroll.

## Open questions

None at design time. Implementation may surface ergonomic tweaks (drag
velocity for the long-press timer, sheet `max-height` on small phones,
mini-bar collision with the iOS keyboard accessory bar) — those are
fine to resolve in the plan or during implementation.

## Out-of-scope follow-ups

- Tablet tier (iPad-shaped layouts that show 2 columns).
- Edge-swipe gestures for drawer open/close.
- Push notifications / installable PWA shell. (Adjacent but
  independent — manifests, service workers, etc.)
- Reduce-motion `prefers-reduced-motion` support for drawer
  transitions.
