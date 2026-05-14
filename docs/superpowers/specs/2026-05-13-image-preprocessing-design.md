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
| [`heic`](https://github.com/imazen/heic) `0.1.4` | HEIC/HEIF decode | Pure Rust, `no_std + alloc`, **explicitly compiles to `wasm32-unknown-unknown`**. `#![forbid(unsafe_code)]`. Latest release April 2026. Covers HEVC (iPhone HEICs), AV1 in HEIF (via `av1` feature), uncompressed (via `unci`). 49/49 ITU-T HEVC conformance vectors pass. |

**Maturity note on `heic` 0.1.x:** the crate is alpha-versioned and the README discloses AI-assisted development with "not all code manually reviewed." 118/162 of its own HEIF test files decode cleanly with all features enabled. We accept this because:
- iPhone HEICs are HEVC I-slice based, which the crate has hardened conformance vectors for.
- The alternative is libheif (C++) → not wasm-friendly without an emscripten detour the workspace's hermeticity rule would force us to vendor.
- We *only* use the decoder; failure mode is "couldn't decode this HEIC, fall back to error or pass-through" — never silent corruption, because we never sign HEIC bytes onto the wire.

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
| `ftyp` box at offset 4 with brand `heic` / `heix` / `mif1` / `msf1` / `heim` / `heis` | HEIC/HEIF | decode (via `heic`) → resize → encode JPEG |
| anything else | — | `Error::UnrecognisedFormat` |

**Always re-encode JPEG?** Yes. The cost (one lossy round-trip at q=85) is small for a chat photo; the benefit is consistency: every JPEG on the wire is q=85, ≤ 2048 px on its longest edge, EXIF-stripped. The alternative — "pass JPEGs through if they look small" — leaks original EXIF (location, device, camera), and the spec aims for one canonical output shape. If profiling later shows this is a real perf problem on huge JPEGs in WASM, we can add a "skip re-encode for small in-budget JPEGs" branch behind a `Config` flag.

**EXIF / metadata:** `image` crate's JPEG encoder writes no EXIF by default. Re-encoding therefore strips orientation, geolocation, camera model. We **apply** EXIF orientation during decode (so the saved JPEG is upright) — needs either `image`'s built-in `orientation` handling (if available on the chosen version) or a small manual EXIF orientation parser before decode. Tracked in implementation tasks.

**Resize:** if both edges are ≤ `max_edge`, no resize. Otherwise scale-to-fit `max_edge` on the longest side via `image::imageops::FilterType::Lanczos3`. Aspect ratio preserved.

**Animated WebP detection:** WebP `VP8X` chunk has an animation bit at byte 0x14. Implemented by hand (10 lines) rather than pulling in another dep.

### Tests

`crates/sunset-image/tests/preprocess.rs` (integration tests, host target):

1. **JPEG roundtrip:** small JPEG fixture → preprocessed → output decodes to a JPEG of the expected dimensions.
2. **PNG → JPEG:** small PNG fixture → preprocessed → output is `image/jpeg`, decodes successfully, dimensions match.
3. **WebP (still) → JPEG:** small still WebP fixture → preprocessed → output is `image/jpeg`.
4. **GIF passthrough:** small GIF → preprocessed → output `mime_type == "image/gif"` and `bytes == input`.
5. **WebP (animated) passthrough:** animated WebP fixture → preprocessed → output mime is `image/webp` and bytes unchanged.
6. **HEIC → JPEG:** an iPhone-style HEIC fixture (HEVC, single frame) → preprocessed → output is JPEG with the expected dimensions (no pixel-exact match required; HEIC→YCbCr→RGB→JPEG is lossy).
7. **Resize:** a 4096×3072 PNG → preprocessed with `max_edge: 2048` → output JPEG with `max(w, h) == 2048` and aspect ratio preserved within 1 px.
8. **No-op resize:** a 512×512 JPEG → preprocessed → output dimensions still 512×512.
9. **Unrecognised:** random bytes → `Err(Error::UnrecognisedFormat)`.
10. **EXIF orientation:** a JPEG with EXIF orientation 6 (rotate 270° CW) → preprocessed → decoded output has the rotated pixel layout. Test the *visible* outcome (e.g. a marker pixel is in the expected corner), not the EXIF tag.

Fixtures live in `crates/sunset-image/tests/fixtures/` (small files, ideally < 50 KB each). For HEIC we can borrow one of the `heic` crate's BSD-3 test vectors or capture a tiny single-frame HEVC HEIC; flake builds shouldn't pay for >100 KB of fixtures.

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

Today `sunset-web-wasm::RoomHandle::send_message` takes a JS array of `{ mime_type, data_base64 }`. The JS layer does a `FileReader.readAsDataURL` round-trip and ships base64 across the boundary. Now that we're preprocessing, we want raw bytes to cross the boundary so we can decode them directly.

Change:

```rust
// crates/sunset-web-wasm/src/room_handle.rs
fn images_from_js(arr: &js_sys::Array) -> Result<Vec<ImageAttachment>, JsError> {
    let mut out = Vec::with_capacity(arr.length() as usize);
    for i in 0..arr.length() {
        // Each entry is a Uint8Array of the raw file bytes.
        let bytes_js: js_sys::Uint8Array = arr.get(i).dyn_into()
            .map_err(|_| JsError::new(&format!("images[{i}]: expected Uint8Array")))?;
        let bytes = bytes_js.to_vec();
        let attachment = ImageAttachment::preprocess(&bytes)
            .map_err(|e| JsError::new(&format!("images[{i}]: {e}")))?;
        out.push(attachment);
    }
    Ok(out)
}
```

And on the JS side, `pickImages()` switches from `readAsDataURL` to `arrayBuffer()` and passes `Uint8Array`s through. The Gleam `Attachment` type stops carrying `data_base64` for outbound use — it becomes a transient `{mime, bytes}` that the JS bridge consumes and discards once preprocessing is done. (The receive path is unchanged; it still gets `{mime_type, data_base64}` objects out of the wasm side.)

### Web composer accept list

Update `web/src/sunset_web/sunset.ffi.mjs:506` from
`"image/jpeg,image/png,image/webp,image/gif"` to
`"image/jpeg,image/png,image/webp,image/gif,image/heic,image/heif,.heic,.heif"`.

(The trailing `.heic`/`.heif` filename hints help Safari/Chrome when MIME detection misses.)

## E2E

Add to `web/e2e/images.spec.js`:

1. **HEIC upload smokes through.** Pick a small HEIC fixture → assert sent message renders as an `<img>` whose `src` starts with `data:image/jpeg;base64,` (not `image/heic`) and the image is visible to the other peer.
2. **Big-image resize cap.** Pick a 4096×4096 PNG fixture → assert the sent attachment's `src` decodes to a JPEG ≤ 2048 px on its longest edge. (Decode via a tiny `Image` in the page and assert `naturalWidth` / `naturalHeight`.)
3. **GIF passthrough.** Pick an animated GIF → assert the rendered `src` is `data:image/gif;base64,…` and the base64 payload byte-length matches the input (modulo base64 padding).
4. **Garbage rejected.** Pick a text file renamed to `.jpg` → assert the composer surfaces an error (sub-spec for the error UI is part of the implementation plan, but the existing toast / error channel can carry it).

These tests obey CLAUDE.md test discipline:
- They drive the actual file picker via Playwright's `setInputFiles`.
- Timeouts are tight (a 4 MP resize on WASM should complete in well under 2 s; if it doesn't, the perf is a real bug worth catching).
- No mocks past the integration boundary — real wasm bundle, real `heic` decoder, real `image` encoder.

## What we explicitly do *not* do

- **Re-encode GIFs into video or animated WebP.** Out of scope; preserves the user's expectation that GIFs are GIFs.
- **Server-side fallback.** Preprocessing is client-side only, by design — the relay must keep seeing opaque encrypted bytes, and no other client should have to trust the sender's preprocessing.
- **Wire-format change** (e.g. switching `data_base64: String` to `Vec<u8>`). Tempting (saves 33% on the wire) but a wire-format break is out of scope; tracked as a future spec.
- **Quality / size knobs in the UI.** The crate exposes `Config` for testing, but production clients use `Default`. If we want a "high quality / data saver" toggle later, it goes through `Config` without API changes.
- **AVIF / JXL encode.** Decode of AVIF-in-HEIF arrives "for free" via `heic`'s `av1` feature; encode stays JPEG-only for now (universal browser support, no extra deps).

## Open questions (none blocking)

1. **EXIF orientation handling**: does `image 0.25` apply orientation automatically on decode, or do we need a small inline parser? Implementation task validates this; if not, we vendor a ~30-line orientation pre-rotate before resize.
2. **Best HEIC fixture source.** Smallest viable HEIC for a test? Either generate one in CI (heic crate may have a fixture we can re-use; check its BSD-3 LICENSE) or capture a 64×64 single-frame HEVC HEIC by hand and commit it.
3. **Wasm bundle-size budget.** Adding `image` + `heic` will bump the bundle. `heic` is small (pure Rust, no_std); `image` with only jpeg/png/webp/gif features is moderate (~150–250 KB pre-gzip historically). We measure and report; if it crosses a threshold we set lazily-instantiated wasm or a separate worker chunk in a follow-up.

## Migration mechanics

1. Ship `sunset-image` crate behind no feature flags — it's pulled in by `sunset-core`'s `ImageAttachment::preprocess`, but the wire format is unchanged so nothing else moves.
2. Switch the wasm boundary + JS bridge in a single PR (atomic — the JS side has to stop sending base64 the same release the Rust side starts expecting raw bytes).
3. Update the web composer's `accept` list to include HEIC/HEIF.
4. Add the e2e tests in the same PR.
5. Future clients (TUI, desktop, mod) just call `ImageAttachment::preprocess(bytes)` and they're done.
