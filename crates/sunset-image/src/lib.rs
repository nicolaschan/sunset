//! Client-side image preprocessing for sunset.
//!
//! See `docs/superpowers/specs/2026-05-13-image-preprocessing-design.md`.
//!
//! The crate exposes a single entry point, [`preprocess`], that turns
//! whatever bytes a file picker handed you into a normalised
//! [`Preprocessed`] payload ready to be base64'd into an
//! `ImageAttachment`. Format is sniffed from magic bytes; the input MIME
//! type is intentionally ignored (browsers' `File.type` is unreliable
//! for HEIC).
//!
//! # HEIC
//!
//! The sniffer recognises HEIC/HEIF inputs but returns
//! [`Error::HeicUnsupported`] for them. Adding decode requires picking
//! a HEIC decoder; the only pure-Rust + wasm option we surveyed
//! (imazen/heic) is AGPL-3.0 / commercial, which is incompatible with
//! the workspace's MIT licence. See the spec for the live decision.

use image::{
    DynamicImage, ImageEncoder, ImageFormat, codecs::jpeg::JpegEncoder, imageops::FilterType,
};

/// Preprocessing rules. Defaults aim at "good enough for chat":
///   - 2048 px max edge (longest side)
///   - JPEG quality 85
#[derive(Clone, Debug)]
pub struct Config {
    /// The longest edge of the output image, in pixels. Images already
    /// within the cap are left at their original dimensions; images
    /// over the cap are scaled-to-fit with the aspect ratio preserved.
    pub max_edge: u32,
    /// JPEG encoder quality (0–100). 85 is the long-standing
    /// chat-quality sweet spot.
    pub jpeg_quality: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_edge: 2048,
            jpeg_quality: 85,
        }
    }
}

/// The output of preprocessing: bytes ready to be base64'd into an
/// `ImageAttachment`, plus the MIME type the receiver should render
/// with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preprocessed {
    /// `"image/jpeg"` for re-encoded inputs; original MIME for
    /// pass-through formats (animated GIF, animated WebP).
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

/// Errors that can come out of [`preprocess`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("unrecognised image format (magic bytes did not match any supported codec)")]
    UnrecognisedFormat,
    #[error(
        "HEIC/HEIF inputs are not yet supported (no permissively-licensed pure-Rust decoder); please convert to JPEG before sending"
    )]
    HeicUnsupported,
    #[error("decode failed: {0}")]
    Decode(String),
    #[error("encode failed: {0}")]
    Encode(String),
    #[error("image is empty (zero dimensions after decode)")]
    Empty,
}

/// What the magic-byte sniffer decided the input is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sniffed {
    /// JPEG / PNG / still WebP — decode + resize + re-encode JPEG.
    StillTranscode(ImageFormat),
    /// Animated GIF — pass through unchanged.
    GifPassthrough,
    /// Animated WebP — pass through unchanged.
    AnimatedWebpPassthrough,
    /// HEIC/HEIF. Currently surfaced as an error pending the license
    /// decision; see the crate docs.
    Heic,
}

/// Preprocess raw image bytes into a wire-ready [`Preprocessed`].
pub fn preprocess(input: &[u8], cfg: &Config) -> Result<Preprocessed, Error> {
    match sniff(input)? {
        Sniffed::StillTranscode(format) => transcode_via_image_crate(input, format, cfg),
        Sniffed::Heic => Err(Error::HeicUnsupported),
        Sniffed::GifPassthrough => Ok(Preprocessed {
            mime_type: "image/gif".to_owned(),
            bytes: input.to_vec(),
        }),
        Sniffed::AnimatedWebpPassthrough => Ok(Preprocessed {
            mime_type: "image/webp".to_owned(),
            bytes: input.to_vec(),
        }),
    }
}

/// Detect the input format from magic bytes. Browser-supplied MIME is
/// ignored; for HEIC in particular it's often wrong (`""` or
/// `application/octet-stream`) and for JPEGs renamed to `.png` it's
/// often lying. Magic bytes are the only thing we trust.
fn sniff(input: &[u8]) -> Result<Sniffed, Error> {
    if input.len() < 12 {
        return Err(Error::UnrecognisedFormat);
    }
    // PNG
    if input.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Ok(Sniffed::StillTranscode(ImageFormat::Png));
    }
    // JPEG (SOI marker)
    if input.starts_with(&[0xff, 0xd8, 0xff]) {
        return Ok(Sniffed::StillTranscode(ImageFormat::Jpeg));
    }
    // GIF (87a or 89a)
    if input.starts_with(b"GIF87a") || input.starts_with(b"GIF89a") {
        return Ok(Sniffed::GifPassthrough);
    }
    // RIFF....WEBP container
    if &input[0..4] == b"RIFF" && &input[8..12] == b"WEBP" {
        // VP8X chunk carries an animation flag. If we see `VP8X` at
        // offset 12 with bit 1 set in the first byte of its payload,
        // treat the file as animated and pass it through verbatim.
        if input.len() >= 21 && &input[12..16] == b"VP8X" {
            let flags = input[20];
            if flags & 0x02 != 0 {
                return Ok(Sniffed::AnimatedWebpPassthrough);
            }
        }
        return Ok(Sniffed::StillTranscode(ImageFormat::WebP));
    }
    // HEIC/HEIF: ISO BMFF `ftyp` box at offset 4, brand at offset 8.
    if &input[4..8] == b"ftyp" {
        let brand = &input[8..12];
        if matches!(
            brand,
            b"heic"
                | b"heix"
                | b"heim"
                | b"heis"
                | b"hevc"
                | b"hevx"
                | b"hevm"
                | b"hevs"
                | b"mif1"
                | b"msf1"
                | b"avif"
                | b"avis"
        ) {
            return Ok(Sniffed::Heic);
        }
    }
    Err(Error::UnrecognisedFormat)
}

