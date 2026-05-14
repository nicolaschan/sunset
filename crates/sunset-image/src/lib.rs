//! Client-side image preprocessing for sunset.
//!
//! See `docs/superpowers/specs/2026-05-13-image-preprocessing-design.md`.

#![cfg_attr(docsrs, feature(doc_auto_cfg))]

// Stub: real implementation lands in the next commit. This commit only
// proves the `image` + `heic` deps resolve and compile on every target
// the workspace cares about (host + wasm32-unknown-unknown).
pub fn _proof_of_deps() {
    let _ = image::ImageFormat::Jpeg;
    let _ = heic::DecoderConfig::new();
}
