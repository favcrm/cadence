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
use std::io::{self, Cursor, Write};

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
}

impl Format {
    pub fn from_mime(mime: &str) -> Option<Self> {
        match mime {
            "image/jpeg" => Some(Self::Jpeg),
            "image/png" => Some(Self::Png),
            "image/webp" => Some(Self::Webp),
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
    source_end: usize,
    clean_source: Option<Vec<u8>>,
    native_png16: bool,
    png_custom_color: bool,
    gray_profile: bool,
    unconvertible_icc: bool,
    unchanged: bool,
    icc: Option<Vec<u8>>,
}

fn dimensions(width: u32, height: u32, max_side: u32, max_pixels: u64) -> Result<()> {
    if width == 0 || height == 0 {
        return Err(ImageError::Invalid);
    }
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

fn decode_jpeg(bytes: &[u8], max_side: u32, max_pixels: u64, native: bool) -> Result<Decoded> {
    let layout = jpeg::inspect(bytes)?;
    if native && layout.end != bytes.len() {
        return Err(ImageError::Invalid);
    }
    dimensions(layout.width, layout.height, max_side, max_pixels)?;
    let scaled = cfg!(target_arch = "wasm32")
        && !native
        && u64::from(layout.width) * u64::from(layout.height) > OTHER_MAX_PIXELS;
    // The decoder's estimate assumes unscaled full-size planes even when DCT
    // scaling is enabled. The Worker module has an independent 96 MiB hard cap.
    let limits = libjpeg_turbo_rs::common::types::DecodeLimits {
        max_width: max_side as usize,
        max_height: max_side as usize,
        max_pixels,
        max_scans: 128,
        max_memory: Some(if scaled {
            192 * 1024 * 1024
        } else {
            DECODE_MEMORY
        }),
    };
    let mut decoder = libjpeg_turbo_rs::Decoder::new_with_limits(&bytes[..layout.end], limits)
        .map_err(jpeg_error)?
        .with_stop_on_warning(true)
        .with_output_format(libjpeg_turbo_rs::PixelFormat::Rgb);
    if scaled {
        decoder = decoder.with_scale(
            libjpeg_turbo_rs::ScalingFactor::try_new(1, 2).map_err(|_| ImageError::Invalid)?,
        );
    }
    let icc = decoder.icc_profile().map_err(jpeg_error)?;
    if layout.has_icc != icc.is_some() {
        return Err(ImageError::Invalid);
    }
    let (gray_profile, unconvertible_icc) = match icc.as_deref() {
        None => (false, false),
        Some(profile) => match (layout.components, containers::validate_icc(profile)?) {
            (1, moxcms::DataColorSpace::Gray) => (true, false),
            (3, moxcms::DataColorSpace::Rgb) => (false, false),
            (4, moxcms::DataColorSpace::Cmyk) => (false, true),
            _ => return Err(ImageError::Unsupported),
        },
    };
    let image = decoder.decode_image().map_err(jpeg_error)?;
    let width = if scaled {
        layout.width.div_ceil(2)
    } else {
        layout.width
    };
    let height = if scaled {
        layout.height.div_ceil(2)
    } else {
        layout.height
    };
    if image.width != width as usize || image.height != height as usize {
        return Err(ImageError::Invalid);
    }
    let mut mapping = Mapping::new(width, height);
    mapping.exif(layout.orientation)?;
    Ok(Decoded {
        pixels: image.into_vec(),
        width,
        channels: 3,
        mapping,
        source_end: layout.end,
        clean_source: None,
        native_png16: false,
        png_custom_color: false,
        gray_profile,
        unconvertible_icc,
        unchanged: layout.orientation == 1 && !scaled,
        icc,
    })
}

fn decode_image(
    bytes: &[u8],
    format: ImageFormat,
    width: u32,
    height: u32,
    max_side: u32,
) -> Result<(Vec<u8>, usize)> {
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

fn decode_indexed_png(bytes: &[u8], width: u32, height: u32) -> Result<(Vec<u8>, usize)> {
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: DECODE_MEMORY as usize,
        },
    );
    decoder.ignore_checksums(false);
    let mut reader = decoder.read_info().map_err(|_| ImageError::Invalid)?;
    let size = reader.output_buffer_size().ok_or(ImageError::TooLarge)?;
    if size > width as usize * height as usize {
        return Err(ImageError::Invalid);
    }
    let mut indexes = vec![0; size];
    let info = reader
        .next_frame(&mut indexes)
        .map_err(|_| ImageError::Invalid)?;
    if info.width != width || info.height != height || info.color_type != png::ColorType::Indexed {
        return Err(ImageError::Invalid);
    }
    let bits = match info.bit_depth {
        png::BitDepth::One => 1,
        png::BitDepth::Two => 2,
        png::BitDepth::Four => 4,
        png::BitDepth::Eight => 8,
        _ => return Err(ImageError::Invalid),
    };
    let palette = reader
        .info()
        .palette
        .as_deref()
        .ok_or(ImageError::Invalid)?;
    let alpha = reader.info().trns.as_deref().unwrap_or_default();
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height as usize {
        let row = &indexes[y * info.line_size..(y + 1) * info.line_size];
        for x in 0..width as usize {
            let bit = x * bits;
            let index = usize::from(
                (u16::from(row[bit / 8]) >> (8 - bits - bit % 8)) & ((1u16 << bits) - 1),
            );
            let color = palette
                .get(index * 3..index * 3 + 3)
                .ok_or(ImageError::Invalid)?;
            pixels.extend_from_slice(color);
            pixels.push(alpha.get(index).copied().unwrap_or(255));
        }
    }
    Ok((pixels, 4))
}

// Retain the PNG decoder's big-endian 16-bit samples for lossless native rotation.
fn decode_png16(bytes: &[u8], width: u32, height: u32) -> Result<(Vec<u8>, usize)> {
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(bytes),
        png::Limits {
            bytes: DECODE_MEMORY as usize,
        },
    );
    decoder.ignore_checksums(false);
    let mut reader = decoder.read_info().map_err(|_| ImageError::Invalid)?;
    let (color, depth) = reader.output_color_type();
    let channels = match color {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err(ImageError::Invalid),
    };
    let size = reader.output_buffer_size().ok_or(ImageError::TooLarge)?;
    if depth != png::BitDepth::Sixteen
        || size != width as usize * height as usize * channels * 2
        || size > DECODE_MEMORY as usize
    {
        return Err(ImageError::Invalid);
    }
    let mut pixels = vec![0; size];
    let info = reader
        .next_frame(&mut pixels)
        .map_err(|_| ImageError::Invalid)?;
    if info.width != width || info.height != height || info.bit_depth != png::BitDepth::Sixteen {
        return Err(ImageError::Invalid);
    }
    Ok((pixels, channels))
}

fn decode(bytes: &[u8], format: Format, native: bool) -> Result<Decoded> {
    let max_size = if native {
        NATIVE_INPUT_MAX
    } else {
        PHOTO_INPUT_MAX
    };
    if bytes.is_empty() {
        return Err(ImageError::Invalid);
    }
    if bytes.len() > max_size {
        return Err(ImageError::TooLarge);
    }
    let max_side = if native {
        NATIVE_MAX_SIDE
    } else {
        PHOTO_MAX_SIDE
    };
    let max_pixels = if native || matches!(format, Format::Jpeg) {
        PHOTO_MAX_PIXELS
    } else {
        OTHER_MAX_PIXELS
    };
    match format {
        Format::Jpeg => decode_jpeg(bytes, max_side, max_pixels, native),
        Format::Png => {
            let containers::Png {
                width,
                height,
                clean,
                icc,
                orientation,
                custom_color,
                gray_profile,
            } = containers::png(bytes)?;
            dimensions(width, height, max_side, max_pixels)?;
            containers::png_stream(&clean, width, height)?;
            let native_png16 = native && orientation != 1 && clean[24] == 16;
            let (pixels, channels) = if native_png16 {
                decode_png16(&clean, width, height)?
            } else if clean[25] == 3 {
                decode_indexed_png(&clean, width, height)?
            } else {
                decode_image(&clean, ImageFormat::Png, width, height, max_side)?
            };
            let mut mapping = Mapping::new(width, height);
            mapping.exif(orientation)?;
            Ok(Decoded {
                pixels,
                width,
                channels,
                mapping,
                source_end: bytes.len(),
                clean_source: match clean {
                    Cow::Borrowed(_) => None,
                    Cow::Owned(bytes) => Some(bytes),
                },
                native_png16,
                png_custom_color: custom_color,
                gray_profile,
                unconvertible_icc: false,
                unchanged: orientation == 1,
                icc,
            })
        }
        Format::Webp => {
            let webp = containers::webp(bytes)?;
            let (width, height) = (webp.width, webp.height);
            dimensions(width, height, max_side, max_pixels)?;
            let (pixels, channels) =
                decode_image(bytes, ImageFormat::WebP, width, height, max_side)?;
            let mut mapping = Mapping::new(width, height);
            mapping.exif(webp.orientation)?;
            Ok(Decoded {
                pixels,
                width,
                channels,
                mapping,
                source_end: bytes.len(),
                clean_source: None,
                native_png16: false,
                png_custom_color: false,
                gray_profile: false,
                unconvertible_icc: false,
                unchanged: webp.orientation == 1,
                icc: webp.icc.map(<[u8]>::to_vec),
            })
        }
    }
}

/// Validate an image under native prompt limits and return the bytes that may
/// be delivered. PNG metadata is removed after applying orientation; callers
/// must use the returned bytes rather than assuming the retained source is safe.
pub fn validate_native<'a>(bytes: &'a [u8], mime: &str) -> Result<Cow<'a, [u8]>> {
    let format = Format::from_mime(mime).ok_or(ImageError::Unsupported)?;
    let decoded = decode(bytes, format, true)?;
    if format == Format::Png && !decoded.unchanged {
        if decoded.native_png16 {
            return encode_native_png16(&decoded, bytes).map(Cow::Owned);
        }
        let mut width = decoded.mapping.width;
        let mut height = decoded.mapping.height;
        let source = decoded.clean_source.as_deref().unwrap_or(bytes);
        for _ in 0..8 {
            let mut pixels = geometry::downsample(
                &decoded.pixels,
                decoded.width,
                decoded.channels,
                &decoded.mapping,
                width,
                height,
            )?;
            let channels = png_samples(
                &mut pixels,
                width,
                height,
                decoded.channels,
                decoded.gray_profile,
            )?;
            let png = png_encode(
                &pixels,
                width,
                height,
                channels,
                decoded.gray_profile,
                source,
            )?;
            if png.len() <= NATIVE_INPUT_MAX {
                return Ok(Cow::Owned(png));
            }
            if width == 1 && height == 1 {
                break;
            }
            width = (width * 72 / 100).max(1);
            height = (height * 72 / 100).max(1);
        }
        return Err(ImageError::CannotFit);
    }
    Ok(match decoded.clean_source {
        Some(clean) => Cow::Owned(clean),
        None => Cow::Borrowed(bytes),
    })
}

