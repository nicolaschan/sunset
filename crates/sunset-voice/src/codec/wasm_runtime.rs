//! Minimal `wasm32-unknown-unknown` C-runtime shim consumed by
//! libopus.
//!
//! libopus is plain ISO C and expects `malloc` / `free` / a handful
//! of `<math.h>` functions to be defined as global C symbols at link
//! time. The Rust target `wasm32-unknown-unknown` provides only the
//! `mem*` symbols (memcpy / memset / memmove / memcmp via
//! compiler-builtins); everything else is on us.
//!
//! All exports are `#[unsafe(no_mangle)] pub extern "C"` so wasm-ld
//! resolves libopus's external references against them. They are
//! gated on `target_arch = "wasm32"` by the parent module's `cfg`,
//! so a host `cargo test -p sunset-voice` keeps using libc's real
//! definitions and these never collide.
//!
//! ## malloc / free
//!
//! Rust's standard allocator on wasm32 (dlmalloc) is not exposed
//! under the C symbol names libopus expects. We forward through
//! Rust's `std::alloc::{alloc, dealloc}` and store an 8-byte size
//! header in front of every block so `free` can reconstruct the
//! `Layout`. Allocations come back 16-byte aligned which is enough
//! for the `f32` / pointer arrays libopus puts on its scratch.
//!
//! ## libm
//!
//! Pulled in via the `libm` crate (pure-Rust software implementations).
//! libopus is built with `FLOAT_APPROX` (see `build.rs`) which keeps
//! the surface small — no `tan` / `atan` / `pow` / `exp` /
//! transcendentals beyond what's listed below.

#![allow(non_snake_case)]
#![allow(unsafe_code)]

use core::ffi::{c_int, c_void};

const HEADER: usize = 16;

