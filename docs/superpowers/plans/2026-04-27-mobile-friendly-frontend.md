# Mobile-friendly frontend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the existing `web/` Gleam+Lustre frontend usable on phones with feature parity to desktop — drawers for navigation, bottom sheets for inspectors, no iOS auto-zoom, real safe-area handling, touch drag-drop reorder.

**Architecture:** Width-aware model + structural branching. A new `viewport: Viewport` field on `Model` is initialised from `matchMedia("(max-width: 767px)")` and updated via a media-query subscription. `shell.view` branches: desktop renders today's 4-column grid unchanged; phone renders a header + chat + drawers + sheets + floating mini-bar. Drawer and sheet elements are always in the DOM (offscreen via CSS `transform`) so transitions are cheap and there's no remount thrash. Column views (`rooms`, `channels`, `main_panel`, `members`, `details_panel`, `voice_popover`) stay viewport-agnostic — the shell hands them the right wrapper.

**Tech Stack:** Gleam 1.15+ (target `javascript`) · Lustre 5.6 · Playwright (Chromium desktop + Pixel 7 mobile project) · Nix flake-driven build (every dep pinned via flake; no system tooling).

**Spec:** [docs/superpowers/specs/2026-04-27-mobile-friendly-frontend-design.md](../specs/2026-04-27-mobile-friendly-frontend-design.md)

---

## Working notes

- All cargo / gleam / nix commands assume direnv is active in the repo root. If not, prefix with `nix develop --command`.
- Conventional commits aren't enforced; use imperative scoped messages like the recent history (`Add X`, `Fix Y`, `Refactor Z`). No `Co-Authored-By` trailer on this branch.
- The plan is structured so each task ends with a green build + green tests + a commit. If a task spans more code than fits in one commit comfortably, the plan calls that out and splits sub-commits.
- Tests run via `nix run .#web-test`. Single test: `nix run .#web-test -- e2e/foo.spec.js -g "test name"`. Single project: append `--project=mobile-chrome` or `--project=chromium`.
- The Lustre `lustre_dev_tools` build step hard-codes the `<meta name="viewport" content="width=device-width, initial-scale=1">` tag in the generated `index.html`. We can't replace it via `gleam.toml`. The plan handles this by appending a *second* viewport meta from JS at startup (browsers honor the last `<meta name="viewport">` parsed) — see Task 4.

---

## Task 1: Add Playwright mobile project + drawer-aware test helpers

**Files:**
- Modify: `web/playwright.config.js`
- Create: `web/e2e/helpers/viewport.js`

This task lands the test-runner config first so subsequent tasks can write phone-specific tests against a real mobile viewport. It does not introduce any UI behavior changes — the mobile project will run all existing tests as-is, and we expect a handful to fail until later tasks adapt them. Those failures are tracked in Task 23.

- [ ] **Step 1: Add the mobile-chrome project to the Playwright config**

Edit `web/playwright.config.js`. Replace the `projects` array:

```js
projects: [
  { name: "chromium", use: { ...devices["Desktop Chrome"] } },
  { name: "mobile-chrome", use: { ...devices["Pixel 7"] } },
],
```

`Pixel 7` is 412×915 — well under the 768px phone breakpoint, so `matchMedia("(max-width: 767px)").matches` will be `true`.

- [ ] **Step 2: Create the viewport helper module**

Create `web/e2e/helpers/viewport.js`:

```js
// Test helpers for viewport-aware actions. On the mobile-chrome
// project, columns live behind drawers — open them before clicking
// elements inside. On desktop the helpers are no-ops.

export function isMobile(testInfo) {
  return testInfo.project.name === "mobile-chrome";
}

export async function openChannelsDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  await page.getByTestId("phone-rooms-toggle").click();
  // Wait for the drawer to finish its 220ms transition.
  await page.waitForTimeout(260);
}

export async function openRoomsDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  await openChannelsDrawer(page, testInfo);
  // Tap the room title inside the channels drawer to swap to rooms.
  await page.getByTestId("channels-room-title").click();
  await page.waitForTimeout(260);
}

export async function openMembersDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  await page.getByTestId("phone-members-toggle").click();
  await page.waitForTimeout(260);
}

export async function closeDrawer(page, testInfo) {
  if (!isMobile(testInfo)) return;
  await page.getByTestId("drawer-backdrop").click();
  await page.waitForTimeout(260);
}
```

The `data-testid` values referenced here (`phone-rooms-toggle`, `phone-members-toggle`, `channels-room-title`, `drawer-backdrop`) are introduced in later tasks. The helpers no-op on desktop, so writing them now lets later tasks use them without churn.

- [ ] **Step 3: Verify config compiles + desktop suite still passes**

Run: `nix run .#web-test -- --project=chromium`
Expected: PASS — all 34 tests on the chromium project, exactly as before.

- [ ] **Step 4: Run mobile project; capture which tests fail**

Run: `nix run .#web-test -- --project=mobile-chrome 2>&1 | tee /tmp/mobile-baseline.log`
Expected: Many failures (since no UI is mobile-aware yet). This run is just to confirm the mobile project boots and run the binary; we don't fix the failures here.

- [ ] **Step 5: Commit**

```bash
git add web/playwright.config.js web/e2e/helpers/viewport.js
git commit -m "Add Playwright mobile-chrome project + drawer-aware test helpers"
```

---

## Task 2: Add Viewport / Drawer / Sheet domain types

**Files:**
- Modify: `web/src/sunset_web/domain.gleam`

Pure type additions. No behavior change yet; the build must stay green.

- [ ] **Step 1: Add the three new types**

Append to `web/src/sunset_web/domain.gleam` (after the existing types):

```gleam
/// Viewport class derived from `matchMedia("(max-width: 767px)")`.
/// Updated on init and on resize. Phone gates the entire mobile
/// layout branch in `shell.view`.
pub type Viewport {
  Phone
  Desktop
}

/// Drawer that's currently open on phone. Desktop ignores this field
/// because drawers don't render on desktop. Channels↔rooms is modeled
/// as a swap (replacing the field's value), not a stack.
pub type Drawer {
  RoomsDrawer
  ChannelsDrawer
  MembersDrawer
}

/// Bottom sheet currently open on phone. Replaces two separate
/// optional fields (detail message id, voice popover member name) so
/// the model can't end up with both the details panel AND the voice
/// popover up at the same time.
pub type Sheet {
  DetailsSheet(message_id: String)
  VoiceSheet(member_name: String)
}
```

- [ ] **Step 2: Verify build passes**

Run: `cd web && gleam build`
Expected: `Compiled in <time>` — no errors. Existing warnings are fine.

- [ ] **Step 3: Run the full test suite to confirm nothing regressed**

Run: `nix run .#web-test -- --project=chromium`
Expected: 34 tests pass.

- [ ] **Step 4: Commit**

```bash
git add web/src/sunset_web/domain.gleam
git commit -m "Add Viewport, Drawer, Sheet domain types"
```

---

## Task 3: Add viewport-detection FFI

**Files:**
- Modify: `web/src/sunset_web/storage.ffi.mjs`
- Modify: `web/src/sunset_web/storage.gleam`

Add the matchMedia bindings. No call sites yet — we wire them up in Task 5.

- [ ] **Step 1: Add the JS helpers**

Append to `web/src/sunset_web/storage.ffi.mjs`:

```js
// Phone vs desktop is gated on a single CSS-media-query equivalent.
// Returns a fresh boolean each call so the caller doesn't need to
// hold a reference to the MediaQueryList.
export function isPhoneViewport() {
  try {
    return (
      typeof window.matchMedia === "function" &&
      window.matchMedia("(max-width: 767px)").matches
    );
  } catch {
    return false;
  }
}

// Subscribes `callback(isPhone: bool)` to viewport changes via
// MediaQueryList.addEventListener. Fires once for each crossing of
// the 768px boundary; not on every resize.
export function onViewportChange(callback) {
  try {
    if (typeof window.matchMedia !== "function") return;
    const mql = window.matchMedia("(max-width: 767px)");
    const handler = (e) => callback(e.matches);
    // addEventListener is the modern API; older Safari needs addListener.
    if (typeof mql.addEventListener === "function") {
      mql.addEventListener("change", handler);
    } else if (typeof mql.addListener === "function") {
      mql.addListener(handler);
    }
  } catch {
    // best-effort: viewport tracking is non-critical.
  }
}
```

- [ ] **Step 2: Add the Gleam bindings**

Append to `web/src/sunset_web/storage.gleam`:

```gleam
/// True when the current viewport width is <= 767px (phone tier).
@external(javascript, "./storage.ffi.mjs", "isPhoneViewport")
pub fn is_phone_viewport() -> Bool

/// Register a callback that fires whenever the viewport crosses the
/// 768px boundary (in either direction). Fires once per crossing, not
/// on every resize.
@external(javascript, "./storage.ffi.mjs", "onViewportChange")
pub fn on_viewport_change(callback: fn(Bool) -> Nil) -> Nil
```

- [ ] **Step 3: Verify build passes**

Run: `cd web && gleam build`
Expected: clean compile.

- [ ] **Step 4: Commit**

```bash
git add web/src/sunset_web/storage.ffi.mjs web/src/sunset_web/storage.gleam
git commit -m "Add viewport-detection FFI (matchMedia, onViewportChange)"
```

---

## Task 4: Inject mobile-friendly viewport meta at runtime

**Files:**
- Modify: `web/src/sunset_web/storage.ffi.mjs`
- Modify: `web/src/sunset_web/storage.gleam`
- Modify: `web/src/sunset_web.gleam`

Lustre's html generator hard-codes `<meta name="viewport" content="width=device-width, initial-scale=1">`. We append a richer one at startup so the keyboard resizes the layout viewport (`interactive-widget=resizes-content`) and `env(safe-area-inset-*)` works (`viewport-fit=cover`). Browsers use the last `<meta name="viewport">` parsed, so appending after page load takes effect.

- [ ] **Step 1: Add the JS helper**

Append to `web/src/sunset_web/storage.ffi.mjs`:

```js
// Override the default viewport meta tag with one that:
//   * cover: enables env(safe-area-inset-*) under iOS notch / dynamic island.
//   * interactive-widget=resizes-content: tells iOS/Android to resize the
//     layout viewport (not just the visual viewport) when the keyboard
//     opens, so position:fixed footers/composers don't get covered.
export function installMobileViewportMeta() {
  try {
    const existing = document.querySelectorAll('meta[name="viewport"]');
    existing.forEach((el) => el.remove());
    const meta = document.createElement("meta");
    meta.setAttribute("name", "viewport");
    meta.setAttribute(
      "content",
      "width=device-width, initial-scale=1, viewport-fit=cover, interactive-widget=resizes-content",
    );
    document.head.appendChild(meta);
  } catch {
    // ignored: best-effort.
  }
}
```

- [ ] **Step 2: Bind it from Gleam**

Append to `web/src/sunset_web/storage.gleam`:

```gleam
/// Replace the default `<meta name="viewport">` with a mobile-friendly
/// one that enables safe-area insets and keyboard-aware resizing.
@external(javascript, "./storage.ffi.mjs", "installMobileViewportMeta")
pub fn install_mobile_viewport_meta() -> Nil
```

- [ ] **Step 3: Call it from `main`**

In `web/src/sunset_web.gleam`, find the `main` function and add the call before `lustre.start`:

```gleam
pub fn main() {
  storage.install_mobile_viewport_meta()
  let app = lustre.application(init, update, view)
  let assert Ok(_) = lustre.start(app, "#app", Nil)
  Nil
}
```

