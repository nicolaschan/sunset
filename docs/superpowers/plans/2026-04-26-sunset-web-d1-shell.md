# sunset-web D1 visual shell — Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans (or subagent-driven-development) to execute this plan task-by-task.

**Goal:** Bring up a Gleam + Lustre frontend that pixel-faithfully reproduces the **D1 (Quiet)** direction from the design hand-off bundle (sunset.chat.html), in both **light** and **dark** modes. Static fixture data only — no Rust/WASM wiring yet. Ship it to GitHub Pages via a fully Nix-driven CI pipeline.

**Out of scope (deferred to follow-up plans):**

- WASM build of `sunset-store` / `sunset-sync` and Gleam ↔ Rust FFI
- Chat-domain → KV-entry mapping (rooms / channels / messages as signed entries)
- Voice controls: per-member volume popover, denoise toggle, self-bar mic/headphones/leave, live waveform animation
- Reactions picker, image attachments, message-details panel (crypto chain), routing-detail-on-hover, search input behaviour, message composer submit
- WebRTC / actual networking
- A real signing scheme (sunset-core / identity)

What we **do** build in this plan: the static D1 shell renders cleanly in light + dark, the design tokens are typed and live in one place, and the artefact deploys to Pages on every `master` push via Nix-built CI.

---

## File structure

```
sunset/
├── flake.nix                                # extend: gleam toolchain + web build derivation
├── nix/
│   └── gleam/                               # NEW: borrowed from sunset-old
│       ├── default.nix
│       ├── build-gleam-package.nix
│       ├── fetch-hex-deps.nix
│       └── hooks/
│           ├── default.nix
│           └── gleam-config-hook.sh
├── web/                                     # NEW Gleam project (parallel to crates/)
│   ├── gleam.toml
│   ├── manifest.toml                        # generated, committed
│   ├── package.json                         # (empty deps; npm just provides esbuild path for lustre)
│   ├── package-lock.json
│   └── src/
│       ├── sunset_web.gleam                 # entrypoint: App init / update / view
│       └── sunset_web/
│           ├── theme.gleam                  # Mode, Palette, palette_for / Theme tokens (D1)
│           ├── domain.gleam                 # Room, Channel, Member, Message, ConnStatus, …
│           ├── fixture.gleam                # static SC_DATA equivalent
│           ├── ui.gleam                     # small helpers: px, color util, svg helpers, sanitise style
│           └── views/
│               ├── shell.gleam              # 4-col grid, font/bg, theme toggle wrapper
│               ├── rooms.gleam              # rooms rail (full + collapsed)
│               ├── channels.gleam           # channels rail (text + voice detail + bridge)
│               ├── main_panel.gleam         # channel header, messages, composer, idle/call self-bar
│               └── members.gleam            # members rail
└── .github/
    └── workflows/
        └── pages.yml                        # NEW: Nix-driven build + deploy to Pages
```

Boundaries:

- **`theme.gleam`** owns every colour and font literal. Views read from a `Palette` record passed top-down. No view module hardcodes a colour.
- **`domain.gleam`** mirrors the *current* mock-data shape (Room, Channel, Member, Message), but field names already use our actual repo's vocabulary (`verifying_key_short` instead of `color`, `name : Name`, etc.) so the eventual swap to real `sunset-store` data is a struct-rename rather than a redesign.
- **`fixture.gleam`** is the *only* place that knows specific people, rooms, message bodies. Replaceable atomically when real data arrives.
- **`views/*`** are pure functions of `(Model, Palette)`. State + dispatch lives in `sunset_web.gleam`.
- **Inline styles** (Lustre `attribute.style`) — matches the React reference, dynamic theming is trivial, and skip the build-time SASS layer.

---

## Cross-cutting design notes

### Typography

`Geist` (sans) + `Geist Mono` from Google Fonts. Loaded via the `<link>` tags in the Lustre dev / build HTML (`gleam.toml` `[tools.lustre.html.links]`). `theme.gleam` exposes two strings: `font_sans` and `font_mono`.

### Theme + mode

`Mode = Light | Dark`. `Palette` is a record (matches the JSX object literally — `bg`, `surface`, `surface_alt`, `surface_sunk`, `border`, `border_soft`, `text`, `text_muted`, `text_faint`, `accent`, `accent_soft`, `accent_deep`, `accent_ink`, `ok`, `ok_soft`, `warn`, `warn_soft`, `live`, `shadow`, `shadow_lg`). Light + dark literals come from `quiet.jsx`'s `QUIET_PALETTES.geist`. A theme-toggle button in the brand row flips `Model.mode`.

