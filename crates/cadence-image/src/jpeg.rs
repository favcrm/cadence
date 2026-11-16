use crate::{ImageError, Result};

/// The first JPEG image in an upload. `end` never includes a phone-photo suffix.
pub(crate) struct Layout {
    pub end: usize,
    pub width: u32,
    pub height: u32,
    pub orientation: u8,
}

#[derive(Clone, Copy)]
struct Component {
    id: u8,
    coefficients: [i8; 64],
    scanned: bool,
}

const EMPTY_COMPONENT: Component = Component {
    id: 0,
    coefficients: [-1; 64],
    scanned: false,
};

fn read_u16(data: &[u8], offset: usize, little: bool) -> Option<u16> {
    let b = data.get(offset..offset.checked_add(2)?)?;
    let pair = [b[0], b[1]];
    Some(if little { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) })
}

fn read_u32(data: &[u8], offset: usize, little: bool) -> Option<u32> {
    let b = data.get(offset..offset.checked_add(4)?)?;
    let bytes = [b[0], b[1], b[2], b[3]];
    Some(if little { u32::from_le_bytes(bytes) } else { u32::from_be_bytes(bytes) })
}

pub(crate) fn exif_orientation(tiff: &[u8]) -> Result<Option<u8>> {
    let little = match tiff.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return Err(ImageError::Invalid),
    };
    if read_u16(tiff, 2, little) != Some(42) {
        return Err(ImageError::Invalid);
    }
    let ifd = read_u32(tiff, 4, little).ok_or(ImageError::Invalid)? as usize;
    if ifd < 8 {
        return Err(ImageError::Invalid);
    }
    let count = read_u16(tiff, ifd, little).ok_or(ImageError::Invalid)? as usize;
    let entries = ifd.checked_add(2).ok_or(ImageError::Invalid)?;
    let end = count
        .checked_mul(12)
        .and_then(|n| entries.checked_add(n))
        .and_then(|n| n.checked_add(4))
        .ok_or(ImageError::Invalid)?;
    if end > tiff.len() {
        return Err(ImageError::Invalid);
    }
    let mut found = None;
    for index in 0..count {
        let at = entries + index * 12;
        if read_u16(tiff, at, little) != Some(0x0112) {
            continue;
        }
        if found.is_some()
            || read_u16(tiff, at + 2, little) != Some(3)
            || read_u32(tiff, at + 4, little) != Some(1)
        {
            return Err(ImageError::Invalid);
        }
        let value = read_u16(tiff, at + 8, little).ok_or(ImageError::Invalid)?;
        if !(1..=8).contains(&value) {
            return Err(ImageError::Invalid);
        }
        found = Some(value as u8);
    }
    Ok(found)
}