- [ ] **Step 4: Verify build + manually verify the meta tag is overridden**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium -g "page title"`
Expected: PASS (we're verifying the entry-point still boots cleanly).

- [ ] **Step 5: Add an e2e regression test**

Append to `web/e2e/shell.spec.js` (after the favicon test):

```js
test("viewport meta is mobile-friendly (safe-area + keyboard resize)", async ({
  page,
}) => {
  const content = await page.evaluate(
    () => document.querySelector('meta[name="viewport"]').getAttribute("content"),
  );
  expect(content).toContain("viewport-fit=cover");
  expect(content).toContain("interactive-widget=resizes-content");
});
```

Run: `nix run .#web-test -- --project=chromium -g "viewport meta is mobile-friendly"`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add web/src/sunset_web/storage.ffi.mjs web/src/sunset_web/storage.gleam web/src/sunset_web.gleam web/e2e/shell.spec.js
git commit -m "Override viewport meta at runtime for safe-area + keyboard-aware layout"
```

---

## Task 5: Wire viewport state through Model + Msg + init

**Files:**
- Modify: `web/src/sunset_web.gleam`

Adds `viewport: Viewport`, `drawer: Option(Drawer)`, `sheet: Option(Sheet)` to `Model`; new `Msg`s for viewport / drawer transitions; init reads viewport via FFI and subscribes for changes. Existing `detail_msg_id` and `voice_popover` fields stay for now (Task 6 migrates them into `sheet`).

- [ ] **Step 1: Extend `Model`**

In `web/src/sunset_web.gleam`, add to the `Model` record:

```gleam
    voice_settings: Dict(String, domain.VoiceSettings),
    /// Viewport class — drives the desktop/phone branch in `shell.view`.
    viewport: domain.Viewport,
    /// Currently open drawer on phone. Ignored on desktop. Channels and
    /// rooms drawers cross-transition (one swaps for the other) rather
    /// than stack.
    drawer: Option(domain.Drawer),
    /// Currently open bottom sheet on phone. Also drives the desktop
    /// right-rail (DetailsSheet → details_panel) and floating voice
    /// popover (VoiceSheet → voice_popover.view).
    sheet: Option(domain.Sheet),
  )
}
```

- [ ] **Step 2: Extend `Msg`**

Add to the `Msg` type:

```gleam
  ResetMemberVoice(String)
  ViewportChanged(domain.Viewport)
  OpenDrawer(domain.Drawer)
  CloseDrawer
}
```

- [ ] **Step 3: Wire init to read + subscribe**

In `init`, replace the model construction and effect batch:

```gleam
  let initial_viewport = case storage.is_phone_viewport() {
    True -> domain.Phone
    False -> domain.Desktop
  }

  let model =
    Model(
      mode: initial_mode,
      view: initial_view,
      joined_rooms: joined,
      rooms_collapsed: False,
      landing_input: "",
      sidebar_search: "",
      current_channel: ChannelId(fixture.initial_channel_id),
      draft: "",
      reacting_to: None,
      detail_msg_id: None,
      reactions: seed_reactions(),
      dragging_room: None,
      drag_over_room: None,
      voice_popover: None,
      voice_settings: seed_voice_settings(),
      viewport: initial_viewport,
      drawer: None,
      sheet: None,
    )

  let subscribe_hash =
    effect.from(fn(dispatch) {
      storage.on_hash_change(fn(hash) { dispatch(HashChanged(hash)) })
    })

  let subscribe_viewport =
    effect.from(fn(dispatch) {
      storage.on_viewport_change(fn(is_phone) {
        let v = case is_phone {
          True -> domain.Phone
          False -> domain.Desktop
        }
        dispatch(ViewportChanged(v))
      })
    })
```

Then add `subscribe_viewport` to the `effect.batch([...])` at the bottom of `init`.

- [ ] **Step 4: Handle the new Msgs**

In `update`, add three new branches at the bottom of the `case msg {` block (before the closing brace):

```gleam
    ViewportChanged(v) -> {
      // Crossing the boundary in either direction closes any open drawer
      // or sheet so we don't leave phone-only chrome stuck on a desktop
      // viewport (or vice versa).
      #(
        Model(..model, viewport: v, drawer: None),
        effect.none(),
      )
    }
    OpenDrawer(d) -> #(Model(..model, drawer: Some(d)), effect.none())
    CloseDrawer -> #(Model(..model, drawer: None), effect.none())
```

- [ ] **Step 5: Verify build + tests**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium`
Expected: 34 + 1 = 35 tests pass (the new viewport-meta test from Task 4 is included).

- [ ] **Step 6: Commit**

```bash
git add web/src/sunset_web.gleam
git commit -m "Add viewport / drawer state + ViewportChanged subscription"
```

---

## Task 6: Migrate detail_msg_id + voice_popover into Sheet ADT

**Files:**
- Modify: `web/src/sunset_web.gleam`

Pure refactor: collapse `detail_msg_id: Option(String)` and `voice_popover: Option(String)` into a single `sheet: Option(Sheet)` field (already added in Task 5). On desktop this is a small behavior change — opening one closes the other — which matches what we agreed in the spec.

- [ ] **Step 1: Drop the two old fields from `Model`**

Remove these two lines from the `Model` record:

```gleam
    detail_msg_id: Option(String),
    voice_popover: Option(String),
```

Update the `Model(...)` construction in `init` to drop the same two fields.

- [ ] **Step 2: Re-point the existing handlers to read/write `sheet`**

Replace the four `update` branches (`OpenDetail`, `CloseDetail`, `OpenVoicePopover`, `CloseVoicePopover`) with:

```gleam
    OpenDetail(id) -> #(
      Model(..model, sheet: Some(domain.DetailsSheet(message_id: id)), reacting_to: None),
      effect.none(),
    )
    CloseDetail -> #(
      // Only close if the active sheet is the details one — guards against
      // a Voice sheet being opened concurrently and accidentally dismissed.
      Model(..model, sheet: case model.sheet {
        Some(domain.DetailsSheet(_)) -> None
        other -> other
      }),
      effect.none(),
    )
    OpenVoicePopover(name) -> #(
      Model(..model, sheet: Some(domain.VoiceSheet(member_name: name))),
      effect.none(),
    )
    CloseVoicePopover -> #(
      Model(..model, sheet: case model.sheet {
        Some(domain.VoiceSheet(_)) -> None
        other -> other
      }),
      effect.none(),
    )
```

- [ ] **Step 3: Replace read sites in `view`**

In `room_view`, the `detail_msg` derivation currently reads `model.detail_msg_id`. Replace:

```gleam
  let detail_msg = case model.detail_msg_id {
    None -> None
    Some(id) -> find_message(messages_with_live_reactions, id)
  }
```

with:

```gleam
  let detail_msg = case model.sheet {
    Some(domain.DetailsSheet(message_id: id)) ->
      find_message(messages_with_live_reactions, id)
    _ -> None
  }
```

Then in the `voice_popover_overlay(palette, model)` helper, replace `model.voice_popover` reads with the equivalent extraction from `model.sheet`:

```gleam
fn voice_popover_overlay(palette, model: Model) -> Element(Msg) {
  case model.sheet {
    Some(domain.VoiceSheet(member_name: name)) ->
      case list.find(fixture.members(), fn(m) { m.name == name }) {
        Error(_) -> element.fragment([])
        Ok(m) ->
          voice_popover.view(
            palette: palette,
            member: m,
            settings: member_voice_settings(model.voice_settings, name),
            on_close: CloseVoicePopover,
            on_set_volume: fn(v) { SetMemberVolume(name, v) },
            on_toggle_denoise: ToggleMemberDenoise(name),
            on_toggle_deafen: ToggleMemberDeafen(name),
            on_reset: ResetMemberVoice(name),
          )
      }
    _ -> element.fragment([])
  }
}
```

Pass through to `main_panel.view`: it currently takes `detail_msg_id: Option(String)` to compute `is_active` for the "is a different message's panel open" affordance. Replace its argument:

```gleam
    main_panel.view(
      palette: palette,
      ...
      detail_msg_id: case model.sheet {
        Some(domain.DetailsSheet(message_id: id)) -> Some(id)
        _ -> None
      },
      ...
    ),
```

- [ ] **Step 4: Verify build + tests**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium`
Expected: 35 tests pass — message-details and voice-popover behaviors are unchanged.

- [ ] **Step 5: Commit**

```bash
git add web/src/sunset_web.gleam
git commit -m "Merge detail_msg_id + voice_popover into single Sheet ADT"
```

---

## Task 7: Build the drawer primitive

**Files:**
- Create: `web/src/sunset_web/views/drawer.gleam`

Reusable left/right drawer with backdrop. Always rendered in the DOM; `transform: translateX(±100%)` when closed so CSS handles the slide. Used in later tasks for rooms / channels / members drawers.

- [ ] **Step 1: Create the module**

Create `web/src/sunset_web/views/drawer.gleam`:

```gleam
//// Reusable side-drawer primitive. Always rendered in the DOM —
//// closed state translates the wrapper offscreen so CSS transitions
//// handle the slide. The host owns drawer state; this module only
//// renders a wrapper + backdrop.

import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub type Side {
  Left
  Right
}

pub fn view(
  palette p: Palette,
  open open: Bool,
  side side: Side,
  on_close on_close: msg,
  test_id test_id: String,
  content content: Element(msg),
) -> Element(msg) {
  let translate_closed = case side {
    Left -> "translateX(-100%)"
    Right -> "translateX(100%)"
  }
  let transform = case open {
    True -> "translateX(0)"
    False -> translate_closed
  }
  let edge_anchor = case side {
    Left -> #("left", "0")
    Right -> #("right", "0")
  }

  html.div([], [
    backdrop(p, open, on_close),
    html.aside(
      [
        attribute.attribute("data-testid", test_id),
        attribute.attribute("aria-hidden", case open {
          True -> "false"
          False -> "true"
        }),
        ui.css([
          #("position", "fixed"),
          #("top", "0"),
          edge_anchor,
          #("height", "100dvh"),
          #("width", "84vw"),
          #("max-width", "320px"),
          #("background", p.surface),
          #("color", p.text),
          #("border-right", case side {
            Left -> "1px solid " <> p.border
            Right -> "0"
          }),
          #("border-left", case side {
            Right -> "1px solid " <> p.border
            Left -> "0"
          }),
          #("box-shadow", p.shadow_lg),
          #("z-index", "30"),
          #("transform", transform),
          #("transition", "transform 220ms ease"),
          #("display", "flex"),
          #("flex-direction", "column"),
          #("overflow", "hidden"),
          #("overscroll-behavior", "contain"),
        ]),
      ],
      [content],
    ),
  ])
}

fn backdrop(p: Palette, open: Bool, on_close: msg) -> Element(msg) {
  let _ = p
  html.div(
    [
      attribute.attribute("data-testid", "drawer-backdrop"),
      event.on_click(on_close),
      ui.css([
        #("position", "fixed"),
        #("inset", "0"),
        #("background", "rgba(0, 0, 0, 0.4)"),
        #("opacity", case open {
          True -> "1"
          False -> "0"
        }),
        #("pointer-events", case open {
          True -> "auto"
          False -> "none"
        }),
        #("transition", "opacity 220ms ease"),
        #("z-index", "29"),
      ]),
    ],
    [],
  )
}
```

- [ ] **Step 2: Verify build**

Run: `cd web && gleam build`
Expected: clean (the new module is unused; that's fine until Task 11).

- [ ] **Step 3: Commit**

```bash
git add web/src/sunset_web/views/drawer.gleam
git commit -m "Add drawer.gleam — reusable side-drawer primitive"
```

---

## Task 8: Build the bottom-sheet primitive

**Files:**
- Create: `web/src/sunset_web/views/bottom_sheet.gleam`

- [ ] **Step 1: Create the module**

Create `web/src/sunset_web/views/bottom_sheet.gleam`:

```gleam
//// Reusable bottom-sheet primitive. Slides up from the bottom edge
//// when open; offscreen via translateY(100%) when closed. Tap-backdrop
//// dismisses. Used for message-details and voice-popover sheets on
//// phone, and for the reaction picker.

