use crate::{ImageError, Result};

/// Map upright coordinates back to the unrotated decoder output. Doing this
/// while downsampling avoids another full-sized allocation for phone photos.
pub(crate) struct Mapping {
    orientation: u8,
    source_width: u32,
    source_height: u32,
    pub width: u32,
    pub height: u32,
}

impl Mapping {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            orientation: 1,
            source_width: width,
            source_height: height,
            width,
            height,
        }
    }

    pub fn exif(&mut self, orientation: u8) -> Result<()> {
        if !(1..=8).contains(&orientation) {
            return Err(ImageError::Invalid);
        }
        self.orientation = orientation;
        (self.width, self.height) = if orientation >= 5 {
            (self.source_height, self.source_width)
        } else {
            (self.source_width, self.source_height)
        };
        Ok(())
    }

    pub(crate) fn source(&self, x: u32, y: u32) -> (u32, u32) {
        let width = self.source_width;
        let height = self.source_height;
        match self.orientation {
            1 => (x, y),
            2 => (width - 1 - x, y),
            3 => (width - 1 - x, height - 1 - y),
            4 => (x, height - 1 - y),
            5 => (y, x),
            6 => (y, height - 1 - x),
            7 => (width - 1 - y, height - 1 - x),
            8 => (width - 1 - y, x),
            _ => unreachable!("orientation is validated in exif"),
        }
    }
}

/// Area downsample in upright coordinates. Source bytes stay in their decoded
/// layout and each source pixel is visited at most once for each attempt.
pub(crate) fn downsample(
    source: &[u8],
    source_width: u32,
    channels: usize,
    map: &Mapping,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    if !matches!(channels, 3 | 4)
        || width == 0
        || height == 0
        || width > map.width
        || height > map.height
    {
        return Err(ImageError::Invalid);
    }
    let size = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(channels))
        .ok_or(ImageError::TooLarge)?;
    let mut output = vec![0u8; size];
    for y in 0..height {
        let top = (u64::from(y) * u64::from(map.height) / u64::from(height)) as u32;
        let bottom = (u64::from(y + 1) * u64::from(map.height) / u64::from(height)) as u32;
        for x in 0..width {
            let left = (u64::from(x) * u64::from(map.width) / u64::from(width)) as u32;
            let right = (u64::from(x + 1) * u64::from(map.width) / u64::from(width)) as u32;
            let mut rgb = [0u64; 3];
            let mut alpha = 0u64;
            let mut count = 0u64;
            for sy in top..bottom {
                for sx in left..right {
                    let (px, py) = map.source(sx, sy);
                    let at = ((py as usize * source_width as usize) + px as usize)
                        .checked_mul(channels)
                        .ok_or(ImageError::Invalid)?;
                    let pixel = source.get(at..at + channels).ok_or(ImageError::Invalid)?;
                    let a = if channels == 4 {
                        u64::from(pixel[3])
                    } else {
                        255
                    };
                    for c in 0..3 {
                        rgb[c] += u64::from(pixel[c]) * a;
                    }
                    alpha += a;
                    count += 1;
                }
            }
            if count == 0 {
                return Err(ImageError::Invalid);
            }
            let dest = ((y as usize * width as usize) + x as usize) * channels;
            if let Some(nonzero_alpha) = std::num::NonZeroU64::new(alpha) {
                for c in 0..3 {
                    output[dest + c] = (rgb[c] / nonzero_alpha.get()) as u8;
                }
            }
            if channels == 4 {
                output[dest + 3] = (alpha / count) as u8;
            }
        }
    }
    Ok(output)
}
