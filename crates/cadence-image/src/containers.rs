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

/// Validate every PNG chunk, and remove ancillary chunks that the raster
/// decoder does not need. Compressed metadata must not be inflated at upload.
pub(crate) fn png(data: &[u8]) -> Result<(u32, u32, Cow<'_, [u8]>)> {
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
    Ok((
        width,
        height,
        match clean {
            Some(bytes) => Cow::Owned(bytes),
            None => Cow::Borrowed(data),
        },
    ))
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

/// Refuse animated, concatenated or inconsistent WebP containers before
/// decoding; the canvas dimensions must agree with the actual still image.
pub(crate) fn webp(data: &[u8]) -> Result<(u32, u32)> {
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
                extended = Some((width, height));
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
                if still.is_some() || size < 5 || payload[0] != 0x2f {
                    return Err(ImageError::Invalid);
                }
                let bits = le32(payload, 1)?;
                still = Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1));
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
        || extended.is_some_and(|canvas| canvas != (width, height))
    {
        return Err(ImageError::Invalid);
    }
    Ok((width, height))
}