import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  open open: Bool,
  on_close on_close: msg,
  test_id test_id: String,
  content content: Element(msg),
) -> Element(msg) {
  let transform = case open {
    True -> "translateY(0)"
    False -> "translateY(100%)"
  }
  html.div([], [
    backdrop(open, on_close),
    html.div(
      [
        attribute.attribute("data-testid", test_id),
        attribute.attribute("role", "dialog"),
        attribute.attribute("aria-hidden", case open {
          True -> "false"
          False -> "true"
        }),
        ui.css([
          #("position", "fixed"),
          #("left", "0"),
          #("right", "0"),
          #("bottom", "0"),
          #("max-height", "75dvh"),
          #("background", p.surface),
          #("color", p.text),
          #("border-top", "1px solid " <> p.border),
          #("border-radius", "16px 16px 0 0"),
          #("box-shadow", p.shadow_lg),
          #("z-index", "40"),
          #("transform", transform),
          #("transition", "transform 220ms ease"),
          #("display", "flex"),
          #("flex-direction", "column"),
          #("overflow", "hidden"),
          #("padding-bottom", "env(safe-area-inset-bottom)"),
          #("overscroll-behavior", "contain"),
        ]),
      ],
      [drag_handle(p), content],
    ),
  ])
}

fn drag_handle(p: Palette) -> Element(msg) {
  // Cosmetic only — there's no swipe-down gesture in v1.
  html.div(
    [
      ui.css([
        #("display", "flex"),
        #("justify-content", "center"),
        #("padding", "8px 0 4px 0"),
      ]),
    ],
    [
      html.span(
        [
          ui.css([
            #("display", "inline-block"),
            #("width", "36px"),
            #("height", "4px"),
            #("border-radius", "999px"),
            #("background", p.border),
          ]),
        ],
        [],
      ),
    ],
  )
}

fn backdrop(open: Bool, on_close: msg) -> Element(msg) {
  html.div(
    [
      attribute.attribute("data-testid", "sheet-backdrop"),
      event.on_click(on_close),
      ui.css([
        #("position", "fixed"),
        #("inset", "0"),
        #("background", "rgba(0, 0, 0, 0.4)"),
        #("opacity", case open {
          True -> "1"
          False -> "0"
        }),
        #("pointer-events", case open {
          True -> "auto"
          False -> "none"
        }),
        #("transition", "opacity 220ms ease"),
        #("z-index", "39"),
      ]),
    ],
    [],
  )
}
```

- [ ] **Step 2: Verify build**

Run: `cd web && gleam build`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add web/src/sunset_web/views/bottom_sheet.gleam
git commit -m "Add bottom_sheet.gleam — reusable bottom-sheet primitive"
```

---

## Task 9: Add `placement` parameter to voice_popover.view

**Files:**
- Modify: `web/src/sunset_web/views/voice_popover.gleam`

Same content body, two outer wrappers: `Floating` (current desktop popover at fixed position) and `InSheet` (full width, transparent wrapper — host bottom-sheet provides the chrome).

- [ ] **Step 1: Add a `Placement` type and an extra parameter**

At the top of `web/src/sunset_web/views/voice_popover.gleam`, add to imports if needed and define:

```gleam
pub type Placement {
  Floating
  InSheet
}
```

Update `pub fn view` signature to add `placement placement: Placement` as the second parameter (right after `palette`).

- [ ] **Step 2: Branch the outer wrapper on placement**

Replace the outer `html.div([... ui.css([floating styles ...])])` wrapper in `view` with a `case placement` branch. The CONTENTS (header, waveform_strip, body, footer) stay the same — only the wrapper attributes differ.

```gleam
pub fn view(
  palette p: Palette,
  placement placement: Placement,
  member m: Member,
  settings settings: VoiceSettings,
  on_close on_close: msg,
  on_set_volume on_set_volume: fn(Int) -> msg,
  on_toggle_denoise on_toggle_denoise: msg,
  on_toggle_deafen on_toggle_deafen: msg,
  on_reset on_reset: msg,
) -> Element(msg) {
  let is_self = m.you
  let max_volume = case is_self {
    True -> 100
    False -> 200
  }

  let body_children = [
    header(p, m, settings, on_close),
    waveform_strip(p, m, settings),
    body(p, m, settings, max_volume, on_set_volume, on_toggle_denoise),
    case is_self {
      True -> element.fragment([])
      False -> footer(p, settings, on_toggle_deafen, on_reset)
    },
  ]

  case placement {
    Floating ->
      html.div(
        [
          attribute.attribute("data-testid", "voice-popover"),
          ui.css([
            #("position", "fixed"),
            #("top", "120px"),
            #("left", "540px"),
            #("width", "320px"),
            #("background", p.surface),
            #("color", p.text),
            #("border", "1px solid " <> p.border),
            #("border-radius", "10px"),
            #("box-shadow", p.shadow_lg),
            #("z-index", "20"),
            #("display", "flex"),
            #("flex-direction", "column"),
          ]),
        ],
        body_children,
      )
    InSheet ->
      html.div(
        [
          attribute.attribute("data-testid", "voice-popover"),
          ui.css([
            #("display", "flex"),
            #("flex-direction", "column"),
            #("width", "100%"),
            #("color", p.text),
          ]),
        ],
        body_children,
      )
  }
}
```

- [ ] **Step 3: Update the existing call site**

In `web/src/sunset_web.gleam`, the `voice_popover_overlay` helper calls `voice_popover.view`. Add `placement: voice_popover.Floating` to the call (this is the desktop usage; the sheet usage gets wired in Task 13). Add `import sunset_web/views/voice_popover.{Floating}` if needed, or qualify as `voice_popover.Floating`.

- [ ] **Step 4: Verify build + tests**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium e2e/voice.spec.js`
Expected: 7 voice tests pass (desktop floating popover unchanged).

- [ ] **Step 5: Commit**

```bash
git add web/src/sunset_web/views/voice_popover.gleam web/src/sunset_web.gleam
git commit -m "Add placement parameter to voice_popover (Floating / InSheet)"
```

---

## Task 10: Build the phone header

**Files:**
- Create: `web/src/sunset_web/views/phone_header.gleam`

56px header with hamburger / room title / members icon.

- [ ] **Step 1: Create the module**

Create `web/src/sunset_web/views/phone_header.gleam`:

```gleam
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
```

- [ ] **Step 2: Verify build**

Run: `cd web && gleam build`
Expected: clean (unused; wired in Task 11).

- [ ] **Step 3: Commit**

```bash
git add web/src/sunset_web/views/phone_header.gleam
git commit -m "Add phone_header.gleam — 56px sticky header with drawer toggles"
```

---

## Task 11: Build the voice mini-bar

**Files:**
- Create: `web/src/sunset_web/views/voice_minibar.gleam`

Floating pill rendered in chat view when in call. Tap → opens self voice sheet.

- [ ] **Step 1: Create the module**

Create `web/src/sunset_web/views/voice_minibar.gleam`:

```gleam
//// Floating mini-bar shown in the phone chat view when the user is
//// in a voice call. PiP-style pill showing the active channel + a
//// mic icon. Tapping opens the user's own voice sheet so they can
//// mute / leave from anywhere.

import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/theme.{type Palette}
import sunset_web/ui

pub fn view(
  palette p: Palette,
  channel_name channel_name: String,
  on_open on_open: msg,
) -> Element(msg) {
  html.button(
    [
      attribute.attribute("data-testid", "voice-minibar"),
      attribute.attribute("aria-label", "Voice controls for " <> channel_name),
      event.on_click(on_open),
      ui.css([
        #("position", "fixed"),
        #("right", "12px"),
        #("bottom", "calc(env(safe-area-inset-bottom) + 76px)"),
        #("display", "inline-flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "8px 14px"),
        #("background", p.accent),
        #("color", p.accent_ink),
        #("border", "none"),
        #("border-radius", "999px"),
        #("box-shadow", p.shadow_lg),
        #("font-family", "inherit"),
        #("font-size", "14px"),
        #("font-weight", "600"),
        #("cursor", "pointer"),
        #("z-index", "20"),
      ]),
    ],
    [
      live_dot(p),
      html.span([], [html.text(channel_name)]),
      mic_icon(),
    ],
  )
}

fn live_dot(p: Palette) -> Element(msg) {
  let _ = p
  html.span(
    [
      ui.css([
        #("width", "8px"),
        #("height", "8px"),
        #("border-radius", "999px"),
        #("background", "#ffffff"),
        #("flex-shrink", "0"),
      ]),
    ],
    [],
  )
}

fn mic_icon() -> Element(msg) {
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
          attribute.attribute("x", "6"),
          attribute.attribute("y", "2.5"),
          attribute.attribute("width", "4"),
          attribute.attribute("height", "8"),
          attribute.attribute("rx", "2"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
        ],
        [],
      ),
      element.namespaced(
        "http://www.w3.org/2000/svg",
        "path",
        [
          attribute.attribute("d", "M3.5 8a4.5 4.5 0 009 0M8 12.5V14"),
          attribute.attribute("stroke", "currentColor"),
          attribute.attribute("stroke-width", "1.4"),
          attribute.attribute("stroke-linecap", "round"),
        ],
        [],
      ),
    ],
  )
}
```

- [ ] **Step 2: Verify build**

Run: `cd web && gleam build`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add web/src/sunset_web/views/voice_minibar.gleam
git commit -m "Add voice_minibar.gleam — floating PiP pill for in-call self controls"
```

---

## Task 12: Branch shell.view on viewport (phone scaffold)

**Files:**
- Modify: `web/src/sunset_web/views/shell.gleam`
- Modify: `web/src/sunset_web.gleam`

This task introduces `shell.phone_view` rendering header + chat + drawers + sheet placeholders (drawers wrap the column-content elements the caller passes in). The desktop path is unchanged.

`shell.view` gains: `viewport: Viewport`, `drawer: Option(Drawer)`, `on_close_drawer: msg` (single dispatcher for all three drawer backdrops), and five pre-rendered `Element(msg)` slots — `phone_header_el`, `voice_minibar`, `details_sheet`, `voice_sheet`, `reaction_sheet`. For the desktop path these are all ignored. Putting the msg dispatcher in via `on_close_drawer` avoids leaking the host's `Msg` type into `shell.gleam`.

- [ ] **Step 1: Update shell.gleam — new signature + phone_view + rename old view to desktop_view**

Replace the contents of `web/src/sunset_web/views/shell.gleam` (showing the new exported `view`, the new `phone_view`, and `desktop_view` which is the prior `view` body verbatim):

