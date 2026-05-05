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
//! Mirrors the `*_sources.mk` files under `vendor/libopus/`. We
//! compile the float-API path (`OPUS_SOURCES + OPUS_SOURCES_FLOAT +
//! CELT_SOURCES + SILK_SOURCES + SILK_SOURCES_FLOAT`) and skip every
//! arch-intrinsic / DRED / LPCNet source — none of those add value
//! on wasm32 and they would pull in additional symbols.

use std::env;
use std::path::PathBuf;

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

fn source_files(opus_root: &std::path::Path) -> Vec<PathBuf> {
    // OPUS_SOURCES + OPUS_SOURCES_FLOAT (from vendor/libopus/opus_sources.mk).
    let opus = [
        "src/opus.c",
        "src/opus_decoder.c",
        "src/opus_encoder.c",
        "src/extensions.c",
        "src/opus_multistream.c",
        "src/opus_multistream_encoder.c",
        "src/opus_multistream_decoder.c",
        "src/repacketizer.c",
        "src/opus_projection_encoder.c",
        "src/opus_projection_decoder.c",
        "src/mapping_matrix.c",
        // OPUS_SOURCES_FLOAT
        "src/analysis.c",
        "src/mlp.c",
        "src/mlp_data.c",
    ];

    // CELT_SOURCES (from vendor/libopus/celt_sources.mk).
    let celt = [
        "celt/bands.c",
        "celt/celt.c",
        "celt/celt_encoder.c",
        "celt/celt_decoder.c",
        "celt/cwrs.c",
        "celt/entcode.c",
        "celt/entdec.c",
        "celt/entenc.c",
        "celt/kiss_fft.c",
        "celt/laplace.c",
        "celt/mathops.c",
        "celt/mdct.c",
        "celt/modes.c",
        "celt/pitch.c",
        "celt/celt_lpc.c",
        "celt/quant_bands.c",
        "celt/rate.c",
        "celt/vq.c",
    ];

    // SILK_SOURCES (from vendor/libopus/silk_sources.mk).
    let silk = [
        "silk/CNG.c",
        "silk/code_signs.c",
        "silk/init_decoder.c",
        "silk/decode_core.c",
        "silk/decode_frame.c",
        "silk/decode_parameters.c",
        "silk/decode_indices.c",
        "silk/decode_pulses.c",
        "silk/decoder_set_fs.c",
        "silk/dec_API.c",
        "silk/enc_API.c",
        "silk/encode_indices.c",
        "silk/encode_pulses.c",
        "silk/gain_quant.c",
        "silk/interpolate.c",
        "silk/LP_variable_cutoff.c",
        "silk/NLSF_decode.c",
        "silk/NSQ.c",
        "silk/NSQ_del_dec.c",
        "silk/PLC.c",
        "silk/shell_coder.c",
        "silk/tables_gain.c",
        "silk/tables_LTP.c",
        "silk/tables_NLSF_CB_NB_MB.c",
        "silk/tables_NLSF_CB_WB.c",
        "silk/tables_other.c",
        "silk/tables_pitch_lag.c",
        "silk/tables_pulses_per_block.c",
        "silk/VAD.c",
        "silk/control_audio_bandwidth.c",
        "silk/quant_LTP_gains.c",
        "silk/VQ_WMat_EC.c",
        "silk/HP_variable_cutoff.c",
        "silk/NLSF_encode.c",
        "silk/NLSF_VQ.c",
        "silk/NLSF_unpack.c",
        "silk/NLSF_del_dec_quant.c",
        "silk/process_NLSFs.c",
        "silk/stereo_LR_to_MS.c",
        "silk/stereo_MS_to_LR.c",
        "silk/check_control_input.c",
        "silk/control_SNR.c",
        "silk/init_encoder.c",
        "silk/control_codec.c",
        "silk/A2NLSF.c",
        "silk/ana_filt_bank_1.c",
        "silk/biquad_alt.c",
        "silk/bwexpander_32.c",
        "silk/bwexpander.c",
        "silk/debug.c",
        "silk/decode_pitch.c",
        "silk/inner_prod_aligned.c",
        "silk/lin2log.c",
        "silk/log2lin.c",
        "silk/LPC_analysis_filter.c",
        "silk/LPC_inv_pred_gain.c",
        "silk/table_LSF_cos.c",
        "silk/NLSF2A.c",
        "silk/NLSF_stabilize.c",
        "silk/NLSF_VQ_weights_laroia.c",
        "silk/pitch_est_tables.c",
        "silk/resampler.c",
        "silk/resampler_down2_3.c",
        "silk/resampler_down2.c",
        "silk/resampler_private_AR2.c",
        "silk/resampler_private_down_FIR.c",
        "silk/resampler_private_IIR_FIR.c",
        "silk/resampler_private_up2_HQ.c",
        "silk/resampler_rom.c",
        "silk/sigm_Q15.c",
        "silk/sort.c",
        "silk/sum_sqr_shift.c",
        "silk/stereo_decode_pred.c",
        "silk/stereo_encode_pred.c",
        "silk/stereo_find_predictor.c",
        "silk/stereo_quant_pred.c",
        "silk/LPC_fit.c",
    ];

    // SILK_SOURCES_FLOAT (from vendor/libopus/silk_sources.mk).
    let silk_float = [
        "silk/float/apply_sine_window_FLP.c",
        "silk/float/corrMatrix_FLP.c",
        "silk/float/encode_frame_FLP.c",
        "silk/float/find_LPC_FLP.c",
        "silk/float/find_LTP_FLP.c",
        "silk/float/find_pitch_lags_FLP.c",
        "silk/float/find_pred_coefs_FLP.c",
        "silk/float/LPC_analysis_filter_FLP.c",
        "silk/float/LTP_analysis_filter_FLP.c",
        "silk/float/LTP_scale_ctrl_FLP.c",
        "silk/float/noise_shape_analysis_FLP.c",
        "silk/float/process_gains_FLP.c",
        "silk/float/regularize_correlations_FLP.c",
        "silk/float/residual_energy_FLP.c",
        "silk/float/warped_autocorrelation_FLP.c",
        "silk/float/wrappers_FLP.c",
        "silk/float/autocorrelation_FLP.c",
        "silk/float/burg_modified_FLP.c",
        "silk/float/bwexpander_FLP.c",
        "silk/float/energy_FLP.c",
        "silk/float/inner_product_FLP.c",
        "silk/float/k2a_FLP.c",
        "silk/float/LPC_inv_pred_gain_FLP.c",
        "silk/float/pitch_analysis_core_FLP.c",
        "silk/float/scale_copy_vector_FLP.c",
        "silk/float/scale_vector_FLP.c",
        "silk/float/schur_FLP.c",
        "silk/float/sort_FLP.c",
    ];

    opus.iter()
        .chain(celt.iter())
        .chain(silk.iter())
        .chain(silk_float.iter())
        .map(|p| opus_root.join(p))
        .collect()
}
