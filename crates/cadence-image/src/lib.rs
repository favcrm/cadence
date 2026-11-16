//! Shared, bounded raster inspection and model-image preparation.
//!
//! The original attachment remains in its caller's custody. This crate only
//! returns a checked model-visible image; it does not open paths or authorize
//! an attachment. The Worker build additionally caps its entire Wasm memory.

mod containers;
mod geometry;
mod jpeg;
#[cfg(target_arch = "wasm32")]
mod wasm;

use std::borrow::Cow;
use std::io::Cursor;

use geometry::Mapping;
use image::{DynamicImage, ExtendedColorType, ImageEncoder, ImageFormat, ImageReader};

pub const PHOTO_INPUT_MAX: usize = 10 * 1024 * 1024;
pub const NATIVE_INPUT_MAX: usize = 4 * 1024 * 1024;
const MODEL_IMAGE_MAX: usize = 145_920; // 190 KiB after base64 encoding
const PHOTO_MAX_PIXELS: u64 = 16_777_216;
const OTHER_MAX_PIXELS: u64 = 4_000_000;
const PHOTO_MAX_SIDE: u32 = 8192;
const NATIVE_MAX_SIDE: u32 = 4096;
const DECODE_MEMORY: u64 = 80 * 1024 * 1024;
const OUTPUT_MAX_SIDE: u32 = 1600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Format {
    Jpeg = 1,
    Png = 2,
    Webp = 3,
    Heic = 4,
}

impl Format {
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/jpeg" => Some(Self::Jpeg),
            "image/png" => Some(Self::Png),
            "image/webp" => Some(Self::Webp),
            "image/heic" | "image/heif" => Some(Self::Heic),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum ImageError {
    Invalid = 1,
    TooLarge = 2,
    Unsupported = 3,
    CannotFit = 4,
    Unavailable = 5,
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "the image is malformed, incomplete or ambiguous",
            Self::TooLarge => "the image exceeds the supported size or resource bound",
            Self::Unsupported => "the image format or encoding is not supported",
            Self::CannotFit => "the image could not be reduced under the model image budget",
            Self::Unavailable => "the image could not be processed",
        })
    }
}

impl std::error::Error for ImageError {}
pub type Result<T, E = ImageError> = std::result::Result<T, E>;

pub struct PreparedImage {
    pub bytes: Vec<u8>,
    pub format: Format,
    pub width: u32,
    pub height: u32,
    pub converted: bool,
}

struct Decoded {
    pixels: Vec<u8>,
    width: u32,
    channels: usize,
    mapping: Mapping,
    unchanged: bool,
    icc: Option<Vec<u8>>,
}

fn dimensions(width: u32, height: u32, max_side: u32, max_pixels: u64) -> Result<()> {
    if width == 0 || height == 0 { return Err(ImageError::Invalid); }
    if width > max_side || height > max_side || u64::from(width) * u64::from(height) > max_pixels {
        return Err(ImageError::TooLarge);
    }
    Ok(())
}

fn jpeg_error(error: libjpeg_turbo_rs::JpegError) -> ImageError {
    if matches!(error, libjpeg_turbo_rs::JpegError::LimitExceeded { .. }) {
        ImageError::TooLarge
    } else {
        ImageError::Invalid
    }
}

fn decode_jpeg(bytes: &[u8], max_side: u32, max_pixels: u64) -> Result<Decoded> {
    let layout = jpeg::inspect(bytes)?;
    dimensions(layout.width, layout.height, max_side, max_pixels)?;
    let limits = libjpeg_turbo_rs::common::types::DecodeLimits {
        max_width: max_side as usize,
        max_height: max_side as usize,
        max_pixels,
        max_scans: 128,
        max_memory: Some(DECODE_MEMORY),
    };
    let decoder = libjpeg_turbo_rs::Decoder::new_with_limits(&bytes[..layout.end], limits)
        .map_err(jpeg_error)?
        .with_stop_on_warning(true)
        .with_output_format(libjpeg_turbo_rs::PixelFormat::Rgb);
    let icc = decoder.icc_profile().map_err(jpeg_error)?;
    let image = decoder.decode_image().map_err(jpeg_error)?;
    if image.width != layout.width as usize || image.height != layout.height as usize {
        return Err(ImageError::Invalid);
    }
    let mut mapping = Mapping::new(layout.width, layout.height);
    mapping.exif(layout.orientation)?;
    Ok(Decoded {
        pixels: image.into_vec(), width: layout.width, channels: 3, mapping,
        unchanged: layout.end == bytes.len() && layout.orientation == 1,
        icc,
    })
}