```gleam
import gleam/option
import lustre/attribute
import lustre/element.{type Element}
import lustre/element/html
import lustre/event
import sunset_web/domain.{type Drawer, type Viewport, ChannelsDrawer, Desktop, MembersDrawer, Phone, RoomsDrawer}
import sunset_web/theme.{type Mode, type Palette, Dark, Light}
import sunset_web/ui
import sunset_web/views/drawer as drawer_module

pub fn view(
  mode: Mode,
  palette: Palette,
  viewport: Viewport,
  rooms_collapsed: Bool,
  detail_open: Bool,
  drawer: option.Option(Drawer),
  toggle_mode: msg,
  on_close_drawer: msg,
  rooms: Element(msg),
  channels: Element(msg),
  main: Element(msg),
  right_rail: Element(msg),
  overlay: Element(msg),
  phone_header_el: Element(msg),
  voice_minibar: Element(msg),
  details_sheet: Element(msg),
  voice_sheet: Element(msg),
  reaction_sheet: Element(msg),
) -> Element(msg) {
  case viewport {
    Desktop ->
      desktop_view(
        mode,
        palette,
        rooms_collapsed,
        detail_open,
        toggle_mode,
        rooms,
        channels,
        main,
        right_rail,
        overlay,
      )
    Phone ->
      phone_view(
        palette,
        drawer,
        on_close_drawer,
        rooms,
        channels,
        main,
        right_rail,
        phone_header_el,
        voice_minibar,
        details_sheet,
        voice_sheet,
        reaction_sheet,
      )
  }
}

fn phone_view(
  palette: Palette,
  drawer: option.Option(Drawer),
  on_close_drawer: msg,
  rooms: Element(msg),
  channels: Element(msg),
  main: Element(msg),
  right_rail: Element(msg),
  phone_header_el: Element(msg),
  voice_minibar: Element(msg),
  details_sheet: Element(msg),
  voice_sheet: Element(msg),
  reaction_sheet: Element(msg),
) -> Element(msg) {
  let rooms_open = drawer == option.Some(RoomsDrawer)
  let channels_open = drawer == option.Some(ChannelsDrawer)
  let members_open = drawer == option.Some(MembersDrawer)

  html.div(
    [
      ui.css([
        #("position", "fixed"),
        #("inset", "0"),
        #("background", palette.bg),
        #("color", palette.text),
        #("font-family", theme.font_sans),
        #("font-size", "16.875px"),
        #("line-height", "1.45"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("height", "100vh"),
        #("height", "100dvh"),
        #("overflow", "hidden"),
      ]),
    ],
    [
      global_reset(),
      phone_header_el,
      html.main(
        [
          ui.css([
            #("flex", "1"),
            #("min-height", "0"),
            #("display", "flex"),
            #("flex-direction", "column"),
            #("overflow", "hidden"),
          ]),
        ],
        [main],
      ),
      voice_minibar,
      drawer_module.view(
        palette: palette,
        open: channels_open,
        side: drawer_module.Left,
        on_close: on_close_drawer,
        test_id: "channels-drawer",
        content: channels,
      ),
      drawer_module.view(
        palette: palette,
        open: rooms_open,
        side: drawer_module.Left,
        on_close: on_close_drawer,
        test_id: "rooms-drawer",
        content: rooms,
      ),
      drawer_module.view(
        palette: palette,
        open: members_open,
        side: drawer_module.Right,
        on_close: on_close_drawer,
        test_id: "members-drawer",
        content: right_rail,
      ),
      details_sheet,
      voice_sheet,
      reaction_sheet,
    ],
  )
}

fn desktop_view(
  // ... existing body of the old `view` function moved verbatim ...
)
```

Move the body of the previous `view` into `desktop_view` with the same parameters as before; nothing changes there.

- [ ] **Step 2: Update sunset_web.gleam call site to pass new args**

In `web/src/sunset_web.gleam`'s `room_view`, add fragments / pre-rendered values for everything new and pass them through `shell.view`. Use `element.fragment([])` for sheet placeholders for now (Tasks 13–17 wire real content):

```gleam
  shell.view(
    model.mode,
    palette,
    model.viewport,
    model.rooms_collapsed,
    detail_msg != None,
    model.drawer,
    ToggleMode,
    CloseDrawer,
    rooms.view(...),
    channels.view(...),
    main_panel.view(...),
    case detail_msg {
      Some(m) -> details_panel.view(palette: palette, message: m, on_close: CloseDetail)
      None -> members.view(palette: palette, members: fixture.members())
    },
    voice_popover_overlay(palette, model),
    phone_header.view(
      palette: palette,
      room: active_room,
      on_open_channels: OpenDrawer(domain.ChannelsDrawer),
      on_open_members: OpenDrawer(domain.MembersDrawer),
    ),
    element.fragment([]),
    element.fragment([]),
    element.fragment([]),
    element.fragment([]),
  )
```

Add `import sunset_web/views/phone_header` to the imports. The four trailing `fragment([])` slots are minibar / details_sheet / voice_sheet / reaction_sheet — wired in Tasks 13–17.

- [ ] **Step 3: Verify build**

Run: `cd web && gleam build`
Expected: clean.

- [ ] **Step 4: Run desktop tests**

Run: `nix run .#web-test -- --project=chromium`
Expected: 35 tests pass — desktop layout unchanged.

- [ ] **Step 5: Add a smoke e2e test for the phone shell**

Append to `web/e2e/shell.spec.js`:

```js
test.describe("phone shell smoke", () => {
  test.skip(({}, testInfo) => testInfo.project.name !== "mobile-chrome");

  test.beforeEach(async ({ page }) => {
    await page.goto("/");
    await page.evaluate(() => { try { localStorage.clear(); } catch {} });
    await page.goto("/#dusk-collective");
    await expect(page.getByTestId("phone-header")).toBeVisible();
  });

  test("phone header is visible on phone viewport", async ({ page }) => {
    await expect(page.getByTestId("phone-rooms-toggle")).toBeVisible();
    await expect(page.getByTestId("phone-members-toggle")).toBeVisible();
    await expect(page.getByText("dusk-collective")).toBeVisible();
  });
});
```

Run: `nix run .#web-test -- --project=mobile-chrome -g "phone header is visible"`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add web/src/sunset_web/views/shell.gleam web/src/sunset_web.gleam web/e2e/shell.spec.js
git commit -m "Branch shell.view on viewport — phone_view scaffold + smoke test"
```

---

## Task 13: Wire the channels drawer + tappable room title

**Files:**
- Modify: `web/src/sunset_web/views/channels.gleam`

Make the room name in the channels-rail header tappable on phone — dispatches `OpenDrawer(RoomsDrawer)`. Add a `viewport` parameter so the channels view knows whether to wrap the title in a button.

- [ ] **Step 1: Add viewport + on_open_rooms parameters**

Update the `channels.view` signature to add (in order):

```gleam
  viewport viewport: domain.Viewport,
  on_open_rooms on_open_rooms: msg,
```

Pass `Phone`/`Desktop` in from `room_view` in `sunset_web.gleam`, and `OpenDrawer(domain.RoomsDrawer)` for `on_open_rooms`.

- [ ] **Step 2: Make room_header tappable on phone**

In `channels.gleam`, replace `room_header(p, r)` with:

```gleam
fn room_header(
  p: Palette,
  r: Room,
  viewport: domain.Viewport,
  on_open_rooms: msg,
) -> Element(msg) {
  let title_el = case viewport {
    domain.Phone ->
      html.button(
        [
          attribute.attribute("data-testid", "channels-room-title"),
          attribute.title("Switch room"),
          attribute.attribute("aria-label", "Switch room"),
          event.on_click(on_open_rooms),
          ui.css([
            #("flex", "1"),
            #("min-width", "0"),
            #("display", "flex"),
            #("align-items", "center"),
            #("gap", "6px"),
            #("padding", "0"),
            #("border", "none"),
            #("background", "transparent"),
            #("color", p.text),
            #("font-family", "inherit"),
            #("font-weight", "600"),
            #("font-size", "18.75px"),
            #("text-align", "left"),
            #("cursor", "pointer"),
          ]),
        ],
        [title_text(r), conn_icon(p, r.status), chevron_right(p)],
      )
    domain.Desktop ->
      html.span(
        [
          ui.css([
            #("font-weight", "600"),
            #("font-size", "18.75px"),
            #("color", p.text),
            #("white-space", "nowrap"),
            #("overflow", "hidden"),
            #("text-overflow", "ellipsis"),
            #("flex", "1"),
            #("min-width", "0"),
          ]),
        ],
        [html.text(r.name)],
      )
  }
  html.div(
    [
      ui.css([
        #("box-sizing", "border-box"),
        #("height", "60px"),
        #("flex-shrink", "0"),
        #("padding", "0 16px"),
        #("border-bottom", "1px solid " <> p.border_soft),
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("min-width", "0"),
      ]),
    ],
    case viewport {
      domain.Phone -> [title_el]
      domain.Desktop -> [title_el, conn_icon(p, r.status)]
    },
  )
}

fn title_text(r: Room) -> Element(msg) {
  html.span(
    [
      ui.css([
        #("white-space", "nowrap"),
        #("overflow", "hidden"),
        #("text-overflow", "ellipsis"),
        #("min-width", "0"),
      ]),
    ],
    [html.text(r.name)],
  )
}

fn chevron_right(p: Palette) -> Element(msg) {
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
        "path",
        [
          attribute.attribute("d", "M6 4l4 4-4 4"),
          attribute.attribute("stroke", p.text_faint),
          attribute.attribute("stroke-width", "1.5"),
          attribute.attribute("stroke-linecap", "round"),
          attribute.attribute("stroke-linejoin", "round"),
        ],
        [],
      ),
    ],
  )
}
```

Add `import sunset_web/domain` if not already imported, and propagate `viewport` + `on_open_rooms` through to the `room_header` call.

- [ ] **Step 3: Hide the self-controls bar on phone**

In `channels.view`, the bottom-of-rail self-controls bar block is rendered conditionally on `active_voice`. Wrap it in a viewport guard:

```gleam
      case viewport, active_voice {
        domain.Desktop, Some(c) -> self_control_bar(p, c.name)
        _, _ -> element.fragment([])
      },
```

- [ ] **Step 4: Verify build + run desktop voice tests**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium e2e/voice.spec.js`
Expected: 7 voice tests pass — desktop self-controls bar still shows.

- [ ] **Step 5: Add a phone-only e2e test**

Append to `web/e2e/shell.spec.js` (inside the `phone shell smoke` describe block):

```js
test("tapping room title in channels drawer opens rooms drawer", async ({
  page,
}) => {
  await page.getByTestId("phone-rooms-toggle").click();
  await expect(page.getByTestId("channels-drawer")).toBeVisible();
  await expect(page.getByTestId("channels-room-title")).toBeVisible();

  await page.getByTestId("channels-room-title").click();
  await expect(page.getByTestId("rooms-drawer")).toBeVisible();
});
```

Run: `nix run .#web-test -- --project=mobile-chrome -g "tapping room title"`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add web/src/sunset_web/views/channels.gleam web/src/sunset_web.gleam web/e2e/shell.spec.js
git commit -m "Make room title tappable on phone; hide self-controls bar on phone"
```

---

## Task 14: Wire the rooms drawer (room selection closes drawer)

**Files:**
- Modify: `web/src/sunset_web.gleam`
- Modify: `web/src/sunset_web/views/rooms.gleam`

When the user taps a room from inside the rooms drawer, we want both `JoinRoom(name)` and `CloseDrawer` to fire. The cleanest path is to update the `JoinRoom` handler in the model: it already handles room joining; we add `drawer: None` to the resulting model. That way every `JoinRoom` (from desktop click, sidebar search Enter, drawer click, etc.) leaves no drawer open.

Also: hide the rooms-rail collapse button on phone and host the theme toggle in the rooms-rail footer on phone.

- [ ] **Step 1: Update JoinRoom in update**

In `web/src/sunset_web.gleam`, the `JoinRoom` branch builds a `new_model`. Add `drawer: None` to the constructor:

```gleam
          let new_model =
            Model(
              ..model,
              joined_rooms: new_rooms,
              view: RoomView(name),
              landing_input: "",
              sidebar_search: "",
              drawer: None,
            )
