//! Build script for sunset-voice.
//!
//! Compiles the vendored libopus C source using cc::Build. Works on the host
//! and cross-compiles to wasm32-unknown-unknown (via clang, which cc selects
//! automatically for the wasm32 target).
//!
//! Fallback 2 from the C2a plan: audiopus_sys uses CMake to build libopus,
//! and CMake cannot target wasm32. This build.rs replaces that mechanism with
//! a direct cc::Build compilation of the vendored C source.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let libopus_dir = manifest_dir.join("vendor").join("libopus");

    // Plain C sources — no SSE, no ARM NEON, no assembly.
    // Taken from celt_sources.mk, silk_sources.mk (SILK_SOURCES + SILK_SOURCES_FIXED),
    // and opus_sources.mk (OPUS_SOURCES + OPUS_SOURCES_FLOAT).

    let celt_sources = [
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

    let silk_sources = [
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

    // Float-point SILK sources (SILK_SOURCES_FLOAT from silk_sources.mk).
    // Use float mode, not fixed-point — matches our config.h which leaves
    // FIXED_POINT undefined.
    let silk_float_sources = [
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

    let opus_sources = [
        "src/opus.c",
        "src/opus_decoder.c",
        "src/opus_encoder.c",
        "src/opus_multistream.c",
        "src/opus_multistream_encoder.c",
        "src/opus_multistream_decoder.c",
        "src/repacketizer.c",
        "src/opus_projection_encoder.c",
        "src/opus_projection_decoder.c",
        "src/mapping_matrix.c",
        // OPUS_SOURCES_FLOAT (floating-point mode):
        "src/analysis.c",
        "src/mlp.c",
        "src/mlp_data.c",
    ];

    let all_sources: Vec<PathBuf> = celt_sources
        .iter()
        .chain(silk_sources.iter())
        .chain(silk_float_sources.iter())
        .chain(opus_sources.iter())
        .map(|s| libopus_dir.join(s))
        .collect();

    let mut build = cc::Build::new();

    build
        // Use the vendored config.h we ship alongside the C source.
        .define("HAVE_CONFIG_H", None)
        // Include dirs: the libopus root (for config.h + include/),
        // and the sub-dirs that files #include relative paths from.
        .include(&libopus_dir)
        .include(libopus_dir.join("include"))
        .include(libopus_dir.join("celt"))
        .include(libopus_dir.join("silk"))
        .include(libopus_dir.join("silk").join("float"))
        // Suppress warnings from vendored C code so CI stays clean.
        .warnings(false)
        // Silence a noisy diagnostic from libopus's math helpers.
        .flag_if_supported("-Wno-strict-prototypes")
        .files(all_sources.iter());

    // Suppress cc::Build's automatic `cargo:rustc-link-lib=static=opus`
    // emission so we can emit our own directive with the `+whole-archive`
    // modifier (rustc rejects overriding modifiers on an existing
    // link-lib directive).
    build.cargo_metadata(false);
    build.compile("opus");

    // Force-include all symbols from libopus.a into the final binary.
    //
    // On wasm32-unknown-unknown, wasm-ld treats static archives lazily —
    // it pulls only symbols that are referenced by previously-seen
    // objects in link order. Rust FFI declarations alone don't get
    // wasm-ld to pull symbols out of the archive, so the unresolved
    // symbols become `env` imports in the final wasm bundle and the
    // page fails to load with "bare specifier 'env' not remapped".
    //
    // The `+whole-archive` modifier tells the linker to include every
    // symbol from libopus.a regardless of whether it's referenced.
    // For native targets this would bloat the binary; for wasm-ld it
    // has no overhead because dead-code elimination happens after link.
    let out_dir = env::var("OUT_DIR").unwrap();
    println!("cargo:rustc-link-search=native={out_dir}");
    println!("cargo:rustc-link-lib=static:+whole-archive=opus");

    // Rerun if the vendored source changes.
    println!("cargo:rerun-if-changed=vendor/libopus");
    println!("cargo:rerun-if-changed=build.rs");
}