fn decode_image(bytes: &[u8], format: ImageFormat, width: u32, height: u32, max_side: u32) -> Result<(Vec<u8>, usize)> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(max_side);
    limits.max_image_height = Some(max_side);
    limits.max_alloc = Some(DECODE_MEMORY);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| ImageError::Invalid)?;
    if decoded.width() != width || decoded.height() != height {
        return Err(ImageError::Invalid);
    }
    match decoded {
        DynamicImage::ImageRgb8(image) => Ok((image.into_raw(), 3)),
        DynamicImage::ImageRgba8(image) => Ok((image.into_raw(), 4)),
        other => Ok((other.into_rgba8().into_raw(), 4)),
    }
}

fn decode_heic(bytes: &[u8], max_side: u32, max_pixels: u64) -> Result<Decoded> {
    let context = heic_rs::context::Context::open(bytes).map_err(|_| ImageError::Invalid)?;
    let id = context.meta.primary;
    let props = context.props(id).map_err(|_| ImageError::Invalid)?;
    let (width, height) = context.coded_size(id, &props).map_err(|_| ImageError::Invalid)?;
    dimensions(width, height, max_side, max_pixels)?;
    let info = heic_rs::probe(bytes).map_err(|_| ImageError::Invalid)?;
    dimensions(info.width, info.height, max_side, max_pixels)?;
    let mut mapping = Mapping::new(width, height);
    for transform in &props.transforms { mapping.push(*transform)?; }
    if props.transforms.is_empty() {
        if let Some(tiff) = context.exif(id).map_err(|_| ImageError::Invalid)? {
            mapping.exif(jpeg::exif_orientation(tiff)?.unwrap_or(1))?;
        }
    }
    let channels = if info.has_alpha { 4 } else { 3 };
    let layout = if channels == 4 { heic_rs::PixelLayout::Rgba8 } else { heic_rs::PixelLayout::Rgb8 };
    let options = heic_rs::DecodeOptions::default()
        .with_layout(layout)
        .with_max_pixels(Some(max_pixels))
        .with_transforms(false)
        .with_strict(true);
    let image = heic_rs::decode(bytes, &options).map_err(|error| match error {
        heic_rs::Error::Unsupported(_) => ImageError::Unsupported,
        heic_rs::Error::PixelLimit { .. } => ImageError::TooLarge,
        _ => ImageError::Invalid,
    })?;
    if image.width != width || image.height != height { return Err(ImageError::Invalid); }
    let icc = props.icc.map(|profile| profile.to_vec());
    Ok(Decoded { pixels: image.data, width, channels, mapping, unchanged: false, icc })
}

fn decode(bytes: &[u8], format: Format, native: bool) -> Result<Decoded> {
    let max_size = if native { NATIVE_INPUT_MAX } else { PHOTO_INPUT_MAX };
    if bytes.is_empty() { return Err(ImageError::Invalid); }
    if bytes.len() > max_size { return Err(ImageError::TooLarge); }
    let max_side = if native { NATIVE_MAX_SIDE } else { PHOTO_MAX_SIDE };
    let max_pixels = if native || matches!(format, Format::Jpeg | Format::Heic) {
        PHOTO_MAX_PIXELS
    } else {
        OTHER_MAX_PIXELS
    };
    match format {
        Format::Jpeg => decode_jpeg(bytes, max_side, max_pixels),
        Format::Heic if !native => decode_heic(bytes, max_side, max_pixels),
        Format::Heic => Err(ImageError::Unsupported),
        Format::Png => {
            let (width, height, clean) = containers::png(bytes)?;
            dimensions(width, height, max_side, max_pixels)?;
            let unchanged = matches!(clean, Cow::Borrowed(_));
            let (pixels, channels) = decode_image(&clean, ImageFormat::Png, width, height, max_side)?;
            Ok(Decoded { pixels, width, channels, mapping: Mapping::new(width, height), unchanged, icc: None })
        }
        Format::Webp => {
            let (width, height) = containers::webp(bytes)?;
            dimensions(width, height, max_side, max_pixels)?;
            let (pixels, channels) = decode_image(bytes, ImageFormat::WebP, width, height, max_side)?;
            Ok(Decoded { pixels, width, channels, mapping: Mapping::new(width, height), unchanged: true, icc: None })
        }
    }
}

