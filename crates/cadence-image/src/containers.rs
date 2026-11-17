use std::borrow::Cow;

use crate::{ImageError, Result};

fn be32(data: &[u8], at: usize) -> Result<u32> {
    let b = data
        .get(at..at.checked_add(4).ok_or(ImageError::Invalid)?)
        .ok_or(ImageError::Invalid)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn le32(data: &[u8], at: usize) -> Result<u32> {
    let b = data
        .get(at..at.checked_add(4).ok_or(ImageError::Invalid)?)
        .ok_or(ImageError::Invalid)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
pub(crate) const ICC_MAX: usize = 256 * 1024;
const SRGB_CHROMATICITIES: [u32; 8] = [31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000];

fn png_icc(data: &[u8], grayscale: bool) -> Result<Vec<u8>> {
    let name_end = data
        .iter()
        .take(80)
        .position(|byte| *byte == 0)
        .ok_or(ImageError::Invalid)?;
    let name = &data[..name_end];
    if name.is_empty()
        || name.len() > 79
        || name.first() == Some(&b' ')
        || name.last() == Some(&b' ')
        || name.windows(2).any(|pair| pair == b"  ")
        || name
            .iter()
            .any(|byte| *byte < 32 || (127..=160).contains(byte))
        || data.get(name_end + 1) != Some(&0)
    {
        return Err(ImageError::Invalid);
    }
    let compressed = data.get(name_end + 2..).ok_or(ImageError::Invalid)?;
    let mut decoder = flate2::Decompress::new(true);
    let mut profile = Vec::new();
    let mut block = [0u8; 8192];
    loop {
        let consumed = decoder.total_in() as usize;
        let before = decoder.total_out();
        let status = decoder
            .decompress(
                &compressed[consumed..],
                &mut block,
                flate2::FlushDecompress::None,
            )
            .map_err(|_| ImageError::Invalid)?;
        let produced = (decoder.total_out() - before) as usize;
        if profile.len() + produced > ICC_MAX {
            return Err(ImageError::TooLarge);
        }
        profile.extend_from_slice(&block[..produced]);
        if status == flate2::Status::StreamEnd {
            if decoder.total_in() as usize != compressed.len() {
                return Err(ImageError::Invalid);
            }
            break;
        }
        if consumed == decoder.total_in() as usize && produced == 0 {
            return Err(ImageError::Invalid);
        }
    }
    let expected = if grayscale {
        moxcms::DataColorSpace::Gray
    } else {
        moxcms::DataColorSpace::Rgb
    };
    if validate_icc(&profile)? != expected {
        return Err(ImageError::Invalid);
    }
    Ok(profile)
}

/// Validate an ICC profile before forwarding it in any raster format.
pub(crate) fn validate_icc(profile: &[u8]) -> Result<moxcms::DataColorSpace> {
    if profile.len() > ICC_MAX {
        return Err(ImageError::TooLarge);
    }
    if profile.len() < 132
        || be32(profile, 0)? as usize != profile.len()
        || &profile[36..40] != b"acsp"
    {
        return Err(ImageError::Invalid);
    }
    let count = be32(profile, 128)? as usize;
    let table_end = 132usize
        .checked_add(count.checked_mul(12).ok_or(ImageError::Invalid)?)
        .ok_or(ImageError::Invalid)?;
    if count > 256 || table_end > profile.len() {
        return Err(ImageError::Invalid);
    }
    let mut entries = [(0u32, 0usize, 0usize); 256];
    for index in 0..count {
        let at = 132 + index * 12;
        let signature = be32(profile, at)?;
        let offset = be32(profile, at + 4)? as usize;
        let length = be32(profile, at + 8)? as usize;
        let end = offset.checked_add(length).ok_or(ImageError::Invalid)?;
        if offset < table_end || !offset.is_multiple_of(4) || length < 8 || end > profile.len() {
            return Err(ImageError::Invalid);
        }
        if entries[..index]
            .iter()
            .any(|&(other_signature, other_offset, other_length)| {
                signature == other_signature
                    || (offset < other_offset + other_length
                        && other_offset < end
                        && (offset != other_offset || length != other_length))
            })
        {
            return Err(ImageError::Invalid);
        }
        entries[index] = (signature, offset, length);
    }
    let parsed = moxcms::ColorProfile::new_from_slice(profile).map_err(|_| ImageError::Invalid)?;
    Ok(parsed.color_space)
}

pub(crate) struct Png<'a> {
    pub width: u32,
    pub height: u32,
    pub clean: Cow<'a, [u8]>,
    pub icc: Option<Vec<u8>>,
    pub orientation: u8,
    pub custom_color: bool,
    pub gray_profile: bool,
}

/// Write validated color declarations, and optionally transparency, before image data.
pub(crate) fn write_png_color<W: std::io::Write>(
    writer: &mut png::Writer<W>,
    data: &[u8],
    keep_transparency: bool,
) -> Result<()> {
    let mut at = 8usize;
    while at < data.len() {
        let length = be32(data, at)? as usize;
        let end = at + 12 + length;
        let kind = &data[at + 4..at + 8];
        if matches!(kind, b"sRGB" | b"gAMA" | b"cHRM" | b"iCCP")
            || (keep_transparency && kind == b"tRNS")
        {
            writer
                .write_chunk(
                    png::chunk::ChunkType([kind[0], kind[1], kind[2], kind[3]]),
                    &data[at + 8..end - 4],
                )
                .map_err(|_| ImageError::Unavailable)?;
        }
        if kind == b"IDAT" {
            break;
        }
        at = end;
    }
    Ok(())
}

/// Validate PNG chunks, keeping bounded color information and removing other
/// ancillary data that the raster decoder does not need.
pub(crate) fn png(data: &[u8]) -> Result<Png<'_>> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !data.starts_with(SIGNATURE) {
        return Err(ImageError::Invalid);
    }
    let mut at = 8usize;
    let mut width = 0;
    let mut height = 0;
    let mut header = false;
    let mut ended = false;
    let mut saw_data = false;
    let mut data_ended = false;
    let mut palette: Option<usize> = None;
    let mut transparency: Option<usize> = None;
    let mut bit_depth = 0;
    let mut color_type = 0;
    let mut gamma = None;
    let mut chromaticities = None;
    let mut srgb = false;
    let mut icc = None;
    let mut orientation = None;
    let mut clean: Option<Vec<u8>> = None;
    while at.checked_add(12).is_some_and(|n| n <= data.len()) {
        let length = be32(data, at)? as usize;
        let end = at
            .checked_add(12)
            .and_then(|n| n.checked_add(length))
            .ok_or(ImageError::Invalid)?;
        if end > data.len() {
            return Err(ImageError::Invalid);
        }
        let kind = &data[at + 4..at + 8];
        if !kind.iter().all(u8::is_ascii_alphabetic)
            || crc32fast::hash(&data[at + 4..end - 4]) != be32(data, end - 4)?
        {
            return Err(ImageError::Invalid);
        }
        if saw_data && kind != b"IDAT" {
            data_ended = true;
        }
        if !header && kind != b"IHDR" {
            return Err(ImageError::Invalid);
        }
        let keep = match kind {
            b"IHDR" => {
                if header || length != 13 {
                    return Err(ImageError::Invalid);
                }
                width = be32(data, at + 8)?;
                height = be32(data, at + 12)?;
                bit_depth = data[at + 16];
                color_type = data[at + 17];
                let valid_depth = match color_type {
                    0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
                    2 | 4 | 6 => matches!(bit_depth, 8 | 16),
                    3 => matches!(bit_depth, 1 | 2 | 4 | 8),
                    _ => false,
                };
                if !valid_depth || data[at + 18] != 0 || data[at + 19] != 0 || data[at + 20] > 1 {
                    return Err(ImageError::Invalid);
                }
                header = true;
                true
            }
            b"PLTE" => {
                if palette.is_some()
                    || saw_data
                    || length == 0
                    || !length.is_multiple_of(3)
                    || length > 768
                {
                    return Err(ImageError::Invalid);
                }
                palette = Some(length / 3);
                true
            }
            b"tRNS" => {
                if transparency.is_some() || saw_data {
                    return Err(ImageError::Invalid);
                }
                transparency = Some(length);
                true
            }
            b"gAMA" => {
                if gamma.is_some() || palette.is_some() || saw_data || length != 4 {
                    return Err(ImageError::Invalid);
                }
                let value = be32(data, at + 8)?;
                if value == 0 {
                    return Err(ImageError::Invalid);
                }
                gamma = Some(value);
                true
            }
            b"cHRM" => {
                if chromaticities.is_some() || palette.is_some() || saw_data || length != 32 {
                    return Err(ImageError::Invalid);
                }
                let mut values = [0u32; 8];
                for (index, value) in values.iter_mut().enumerate() {
                    *value = be32(data, at + 8 + index * 4)?;
                }
                if values.as_chunks::<2>().0.iter().any(|pair| {
                    pair[0] > 100_000 || pair[1] > 100_000 || pair[0] + pair[1] > 100_000
                }) {
                    return Err(ImageError::Invalid);
                }
                chromaticities = Some(values);
                true
            }
            b"sRGB" => {
                if srgb || palette.is_some() || saw_data || length != 1 || data[at + 8] > 3 {
                    return Err(ImageError::Invalid);
                }
                srgb = true;
                true
            }
            b"iCCP" => {
                if icc.is_some() || palette.is_some() || saw_data {
                    return Err(ImageError::Invalid);
                }
                icc = Some(png_icc(
                    &data[at + 8..end - 4],
                    matches!(color_type, 0 | 4),
                )?);
                true
            }
            b"eXIf" => {
                if orientation.is_some() {
                    return Err(ImageError::Invalid);
                }
                orientation =
                    Some(crate::jpeg::exif_orientation(&data[at + 8..end - 4])?.unwrap_or(1));
                false
            }
            b"cICP" | b"mDCV" | b"cLLI" => return Err(ImageError::Unsupported),
            b"IDAT" => {
                if data_ended {
                    return Err(ImageError::Invalid);
                }
                saw_data = true;
                true
            }
            b"IEND" => {
                if length != 0 || !saw_data {
                    return Err(ImageError::Invalid);
                }
                ended = true;
                true
            }
            b"acTL" | b"fcTL" | b"fdAT" => return Err(ImageError::Unsupported),
            _ if kind[0].is_ascii_uppercase() => return Err(ImageError::Unsupported),
            _ => false,
        };
        if !keep && clean.is_none() {
            clean = Some(data[..at].to_vec());
        }
        if keep {
            if let Some(bytes) = &mut clean {
                bytes.extend_from_slice(&data[at..end]);
            }
        }
        at = end;
        if ended {
            break;
        }
    }
    if !header || !ended || at != data.len() || width == 0 || height == 0 {
        return Err(ImageError::Invalid);
    }
    if (color_type == 3 && palette.is_none())
        || (matches!(color_type, 0 | 4) && palette.is_some())
        || (color_type == 3 && palette.is_some_and(|entries| entries > 1usize << bit_depth))
        || transparency.is_some_and(|length| match color_type {
            0 => length != 2,
            2 => length != 6,
            3 => length == 0 || palette.is_none_or(|entries| length > entries),
            _ => true,
        })
    {
        return Err(ImageError::Invalid);
    }
    if srgb
        && (icc.is_some()
            || gamma.is_some_and(|value| value != 45_455)
            || chromaticities.is_some_and(|value| value != SRGB_CHROMATICITIES))
    {
        return Err(ImageError::Invalid);
    }
    Ok(Png {
        width,
        height,
        clean: match clean {
            Some(bytes) => Cow::Owned(bytes),
            None => Cow::Borrowed(data),
        },
        custom_color: !srgb && icc.is_none() && (gamma.is_some() || chromaticities.is_some()),
        gray_profile: icc.is_some() && matches!(color_type, 0 | 4),
        orientation: orientation.unwrap_or(1),
        icc,
    })
}