Initial mode reads `prefers-color-scheme` via a tiny FFI shim (acceptable JS — it's a one-line `window.matchMedia` call). Subsequent toggles override it for the session.

### State surface (this plan)

```
Model {
  mode: Mode,
  current_room: RoomId,
  current_channel: ChannelId,
  rooms_collapsed: Bool,
  draft: String,                 // bound to composer input; submit unwired
}
Msg = SetMode(Mode) | SelectRoom(RoomId) | SelectChannel(ChannelId)
    | ToggleRoomsRail | UpdateDraft(String)
```

Voice / popover / reactions / details are render-only — fixture supplies a "speaking" / "muted" / "in call" state and the views render that, but no `Msg` mutates them. This keeps the plan small and the static rendering complete.

### Build pipeline (hermetic via Nix)

- `flake.nix` adds `pkgs.gleam`, `pkgs.erlang`, `pkgs.rebar3`, `pkgs.nodejs`, `pkgs.bun` to the dev shell.
- A new `packages.web` derivation in the flake calls `gleamLib.buildGleamPackage` with `target = "javascript"` and `lustre = true`, runs `gleam run -m lustre/dev build sunset_web --minify`, and copies `web/dist` to `$out`.
- `apps.web-dev` runs `gleam run -m lustre/dev start` for local hot reload.
- `nix flake check` includes `gleam test` running under the hex-deps cache.

### CI

`.github/workflows/pages.yml` follows the project's hermeticity rule:

```yaml
- uses: actions/checkout@v4
- uses: cachix/install-nix-action@v27
- run: nix build .#web
- uses: actions/upload-pages-artifact@v3
  with: { path: result }
- # deploy job: actions/deploy-pages@v4
```

No `setup-beam` / `setup-node` — the Nix build provides every dep. (sunset-old's workflow used setup-beam directly; we don't.)

---

## Tasks

### Task 1: Bring across the Gleam Nix toolkit

**Files:**
- Create: `nix/gleam/default.nix`
- Create: `nix/gleam/build-gleam-package.nix`
- Create: `nix/gleam/fetch-hex-deps.nix`
- Create: `nix/gleam/hooks/default.nix`
- Create: `nix/gleam/hooks/gleam-config-hook.sh`

- [ ] **Step 1:** Copy the four Nix files + the shell hook from `~/src/sunset-old/nix/gleam/` verbatim. They are self-contained and have no project-specific assumptions.
- [ ] **Step 2:** Stage and commit:
  ```
  git add nix/gleam/
  git commit -m "Add Nix toolkit for hermetic Gleam builds"
  ```

(No verification yet — wired up in Task 3.)

---

### Task 2: Scaffold the `web/` Gleam project

**Files:**
- Create: `web/gleam.toml`
- Create: `web/package.json`
- Create: `web/src/sunset_web.gleam`
- Create: `web/.gitignore`

- [ ] **Step 1:** Write `web/gleam.toml`:

  ```toml
  name = "sunset_web"
  version = "0.1.0"
  target = "javascript"

  [dependencies]
  gleam_stdlib = ">= 0.44.0 and < 2.0.0"
  lustre = ">= 5.6.0 and < 6.0.0"
  modem = ">= 2.1.2 and < 3.0.0"

  [dev-dependencies]
  gleeunit = ">= 1.0.0 and < 2.0.0"
  lustre_dev_tools = ">= 2.3.0 and < 3.0.0"

  [tools.lustre.dev]
  host = "0.0.0.0"

  [tools.lustre.html]
  title = "sunset.chat"

  [[tools.lustre.html.links]]
  rel = "preconnect"
  href = "https://fonts.googleapis.com"

  [[tools.lustre.html.links]]
  rel = "preconnect"
  href = "https://fonts.gstatic.com"
  crossorigin = ""

  [[tools.lustre.html.links]]
  rel = "stylesheet"
  href = "https://fonts.googleapis.com/css2?family=Geist:wght@400;500;600;700&family=Geist+Mono:wght@400;500;600&display=swap"
  ```

- [ ] **Step 2:** Write a minimal `web/package.json` (lustre_dev_tools shells out to esbuild via npm; an empty package.json with the dev dep handles the bundler path):

  ```json
  {
    "name": "sunset-web",
    "version": "0.1.0",
    "private": true,
    "type": "module"
  }
  ```

- [ ] **Step 3:** Write `web/src/sunset_web.gleam` — a placeholder Lustre app that renders a single `<div>` with the text `"sunset.chat"` so the toolchain has something to compile.

  ```gleam
  import lustre
  import lustre/element/html

  pub fn main() {
    let app = lustre.element(html.div([], [html.text("sunset.chat")]))
    let assert Ok(_) = lustre.start(app, "#app", Nil)
    Nil
  }
  ```

- [ ] **Step 4:** Write `web/.gitignore` with `build/`, `dist/`, `node_modules/`.

- [ ] **Step 5:** Commit (manifest.toml is regenerated in Task 3).

  ```
  git add web/
  git commit -m "Scaffold web/ Gleam + Lustre project"
  ```

---

### Task 3: Wire Gleam into `flake.nix` + generate `manifest.toml`

**Files:**
- Modify: `flake.nix`
- Create: `web/manifest.toml`

- [ ] **Step 1:** Extend the dev shell `buildInputs` with `pkgs.gleam`, `pkgs.erlang`, `pkgs.rebar3`, `pkgs.nodejs`, `pkgs.bun`. Wire in `gleamLib = import ./nix/gleam { inherit pkgs; }`.

- [ ] **Step 2:** Run `nix develop --command bash -c 'cd web && gleam deps download'` — this both downloads the Hex packages and writes `web/manifest.toml`. Commit the manifest.

- [ ] **Step 3:** Add `packages.web` to the flake — calls `gleamLib.buildGleamPackage` with `src = ./.`, `manifest = ./web/manifest.toml`, `target = "javascript"`, `lustre = true`, `buildPhase = "cd web && gleam run -m lustre/dev build sunset_web --minify"`, `installPhase = "cp -r web/dist $out"`.

- [ ] **Step 4:** Add `apps.web-dev` — a `writeShellScriptBin` that `cd web && gleam run -m lustre/dev start` under the dev shell's PATH.

- [ ] **Step 5:** Verify:
  ```
  nix develop --command bash -c 'cd web && gleam build'
  nix build .#web --no-link
  ```
  Both should succeed and produce a `dist/` containing minified JS + an `index.html` linked to it.

- [ ] **Step 6:** Commit:
  ```
  git add flake.nix flake.lock web/manifest.toml
  git commit -m "Wire Gleam toolchain into flake + buildable web target"
  ```

---

### Task 4: `theme.gleam` — palette + mode

**Files:**
- Create: `web/src/sunset_web/theme.gleam`

- [ ] **Step 1:** Define `Mode { Light Dark }`, a `Palette` record with every field from `QUIET_PALETTES.geist.{light,dark}`, and `palette_for(Mode) -> Palette`. Light + dark colour literals straight from `quiet.jsx` lines 9–25. Plus `font_sans` and `font_mono` strings.

- [ ] **Step 2:** Add an inline `gleeunit` test asserting that `palette_for(Light).accent == "#226d6f"` and `palette_for(Dark).accent == "#5dbab0"`.

- [ ] **Step 3:** Run `nix develop --command bash -c 'cd web && gleam test'`. PASS.

- [ ] **Step 4:** Commit `Add Quiet (D1) palette + light/dark modes`.

---

### Task 5: `domain.gleam` + `fixture.gleam`

**Files:**
- Create: `web/src/sunset_web/domain.gleam`
- Create: `web/src/sunset_web/fixture.gleam`

- [ ] **Step 1:** In `domain.gleam`, declare the data types — `RoomId`, `ChannelId`, `MemberId` as opaque(-ish) wrappers (custom `pub type`s), then:

  ```
  ConnStatus { Connected Reconnecting Offline }
  ChannelKind { TextChannel Voice Bridge(BridgeKind) }
  BridgeKind { Minecraft }                       // start with one; add more later
  Presence { Online Speaking Muted Away OfflineP }
  RelayStatus { Direct OneHop TwoHop ThreeHop ViaPeer(String) BridgeRelay SelfRelay None }
  Room { id: RoomId, name: String, members: Int, online: Int, in_call: Int,
         status: ConnStatus, last_active: String, unread: Int, relay_hops: Option(Int),
         bridge: Option(BridgeKind) }
  Channel { id: ChannelId, name: String, kind: ChannelKind, in_call: Int, unread: Int }
  Member { id: MemberId, name: String, initials: String, status: Presence,
           role: Option(String), relay: RelayStatus, you: Bool, in_call: Bool,
           bridge: Option(BridgeKind) }
  Reaction { emoji: String, count: Int, by_you: Bool }
  Message { id: String, author: String, time: String, body: String,
            seen_by: Int, relay: RelayStatus, you: Bool, pending: Bool,
            reactions: List(Reaction), bridge: Option(BridgeKind) }
  ```

- [ ] **Step 2:** In `fixture.gleam` translate `data.jsx`'s SC_ROOMS / SC_CHANNELS / SC_MEMBERS / SC_MESSAGES into Gleam values of the types above. Drop fields we don't render (`color`, `crypto.*`, `image.*` — image attachment is deferred).

- [ ] **Step 3:** Add a `gleeunit` test that the fixture decodes (e.g. `list.length(rooms()) == 6`).

- [ ] **Step 4:** Commit `Add domain types + static fixture data`.

---

### Task 6: Layout shell + theme toggle (entrypoint)

**Files:**
- Modify: `web/src/sunset_web.gleam`
- Create: `web/src/sunset_web/views/shell.gleam`
- Create: `web/src/sunset_web/ui.gleam`

- [ ] **Step 1:** Build the Lustre `Model + Msg + init + update` pair as described in *State surface*. `init` reads `prefers-color-scheme` via a tiny FFI helper at `web/src/sunset_web/ui.ffi.mjs` exporting `prefersDark()`.

- [ ] **Step 2:** `views/shell.gleam` renders:
  - `body`-level wrapper with `bg`, `font_sans`, `text` colour from palette.
  - 4-column CSS grid: `260px 230px 1fr 220px` (or `54px ...` when collapsed). Children placeholders (`rooms · channels · main · members`) — plain coloured boxes for now.
  - Theme toggle button (sun/moon SVG) anchored top-right of the brand row (built later in Task 7).

- [ ] **Step 3:** Run the dev server (`nix run .#web-dev`); visually confirm the four columns + theme toggle works (light ↔ dark).

- [ ] **Step 4:** Commit `Add app shell + 4-col grid + theme toggle`.

---

### Task 7: Rooms rail (full + collapsed)

**Files:**
- Create: `web/src/sunset_web/views/rooms.gleam`

- [ ] **Step 1:** Render rail from `fixture.rooms()`. Full layout: brand row (logo + "sunset" + collapse chevron), search input (visual only), list of rooms with conn icon + name + meta line + unread pill, "you · 8f3c…a2" pinned at bottom.

- [ ] **Step 2:** Collapsed mode (54px wide): logo only, list of letter-only buttons with status dot + unread badge.

- [ ] **Step 3:** Inline SVG sun logo (port `QuietLogo` from quiet.jsx lines 31–38).

- [ ] **Step 4:** Wire `SelectRoom(id)` and `ToggleRoomsRail` into the buttons.

- [ ] **Step 5:** Commit `Add rooms rail (full + collapsed)`.

---

### Task 8: Channels rail (text + voice + bridge)

**Files:**
- Create: `web/src/sunset_web/views/channels.gleam`

- [ ] **Step 1:** Top: room title + conn icon, "X of N online".
- [ ] **Step 2:** "Channels" group: text channels with `#` prefix, unread pill.
- [ ] **Step 3:** "Voice" group: voice channels, with the active Lounge channel rendered as a *grouped* block — channel row gets `accent_soft` background + live dot, immediately followed by indented voice-member rows (name + speaking dot + flat waveform placeholder + muted badge), with a vertical accent connector line on the left.
- [ ] **Step 4:** Static "self control bar" (mic / headphones / leave buttons) below the in-call list — rendered statically, no toggle behaviour.
- [ ] **Step 5:** Bridge channels appear in their own subdued row.
- [ ] **Step 6:** Commit `Add channels rail with grouped voice detail block`.

---

### Task 9: Main column (header, messages, composer)

**Files:**
- Create: `web/src/sunset_web/views/main_panel.gleam`

- [ ] **Step 1:** Channel header (`# general`).
- [ ] **Step 2:** Messages list rendering grouping (consecutive same-author rows omit the header), pending state (opacity 0.55), bridge tag, reactions pills (count + "you" highlight), "read up to here" divider rendered after the last own-message that has been seen.
- [ ] **Step 3:** Image attachments: skipped in v1 — comment out or stub.
- [ ] **Step 4:** Typing indicator under the list.
- [ ] **Step 5:** Composer: image-attach icon (visual only) + text input bound to `Model.draft` + `↵ send` hint. No submit handler.
- [ ] **Step 6:** Commit `Add main column: channel header, messages, composer`.

---

### Task 10: Members rail

**Files:**
- Create: `web/src/sunset_web/views/members.gleam`

- [ ] **Step 1:** "Online — N" header, then in-call members first (sans you), then online-not-in-call.
- [ ] **Step 2:** Each member row: status dot, name (bold if speaking), bridged tag, no avatar circle.
- [ ] **Step 3:** "Offline — N" header + dimmed rows below.
- [ ] **Step 4:** Routing-detail-on-hover: skipped in v1; member rows are non-interactive.
- [ ] **Step 5:** Commit `Add members rail`.

---

### Task 11: Wiring + visual audit

**Files:**
- Modify: `web/src/sunset_web.gleam`, `web/src/sunset_web/views/shell.gleam`

- [ ] **Step 1:** Replace shell column placeholders with the real view modules.
- [ ] **Step 2:** Run dev server in light + dark, switch rooms (only the title in the channels rail changes — channels list is fixture-fixed for v1), collapse/expand rooms rail. No layout regressions, no console errors.
- [ ] **Step 3:** Spacing / colour audit against `quiet.jsx`. If anything's clearly off, fix in this task.
- [ ] **Step 4:** Commit `Wire all panels into the shell + visual audit`.

---

### Task 12: Production build artefact

**Files:**
- (no new files — verify `nix build .#web`)

- [ ] **Step 1:** `nix build .#web` succeeds and produces `result/index.html` + `result/<name>.min.mjs`.
- [ ] **Step 2:** Open `result/index.html` in a browser (file:// is fine) — page renders identically to the dev server.
- [ ] **Step 3:** Compute artefact size: `du -sh result/`. Should be ~tens of KB minified.
- [ ] **Step 4:** No commit needed if the only changes are derived files — but if the previous tasks left build/dist mistakenly in `web/.gitignore`, fix here.

---

### Task 13: GitHub Pages workflow + base-href

**Files:**
- Create: `.github/workflows/pages.yml`
- Modify: `web/gleam.toml` (or `flake.nix`) — set `<base href>` to the Pages sub-path

- [ ] **Step 1:** Add `<base href="/sunset/">` (or whatever the repo path is) to the Lustre HTML config so Pages serves correctly. Default is `/sunset/` matching the GitHub repo name; reconcile with the user's actual repo at deploy time.

- [ ] **Step 2:** Write `pages.yml`:

  ```yaml
  name: Deploy to GitHub Pages
  on:
    push: { branches: [master, main] }
  permissions: { contents: read, pages: write, id-token: write }
  concurrency: { group: pages, cancel-in-progress: true }
  jobs:
    build:
      runs-on: ubuntu-latest
      steps:
        - uses: actions/checkout@v4
        - uses: cachix/install-nix-action@v27
          with:
            extra_nix_config: |
              experimental-features = nix-command flakes
        - run: nix build .#web
        - run: cp -rL result web-dist
        - uses: actions/upload-pages-artifact@v3
          with: { path: web-dist }
    deploy:
      needs: build
      runs-on: ubuntu-latest
      environment:
        name: github-pages
        url: ${{ steps.deployment.outputs.page_url }}
      steps:
        - id: deployment
          uses: actions/deploy-pages@v4
  ```

- [ ] **Step 3:** Commit `Add Nix-built GitHub Pages deploy workflow`.

- [ ] **Step 4:** Push to remote (manually, after merge — not in automation). The first deploy needs the user to flip Pages source to "GitHub Actions" in the repo settings; flag this in the task summary.

---

### Task 14: Final pass

- [ ] `nix develop --command bash -c 'cd web && gleam format --check src test'` clean.
- [ ] `nix develop --command bash -c 'cd web && gleam test'` green.
- [ ] `nix build .#web` clean.
- [ ] `nix flake check` clean (workspace Rust tests still pass).
- [ ] If any cleanup commits, commit as `Final lint / fmt pass`.

---

## Verification (end-state acceptance)

After all 14 tasks land:

- `nix develop --command bash -c 'cd web && gleam test'` passes.
- `nix build .#web` produces a `result/` directory; opening `result/index.html` in a browser shows the D1 layout in light mode, the toggle flips to dark.
- No hand-written `.js` or `.ts` files anywhere in `web/` outside of the tiny `ui.ffi.mjs` shim for `prefersDark()`.
- `cargo test --workspace --all-features` (existing Rust workspace) still passes.
- `git log --oneline master..HEAD` shows roughly 14 task-by-task commits.
- `.github/workflows/pages.yml` exists and only invokes Nix; no `setup-beam` / `setup-node`.