#[derive(Default)]
struct NativePngOutput {
    bytes: Vec<u8>,
    exceeded: bool,
}

impl Write for NativePngOutput {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > NATIVE_INPUT_MAX.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("native PNG output exceeds its limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

// Rotate without reducing 16-bit precision or allocating a second full raster.
fn encode_native_png16(decoded: &Decoded, original: &[u8]) -> Result<Vec<u8>> {
    let color = match decoded.channels {
        1 => png::ColorType::Grayscale,
        2 => png::ColorType::GrayscaleAlpha,
        3 => png::ColorType::Rgb,
        4 => png::ColorType::Rgba,
        _ => return Err(ImageError::Invalid),
    };
    let mut output = NativePngOutput::default();
    let result = (|| -> Result<()> {
        let mut encoder =
            png::Encoder::new(&mut output, decoded.mapping.width, decoded.mapping.height);
        encoder.set_depth(png::BitDepth::Sixteen);
        encoder.set_color(color);
        let mut writer = encoder
            .write_header()
            .map_err(|_| ImageError::Unavailable)?;
        let source = decoded.clean_source.as_deref().unwrap_or(original);
        containers::write_png_color(&mut writer, source, true)?;
        {
            let mut stream = writer
                .stream_writer()
                .map_err(|_| ImageError::Unavailable)?;
            let stride = decoded.channels * 2;
            let mut row = vec![0; decoded.mapping.width as usize * stride];
            for y in 0..decoded.mapping.height {
                for x in 0..decoded.mapping.width {
                    let (sx, sy) = decoded.mapping.source(x, y);
                    let from = (sy as usize * decoded.width as usize + sx as usize) * stride;
                    let to = x as usize * stride;
                    row[to..to + stride].copy_from_slice(&decoded.pixels[from..from + stride]);
                }
                stream
                    .write_all(&row)
                    .map_err(|_| ImageError::Unavailable)?;
            }
            stream.finish().map_err(|_| ImageError::Unavailable)?;
        }
        writer.finish().map_err(|_| ImageError::Unavailable)
    })();
    if output.exceeded {
        return Err(ImageError::CannotFit);
    }
    result?;
    Ok(output.bytes)
}

fn png_samples(
    pixels: &mut Vec<u8>,
    width: u32,
    height: u32,
    channels: usize,
    gray: bool,
) -> Result<usize> {
    if !gray {
        return Ok(channels);
    }
    if channels != 4 {
        return Err(ImageError::Invalid);
    }
    for index in 0..(width as usize * height as usize) {
        pixels[index * 2] = pixels[index * 4];
        pixels[index * 2 + 1] = pixels[index * 4 + 3];
    }
    pixels.truncate(width as usize * height as usize * 2);
    Ok(2)
}

fn png_encode(
    pixels: &[u8],
    width: u32,
    height: u32,
    channels: usize,
    gray: bool,
    source: &[u8],
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_color(if gray {
        png::ColorType::GrayscaleAlpha
    } else if channels == 4 {
        png::ColorType::Rgba
    } else {
        png::ColorType::Rgb
    });
    let mut writer = encoder
        .write_header()
        .map_err(|_| ImageError::Unavailable)?;
    containers::write_png_color(&mut writer, source, false)?;
    writer
        .write_image_data(pixels)
        .map_err(|_| ImageError::Unavailable)?;
    writer.finish().map_err(|_| ImageError::Unavailable)?;
    Ok(out)
}

fn jpeg_encode(
    mut pixels: Vec<u8>,
    width: u32,
    height: u32,
    channels: usize,
    icc: Option<&[u8]>,
    gray_profile: bool,
) -> Result<Vec<u8>> {
    let color = if gray_profile {
        for index in 0..(width as usize * height as usize) {
            let src = index * channels;
            let alpha = if matches!(channels, 2 | 4) {
                u32::from(pixels[src + channels - 1])
            } else {
                255
            };
            pixels[index] = ((u32::from(pixels[src]) * alpha + 255 * (255 - alpha)) / 255) as u8;
        }
        pixels.truncate(width as usize * height as usize);
        ExtendedColorType::L8
    } else {
        if channels == 4 {
            for i in 0..(width as usize * height as usize) {
                let src = i * 4;
                let dst = i * 3;
                let alpha = u32::from(pixels[src + 3]);
                for c in 0..3 {
                    pixels[dst + c] =
                        ((u32::from(pixels[src + c]) * alpha + 255 * (255 - alpha)) / 255) as u8;
                }
            }
            pixels.truncate(width as usize * height as usize * 3);
        }
        ExtendedColorType::Rgb8
    };
    let mut out = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85);
    if let Some(profile) = icc {
        encoder
            .set_icc_profile(profile.to_vec())
            .map_err(|_| ImageError::Unsupported)?;
    }
    encoder
        .encode(&pixels, width, height, color)
        .map_err(|_| ImageError::Unavailable)?;
    Ok(out)
}

/// Prepare exactly one checked image for the model. The caller retains the
/// upload separately and must still enforce the whole-turn image budget.
pub fn prepare(bytes: &[u8], format: Format) -> Result<PreparedImage> {
    let decoded = decode(bytes, format, false)?;
    let source_len = decoded
        .clean_source
        .as_ref()
        .map_or(decoded.source_end, Vec::len);
    // Oversized PNGs still need pixel normalization after metadata is stripped.
    if decoded.unchanged
        && source_len <= MODEL_IMAGE_MAX
        && (format != Format::Png || bytes.len() <= MODEL_IMAGE_MAX)
        && u64::from(decoded.mapping.width) * u64::from(decoded.mapping.height) <= OTHER_MAX_PIXELS
    {
        let converted = decoded.clean_source.is_some() || decoded.source_end != bytes.len();
        return Ok(PreparedImage {
            bytes: decoded
                .clean_source
                .unwrap_or_else(|| bytes[..decoded.source_end].to_vec()),
            format,
            width: decoded.mapping.width,
            height: decoded.mapping.height,
            converted,
        });
    }
    if decoded.unconvertible_icc {
        return Err(ImageError::Unsupported);
    }
    let longest = decoded.mapping.width.max(decoded.mapping.height);
    let target = longest.min(OUTPUT_MAX_SIDE);
    let mut width = ((u64::from(decoded.mapping.width) * u64::from(target)
        + u64::from(longest) / 2)
        / u64::from(longest))
    .max(1) as u32;
    let mut height = ((u64::from(decoded.mapping.height) * u64::from(target)
        + u64::from(longest) / 2)
        / u64::from(longest))
    .max(1) as u32;
    for _ in 0..8 {
        let mut pixels = geometry::downsample(
            &decoded.pixels,
            decoded.width,
            decoded.channels,
            &decoded.mapping,
            width,
            height,
        )?;
        let mut channels = decoded.channels;
        if format == Format::Png {
            channels = png_samples(&mut pixels, width, height, channels, decoded.gray_profile)?;
            let source = decoded
                .clean_source
                .as_deref()
                .unwrap_or(&bytes[..decoded.source_end]);
            let png = png_encode(
                &pixels,
                width,
                height,
                channels,
                decoded.gray_profile,
                source,
            )?;
            if png.len() <= MODEL_IMAGE_MAX {
                return Ok(PreparedImage {
                    bytes: png,
                    format: Format::Png,
                    width,
                    height,
                    converted: true,
                });
            }
        }
        if !decoded.png_custom_color {
            let jpeg = jpeg_encode(
                pixels,
                width,
                height,
                channels,
                decoded.icc.as_deref(),
                decoded.gray_profile,
            )?;
            if jpeg.len() <= MODEL_IMAGE_MAX {
                return Ok(PreparedImage {
                    bytes: jpeg,
                    format: Format::Jpeg,
                    width,
                    height,
                    converted: true,
                });
            }
        }
        if width == 1 && height == 1 {
            break;
        }
        width = (width * 72 / 100).max(1);
        height = (height * 72 / 100).max(1);
    }
    Err(ImageError::CannotFit)
}