```

- [ ] **Step 2: Add viewport + theme toggle to rooms.view**

In `web/src/sunset_web/views/rooms.gleam`, add to the `pub fn view` signature:

```gleam
  viewport viewport: domain.Viewport,
  mode mode: theme.Mode,
  on_toggle_mode on_toggle_mode: msg,
```

(`mode` lets the footer render the right icon; `on_toggle_mode` is the dispatcher.)

- [ ] **Step 3: Hide the collapse button on phone**

Wherever the collapse button is rendered (look for the "Collapse rooms" / "Expand rooms" `attribute.title`), wrap it in `case viewport { domain.Phone -> element.fragment([]); domain.Desktop -> existing_button }`.

- [ ] **Step 4: Render theme-toggle slot in the footer on phone**

After the rooms list (or wherever the rail's footer/last-row lives, near the end of `view`), add:

```gleam
      case viewport {
        domain.Phone -> phone_theme_toggle_row(p, mode, on_toggle_mode)
        domain.Desktop -> element.fragment([])
      },
```

Define:

```gleam
fn phone_theme_toggle_row(
  p: Palette,
  mode: theme.Mode,
  on_toggle: msg,
) -> Element(msg) {
  let label = case mode {
    theme.Light -> "Switch to dark mode"
    theme.Dark -> "Switch to light mode"
  }
  html.button(
    [
      attribute.attribute("data-testid", "phone-theme-toggle"),
      attribute.title(label),
      attribute.attribute("aria-label", label),
      event.on_click(on_toggle),
      ui.css([
        #("display", "flex"),
        #("align-items", "center"),
        #("gap", "8px"),
        #("padding", "12px 16px"),
        #("border", "none"),
        #("border-top", "1px solid " <> p.border_soft),
        #("background", "transparent"),
        #("color", p.text),
        #("font-family", "inherit"),
        #("font-size", "16.875px"),
        #("cursor", "pointer"),
        #("text-align", "left"),
      ]),
    ],
    [html.text(label)],
  )
}
```

- [ ] **Step 5: Hide the desktop fixed theme-toggle on phone**

In `web/src/sunset_web/views/shell.gleam`'s `desktop_view`, the existing `theme_toggle(...)` is rendered as a sibling. We want to keep it on desktop only — which is already the case since `desktop_view` is only invoked when `viewport == Desktop`. No additional change needed; the phone path doesn't render `theme_toggle` at all.

- [ ] **Step 6: Update call site in sunset_web.gleam**

The `rooms.view(...)` call in `room_view` gains the new args:

```gleam
    rooms.view(
      palette: palette,
      ...
      viewport: model.viewport,
      mode: model.mode,
      on_toggle_mode: ToggleMode,
    ),
```

- [ ] **Step 7: Verify build + run existing tests**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium e2e/routing.spec.js`
Expected: all routing tests pass — desktop rooms behavior unchanged.

- [ ] **Step 8: Add phone tests**

Append to `web/e2e/shell.spec.js` (inside `phone shell smoke`):

```js
test("rooms drawer closes after selecting a room", async ({ page }) => {
  // Add a second room so we have one to switch to.
  await page.getByTestId("phone-rooms-toggle").click();
  await page.getByTestId("channels-room-title").click();
  // Sidebar search lives inside the rooms drawer.
  const drawer = page.getByTestId("rooms-drawer");
  await drawer.getByTestId("rooms-search").fill("design-crit");
  await drawer.getByTestId("rooms-search").press("Enter");

  await expect(drawer).toHaveCSS("transform", /matrix.*-/);
  // Either the drawer is closed (translateX(-100%)) or no longer visible.
  // The simplest assertion: backdrop is gone.
  await expect(page.getByTestId("drawer-backdrop")).toHaveCSS("opacity", "0");
  // We landed in the new room.
  await expect(page).toHaveURL(/#design-crit$/);
});

test("phone has theme toggle in rooms drawer footer (and not as a fixed pill)", async ({
  page,
}) => {
  await page.getByTestId("phone-rooms-toggle").click();
  await page.getByTestId("channels-room-title").click();
  await expect(page.getByTestId("phone-theme-toggle")).toBeVisible();
  // Desktop fixed toggle isn't rendered on phone.
  expect(await page.getByTestId("theme-toggle").count()).toBe(0);
});
```

Run: `nix run .#web-test -- --project=mobile-chrome -g "phone shell smoke"`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add web/src/sunset_web.gleam web/src/sunset_web/views/rooms.gleam web/e2e/shell.spec.js
git commit -m "Wire rooms drawer: close on JoinRoom; phone theme toggle in footer"
```

---

## Task 15: Wire details + voice bottom sheets

**Files:**
- Modify: `web/src/sunset_web.gleam`
- Modify: `web/src/sunset_web/views/shell.gleam` (no changes, but verify slots receive content)

`details_sheet` and `voice_sheet` were placeholder `element.fragment([])` slots in Task 12. Wire them to real content gated on the `Sheet` ADT and the viewport: on phone, render `details_panel.view` / `voice_popover.view` content inside `bottom_sheet.view`. On desktop the existing right-rail / floating popover render unchanged from the existing `right_rail` and `overlay` slots.

- [ ] **Step 1: Build the sheet content from `room_view`**

In `web/src/sunset_web.gleam`'s `room_view`, before the `shell.view(...)` call, derive:

```gleam
  let details_sheet_el = case model.viewport, model.sheet {
    domain.Phone, Some(domain.DetailsSheet(message_id: id)) ->
      case find_message(messages_with_live_reactions, id) {
        Some(m) ->
          bottom_sheet.view(
            palette: palette,
            open: True,
            on_close: CloseDetail,
            test_id: "details-sheet",
            content: details_panel.view(
              palette: palette,
              message: m,
              on_close: CloseDetail,
            ),
          )
        None -> element.fragment([])
      }
    _, _ -> element.fragment([])
  }

  let voice_sheet_el = case model.viewport, model.sheet {
    domain.Phone, Some(domain.VoiceSheet(member_name: name)) ->
      case list.find(fixture.members(), fn(m) { m.name == name }) {
        Ok(m) ->
          bottom_sheet.view(
            palette: palette,
            open: True,
            on_close: CloseVoicePopover,
            test_id: "voice-sheet",
            content: voice_popover.view(
              palette: palette,
              placement: voice_popover.InSheet,
              member: m,
              settings: member_voice_settings(model.voice_settings, name),
              on_close: CloseVoicePopover,
              on_set_volume: fn(v) { SetMemberVolume(name, v) },
              on_toggle_denoise: ToggleMemberDenoise(name),
              on_toggle_deafen: ToggleMemberDeafen(name),
              on_reset: ResetMemberVoice(name),
            ),
          )
        Error(_) -> element.fragment([])
      }
    _, _ -> element.fragment([])
  }
```

Add `import sunset_web/views/bottom_sheet` to the imports.

- [ ] **Step 2: Suppress the desktop-floating popover on phone**

The existing `voice_popover_overlay` renders a Floating popover for any `VoiceSheet` value. On phone, it should not render (the sheet replaces it). Update:

```gleam
fn voice_popover_overlay(palette, model: Model) -> Element(Msg) {
  case model.viewport, model.sheet {
    domain.Desktop, Some(domain.VoiceSheet(member_name: name)) -> // existing body
    _, _ -> element.fragment([])
  }
}
```

Likewise, the right-rail panel should not render the details panel on phone (it's in a sheet) — but on phone the right-rail is in a drawer anyway, so the details panel there would be hidden behind the closed drawer. Cleaner: on phone, always render `members.view` in the right-rail slot:

```gleam
    case model.viewport, detail_msg {
      domain.Desktop, Some(m) ->
        details_panel.view(palette: palette, message: m, on_close: CloseDetail)
      _, _ -> members.view(palette: palette, members: fixture.members())
    },
```

- [ ] **Step 3: Pass sheets through to shell.view**

Replace the four trailing `element.fragment([])` placeholders in the `shell.view(...)` call with `element.fragment([])` (minibar still pending Task 16), `details_sheet_el`, `voice_sheet_el`, and `element.fragment([])` (reaction picker pending Task 18).

- [ ] **Step 4: Verify build + tests**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium`
Expected: 35+ tests pass — desktop unchanged.

- [ ] **Step 5: Add phone sheet tests**

Append to `web/e2e/voice.spec.js`:

```js
test.describe("phone — voice sheet", () => {
  test.skip(({}, testInfo) => testInfo.project.name !== "mobile-chrome");

  test("tapping an in-call member opens the voice bottom sheet", async ({
    page,
  }) => {
    await page.getByTestId("phone-rooms-toggle").click();
    // The voice channel block lives in the channels drawer.
    const channelsDrawer = page.getByTestId("channels-drawer");
    await channelsDrawer
      .locator('[data-testid="voice-member"][data-voice-name="ravi"]')
      .click();

    const sheet = page.getByTestId("voice-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet.getByText("ravi", { exact: true })).toBeVisible();
    await expect(sheet.getByTestId("voice-popover-volume")).toBeVisible();
  });
});
```

Append to `web/e2e/shell.spec.js`:

```js
test.describe("phone — details sheet", () => {
  test.skip(({}, testInfo) => testInfo.project.name !== "mobile-chrome");

  test.beforeEach(async ({ page }) => {
    await page.goto("/");
    await page.evaluate(() => { try { localStorage.clear(); } catch {} });
    await page.goto("/#dusk-collective");
    await expect(page.getByTestId("phone-header")).toBeVisible();
  });

  test("info button on a delivered message opens the details bottom sheet", async ({
    page,
  }) => {
    const row = page.locator(".msg-row", { hasText: "routing thru ravi" });
    await row.getByRole("button", { name: /Message details/i }).click();
    const sheet = page.getByTestId("details-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet.getByText(/8f3c…a2/)).toBeVisible();
  });
});
```

Run: `nix run .#web-test -- --project=mobile-chrome -g "phone — voice sheet|phone — details sheet"`
Expected: PASS for both.

- [ ] **Step 6: Commit**

```bash
git add web/src/sunset_web.gleam web/e2e/voice.spec.js web/e2e/shell.spec.js
git commit -m "Wire details + voice bottom sheets on phone"
```

---

## Task 16: Wire the voice mini-bar in chat view

**Files:**
- Modify: `web/src/sunset_web.gleam`

Render `voice_minibar.view` only when `viewport == Phone` and the user is in a call (in fixture: any member with `you: True` AND `in_call: True`). Tapping it dispatches `OpenVoicePopover("you")`.

- [ ] **Step 1: Compute call state + render the minibar**

In `room_view`, derive:

```gleam
  let user_in_call =
    list.any(fixture.members(), fn(m) { m.you && m.in_call })

  let active_voice_channel_name =
    list.find(
      fixture.channels(),
      fn(c) { c.kind == domain.Voice && c.in_call > 0 },
    )
    |> result.map(fn(c) { c.name })
    |> result.unwrap("")

  let voice_minibar_el = case model.viewport, user_in_call {
    domain.Phone, True ->
      voice_minibar.view(
        palette: palette,
        channel_name: active_voice_channel_name,
        on_open: OpenVoicePopover("you"),
      )
    _, _ -> element.fragment([])
  }
```

Add `import gleam/result`, `import sunset_web/views/voice_minibar`.

- [ ] **Step 2: Pass through to shell.view**