/// Validate one complete zlib stream, including its Adler checksum and the
/// exact decompressed size promised by the PNG header. Never retain raw rows.
pub(crate) fn png_stream(data: &[u8], width: u32, height: u32) -> Result<()> {
    let depth = u64::from(data[24]);
    let channels = match data[25] {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err(ImageError::Invalid),
    };
    let row_size = |w: u64| 1 + (w * channels * depth).div_ceil(8);
    let expected = if data[28] == 0 {
        u64::from(height) * row_size(u64::from(width))
    } else {
        const ADAM7: [(u64, u64, u64, u64); 7] = [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ];
        ADAM7
            .into_iter()
            .map(|(x, y, dx, dy)| {
                let w = u64::from(width).saturating_sub(x).div_ceil(dx);
                let h = u64::from(height).saturating_sub(y).div_ceil(dy);
                if w == 0 || h == 0 {
                    0
                } else {
                    h * row_size(w)
                }
            })
            .sum()
    };
    if expected > crate::DECODE_MEMORY {
        return Err(ImageError::TooLarge);
    }
    let mut decoder = flate2::Decompress::new(true);
    let mut output = [0u8; 8192];
    let mut finished = false;
    let mut at = 8usize;
    while at < data.len() {
        let length = be32(data, at)? as usize;
        let end = at + 12 + length;
        if &data[at + 4..at + 8] == b"IDAT" {
            let mut input = &data[at + 8..end - 4];
            let mut pending_output = false;
            while !input.is_empty() || pending_output {
                if finished {
                    return Err(ImageError::Invalid);
                }
                let before_in = decoder.total_in();
                let before_out = decoder.total_out();
                let status = decoder
                    .decompress(input, &mut output, flate2::FlushDecompress::None)
                    .map_err(|_| ImageError::Invalid)?;
                let consumed = (decoder.total_in() - before_in) as usize;
                let produced = decoder.total_out() - before_out;
                input = &input[consumed..];
                if decoder.total_out() > expected {
                    return Err(ImageError::Invalid);
                }
                if consumed == 0 && produced == 0 {
                    if input.is_empty() {
                        break;
                    }
                    return Err(ImageError::Invalid);
                }
                pending_output = produced as usize == output.len();
                if status == flate2::Status::StreamEnd {
                    finished = true;
                    if !input.is_empty() {
                        return Err(ImageError::Invalid);
                    }
                    break;
                }
            }
        }
        at = end;
    }
    if !finished || decoder.total_out() != expected {
        return Err(ImageError::Invalid);
    }
    Ok(())
}