fn transcode_via_image_crate(
    input: &[u8],
    format: ImageFormat,
    cfg: &Config,
) -> Result<Preprocessed, Error> {
    let img = image::load_from_memory_with_format(input, format)
        .map_err(|e| Error::Decode(format!("{format:?}: {e}")))?;
    finalise_to_jpeg(img, cfg)
}

fn finalise_to_jpeg(img: DynamicImage, cfg: &Config) -> Result<Preprocessed, Error> {
    if img.width() == 0 || img.height() == 0 {
        return Err(Error::Empty);
    }
    let resized = if img.width() > cfg.max_edge || img.height() > cfg.max_edge {
        img.resize(cfg.max_edge, cfg.max_edge, FilterType::Lanczos3)
    } else {
        img
    };
    // JPEG can't carry an alpha channel — flatten RGBA inputs (PNG,
    // WebP-with-alpha) to RGB. We composite over black; the
    // alternative is white. Black matches dark-themed chat UIs more
    // often than white does and is the choice most chat clients
    // (Signal, Telegram) make.
    let rgb = resized.to_rgb8();
    let mut out = Vec::new();
    let encoder = JpegEncoder::new_with_quality(&mut out, cfg.jpeg_quality);
    encoder
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| Error::Encode(format!("jpeg: {e}")))?;
    Ok(Preprocessed {
        mime_type: "image/jpeg".to_owned(),
        bytes: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_rejects_short_input() {
        assert!(matches!(sniff(b""), Err(Error::UnrecognisedFormat)));
        assert!(matches!(sniff(b"abc"), Err(Error::UnrecognisedFormat)));
    }

    #[test]
    fn sniff_png() {
        let bytes = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
        assert_eq!(
            sniff(&bytes).unwrap(),
            Sniffed::StillTranscode(ImageFormat::Png)
        );
    }

    #[test]
    fn sniff_jpeg() {
        let bytes = [0xff, 0xd8, 0xff, 0xe0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(
            sniff(&bytes).unwrap(),
            Sniffed::StillTranscode(ImageFormat::Jpeg)
        );
    }

    #[test]
    fn sniff_gif_passthrough() {
        let mut bytes = Vec::from(*b"GIF89a");
        bytes.extend_from_slice(&[0; 16]);
        assert_eq!(sniff(&bytes).unwrap(), Sniffed::GifPassthrough);
    }

    #[test]
    fn sniff_still_webp_transcodes() {
        // RIFF <4 bytes len> WEBPVP8 (still)
        let mut bytes = Vec::from(*b"RIFF\0\0\0\0WEBPVP8 ");
        bytes.extend_from_slice(&[0; 16]);
        assert_eq!(
            sniff(&bytes).unwrap(),
            Sniffed::StillTranscode(ImageFormat::WebP)
        );
    }

    #[test]
    fn sniff_animated_webp_passthrough() {
        // VP8X chunk with animation flag (bit 1) set.
        let mut bytes = Vec::from(*b"RIFF\0\0\0\0WEBPVP8X");
        bytes.extend_from_slice(&[0, 0, 0, 0]); // chunk size
        bytes.push(0x02); // flags: animation
        bytes.extend_from_slice(&[0; 16]);
        assert_eq!(sniff(&bytes).unwrap(), Sniffed::AnimatedWebpPassthrough);
    }

    #[test]
    fn sniff_heic_brands() {
        for brand in [b"heic", b"heix", b"mif1", b"hevc"] {
            let mut bytes = vec![0u8; 4];
            bytes.extend_from_slice(b"ftyp");
            bytes.extend_from_slice(brand);
            assert_eq!(sniff(&bytes).unwrap(), Sniffed::Heic, "brand: {brand:?}");
        }
    }

    #[test]
    fn sniff_rejects_unknown_ftyp_brand() {
        let mut bytes = vec![0u8; 4];
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"isom"); // generic ISOBMFF brand, not HEIF
        assert!(matches!(sniff(&bytes), Err(Error::UnrecognisedFormat)));
    }

    #[test]
    fn heic_inputs_surface_distinct_error() {
        let mut bytes = vec![0u8; 4];
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(b"heic");
        bytes.extend_from_slice(&[0; 8]);
        assert!(matches!(
            preprocess(&bytes, &Config::default()),
            Err(Error::HeicUnsupported)
        ));
    }
}
