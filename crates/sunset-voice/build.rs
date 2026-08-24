//! Compile vendored libopus 1.5.2 into a static archive that
//! `sunset-voice` and any downstream crate (e.g. `sunset-web-wasm`)
//! can link against.
//!
//! ## Where libopus comes from
//!
//! `flake.nix` declares `libopus` as a flake input pinned by rev to
//! v1.5.2 (the canonical version pin lives in `flake.lock`). The
//! dev shell's `shellHook` plants a symlink at `vendor/libopus`
//! pointing into that input, and the `srcWithLibopus` derivation
//! does the equivalent paste for `nix build` outputs. Either path
//! lands the libopus tree at the relative path this script reads
//! below; building outside `nix develop` is unsupported.
//!
//! ## Why a build script lives here
//!
//! The codec FFI lives in `sunset-voice` (the layered home for
//! everything voice-related). For host-target builds (`cargo test
//! -p sunset-voice`) the link directives this script emits resolve
//! at the test binary's link step, the way Cargo intends.
//!
//! For wasm32 builds the cdylib lives downstream in `sunset-web-wasm`.
//! Cargo does not propagate `cargo:rustc-link-lib=...` directives
//! from a transitive `rlib` dependency's build script to the
//! `cdylib`'s link step (this is the link-propagation gap documented
//! in `docs/superpowers/specs/2026-04-30-sunset-voice-codec-decision.md`).
//! The path we take instead: this script publishes the OUT_DIR via
//! `cargo:lib_dir=...` (surfaced as `DEP_OPUS_LIB_DIR` to downstream
//! crates that declare us under `links="opus"`), and
//! `sunset-web-wasm/build.rs` re-emits the link directives for its
//! own cdylib link step.
//!
//! ## Source list
//!
//! Read out of the `*_sources.mk` files under `vendor/libopus/` at
//! build time rather than transcribed here, so it cannot drift from
//! upstream. We take the float-API variables and skip the
//! arch-intrinsic / DRED / LPCNet ones — none of those add value on
//! wasm32 and they would pull in additional symbols.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../vendor/libopus");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let opus_root = manifest_dir
        .join("..")
        .join("..")
        .join("vendor")
        .join("libopus")
        .canonicalize()
        .expect(
            "vendor/libopus is missing — enter the dev shell so the libopus \
             flake input is symlinked into place (`nix develop`, or any direnv \
             shell with `use flake`). Direct `cargo build` outside the flake \
             is not supported.",
        );

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let is_wasm = target_arch == "wasm32";

    let mut build = cc::Build::new();

    // On Nix the dev shell exports `CC=gcc` for the host stdenv,
    // which cc-rs picks up as a global override and uses regardless
    // of target. For the wasm32 build we need clang (gcc cannot
    // target wasm32). Force it here so the choice doesn't depend on
    // whoever happens to invoke the build.
    if is_wasm {
        let clang = env::var("CC_wasm32_unknown_unknown")
            .ok()
            .or_else(|| env::var("WASM_CC").ok())
            .unwrap_or_else(|| "clang".to_string());
        build.compiler(&clang);
        build.archiver("llvm-ar");
        // cc-rs honors `--target=...` from the compiler flags; it
        // also passes the target itself, but being explicit keeps
        // the build deterministic against future cc-rs changes.
        build.flag(format!("--target={}", env::var("TARGET").unwrap()).as_str());
    }

    build
        .include(opus_root.join("include"))
        .include(&opus_root)
        .include(opus_root.join("celt"))
        .include(opus_root.join("silk"))
        .include(opus_root.join("silk").join("float"))
        .define("OPUS_BUILD", None)
        .define("USE_ALLOCA", None)
        .define("HAVE_LRINTF", None)
        .define("HAVE_LRINT", None)
        // FLOAT_APPROX swaps libopus's `log/exp/sin/cos`-using helpers
        // for polynomial approximations. Cuts the libm surface we
        // need to provide on wasm32 and is the configuration the
        // upstream CMake build defaults to in Release mode.
        .define("FLOAT_APPROX", None)
        // Silence noisy warnings from upstream C source — it compiles
        // clean under -Wall but we are not the maintainers of it.
        .warnings(false)
        .extra_warnings(false)
        .opt_level(3);

    if is_wasm {
        build.flag_if_supported("-fno-exceptions");
        build.flag_if_supported("-ffast-math");
        // wasm32-unknown-unknown is freestanding — there is no libc.
        // We ship minimal stubs for the headers libopus reaches for
        // (`<math.h>`, `<string.h>`, `<stdlib.h>`, `<stdio.h>`,
        // `<alloca.h>`); the matching symbol definitions live in
        // `src/codec/wasm_runtime.rs` and resolve at wasm-ld time.
        let stub_dir = manifest_dir.join("csrc").join("wasm_libc");
        build.flag("-nostdlibinc");
        // cc-rs emits each `flag(...)` as a single argv token, so
        // `-isystem` + `path` need to be two separate calls (clang
        // does NOT accept `-isystem=path`).
        build.flag("-isystem");
        build.flag(stub_dir.to_str().unwrap());
    }

    for path in source_files(&opus_root) {
        build.file(path);
    }

    build.compile("opus");

    let out_dir = env::var("OUT_DIR").unwrap();
    // Surface the static-archive directory to dependents (Cargo
    // converts `cargo:lib_dir=...` into `DEP_OPUS_LIB_DIR` for any
    // crate that declares `links = "opus"` and depends on this one).
    println!("cargo:lib_dir={}", out_dir);
    println!("cargo:include={}", opus_root.join("include").display());
}

fn source_files(opus_root: &Path) -> Vec<PathBuf> {
    const LISTS: [(&str, &[&str]); 3] = [
        ("opus_sources.mk", &["OPUS_SOURCES", "OPUS_SOURCES_FLOAT"]),
        ("celt_sources.mk", &["CELT_SOURCES"]),
        ("silk_sources.mk", &["SILK_SOURCES", "SILK_SOURCES_FLOAT"]),
    ];

    let mut files = Vec::new();
    for (mk_name, vars) in LISTS {
        let mk_path = opus_root.join(mk_name);
        let mk = fs::read_to_string(&mk_path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", mk_path.display()));
        for var in vars {
            files.extend(mk_var(&mk, &mk_path, var).map(|rel| opus_root.join(rel)));
        }
    }
    files
}

/// The names listed under `var` in a `*_sources.mk` file, following
/// make's `\`-continued line syntax.
fn mk_var<'a>(mk: &'a str, mk_path: &Path, var: &str) -> impl Iterator<Item = &'a str> {
    let mut lines = mk.lines();
    let mut line = lines
        .find_map(|l| {
            l.split_once('=')
                .filter(|(name, _)| name.trim() == var)
                .map(|(_, value)| value)
        })
        .unwrap_or_else(|| {
            panic!(
                "{} does not define {var} — the vendored libopus source layout \
                 changed; reconcile build.rs with it",
                mk_path.display()
            )
        });

    let mut names = Vec::new();
    loop {
        let continued = line.trim_end().ends_with('\\');
        names.extend(line.trim_end().trim_end_matches('\\').split_whitespace());
        match lines.next().filter(|_| continued) {
            Some(next) => line = next,
            None => return names.into_iter(),
        }
    }
}