Replace the `element.fragment([])` placeholder for the minibar slot in the `shell.view` call with `voice_minibar_el`.

- [ ] **Step 3: Verify build + tests**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium`
Expected: existing 35+ tests pass.

- [ ] **Step 4: Add phone-only minibar test**

Append to `web/e2e/voice.spec.js`:

```js
test.describe("phone — voice mini-bar", () => {
  test.skip(({}, testInfo) => testInfo.project.name !== "mobile-chrome");

  test.beforeEach(async ({ page }) => {
    await page.goto("/");
    await page.evaluate(() => { try { localStorage.clear(); } catch {} });
    await page.goto("/#dusk-collective");
    await expect(page.getByTestId("phone-header")).toBeVisible();
  });

  test("mini-bar visible in chat view while in call", async ({ page }) => {
    await expect(page.getByTestId("voice-minibar")).toBeVisible();
    await expect(page.getByTestId("voice-minibar")).toContainText("Lounge");
  });

  test("tapping mini-bar opens self voice sheet", async ({ page }) => {
    await page.getByTestId("voice-minibar").click();
    const sheet = page.getByTestId("voice-sheet");
    await expect(sheet).toBeVisible();
    await expect(sheet.getByText("you", { exact: true })).toBeVisible();
    // Self row hides mute-for-me + reset.
    await expect(sheet.getByTestId("voice-popover-deafen")).toHaveCount(0);
  });
});
```

Run: `nix run .#web-test -- --project=mobile-chrome -g "phone — voice mini-bar"`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/sunset_web.gleam web/e2e/voice.spec.js
git commit -m "Wire voice mini-bar — opens self voice sheet on tap"
```

---

## Task 17: Phone variant for landing.gleam

**Files:**
- Modify: `web/src/sunset_web/views/landing.gleam`
- Modify: `web/src/sunset_web.gleam`

On phone, the landing view drops the centered card framing for full-screen edge-to-edge content with bigger hero text and full-width input/button.

- [ ] **Step 1: Add viewport parameter**

In `web/src/sunset_web/views/landing.gleam`, add `viewport viewport: domain.Viewport` to `pub fn view`. Branch the outer wrapper:

```gleam
pub fn view(
  palette p: Palette,
  mode mode: Mode,
  viewport viewport: domain.Viewport,
  input input: String,
  noop noop: msg,
  on_input on_input: fn(String) -> msg,
  on_join on_join: fn(String) -> msg,
  on_toggle_mode on_toggle_mode: msg,
) -> Element(msg) {
  case viewport {
    domain.Phone -> phone_view(p, mode, input, noop, on_input, on_join, on_toggle_mode)
    domain.Desktop -> desktop_view(p, mode, input, noop, on_input, on_join, on_toggle_mode)
  }
}
```

Move the existing body into `desktop_view`. Define `phone_view` with edge-to-edge layout — full-width input, full-width button stacked below, hero text scaled larger:

```gleam
fn phone_view(
  p: Palette,
  mode: Mode,
  input: String,
  noop: msg,
  on_input: fn(String) -> msg,
  on_join: fn(String) -> msg,
  on_toggle_mode: msg,
) -> Element(msg) {
  let _ = noop
  html.div(
    [
      attribute.attribute("data-testid", "landing-view"),
      ui.css([
        #("position", "fixed"),
        #("inset", "0"),
        #("display", "flex"),
        #("flex-direction", "column"),
        #("justify-content", "center"),
        #("padding", "24px"),
        #("padding-top", "calc(env(safe-area-inset-top) + 24px)"),
        #("padding-bottom", "calc(env(safe-area-inset-bottom) + 24px)"),
        #("background", p.bg),
        #("color", p.text),
        #("font-family", theme.font_sans),
      ]),
    ],
    [
      html.h1(
        [
          ui.css([
            #("font-size", "44px"),
            #("font-weight", "700"),
            #("margin", "0 0 12px 0"),
            #("color", p.text),
          ]),
        ],
        [html.text("sunset.chat")],
      ),
      html.p(
        [
          ui.css([
            #("font-size", "18px"),
            #("color", p.text_muted),
            #("margin", "0 0 32px 0"),
          ]),
        ],
        [html.text("Pick a room name to join.")],
      ),
      html.input([
        attribute.attribute("data-testid", "landing-input"),
        attribute.attribute("type", "text"),
        attribute.value(input),
        attribute.placeholder("room-name"),
        event.on_input(on_input),
        on_enter_with_value(noop, on_join),
        ui.css([
          #("width", "100%"),
          #("box-sizing", "border-box"),
          #("padding", "14px 16px"),
          #("font-size", "18px"),
          #("font-family", "inherit"),
          #("border", "1px solid " <> p.border),
          #("border-radius", "10px"),
          #("background", p.surface),
          #("color", p.text),
          #("margin-bottom", "12px"),
        ]),
      ]),
      html.button(
        [
          attribute.attribute("data-testid", "landing-join"),
          attribute.disabled(input == ""),
          event.on_click(on_join(input)),
          ui.css([
            #("width", "100%"),
            #("padding", "14px"),
            #("font-size", "18px"),
            #("font-weight", "600"),
            #("font-family", "inherit"),
            #("border", "none"),
            #("border-radius", "10px"),
            #("background", p.accent),
            #("color", p.accent_ink),
            #("cursor", case input {
              "" -> "default"
              _ -> "pointer"
            }),
          ]),
        ],
        [html.text("Join")],
      ),
      html.button(
        [
          attribute.attribute("data-testid", "theme-toggle"),
          event.on_click(on_toggle_mode),
          ui.css([
            #("position", "fixed"),
            #("top", "calc(env(safe-area-inset-top) + 12px)"),
            #("right", "12px"),
            #("padding", "8px 12px"),
            #("border", "1px solid " <> p.border),
            #("background", p.surface),
            #("color", p.text_muted),
            #("border-radius", "999px"),
            #("font-family", "inherit"),
            #("font-size", "13px"),
            #("cursor", "pointer"),
          ]),
        ],
        [
          html.text(case mode {
            Light -> "🌙"
            Dark -> "☀"
          }),
        ],
      ),
    ],
  )
}
```

`on_enter_with_value` is the existing helper at the bottom of `landing.gleam`; reuse it. `event.on_input` is from `lustre/event`. If `on_enter_with_value` is currently private and only called from `desktop_view`'s body, leave it private — `phone_view` lives in the same module.

- [ ] **Step 2: Pass viewport from sunset_web.gleam**

Update the `landing.view(...)` call in the top-level `view` function:

```gleam
    LandingView ->
      landing.view(
        palette: palette,
        mode: model.mode,
        viewport: model.viewport,
        input: model.landing_input,
        noop: NoOp,
        on_input: UpdateLandingInput,
        on_join: JoinRoom,
        on_toggle_mode: ToggleMode,
      )
```

- [ ] **Step 3: Verify build + tests**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium e2e/routing.spec.js`
Expected: routing tests pass — desktop landing layout unchanged.

- [ ] **Step 4: Add phone landing test**

Append to `web/e2e/routing.spec.js` (inside the existing describe):

```js
test.describe("phone — landing", () => {
  test.skip(({}, testInfo) => testInfo.project.name !== "mobile-chrome");

  test("landing fills the viewport edge-to-edge", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByTestId("landing-view")).toBeVisible();
    const input = page.getByTestId("landing-input");
    await expect(input).toBeVisible();
    const inputBox = await input.boundingBox();
    const viewport = page.viewportSize();
    // Input should be near full-width minus our 24px gutters.
    expect(inputBox.width).toBeGreaterThan(viewport.width - 60);
  });
});
```