/// Walk markers and scans; the decoder must still validate the entropy data.
pub(crate) fn inspect(data: &[u8]) -> Result<Layout> {
    if !data.starts_with(&[0xff, 0xd8]) {
        return Err(ImageError::Invalid);
    }
    let mut offset = 2usize;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut progressive = false;
    let mut components = [EMPTY_COMPONENT; 4];
    let mut component_count = 0usize;
    let mut saw_scan = false;
    let mut orientation = None;
    while offset + 1 < data.len() {
        if data[offset] != 0xff {
            return Err(ImageError::Invalid);
        }
        while data.get(offset) == Some(&0xff) {
            offset += 1;
        }
        let marker = *data.get(offset).ok_or(ImageError::Invalid)?;
        offset += 1;
        if marker == 0xd9 {
            if width == 0 || height == 0 || !saw_scan || component_count == 0
                || components[..component_count].iter().any(|c| !c.scanned || c.coefficients[0] < 0)
            {
                return Err(ImageError::Invalid);
            }
            return Ok(Layout { end: offset, width, height, orientation: orientation.unwrap_or(1) });
        }
        if marker == 0x00 || marker == 0xd8 {
            return Err(ImageError::Invalid);
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let length = read_u16(data, offset, false).ok_or(ImageError::Invalid)? as usize;
        let end = offset.checked_add(length).ok_or(ImageError::Invalid)?;
        if length < 2 || end > data.len() {
            return Err(ImageError::Invalid);
        }
        if marker == 0xe1 && length >= 8 && data.get(offset + 2..offset + 8) == Some(b"Exif\0\0") {
            if let Some(value) = exif_orientation(&data[offset + 8..end])? {
                if orientation.is_some_and(|old| old != value) {
                    return Err(ImageError::Invalid);
                }
                orientation = Some(value);
            }
        }
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            if width != 0 || height != 0 {
                return Err(ImageError::Invalid);
            }
            if !matches!(marker, 0xc0..=0xc2) {
                return Err(ImageError::Unsupported);
            }
            if length < 8 || data[offset + 2] != 8 {
                return Err(ImageError::Invalid);
            }
            component_count = data[offset + 7] as usize;
            if component_count == 0 || component_count > 4 || length != 8 + 3 * component_count {
                return Err(ImageError::Invalid);
            }
            for i in 0..component_count {
                let id = data[offset + 8 + i * 3];
                if components[..i].iter().any(|c| c.id == id) {
                    return Err(ImageError::Invalid);
                }
                components[i].id = id;
            }
            height = read_u16(data, offset + 3, false).ok_or(ImageError::Invalid)? as u32;
            width = read_u16(data, offset + 5, false).ok_or(ImageError::Invalid)? as u32;
            progressive = marker == 0xc2;
        }
        if marker == 0xda {
            let count = *data.get(offset + 2).ok_or(ImageError::Invalid)? as usize;
            if width == 0 || height == 0 || count == 0 || count > component_count
                || length != 6 + 2 * count
            {
                return Err(ImageError::Invalid);
            }
            let start = data[offset + 3 + 2 * count] as usize;
            let last = data[offset + 4 + 2 * count] as usize;
            let approximation = data[offset + 5 + 2 * count];
            let high = approximation >> 4;
            let low = approximation & 15;
            if start > last || last > 63 || high > 13 || low > 13 || (high > 0 && high != low + 1)
                || (!progressive && (start != 0 || last != 63 || approximation != 0))
                || (progressive && ((start == 0 && last != 0) || (start > 0 && count != 1)))
            {
                return Err(ImageError::Invalid);
            }
            let mut selected = [false; 4];
            for i in 0..count {
                let id = data[offset + 3 + 2 * i];
                let index = components[..component_count].iter().position(|c| c.id == id)
                    .ok_or(ImageError::Invalid)?;
                if selected[index] {
                    return Err(ImageError::Invalid);
                }
                selected[index] = true;
                let component = &mut components[index];
                if progressive && start > 0 && component.coefficients[0] < 0 {
                    return Err(ImageError::Invalid);
                }
                for coefficient in start..=last {
                    let previous = component.coefficients[coefficient];
                    if (high == 0 && previous != -1) || (high > 0 && previous != high as i8) {
                        return Err(ImageError::Invalid);
                    }
                    component.coefficients[coefficient] = low as i8;
                }
                component.scanned = true;
            }
            saw_scan = true;
        }
        offset = end;
        if marker == 0xda {
            let mut entropy = false;
            while offset + 1 < data.len() {
                if data[offset] != 0xff {
                    entropy = true;
                    offset += 1;
                    continue;
                }
                let mut next = offset + 1;
                while data.get(next) == Some(&0xff) {
                    next += 1;
                }
                let code = *data.get(next).ok_or(ImageError::Invalid)?;
                if code == 0 || (0xd0..=0xd7).contains(&code) {
                    entropy |= code == 0;
                    offset = next + 1;
                } else {
                    break;
                }
            }
            if !entropy {
                return Err(ImageError::Invalid);
            }
        }
    }
    Err(ImageError::Invalid)
}
