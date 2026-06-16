//! Integration tests for [`sunset_image::preprocess`].
//!
//! Fixtures are generated in-process (via the `image` crate) so the
//! suite doesn't ship binary blobs. The exception would be HEIC, but
//! HEIC is currently rejected by `preprocess` (license decision
//! pending), so the only HEIC-shaped test lives in the unit suite and
//! asserts the `Error::HeicUnsupported` path.

use image::{ImageBuffer, ImageEncoder, Rgb, Rgba, codecs::jpeg::JpegEncoder};
use sunset_image::{Config, Error, preprocess};

fn synth_rgb(width: u32, height: u32) -> ImageBuffer<Rgb<u8>, Vec<u8>> {
    ImageBuffer::from_fn(width, height, |x, y| {
        Rgb([
            ((x * 255 / width.max(1)) & 0xff) as u8,
            ((y * 255 / height.max(1)) & 0xff) as u8,
            (((x + y) * 255 / (width + height).max(1)) & 0xff) as u8,
        ])
    })
}

fn synth_rgba(width: u32, height: u32) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    ImageBuffer::from_fn(width, height, |x, y| {
        Rgba([
            ((x * 255 / width.max(1)) & 0xff) as u8,
            ((y * 255 / height.max(1)) & 0xff) as u8,
            0x80,
            // Half-transparent in the top half, opaque in the bottom —
            // gives the JPEG flatten path something to chew on.
            if y < height / 2 { 0x80 } else { 0xff },
        ])
    })
}

fn encode_jpeg(buf: &ImageBuffer<Rgb<u8>, Vec<u8>>, quality: u8) -> Vec<u8> {
    let mut out = Vec::new();
    let enc = JpegEncoder::new_with_quality(&mut out, quality);
    enc.write_image(
        buf.as_raw(),
        buf.width(),
        buf.height(),
        image::ExtendedColorType::Rgb8,
    )
    .unwrap();
    out
}

fn encode_png(buf: &ImageBuffer<Rgba<u8>, Vec<u8>>) -> Vec<u8> {
    let mut out = Vec::new();
    let enc = image::codecs::png::PngEncoder::new(&mut out);
    enc.write_image(
        buf.as_raw(),
        buf.width(),
        buf.height(),
        image::ExtendedColorType::Rgba8,
    )
    .unwrap();
    out
}

fn encode_webp(buf: &ImageBuffer<Rgba<u8>, Vec<u8>>) -> Vec<u8> {
    // The `image` crate's WebP encoder writes a lossless still-image
    // VP8L bitstream, which is exactly what we want for a fixture: a
    // still WebP that our sniffer will route through the transcode
    // path (not the animated-pass-through path).
    let mut out = Vec::new();
    let enc = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
    enc.write_image(
        buf.as_raw(),
        buf.width(),
        buf.height(),
        image::ExtendedColorType::Rgba8,
    )
    .unwrap();
    out
}

fn decoded_dims(jpeg_bytes: &[u8]) -> (u32, u32) {
    let img = image::load_from_memory_with_format(jpeg_bytes, image::ImageFormat::Jpeg)
        .expect("output should be a valid JPEG");
    (img.width(), img.height())
}

#[test]
fn jpeg_input_is_renormalised_to_jpeg() {
    let src = encode_jpeg(&synth_rgb(640, 480), 90);
    let out = preprocess(&src, &Config::default()).unwrap();
    assert_eq!(out.mime_type, "image/jpeg");
    assert_eq!(decoded_dims(&out.bytes), (640, 480));
}

#[test]
fn png_input_transcodes_to_jpeg() {
    let src = encode_png(&synth_rgba(320, 240));
    let out = preprocess(&src, &Config::default()).unwrap();
    assert_eq!(out.mime_type, "image/jpeg");
    assert_eq!(decoded_dims(&out.bytes), (320, 240));
}

#[test]
fn still_webp_transcodes_to_jpeg() {
    let src = encode_webp(&synth_rgba(256, 128));
    let out = preprocess(&src, &Config::default()).unwrap();
    assert_eq!(out.mime_type, "image/jpeg");
    assert_eq!(decoded_dims(&out.bytes), (256, 128));
}

#[test]
fn gif_passes_through_unchanged() {
    // The `image` crate's GIF encoder writes a single-frame GIF, which
    // is what 99% of "GIF" attachments are in practice. The
    // pass-through rule is "any GIF stays a GIF", so a single-frame
    // GIF should also round-trip byte-for-byte.
    let mut src = Vec::new();
    {
        let mut enc = image::codecs::gif::GifEncoder::new(&mut src);
        // `image` 0.25's GIF encoder accepts RGBA frames.
        let frame = image::Frame::new(synth_rgba(48, 48));
        enc.encode_frame(frame).unwrap();
    }
    assert!(src.starts_with(b"GIF89a") || src.starts_with(b"GIF87a"));

    let out = preprocess(&src, &Config::default()).unwrap();
    assert_eq!(out.mime_type, "image/gif");
    assert_eq!(out.bytes, src, "GIF must pass through byte-for-byte");
}

#[test]
fn oversize_image_is_resized_within_max_edge() {
    // 4096 × 3072 source → 2048-cap should give 2048 × 1536 (4:3).
    let src = encode_png(&synth_rgba(4096, 3072));
    let cfg = Config {
        max_edge: 2048,
        jpeg_quality: 85,
    };
    let out = preprocess(&src, &cfg).unwrap();
    assert_eq!(out.mime_type, "image/jpeg");
    let (w, h) = decoded_dims(&out.bytes);
    assert!(
        w.max(h) == 2048,
        "longest edge should be exactly the cap, got {w}x{h}"
    );
    // Aspect ratio preserved within 1 px tolerance.
    let expected_h = 2048u32 * 3072 / 4096; // 1536
    assert!(
        h.abs_diff(expected_h) <= 1,
        "height should be ~{expected_h}, got {h}"
    );
}

#[test]
fn small_image_is_not_upscaled() {
    let src = encode_jpeg(&synth_rgb(64, 64), 85);
    let cfg = Config {
        max_edge: 2048,
        jpeg_quality: 85,
    };
    let out = preprocess(&src, &cfg).unwrap();
    assert_eq!(decoded_dims(&out.bytes), (64, 64));
}

#[test]
fn random_bytes_are_rejected() {
    let garbage = vec![0xab; 64];
    assert!(matches!(
        preprocess(&garbage, &Config::default()),
        Err(Error::UnrecognisedFormat)
    ));
}

#[test]
fn truncated_input_is_rejected() {
    assert!(matches!(
        preprocess(b"", &Config::default()),
        Err(Error::UnrecognisedFormat)
    ));
    assert!(matches!(
        preprocess(b"abc", &Config::default()),
        Err(Error::UnrecognisedFormat)
    ));
}

#[test]
fn truncated_png_decode_failure_is_surfaced() {
    // Valid PNG magic, but no chunks — the decoder must fail, and we
    // must report it as a decode error rather than a panic.
    let bad_png = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    assert!(matches!(
        preprocess(&bad_png, &Config::default()),
        Err(Error::Decode(_))
    ));
}
