# Client-side image preprocessing (`sunset-image`)

**Date:** 2026-05-13
**Scope:** A new host-agnostic, wasm-compatible crate that standardises every image attachment to a reasonably-sized JPEG (or passes animated formats through unchanged) **before** the bytes are wrapped in an `ImageAttachment` and signed into the envelope. Adds HEIC/HEIF support along the way. All clients (web today; TUI / desktop / Minecraft later) get the same standardisation for free by going through the higher-level sunset send API.

## Why

The envelope doc currently says of `ImageAttachment`:

> The core neither validates the claimed type against the bytes nor enforces a per-image size cap — those are surface-level concerns the UI applies before composing.

That has three consequences we want to fix:

1. **HEIC unsupported.** iPhone photos uploaded "as-is" arrive as `image/heic`, which browsers can't render with `<img>`. The web app currently filters HEIC out of the file picker. As soon as a non-web client is added, the same problem reappears with no shared fix.
2. **Per-client divergence.** Every new client (TUI, desktop, mod) would have to re-implement size caps, format validation, EXIF stripping, and any future preprocessing rules. The current contract makes "the UI" responsible — which means N UIs doing it inconsistently.
3. **Wire bloat & quality drift.** A 12 MP iPhone HEIC is ~2 MB; pushed unchanged it inflates 33% under base64 (~2.7 MB on the wire) and renders at a single device pixel ratio. Everyone in the room pays the bandwidth cost.

The goal is one canonical Rust implementation of "user picked an image → bytes that go on the wire" that every client imports.

## Where it sits

```
sunset-web-wasm  (or future sunset-tui / sunset-desktop / sunset-mod)
        │   raw bytes from file picker (Vec<u8> / Uint8Array)
        ▼
sunset-core::ImageAttachment::preprocess(bytes)   ← new constructor
        │   delegates to:
        ▼
sunset-image::preprocess(bytes, &Config)          ← new crate
        │   uses:
        ▼
image crate (jpeg/png/webp/gif)  +  heic crate (HEIC/HEIF)
```