pub(crate) struct Webp<'a> {
    pub width: u32,
    pub height: u32,
    pub orientation: u8,
    pub icc: Option<&'a [u8]>,
}

/// Refuse animated, concatenated or inconsistent WebP containers before
/// decoding; the canvas dimensions must agree with the actual still image.
pub(crate) fn webp(data: &[u8]) -> Result<Webp<'_>> {
    if data.len() < 20
        || &data[..4] != b"RIFF"
        || &data[8..12] != b"WEBP"
        || le32(data, 4)? as u64 + 8 != data.len() as u64
    {
        return Err(ImageError::Invalid);
    }
    let mut at = 12usize;
    let mut extended = None;
    let mut still = None;
    let mut animated = false;
    let mut alpha = false;
    let mut lossless_alpha = false;
    let mut icc = None;
    let mut exif = None;
    while at.checked_add(8).is_some_and(|n| n <= data.len()) {
        let kind = &data[at..at + 4];
        let size = le32(data, at + 4)? as usize;
        let start = at + 8;
        let end = start.checked_add(size).ok_or(ImageError::Invalid)?;
        if end > data.len() {
            return Err(ImageError::Invalid);
        }
        let payload = &data[start..end];
        match kind {
            b"VP8X" => {
                if extended.is_some() || at != 12 || size != 10 {
                    return Err(ImageError::Invalid);
                }
                animated = payload[0] & 2 != 0;
                let width = 1 + u32::from_le_bytes([payload[4], payload[5], payload[6], 0]);
                let height = 1 + u32::from_le_bytes([payload[7], payload[8], payload[9], 0]);
                extended = Some((width, height, payload[0]));
            }
            b"VP8 " => {
                if still.is_some() || size < 10 || payload[3..6] != [0x9d, 0x01, 0x2a] {
                    return Err(ImageError::Invalid);
                }
                let width = u16::from_le_bytes([payload[6], payload[7]]) & 0x3fff;
                let height = u16::from_le_bytes([payload[8], payload[9]]) & 0x3fff;
                still = Some((u32::from(width), u32::from(height)));
            }
            b"VP8L" => {
                if still.is_some() || alpha || size < 5 || payload[0] != 0x2f {
                    return Err(ImageError::Invalid);
                }
                let bits = le32(payload, 1)?;
                if bits >> 29 != 0 {
                    return Err(ImageError::Invalid);
                }
                lossless_alpha = bits & (1 << 28) != 0;
                still = Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1));
            }
            b"ALPH" => {
                if extended.is_none() || alpha || still.is_some() || payload.is_empty() {
                    return Err(ImageError::Invalid);
                }
                alpha = true;
            }
            b"ICCP" => {
                if icc.is_some() || extended.is_none() || still.is_some() {
                    return Err(ImageError::Invalid);
                }
                if validate_icc(payload)? != moxcms::DataColorSpace::Rgb {
                    return Err(ImageError::Unsupported);
                }
                icc = Some(payload);
            }
            b"EXIF" => {
                if exif.is_some() || extended.is_none() || still.is_none() {
                    return Err(ImageError::Invalid);
                }
                let tiff = payload.strip_prefix(b"Exif\0\0").unwrap_or(payload);
                exif = Some(crate::jpeg::exif_orientation(tiff)?.unwrap_or(1));
            }
            b"ANIM" | b"ANMF" => animated = true,
            _ => {}
        }
        at = end.checked_add(size % 2).ok_or(ImageError::Invalid)?;
    }
    let (width, height) = still.ok_or(ImageError::Invalid)?;
    if at != data.len()
        || animated
        || width == 0
        || height == 0
        || extended.is_some_and(|(w, h, flags)| {
            (w, h) != (width, height)
                || (flags & 0x10 != 0) != (alpha || lossless_alpha)
                || (flags & 0x20 != 0) != icc.is_some()
                || (flags & 0x08 != 0) != exif.is_some()
        })
    {
        return Err(ImageError::Invalid);
    }
    Ok(Webp {
        width,
        height,
        orientation: exif.unwrap_or(1),
        icc,
    })
}
