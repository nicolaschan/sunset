//! Minimal hand-written extern "C" surface for libopus 1.5.2.
//!
//! Only the float-API encoder/decoder calls plus the CTL macros we
//! actually use are declared. Adding a new CTL here is one line —
//! there's no need for a generated bindgen wall.

#![allow(non_camel_case_types)]
#![allow(unsafe_code)]

#[repr(C)]
pub struct OpusEncoder {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct OpusDecoder {
    _opaque: [u8; 0],
}

pub const OPUS_OK: i32 = 0;
pub const OPUS_APPLICATION_VOIP: i32 = 2048;

// CTL request codes from `vendor/libopus/include/opus_defines.h`.
pub const OPUS_SET_BITRATE_REQUEST: i32 = 4002;
pub const OPUS_SET_INBAND_FEC_REQUEST: i32 = 4012;
pub const OPUS_SET_PACKET_LOSS_PERC_REQUEST: i32 = 4014;

unsafe extern "C" {
    pub fn opus_encoder_create(
        fs: i32,
        channels: i32,
        application: i32,
        error: *mut i32,
    ) -> *mut OpusEncoder;

    pub fn opus_encoder_destroy(st: *mut OpusEncoder);

    pub fn opus_encode_float(
        st: *mut OpusEncoder,
        pcm: *const f32,
        frame_size: i32,
        data: *mut u8,
        max_data_bytes: i32,
    ) -> i32;

    // libopus exposes encoder configuration via a C varargs entry
    // point (`int opus_encoder_ctl(OpusEncoder *st, int request, ...)`)
    // — its handler reads the value out of `va_list` according to
    // the request type. On wasm32 the variadic ABI differs from the
    // fixed-args ABI (variadic args live in a heap buffer pointed to
    // by a hidden pointer; fixed args go on the wasm value stack), so
    // declaring this with concrete `value: i32` produces a call that
    // libopus's `va_arg` reads as garbage, surfacing as `OPUS_BAD_ARG`.
    // Declaring the function variadic in Rust matches what clang
    // emits on the C side.
    //
    // **Restriction**: only `opus_int32`-valued SET requests are
    // safe to call through this signature. CTLs that expect a
    // pointer (`OPUS_GET_BITRATE_REQUEST` and friends) read
    // `*opus_int32` from `va_list`; calling them through a `...`
    // signature with a Rust `i32` would corrupt memory. The
    // `OpusFrameEncoder::new` call sites are the only callers and
    // hard-code int-only requests; new CTL usage that needs a
    // pointer must add a separately-typed declaration.
    pub fn opus_encoder_ctl(st: *mut OpusEncoder, request: i32, ...) -> i32;

    pub fn opus_decoder_create(fs: i32, channels: i32, error: *mut i32) -> *mut OpusDecoder;

    pub fn opus_decoder_destroy(st: *mut OpusDecoder);

    pub fn opus_decode_float(
        st: *mut OpusDecoder,
        data: *const u8,
        len: i32,
        pcm: *mut f32,
        frame_size: i32,
        decode_fec: i32,
    ) -> i32;
}
