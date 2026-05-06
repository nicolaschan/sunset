//! Re-emit the libopus link directives at the cdylib's link step.
//!
//! `sunset-voice/build.rs` builds `libopus.a` and emits
//! `cargo:rustc-link-lib=static=opus` plus
//! `cargo:rustc-link-search=...` from its own build script. For the
//! host-target unit tests in `sunset-voice` those directives reach
//! the test binary's link command directly.
//!
//! For wasm32 builds the cdylib lives here (`sunset-web-wasm`), and
//! Cargo does not propagate `rustc-link-lib=...` from a transitive
//! `rlib`'s build script to the `cdylib`'s link step (this is the
//! gap documented in
//! `docs/superpowers/specs/2026-04-30-sunset-voice-codec-decision.md`).
//! The fix is to declare `links = "opus"` on `sunset-voice` so Cargo
//! exposes its `cargo:lib_dir=...` output as `DEP_OPUS_LIB_DIR` here,
//! then re-emit the directives directly from this build script — at
//! which point they land on this cdylib's own rustc command line.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=DEP_OPUS_LIB_DIR");

    let lib_dir = env::var("DEP_OPUS_LIB_DIR").expect(
        "DEP_OPUS_LIB_DIR must be set by sunset-voice's build.rs (links=\"opus\"); \
         this crate cannot link without it",
    );

    println!("cargo:rustc-link-search=native={}", lib_dir);
    println!("cargo:rustc-link-lib=static=opus");
}