/// Validate a native retained image under native prompt limits. A suffix is
/// not delivered to a native model as if it were part of a checked JPEG.
pub fn validate_native(bytes: &[u8], mime: &str) -> Result<()> {
    let format = Format::from_mime(mime).ok_or(ImageError::Unsupported)?;
    if format == Format::Jpeg && jpeg::inspect(bytes)?.end != bytes.len() {
        return Err(ImageError::Invalid);
    }
    decode(bytes, format, true).map(|_| ())
}

fn png_encode(pixels: &[u8], width: u32, height: u32, channels: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let color = if channels == 4 { ExtendedColorType::Rgba8 } else { ExtendedColorType::Rgb8 };
    image::codecs::png::PngEncoder::new(&mut out)
        .write_image(pixels, width, height, color).map_err(|_| ImageError::Unavailable)?;
    Ok(out)
}

fn jpeg_encode(mut pixels: Vec<u8>, width: u32, height: u32, channels: usize, icc: Option<&[u8]>) -> Result<Vec<u8>> {
    if channels == 4 {
        for i in 0..(width as usize * height as usize) {
            let src = i * 4;
            let dst = i * 3;
            let alpha = u32::from(pixels[src + 3]);
            for c in 0..3 {
                pixels[dst + c] = ((u32::from(pixels[src + c]) * alpha + 255 * (255 - alpha)) / 255) as u8;
            }
        }
        pixels.truncate(width as usize * height as usize * 3);
    }
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    if let Some(profile) = icc {
        encoder.set_icc_profile(profile.to_vec()).map_err(|_| ImageError::Unsupported)?;
    }
    encoder.encode(&pixels, width, height, ExtendedColorType::Rgb8).map_err(|_| ImageError::Unavailable)?;
    Ok(out)
}

/// Prepare exactly one checked image for the model. The caller retains the
/// upload separately and must still enforce the whole-turn image budget.
pub fn prepare(bytes: &[u8], format: Format) -> Result<PreparedImage> {
    let decoded = decode(bytes, format, false)?;
    if decoded.unchanged && bytes.len() <= MODEL_IMAGE_MAX {
        return Ok(PreparedImage { bytes: bytes.to_vec(), format, width: decoded.mapping.width, height: decoded.mapping.height, converted: false });
    }
    let longest = decoded.mapping.width.max(decoded.mapping.height);
    let target = longest.min(OUTPUT_MAX_SIDE);
    let mut width = ((u64::from(decoded.mapping.width) * u64::from(target) + u64::from(longest) / 2) / u64::from(longest)).max(1) as u32;
    let mut height = ((u64::from(decoded.mapping.height) * u64::from(target) + u64::from(longest) / 2) / u64::from(longest)).max(1) as u32;
    for _ in 0..8 {
        let pixels = geometry::downsample(&decoded.pixels, decoded.width, decoded.channels, &decoded.mapping, width, height)?;
        if format == Format::Png {
            let png = png_encode(&pixels, width, height, decoded.channels)?;
            if png.len() <= MODEL_IMAGE_MAX {
                return Ok(PreparedImage { bytes: png, format: Format::Png, width, height, converted: true });
            }
        }
        let jpeg = jpeg_encode(pixels, width, height, decoded.channels, decoded.icc.as_deref())?;
        if jpeg.len() <= MODEL_IMAGE_MAX {
            return Ok(PreparedImage { bytes: jpeg, format: Format::Jpeg, width, height, converted: true });
        }
        if width == 1 && height == 1 { break; }
        width = (width * 72 / 100).max(1);
        height = (height * 72 / 100).max(1);
    }
    Err(ImageError::CannotFit)
}