/// Rust-side `malloc` shim. Allocates `HEADER + size` bytes with
/// 16-byte alignment, writes the requested size into the header, and
/// returns a pointer past the header that `free` can recover.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    if size == 0 {
        return core::ptr::null_mut();
    }
    let total = size + HEADER;
    let layout = core::alloc::Layout::from_size_align(total, 16).expect("malloc: bad layout");
    // SAFETY: `layout` has nonzero size; we just constructed it from
    // valid alignment and bounded size.
    let raw = unsafe { std::alloc::alloc(layout) };
    if raw.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: `raw` is the start of a freshly-allocated block of
    // `total` bytes; writing a usize at offset 0 is in bounds.
    unsafe { (raw as *mut usize).write(size) };
    // SAFETY: `HEADER <= total`, so this stays inside the allocation.
    unsafe { raw.add(HEADER) as *mut c_void }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` was returned by `malloc` above (we control all
    // call sites since libopus only frees blocks we allocated). The
    // header sits at `ptr - HEADER`.
    let raw = unsafe { (ptr as *mut u8).sub(HEADER) };
    let size = unsafe { (raw as *const usize).read() };
    let total = size + HEADER;
    let layout = core::alloc::Layout::from_size_align(total, 16).expect("free: bad layout");
    // SAFETY: same layout we passed to `alloc`.
    unsafe { std::alloc::dealloc(raw, layout) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn calloc(nmemb: usize, size: usize) -> *mut c_void {
    let total = match nmemb.checked_mul(size) {
        Some(n) => n,
        None => return core::ptr::null_mut(),
    };
    if total == 0 {
        return core::ptr::null_mut();
    }
    let raw = unsafe { malloc(total) };
    if raw.is_null() {
        return core::ptr::null_mut();
    }
    // SAFETY: we just allocated `total` bytes at `raw`.
    unsafe { core::ptr::write_bytes(raw as *mut u8, 0, total) };
    raw
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(ptr: *mut c_void, new_size: usize) -> *mut c_void {
    if ptr.is_null() {
        return unsafe { malloc(new_size) };
    }
    if new_size == 0 {
        unsafe { free(ptr) };
        return core::ptr::null_mut();
    }
    // SAFETY: header layout matches malloc's; this reads back the
    // size we wrote in.
    let old_size = unsafe { ((ptr as *mut u8).sub(HEADER) as *const usize).read() };
    let new_ptr = unsafe { malloc(new_size) };
    if new_ptr.is_null() {
        return core::ptr::null_mut();
    }
    let copy_len = old_size.min(new_size);
    // SAFETY: both blocks have at least `copy_len` valid bytes from
    // their start; they don't overlap because malloc returned a
    // fresh block.
    unsafe { core::ptr::copy_nonoverlapping(ptr as *const u8, new_ptr as *mut u8, copy_len) };
    unsafe { free(ptr) };
    new_ptr
}

#[unsafe(no_mangle)]
pub extern "C" fn abort() -> ! {
    core::panic!("libopus called abort() — unrecoverable");
}

// `<math.h>` — pure-Rust implementations from `libm`. libopus uses
// these from `<math.h>` directly (despite our FLOAT_APPROX defines)
// for a few operations; this list grows only when wasm-ld reports a
// new undefined symbol.
#[unsafe(no_mangle)]
pub extern "C" fn sin(x: f64) -> f64 {
    libm::sin(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn cos(x: f64) -> f64 {
    libm::cos(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn exp(x: f64) -> f64 {
    libm::exp(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn log(x: f64) -> f64 {
    libm::log(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn pow(x: f64, y: f64) -> f64 {
    libm::pow(x, y)
}
#[unsafe(no_mangle)]
pub extern "C" fn sqrt(x: f64) -> f64 {
    libm::sqrt(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn floor(x: f64) -> f64 {
    libm::floor(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn ceil(x: f64) -> f64 {
    libm::ceil(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn fabs(x: f64) -> f64 {
    libm::fabs(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn fabsf(x: f32) -> f32 {
    libm::fabsf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn sinf(x: f32) -> f32 {
    libm::sinf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn cosf(x: f32) -> f32 {
    libm::cosf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn expf(x: f32) -> f32 {
    libm::expf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn logf(x: f32) -> f32 {
    libm::logf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn sqrtf(x: f32) -> f32 {
    libm::sqrtf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn floorf(x: f32) -> f32 {
    libm::floorf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn ceilf(x: f32) -> f32 {
    libm::ceilf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn powf(x: f32, y: f32) -> f32 {
    libm::powf(x, y)
}
#[unsafe(no_mangle)]
pub extern "C" fn fmod(x: f64, y: f64) -> f64 {
    libm::fmod(x, y)
}
#[unsafe(no_mangle)]
pub extern "C" fn fmodf(x: f32, y: f32) -> f32 {
    libm::fmodf(x, y)
}
#[unsafe(no_mangle)]
pub extern "C" fn ldexp(x: f64, n: c_int) -> f64 {
    libm::ldexp(x, n)
}
#[unsafe(no_mangle)]
pub extern "C" fn frexp(x: f64, e: *mut c_int) -> f64 {
    let (m, exp) = libm::frexp(x);
    if !e.is_null() {
        // SAFETY: caller-supplied `e` must point to a valid c_int per
        // the C API contract.
        unsafe { e.write(exp) };
    }
    m
}
#[unsafe(no_mangle)]
pub extern "C" fn lrint(x: f64) -> i32 {
    libm::round(x) as i32
}
#[unsafe(no_mangle)]
pub extern "C" fn lrintf(x: f32) -> i32 {
    libm::roundf(x) as i32
}
#[unsafe(no_mangle)]
pub extern "C" fn round(x: f64) -> f64 {
    libm::round(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn roundf(x: f32) -> f32 {
    libm::roundf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn atan2(y: f64, x: f64) -> f64 {
    libm::atan2(y, x)
}
#[unsafe(no_mangle)]
pub extern "C" fn atan2f(y: f32, x: f32) -> f32 {
    libm::atan2f(y, x)
}
#[unsafe(no_mangle)]
pub extern "C" fn atan(x: f64) -> f64 {
    libm::atan(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn atanf(x: f32) -> f32 {
    libm::atanf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn tan(x: f64) -> f64 {
    libm::tan(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn tanf(x: f32) -> f32 {
    libm::tanf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn log10(x: f64) -> f64 {
    libm::log10(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn log10f(x: f32) -> f32 {
    libm::log10f(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn log2(x: f64) -> f64 {
    libm::log2(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn log2f(x: f32) -> f32 {
    libm::log2f(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn asin(x: f64) -> f64 {
    libm::asin(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn asinf(x: f32) -> f32 {
    libm::asinf(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn acos(x: f64) -> f64 {
    libm::acos(x)
}
#[unsafe(no_mangle)]
pub extern "C" fn acosf(x: f32) -> f32 {
    libm::acosf(x)
}

// libopus does its own logging via fprintf when assertions fire and
// the macro is defined. `silk_assert` and `OPUS_ASSERT` compile to
// no-ops unless `ENABLE_ASSERTIONS` is set, which we don't set in
// `build.rs`, so we should not actually hit `__assert_fail`. If we
// ever do, abort.
#[unsafe(no_mangle)]
pub extern "C" fn __assert_fail(
    _expr: *const u8,
    _file: *const u8,
    _line: c_int,
    _func: *const u8,
) -> ! {
    core::panic!("libopus assertion failed");
}