Run: `nix run .#web-test -- --project=mobile-chrome -g "phone — landing"`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/sunset_web/views/landing.gleam web/src/sunset_web.gleam web/e2e/routing.spec.js
git commit -m "Add phone variant of landing.gleam — edge-to-edge takeover"
```

---

## Task 18: Global stylesheet — dvh, hover:none, no-zoom, padding tweaks

**Files:**
- Modify: `web/src/sunset_web/views/shell.gleam`
- Modify: `web/src/sunset_web/views/main_panel.gleam`
- Modify: `web/src/sunset_web/views/channels.gleam`
- Modify: `web/src/sunset_web/views/rooms.gleam`

Extend the existing `<style>` block in `shell.gleam`'s `global_reset()` with mobile-specific rules. These cover hover-none affordances, iOS no-zoom, overscroll-behavior. Replace `100vh` with `100dvh` (with a `100vh` fallback) in column wrappers so iOS Safari's collapsing url-bar doesn't push content offscreen. Add safe-area padding to the composer wrapper.

- [ ] **Step 1: Add the rules**

In `web/src/sunset_web/views/shell.gleam`, replace the contents of the `global_reset()` style string:

```gleam
fn global_reset() -> Element(msg) {
  html.style(
    [],
    "html, body { margin: 0; padding: 0; height: 100%; overflow: hidden; }
     #app { height: 100%; }
     *, *::before, *::after { box-sizing: border-box; }
     .msg-row .msg-actions {
       opacity: 0;
       pointer-events: none;
       transition: opacity 120ms ease;
     }
     .msg-row:hover .msg-actions,
     .msg-row.is-active .msg-actions {
       opacity: 1;
       pointer-events: auto;
     }
     .room-row .room-delete {
       opacity: 0;
       pointer-events: none;
       transition: opacity 120ms ease;
     }
     .room-row:hover .room-delete,
     .room-row:focus-within .room-delete {
       opacity: 1;
       pointer-events: auto;
     }
     /* iOS no-zoom: input fonts must be >= 16px on phone. */
     @media (max-width: 767px) {
       input, textarea, select { font-size: 16px; }
     }
     /* Touch devices: hover-only affordances are always visible. */
     @media (hover: none) {
       .msg-row .msg-actions,
       .room-row .room-delete {
         opacity: 1;
         pointer-events: auto;
       }
     }
     /* Stop page-bounce from rubber-banding the address bar. */
     .scroll-area { overscroll-behavior: contain; }",
  )
}
```

- [ ] **Step 2: Add the `scroll-area` class to the chat-messages list**

In `web/src/sunset_web/views/main_panel.gleam`, find the messages-list scroll container (the `<div>` with `overflow-y: auto`) and add `attribute.class("scroll-area")` to it (or extend an existing class list). If no class is present, add `attribute.class("scroll-area")`.

- [ ] **Step 3: Replace `100vh` with `100dvh` (with fallback) in column wrappers**

The `ui.css([...])` helper takes a list of `(prop, value)` tuples; later entries with the same prop override earlier ones. To get a fallback chain, declare `height` twice — first `100vh`, then `100dvh`:

In `web/src/sunset_web/views/channels.gleam`, find the outer `html.aside([... ui.css([...])` for the channels rail and replace:

```gleam
        #("height", "100vh"),
```

with:

```gleam
        #("height", "100vh"),
        #("height", "100dvh"),
```

Apply the same change in `web/src/sunset_web/views/rooms.gleam` (the rooms-rail outer wrapper) and any other column wrapper that uses `100vh`. `desktop_view` in `shell.gleam` also uses `100vh` for `grid-template-rows`; same treatment.

- [ ] **Step 4: Add safe-area padding-bottom to the composer wrapper**

In `web/src/sunset_web/views/main_panel.gleam`, find the composer wrapper (the `<div>` containing the message input + send button). Update its padding to consume the home-indicator inset. Replace whatever `padding`/`padding-bottom` is currently set with:

```gleam
        #("padding", "12px 16px"),
        #("padding-bottom", "max(12px, env(safe-area-inset-bottom))"),
```

(Adjust horizontal/top values to match the existing styling; only the `padding-bottom` introduces the safe-area inset.)

- [ ] **Step 5: Verify build**

Run: `cd web && gleam build`
Expected: clean.

- [ ] **Step 6: Add a regression test for input font-size**

Append to `web/e2e/shell.spec.js`:

```js
test("composer input font-size is at least 16px (iOS no-zoom)", async ({
  page,
}) => {
  // The composer is in main_panel; data-testid lives on the textarea.
  // We assert via computed style on the first input or textarea in the chat view.
  const fontSize = await page.evaluate(() => {
    const el = document.querySelector("main input, main textarea");
    return el ? parseFloat(getComputedStyle(el).fontSize) : 0;
  });
  expect(fontSize).toBeGreaterThanOrEqual(16);
});
```

This test runs on both projects. Desktop typically inherits 16.875px and passes; mobile gets the explicit 16px rule and passes.

Run: `nix run .#web-test -- -g "composer input font-size"`
Expected: PASS on both projects.

- [ ] **Step 7: Commit**

```bash
git add web/src/sunset_web/views/shell.gleam web/src/sunset_web/views/main_panel.gleam web/src/sunset_web/views/channels.gleam web/src/sunset_web/views/rooms.gleam web/e2e/shell.spec.js
git commit -m "Global stylesheet + dvh fallbacks + composer safe-area padding"
```

---

## Task 19: Phone padding tweaks for main_panel

**Files:**
- Modify: `web/src/sunset_web/views/main_panel.gleam`

Tighten chat padding from 16px to 12px on phone. Adds a `viewport` parameter to `main_panel.view`.

- [ ] **Step 1: Thread viewport into main_panel.view**

Add `viewport viewport: domain.Viewport` to the signature.

- [ ] **Step 2: Use viewport in padding**

Find the body container's padding rules. Replace the hardcoded `padding` values with:

```gleam
        #("padding", case viewport {
          domain.Phone -> "12px"
          domain.Desktop -> "16px"
        }),
```

Apply this pattern to (a) the messages-list outer wrapper, (b) the composer wrapper. Don't touch internal message-row padding; those stay.

- [ ] **Step 3: Pass viewport from sunset_web.gleam**

Add `viewport: model.viewport,` to the `main_panel.view(...)` call in `room_view`.

- [ ] **Step 4: Verify build + tests**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium`
Expected: 35+ tests pass.

- [ ] **Step 5: Commit**

```bash
git add web/src/sunset_web/views/main_panel.gleam web/src/sunset_web.gleam
git commit -m "Tighten main_panel padding from 16px to 12px on phone"
```

---

## Task 20: Reaction picker uses bottom sheet on phone

**Files:**
- Modify: `web/src/sunset_web/views/main_panel.gleam`
- Modify: `web/src/sunset_web.gleam`

Today's reaction picker is a `position: absolute` overlay anchored to a hovered message row. On phone there's no hover, and absolute positioning fights the message scroll. Wrap the picker in `bottom_sheet.view` when `viewport == Phone`.

- [ ] **Step 1: Make the picker viewport-aware**

In `main_panel.gleam`, find where `reaction_picker(...)` is rendered. There will be a small overlay `<div>` in `message_row` (or similar). Add `viewport` to the relevant function signatures and branch:

- On desktop: existing absolute-positioned overlay.
- On phone: render only a marker (`element.fragment([])`) inline, and pull the picker out to the shell layer where the bottom sheet lives.

The cleanest way: leave the desktop overlay in place; on phone, expose a `picker_message_id: Option(String)` so the shell layer (which knows about the `bottom_sheet` primitive) renders it.

Alternative shorter path: the reaction picker sheet is rendered from `room_view` in `sunset_web.gleam` based on `model.reacting_to`. That keeps the picker rendering in one place per viewport.

Use the alternative. In `room_view`:

```gleam
  let reaction_sheet_el = case model.viewport, model.reacting_to {
    domain.Phone, Some(id) ->
      bottom_sheet.view(
        palette: palette,
        open: True,
        on_close: ToggleReactionPicker(id),
        test_id: "reaction-sheet",
        content: reaction_grid(palette, id),
      )
    _, _ -> element.fragment([])
  }
```

Define `reaction_grid` locally in `sunset_web.gleam` (or in `main_panel.gleam` and export it). It renders the same emoji buttons the desktop picker shows, each dispatching `AddReaction(id, emoji)`. Inline it for clarity:

```gleam
fn reaction_grid(palette, message_id: String) -> Element(Msg) {
  let emojis = ["👍", "❤", "🔥", "👀", "🌅", "🙏", "🤔", "🚀"]
  html.div(
    [
      attribute.attribute("data-testid", "reaction-picker"),
      ui.css([
        #("display", "grid"),
        #("grid-template-columns", "repeat(4, 1fr)"),
        #("gap", "8px"),
        #("padding", "16px 16px 24px 16px"),
      ]),
    ],
    list.map(emojis, fn(e) {
      html.button(
        [
          attribute.attribute("aria-label", e),
          event.on_click(AddReaction(message_id, e)),
          ui.css([
            #("padding", "12px"),
            #("font-size", "26px"),
            #("border", "1px solid " <> palette.border_soft),
            #("background", palette.surface),
            #("color", palette.text),
            #("border-radius", "10px"),
            #("font-family", "inherit"),
            #("cursor", "pointer"),
          ]),
        ],
        [html.text(e)],
      )
    }),
  )
}
```

Match the emoji list to whatever `main_panel`'s desktop picker uses; copy it verbatim.

- [ ] **Step 2: Suppress the desktop overlay picker on phone**

In `main_panel.gleam`, find the picker render-site. Branch:

```gleam
  case viewport, reacting_to == Some(c.id) {
    domain.Desktop, True -> existing_overlay
    _, _ -> element.fragment([])
  }
```

(`viewport` was already threaded in Task 19; reuse it.)

- [ ] **Step 3: Pass `reaction_sheet_el` through to shell.view**

Replace the trailing `element.fragment([])` placeholder for the reaction-picker slot in the `shell.view` call with `reaction_sheet_el`.

- [ ] **Step 4: Verify build + tests**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium e2e/shell.spec.js -g "react button"`
Expected: PASS — desktop picker still works.

- [ ] **Step 5: Add phone reaction picker test**

Append to `web/e2e/shell.spec.js`:

```js
test.describe("phone — reaction picker", () => {
  test.skip(({}, testInfo) => testInfo.project.name !== "mobile-chrome");

  test.beforeEach(async ({ page }) => {
    await page.goto("/");
    await page.evaluate(() => { try { localStorage.clear(); } catch {} });
    await page.goto("/#dusk-collective");
  });

  test("react button opens the picker as a bottom sheet", async ({ page }) => {
    const row = page.locator(".msg-row", { hasText: "routing thru ravi" });
    await row.getByRole("button", { name: /^React$/ }).click();
    const sheet = page.getByTestId("reaction-sheet");
    await expect(sheet).toBeVisible();
    await sheet.getByRole("button", { name: /🔥/ }).click();
    await expect(sheet).not.toBeVisible();
    await expect(row.getByText("🔥")).toBeVisible();
  });
});
```

Run: `nix run .#web-test -- --project=mobile-chrome -g "phone — reaction picker"`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add web/src/sunset_web/views/main_panel.gleam web/src/sunset_web.gleam web/e2e/shell.spec.js
git commit -m "Render reaction picker as a bottom sheet on phone"
```

---

## Task 21: Touch drag-drop FFI module

**Files:**
- Create: `web/src/sunset_web/views/touch_drag.gleam`
- Create: `web/src/sunset_web/views/touch_drag.ffi.mjs`

Long-press detection + hit-test. Pure FFI; wired into `rooms.gleam` in Task 22.

- [ ] **Step 1: Create the JS helper**

Create `web/src/sunset_web/views/touch_drag.ffi.mjs`:

```js
// Touch-driven drag-and-drop for room rows. HTML5 drag events don't
// fire reliably on touch, so we build a parallel path:
//   pointerdown (touch only) → 400ms hold timer → drag mode.
//   pointermove → hit-test against [data-room-row=name] → over callback.
//   pointerup → drop callback.
//   pointercancel / scroll → cancel timer.
//
// The desktop drag handlers in rooms.gleam are untouched; this only
// fires for pointerType === "touch".

const HOLD_MS = 400;

export function attach(callbacks) {
  const onStart = callbacks.on_start;
  const onOver = callbacks.on_over;
  const onDrop = callbacks.on_drop;
  const onEnd = callbacks.on_end;

  let timer = null;
  let active = null; // string room name once timer fires
  let lastTarget = null;

  function rowNameAt(x, y) {
    const el = document.elementFromPoint(x, y);
    if (!el) return null;
    const row = el.closest("[data-room-row]");
    return row ? row.getAttribute("data-room-row") : null;
  }

  function reset() {
    if (timer) {
      clearTimeout(timer);
      timer = null;
    }
    active = null;
    lastTarget = null;
  }

  function handleDown(e) {
    if (e.pointerType !== "touch") return;
    const startName = rowNameAt(e.clientX, e.clientY);
    if (!startName) return;
    timer = setTimeout(() => {
      timer = null;
      active = startName;
      onStart(active);
    }, HOLD_MS);
  }

  function handleMove(e) {
    if (e.pointerType !== "touch") return;
    if (!active) {
      // Movement before the timer fires aborts the long-press.
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      return;
    }
    const target = rowNameAt(e.clientX, e.clientY);
    if (target && target !== lastTarget) {
      lastTarget = target;
      onOver(target);
    }
  }

  function handleUp(e) {
    if (e.pointerType !== "touch") return;
    if (!active) {
      reset();
      return;
    }
    const target = rowNameAt(e.clientX, e.clientY);
    if (target) onDrop(target);
    onEnd();
    reset();
  }

  function handleCancel(e) {
    if (e.pointerType !== "touch") return;
    if (active) onEnd();
    reset();
  }

  document.addEventListener("pointerdown", handleDown, { passive: true });
  document.addEventListener("pointermove", handleMove, { passive: true });
  document.addEventListener("pointerup", handleUp, { passive: true });
  document.addEventListener("pointercancel", handleCancel, { passive: true });
  // Scroll cancels an in-progress hold (timer hasn't fired).
  window.addEventListener(
    "scroll",
    () => {
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
    },
    { passive: true, capture: true },
  );
}
```

- [ ] **Step 2: Create the Gleam binding**

Create `web/src/sunset_web/views/touch_drag.gleam`:

```gleam
//// Touch-driven drag-drop helper. Wires `pointerdown`/`pointermove`/
//// `pointerup` against rows marked `data-room-row="<name>"`, with a
//// 400ms long-press to enter drag mode. Mouse pointers are ignored
//// (desktop already handles HTML5 drag events).

pub type Callbacks {
  Callbacks(
    on_start: fn(String) -> Nil,
    on_over: fn(String) -> Nil,
    on_drop: fn(String) -> Nil,
    on_end: fn() -> Nil,
  )
}

@external(javascript, "./touch_drag.ffi.mjs", "attach")
pub fn attach(callbacks: Callbacks) -> Nil
```

- [ ] **Step 3: Verify build**

Run: `cd web && gleam build`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add web/src/sunset_web/views/touch_drag.gleam web/src/sunset_web/views/touch_drag.ffi.mjs
git commit -m "Add touch_drag helper — long-press + hit-test for room reorder"
```

---

## Task 22: Wire touch_drag into rooms.gleam

**Files:**
- Modify: `web/src/sunset_web.gleam`
- Modify: `web/src/sunset_web/views/rooms.gleam`

Subscribe to touch_drag events from init's effect batch and dispatch the existing `DragRoomStart` / `DragRoomOver` / `DropRoomOn` / `DragRoomEnd` Msgs. Add the `data-room-row="<name>"` attribute to each rendered room row so the JS hit-test can find them.

- [ ] **Step 1: Add data-room-row attribute to rendered rows**

In `web/src/sunset_web/views/rooms.gleam`, find where each room row's `<div>` is rendered and add:

```gleam
        attribute.attribute("data-room-row", room.name),
```

- [ ] **Step 2: Subscribe in init**

In `web/src/sunset_web.gleam`, add to the `effect.batch([...])` in `init`:

```gleam
  let subscribe_touch_drag =
    effect.from(fn(dispatch) {
      touch_drag.attach(
        touch_drag.Callbacks(
          on_start: fn(name) { dispatch(DragRoomStart(name)) },
          on_over: fn(name) { dispatch(DragRoomOver(name)) },
          on_drop: fn(name) { dispatch(DropRoomOn(name)) },
          on_end: fn() { dispatch(DragRoomEnd) },
        ),
      )
    })
```

Add `subscribe_touch_drag` to the batch list. Add `import sunset_web/views/touch_drag` to imports.

- [ ] **Step 3: Verify build + desktop tests**

Run: `cd web && gleam build`
Expected: clean.

Run: `nix run .#web-test -- --project=chromium e2e/routing.spec.js -g "drag-drop reorders"`
Expected: PASS — desktop HTML5 drag still works (touch path is dormant on mouse pointers).

- [ ] **Step 4: Add phone touch drag-drop test**

Append to `web/e2e/routing.spec.js` (inside the `landing + routing` describe):

```js
test.describe("phone — touch drag-drop", () => {
  test.skip(({}, testInfo) => testInfo.project.name !== "mobile-chrome");

  test("long-press + drag reorders rooms", async ({ page }) => {
    await page.goto("/");
    await page.evaluate(() => { try { localStorage.clear(); } catch {} });
    await page.goto("/#alpha");
    // Add two more rooms via the rooms drawer.
    await page.getByTestId("phone-rooms-toggle").click();
    await page.getByTestId("channels-room-title").click();
    await page.getByTestId("rooms-search").fill("beta");
    await page.getByTestId("rooms-search").press("Enter");
    await page.getByTestId("phone-rooms-toggle").click();
    await page.getByTestId("channels-room-title").click();
    await page.getByTestId("rooms-search").fill("gamma");
    await page.getByTestId("rooms-search").press("Enter");

    await page.getByTestId("phone-rooms-toggle").click();
    await page.getByTestId("channels-room-title").click();

    const drawer = page.getByTestId("rooms-drawer");
    await expect.poll(async () =>
      drawer.locator("[data-room-row]").evaluateAll((rows) =>
        rows.map((r) => r.getAttribute("data-room-row")),
      ),
    ).toEqual(["gamma", "beta", "alpha"]);

    // Simulate touch long-press on alpha then drag onto gamma.
    const alpha = drawer.locator('[data-room-row="alpha"]');
    const gamma = drawer.locator('[data-room-row="gamma"]');
    const aBox = await alpha.boundingBox();
    const gBox = await gamma.boundingBox();

    await page.evaluate(
      ({ ax, ay, gx, gy }) => {
        const fire = (type, x, y) => {
          const ev = new PointerEvent(type, {
            pointerType: "touch",
            clientX: x,
            clientY: y,
            bubbles: true,
            cancelable: true,
          });
          document.dispatchEvent(ev);
        };
        fire("pointerdown", ax, ay);
        // Wait for the 400ms hold timer.
        return new Promise((res) => setTimeout(() => {
          fire("pointermove", gx, gy);
          fire("pointerup", gx, gy);
          res();
        }, 450));
      },
      {
        ax: aBox.x + aBox.width / 2,
        ay: aBox.y + aBox.height / 2,
        gx: gBox.x + gBox.width / 2,
        gy: gBox.y + gBox.height / 2,
      },
    );

    await expect.poll(async () =>
      drawer.locator("[data-room-row]").evaluateAll((rows) =>
        rows.map((r) => r.getAttribute("data-room-row")),
      ),
    ).toEqual(["alpha", "gamma", "beta"]);
  });
});
```

Run: `nix run .#web-test -- --project=mobile-chrome -g "long-press"`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add web/src/sunset_web/views/rooms.gleam web/src/sunset_web.gleam web/e2e/routing.spec.js
git commit -m "Wire touch drag-drop reorder for room rail on phone"
```

---

## Task 23: Adapt existing tests to pass on both viewports

**Files:**
- Modify: `web/e2e/routing.spec.js`
- Modify: `web/e2e/shell.spec.js`
- Modify: `web/e2e/voice.spec.js`

After Tasks 1–22 the desktop suite passes. The mobile project has many failing tests because elements that lived in always-visible columns now live in drawers. This task wraps the affected tests with the helpers from Task 1, and skips a small set of tests that are inherently desktop-only (rooms-rail collapse button; column bottom-border alignment).

For each test:
- If it interacts with the rooms rail → call `await openRoomsDrawer(page, testInfo)` first.
- If it interacts with channels → call `await openChannelsDrawer(page, testInfo)` first.
- If it interacts with members → call `await openMembersDrawer(page, testInfo)` first.
- If it asserts column-specific layout (collapse button, four-column grid, etc.) → wrap with `test.skip(({}, testInfo) => testInfo.project.name === "mobile-chrome")`.

- [ ] **Step 1: Audit shell.spec.js**

Run the suite to see current failures: `nix run .#web-test -- --project=mobile-chrome 2>&1 | tee /tmp/mobile-fail.log`

Expected categories:
- "all four columns render in light mode" — desktop only (skip on mobile).
- "rooms rail collapse button" — desktop only (skip on mobile).
- "collapsed rail hides the logo" — desktop only (skip on mobile).
- "channels and main column bottom borders line up" — desktop only (skip on mobile).
- "column-bottom rows share a top y-coordinate" — desktop only (skip on mobile).
- "channels header has no online subtitle" — needs `openChannelsDrawer` first.
- "rooms list does not render timestamps" — needs `openRoomsDrawer` first.
- "react button" / "info button" / "info on incoming" — should already work (chat view is primary on phone).
- "info button is disabled while sending" — should already work.

Apply `test.skip` or open-helper calls one test at a time. For each pre-existing test that requires a drawer, the modification looks like:

```js
test("rooms list does not render timestamps", async ({ page }, testInfo) => {
  await openRoomsDrawer(page, testInfo);
  const railText = await page
    .getByTestId("rooms-rail")
    .evaluate((el) => el.textContent);
  // ... rest unchanged
});
```

Add the import at the top:

```js
import {
  openChannelsDrawer,
  openMembersDrawer,
  openRoomsDrawer,
} from "./helpers/viewport.js";
```

- [ ] **Step 2: Audit routing.spec.js**

Same approach. Tests that touch the rooms rail (the search field, the per-row delete, drag-drop) need `openRoomsDrawer` first. The drag-drop reorder test reorganizes inside the drawer on mobile — its existing assertions still work since they query inside `getByTestId("rooms-rail")`.

The `selecting a room does not reorder` test on mobile: opening the drawer, clicking a room, then asserting the order in the rail. After click, the drawer auto-closes (Task 14), so re-open it to read the order:

```js
test("selecting a room does not reorder the list", async ({ page }, testInfo) => {
  // ... existing setup ...
  await openRoomsDrawer(page, testInfo);
  const railOrder = async () => {
    if (testInfo.project.name === "mobile-chrome") {
      // The drawer closes when a room is selected; re-open it for the next read.
      await openRoomsDrawer(page, testInfo);
    }
    return page
      .getByTestId("rooms-rail")
      .locator(".room-row")
      .evaluateAll((rows) =>
        rows.map((r) => r.getAttribute("data-room-name") || ""),
      );
  };
  // ... rest ...
});
```

- [ ] **Step 3: Audit voice.spec.js**

The voice tests interact with voice-member rows in the channels rail. Add `openChannelsDrawer(page, testInfo)` before the click. The voice popover renders as a bottom sheet on phone (testid `voice-sheet`); but the *content* still has `data-testid="voice-popover"` (from Task 9, both Floating and InSheet share that testid). The existing assertions on `voice-popover` therefore work on mobile too. Where tests query for `voice-popover-volume` etc., those still work because the inner controls keep their testids.

For the test "non-self volume slider goes up to 200%, self caps at 100%": after closing one popover (via the close button or backdrop), reopen the channels drawer to access the next member row.

- [ ] **Step 4: Run the full suite, both projects**

Run: `nix run .#web-test`
Expected: All tests pass on both projects (some skipped on mobile-chrome where flagged).

- [ ] **Step 5: Commit**

```bash
git add web/e2e/routing.spec.js web/e2e/shell.spec.js web/e2e/voice.spec.js
git commit -m "Adapt e2e tests to pass on both desktop and mobile-chrome projects"
```

---

## Task 24: Manual verification checklist + close-out

**Files:**
- Create: `docs/superpowers/notes/2026-04-27-mobile-friendly-frontend-manual-verification.md`

Headless Chromium can't reproduce iOS Safari's collapsing url-bar, dynamic island, or keyboard behavior. This task documents the pre-merge manual checks.

- [ ] **Step 1: Create the checklist**

Create `docs/superpowers/notes/2026-04-27-mobile-friendly-frontend-manual-verification.md`:

```markdown
# Mobile frontend — manual verification

Run on a real iOS device (iPhone with Safari) before merging the
mobile-friendly branch. Headless Chromium does not reproduce these
quirks.

## Checklist

- [ ] Open https://<staging-url>/#dusk-collective in Mobile Safari.
- [ ] Tap the composer input — the page must NOT zoom in.
- [ ] Type a long draft until the keyboard is clearly up — the
      composer stays visible above the keyboard, not hidden behind it.
- [ ] Open the rooms drawer; tap the search input — no zoom.
- [ ] Scroll the chat view — no rubber-band on the address bar; the
      sticky header stays at the top.
- [ ] Rotate to landscape — header + composer reflow without horizontal
      scrollbar.
- [ ] Notch / dynamic island devices: header content is below the
      notch, not under it.
- [ ] Home-indicator devices: composer + voice mini-bar sit above the
      indicator, not under it.
- [ ] Long-press a room in the rooms drawer; drag onto a different
      row; release — order updates.
- [ ] Tap an in-call peer in the channels drawer — voice sheet opens.
- [ ] Tap the message info button — details sheet opens; backdrop tap
      dismisses.
- [ ] Tap the voice mini-bar — opens the user's own voice sheet.
- [ ] Switch theme via the rooms-drawer footer — persists across
      reload.

If any item fails, file the issue with a video / screenshot and link
back to this branch's PR.
```

- [ ] **Step 2: Commit**

```bash
git add docs/superpowers/notes/2026-04-27-mobile-friendly-frontend-manual-verification.md
git commit -m "Add manual verification checklist for mobile frontend"
```

- [ ] **Step 3: Final whole-suite run**

Run: `nix run .#web-test`
Expected: All tests pass on both projects.

- [ ] **Step 4: Run lint + format**

Run: `cd web && gleam format --check src`
Expected: clean. If not, run `gleam format src` and commit:

```bash
git add web/src
git commit -m "Apply gleam format"
```

---

## Done

After all 24 tasks land, the frontend is mobile-friendly with feature parity. Use `superpowers:finishing-a-development-branch` to merge to master.