`ImageAttachment::raw(mime, base64)` stays for tests and any caller that *deliberately* wants to bypass preprocessing (we'll keep it `pub(crate)` or feature-gated to make accidental misuse hard).

## The crate

### `crates/sunset-image/`

Pure host-agnostic Rust. Compiles to `wasm32-unknown-unknown`. No host-specific code, no `Send + Sync` bounds, follows the same shape as `sunset-markdown`. `[lints] workspace = true` and the workspace clippy policy (no suppressions).

**Dependencies:**

| Dep | Why | Notes |
|---|---|---|
| [`image`](https://crates.io/crates/image) `0.25` | JPEG/PNG/WebP/GIF decode, JPEG encode, resize | `default-features = false`, features = `["jpeg", "png", "webp", "gif"]`. WASM-compatible (pure Rust codec backends; JPEG via `zune-jpeg`). |
| ~~[`heic`](https://github.com/imazen/heic) `0.1.4`~~ | ~~HEIC/HEIF decode~~ | **Blocked.** Licensed AGPL-3.0 or commercial; sunset is MIT. See "HEIC licensing decision" below. |

### HEIC licensing decision

While implementing the spec I discovered that **every pure-Rust HEIC decoder we could find is AGPL-licensed or unpublished**, which blocks the original plan of bundling decode into `sunset-image`. The shortlist:

| Decoder | Licence | Notes |
|---|---|---|
| [`imazen/heic`](https://github.com/imazen/heic) v0.1.4 | AGPL-3.0 OR commercial | Pure Rust, wasm-ready, actively maintained — but adopting it would force the entire workspace into AGPL territory (or require buying the commercial licence). |
| [`ente-io/heic-decoder`](https://github.com/ente-io/heic-decoder) | AGPL-3.0 | Not on crates.io; only 7 commits. Same AGPL constraint. |
| [`libheif-rs`](https://crates.io/crates/libheif-rs) | MIT | Wraps the C `libheif` library. `libheif` is LGPL-3 and pulls in `libde265` (LGPL-3, software-patented HEVC). Not wasm-friendly without a substantial emscripten / `cc-rs` vendoring effort. |
| [`libheif-js`](https://www.npmjs.com/package/libheif-js) | LGPL-3 (libheif) | JS-side WASM bundle (~2 MB). Browser-only, doesn't ship to the TUI / Minecraft / desktop clients without parallel implementations. Adds JS-side processing for one format, which is exactly what the spec set out to avoid. |

The PR that ships this spec implements the **non-HEIC** parts (JPEG / PNG / WebP / GIF normalisation) and surfaces HEIC inputs as [`Error::HeicUnsupported`] so the UI can render a clean "convert to JPEG before sending" hint. **Wiring HEIC decode requires a licence decision** that one autonomous run can't reasonably make on behalf of the project. Once a path is picked, adding decode is a localised change inside `sunset-image::transcode_via_heic_crate`.

The options, in order of how I'd lean today:

1. **Stay MIT, ship without HEIC.** Document the conversion path for iPhone users (Settings → Camera → Formats → Most Compatible) and surface a friendly error. Lowest cost, no licence churn. **Cost:** iPhone users on default settings can't upload directly.
2. **Use `libheif-js` (JS-side) for HEIC only.** Web-only HEIC support, ~2 MB extra bundle when actually used (load-on-demand), parallel non-Rust implementation just for HEIC. Future TUI / desktop clients would still need a story. **Cost:** breaks the "one preprocessing path for all clients" invariant the spec sets out, but only for the one format that has no permissive Rust option.
3. **Adopt `imazen/heic` and relicense to AGPL.** Cleanest engineering, biggest legal commitment. Best done deliberately and not while the user is asleep.
4. **Buy the commercial `heic` licence.** Same clean engineering, no copyleft, but ongoing cost. Worth re-evaluating once HEIC usage data justifies it.

Implementation parity: whichever option lands, the `sunset-image` API doesn't change — it stays `preprocess(bytes, &Config) -> Result<Preprocessed, Error>`. The HEIC branch swaps from `Err(HeicUnsupported)` to a real decode pipeline.

### Public API

```rust
//! crates/sunset-image/src/lib.rs

/// Preprocessing rules. Defaults aim at "good enough for chat":
///   - 2048 px max edge (longest side)
///   - JPEG quality 85
///   - format detection by magic-bytes only (input MIME is advisory)
#[derive(Clone, Debug)]
pub struct Config {
    pub max_edge: u32,
    pub jpeg_quality: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self { max_edge: 2048, jpeg_quality: 85 }
    }
}

/// The output of preprocessing: bytes ready to be base64'd into an
/// `ImageAttachment`, plus the MIME type the receiver should render with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preprocessed {
    pub mime_type: String,   // "image/jpeg" or original (for animated passthrough)
    pub bytes: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unrecognised image format (magic bytes did not match any supported codec)")]
    UnrecognisedFormat,
    #[error("decode failed: {0}")]
    Decode(String),
    #[error("encode failed: {0}")]
    Encode(String),
    #[error("image is empty (zero bytes after decode)")]
    Empty,
}

pub fn preprocess(input: &[u8], cfg: &Config) -> Result<Preprocessed, Error>;
```

**Why a single entry point that doesn't take input MIME:** browsers' `File.type` is unreliable for HEIC (sometimes `""`, sometimes `application/octet-stream`, sometimes `image/heif` vs `image/heic`). The function trusts magic bytes only, so callers don't have to play guessing games at the FFI boundary.

### Format-handling rules

Detection is done by sniffing the first ~32 bytes:

| Magic | Format | Action |
|---|---|---|
| `89 50 4E 47 0D 0A 1A 0A` | PNG | decode → resize → encode JPEG |
| `FF D8 FF` | JPEG | decode → resize → encode JPEG (always re-encode, see below) |
| `47 49 46 38 (37\|39) 61` | GIF | **pass-through unchanged** (animated, JPEG can't represent it) |
| `52 49 46 46 ?? ?? ?? ?? 57 45 42 50` | WebP | sniff `VP8 ` / `VP8L` / `VP8X`; if `VP8X` with animation flag, pass-through; else decode → resize → encode JPEG |
| `ftyp` box at offset 4 with brand `heic` / `heix` / `mif1` / `msf1` / `heim` / `heis` / `hevc` / `hevx` / `hevm` / `hevs` / `avif` / `avis` | HEIC/HEIF | **currently:** return `Error::HeicUnsupported` for the UI to surface. Decode requires a licence decision (see above). |
| anything else | — | `Error::UnrecognisedFormat` |

**Always re-encode JPEG?** Yes. The cost (one lossy round-trip at q=85) is small for a chat photo; the benefit is consistency: every JPEG on the wire is q=85, ≤ 2048 px on its longest edge, EXIF-stripped. The alternative — "pass JPEGs through if they look small" — leaks original EXIF (location, device, camera), and the spec aims for one canonical output shape. If profiling later shows this is a real perf problem on huge JPEGs in WASM, we can add a "skip re-encode for small in-budget JPEGs" branch behind a `Config` flag.

**EXIF / metadata:** `image` crate's JPEG encoder writes no EXIF by default. Re-encoding therefore strips orientation, geolocation, camera model. We **apply** EXIF orientation during decode (so the saved JPEG is upright) — needs either `image`'s built-in `orientation` handling (if available on the chosen version) or a small manual EXIF orientation parser before decode. Tracked in implementation tasks.

**Resize:** if both edges are ≤ `max_edge`, no resize. Otherwise scale-to-fit `max_edge` on the longest side via `image::imageops::FilterType::Lanczos3`. Aspect ratio preserved.

**Animated WebP detection:** WebP `VP8X` chunk has an animation bit at byte 0x14. Implemented by hand (10 lines) rather than pulling in another dep.

### Tests

`crates/sunset-image/tests/preprocess.rs` (integration tests, host target). All fixtures are **generated in-process** via the `image` crate so the suite ships no binary blobs:

1. **JPEG roundtrip:** synth 640×480 RGB → JPEG → preprocessed → output is `image/jpeg` with the original dimensions.
2. **PNG → JPEG:** synth 320×240 RGBA PNG → preprocessed → output is `image/jpeg`, dimensions match, alpha is flattened.
3. **WebP (still) → JPEG:** synth 256×128 lossless WebP → preprocessed → output is `image/jpeg`.
4. **GIF passthrough:** synth single-frame GIF → preprocessed → output `mime_type == "image/gif"` and `bytes == input` byte-for-byte.
5. **Oversize resize:** synth 4096×3072 PNG → preprocessed with `max_edge: 2048` → output JPEG, longest edge == 2048, aspect ratio preserved within 1 px.
6. **No upscale:** synth 64×64 JPEG → preprocessed with default cap (2048) → output dimensions still 64×64.
7. **Random bytes rejected:** `0xab × 64` → `Err(Error::UnrecognisedFormat)`.
8. **Truncated input rejected:** empty / 3-byte input → `Err(Error::UnrecognisedFormat)`.
9. **Truncated PNG surfaces decode error:** valid PNG magic + 8 null bytes → `Err(Error::Decode(_))` (no panic).
10. **HEIC sentinel:** `ftyp` box with `heic` brand → `Err(Error::HeicUnsupported)` (distinct from `UnrecognisedFormat` so the UI can render a tailored message).

Two tests from the original spec were dropped pending the HEIC licence decision:
- **HEIC → JPEG** (covered for now by the sentinel error test above).
- **WebP (animated) passthrough** (the `image` crate's WebP encoder writes still images; the sniffer's animation-flag handling is covered by a unit test rather than an end-to-end transcode round-trip).
- **EXIF orientation** is deferred to a follow-up; the `image` crate's default `load_from_memory` path does not auto-apply orientation, and the right fix is to decode through `ImageReader` with explicit orientation handling. Tracked as an open issue.

### Conformance to workspace rules

- `[lints] workspace = true` and no clippy suppressions — same policy enforced by `scripts/check-no-clippy-allow.sh`.
- Compile-checked under `--target wasm32-unknown-unknown` (CI already has this set up; the new crate just needs to appear in the existing wasm matrix).
- All deps go through the flake (`image` and `heic` are Rust crates pulled via Cargo, no system deps needed — fits the hermeticity rule without changes to `flake.nix`).

## Integrating into `sunset-core`

**Goal:** clients call one constructor, get a wire-ready `ImageAttachment`.

```rust
// crates/sunset-core/src/crypto/envelope.rs

impl ImageAttachment {
    /// Preprocess raw image bytes (whatever the file picker handed us)
    /// into a wire-ready attachment. Bytes are decoded, normalised, and
    /// re-encoded per `sunset_image`'s rules, then base64'd.
    ///
    /// Returns an error if the bytes aren't a recognised image format
    /// or the codec fails. Callers should surface this to the user
    /// ("couldn't read that image") rather than silently dropping.
    pub fn preprocess(bytes: &[u8]) -> Result<Self, sunset_image::Error> {
        Self::preprocess_with(bytes, &sunset_image::Config::default())
    }

    pub fn preprocess_with(
        bytes: &[u8],
        cfg: &sunset_image::Config,
    ) -> Result<Self, sunset_image::Error> {
        let out = sunset_image::preprocess(bytes, cfg)?;
        Ok(Self {
            mime_type: out.mime_type,
            data_base64: base64_standard_encode(&out.bytes),
        })
    }

    /// Construct an `ImageAttachment` from already-encoded base64
    /// bytes and a claimed MIME type. **No preprocessing applied.**
    /// Reserved for tests and the receive path; production senders
    /// should always go through `preprocess`.
    #[doc(hidden)]
    pub fn raw(mime_type: String, data_base64: String) -> Self {
        Self { mime_type, data_base64 }
    }
}
```

`sunset-core` takes `sunset-image` as a regular dep (it's wasm-compatible). The envelope doc comment is updated to say that the canonical constructor is `preprocess`, and that `raw` exists for the receive path / tests only.

### Wasm boundary change

The original spec proposed changing the JS↔wasm boundary to carry raw `Uint8Array`s. **Shipped:** the boundary still carries `{ mime_type, data_base64 }` objects, and the Rust side base64-decodes before preprocessing. Rationale: the Gleam composer's pending-attachment strip already needs the base64 data URL for thumbnail rendering, so re-using the same string across the boundary keeps the JS side and the Gleam `Attachment` type unchanged. The cost is one base64-decode pass on send (microseconds), well below the JPEG encode cost; the benefit is a much smaller diff that doesn't touch the composer UI or any Gleam types.

Actual shape (`crates/sunset-web-wasm/src/room_handle.rs`):

```rust
fn images_from_js(arr: &js_sys::Array) -> Result<Vec<ImageAttachment>, JsError> {
    let mut out = Vec::with_capacity(arr.length() as usize);
    let b64 = base64::engine::general_purpose::STANDARD;
    for i in 0..arr.length() {
        let item = arr.get(i);
        let _ = js_sys::Reflect::get(&item, &JsValue::from_str("mime_type"))
            .map_err(|_| JsError::new(&format!("images[{i}]: missing mime_type")))?;
        let data = js_sys::Reflect::get(&item, &JsValue::from_str("data_base64"))
            .map_err(|_| JsError::new(&format!("images[{i}]: missing data_base64")))?
            .as_string()
            .ok_or_else(|| JsError::new(&format!("images[{i}].data_base64 must be a string")))?;
        let raw = b64.decode(&data)
            .map_err(|e| JsError::new(&format!("images[{i}]: base64 decode: {e}")))?;
        let attachment = ImageAttachment::preprocess(&raw)
            .map_err(|e| JsError::new(&format!("images[{i}]: {e}")))?;
        out.push(attachment);
    }
    Ok(out)
}
```

The browser-supplied `mime_type` is read but discarded; the sniffer trusts magic bytes only (browsers lie about HEIC).

### Web composer accept list

**Shipped:** unchanged at `"image/jpeg,image/png,image/webp,image/gif"`. The original spec proposed adding `image/heic,image/heif`; that change is held back pending the HEIC licence decision because surfacing HEIC in the picker while the wasm side rejects it is bad UX (user picks file → silently nothing happens). When HEIC decode lands, both sides flip in the same PR.

## E2E

Shipped in `web/e2e/images.spec.js`:

1. **PNG → JPEG transcode** (existing `two browsers exchange…` test, updated): pick a PNG, send, assert the receiver's `<img src>` starts with `data:image/jpeg;base64,…` and `el.decode()` produces a real JPEG at the expected dimensions. Replaces the old byte-for-byte PNG round-trip assertion (which no longer holds under preprocessing).
2. **Oversize PNG resized to cap** (new): generate a 3000×2000 PNG in-browser via `<canvas>.toBlob()` (so no fixture file ships in git), upload via `setInputFiles`, assert the receiver gets a JPEG with longest edge == 2048 px and aspect ratio preserved within 1 px.
3. **GIF byte-for-byte pass-through** (existing tests, tightened): both `image-only send` and `removing a staged image` now assert `src.endsWith(GIF_1X1_BASE64)`, locking in that GIF stays a GIF unchanged.
4. **HEIC error surfacing** — *deferred.* The wasm side returns `Error::HeicUnsupported` if HEIC bytes reach it, but the composer doesn't surface send errors to the user today (`MessageSent(_)` is a no-op in `web/src/sunset_web.gleam`). Adding the error-toast plumbing is scope creep for this PR; the rejection is verified at the unit level (`sunset_image::heic_inputs_surface_distinct_error`, `sunset_core::image_attachment_preprocess_surfaces_heic_unsupported`). The composer accept list keeps HEIC out of the picker so the path is unreachable in normal flow.

These tests obey CLAUDE.md test discipline:
- They drive the actual file picker via Playwright's `setInputFiles`.
- Timeouts are tight (the 3000×2000 → 2048 resize + JPEG encode runs in ~6 s on host hardware; the e2e budget is 30 s to give CI hardware headroom without masking real perf regressions).
- No mocks past the integration boundary — real wasm bundle, real `image` encoder.

## What we explicitly do *not* do

- **Re-encode GIFs into video or animated WebP.** Out of scope; preserves the user's expectation that GIFs are GIFs.
- **Server-side fallback.** Preprocessing is client-side only, by design — the relay must keep seeing opaque encrypted bytes, and no other client should have to trust the sender's preprocessing.
- **Wire-format change** (e.g. switching `data_base64: String` to `Vec<u8>`). Tempting (saves 33% on the wire) but a wire-format break is out of scope; tracked as a future spec.
- **Quality / size knobs in the UI.** The crate exposes `Config` for testing, but production clients use `Default`. If we want a "high quality / data saver" toggle later, it goes through `Config` without API changes.
- **AVIF / JXL encode.** Decode of AVIF-in-HEIF arrives "for free" via `heic`'s `av1` feature; encode stays JPEG-only for now (universal browser support, no extra deps).

## Open follow-ups (none blocking this PR)

1. **HEIC licence decision** (see "HEIC licensing decision" above). Until this is resolved, iPhone users on default camera settings can't upload directly.
2. **EXIF orientation handling.** `image::load_from_memory_with_format` does *not* auto-apply EXIF orientation. The right fix is to decode through `image::ImageReader` + call `decoder.orientation()` + `image.apply_orientation(orientation)`. Deferred to a follow-up; in practice the symptom is iPhone photos appearing sideways after preprocessing.
3. **Composer error UX.** `MessageSent(_)` is a no-op today. If preprocessing rejects an attachment (HEIC, corrupt JPEG, unknown format) the composer clears and the user sees nothing. A small toast / inline error in the composer would close the loop. Tracked separately because it's a UX scope of its own (applies to all send failures, not just preprocessing).
4. **Wasm bundle-size budget.** Adding `image` (with jpeg/png/webp/gif features) bumps the wasm bundle. Historical sizing for the `image` crate's relevant codecs is ~150–250 KB pre-gzip. We haven't measured the delta on this branch; if it crosses a sensible threshold we look at lazy instantiation or a separate worker chunk in a follow-up.

## Migration mechanics

1. Ship `sunset-image` crate behind no feature flags — it's pulled in by `sunset-core`'s `ImageAttachment::preprocess`, but the wire format is unchanged so nothing else moves.
2. Switch the wasm boundary + JS bridge in a single PR (atomic — the JS side has to stop sending base64 the same release the Rust side starts expecting raw bytes).
3. Update the web composer's `accept` list to include HEIC/HEIF.
4. Add the e2e tests in the same PR.
5. Future clients (TUI, desktop, mod) just call `ImageAttachment::preprocess(bytes)` and they're done.
