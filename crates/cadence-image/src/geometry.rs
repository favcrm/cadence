use heic_rs::props::simple::{Mirror, Rotation};
use heic_rs::Transform;

use crate::{ImageError, Result};

#[derive(Clone, Copy)]
enum Inverse {
    Crop(u32, u32),
    Rotate(Rotation, u32, u32),
    Mirror(Mirror, u32, u32),
}

/// Map upright coordinates back to the unrotated decoder output. Doing this
/// while downsampling avoids another full-sized allocation for phone photos.
pub(crate) struct Mapping {
    steps: Vec<Inverse>,
    pub width: u32,
    pub height: u32,
}

impl Mapping {
    pub fn new(width: u32, height: u32) -> Self {
        Self { steps: Vec::new(), width, height }
    }

    pub fn push(&mut self, transform: Transform) -> Result<()> {
        if self.steps.len() >= 16 {
            return Err(ImageError::Invalid);
        }
        match transform {
            Transform::Crop(crop) => {
                let (x, y, width, height) = heic_rs::transform::crop_rect(self.width, self.height, crop)
                    .map_err(|_| ImageError::Invalid)?;
                self.steps.push(Inverse::Crop(x, y));
                self.width = width;
                self.height = height;
            }
            Transform::Rotate(rotation) => {
                if rotation != Rotation::None {
                    self.steps.push(Inverse::Rotate(rotation, self.width, self.height));
                    if rotation.swaps_axes() {
                        core::mem::swap(&mut self.width, &mut self.height);
                    }
                }
            }
            Transform::Mirror(mirror) => {
                self.steps.push(Inverse::Mirror(mirror, self.width, self.height));
            }
        }
        Ok(())
    }

    pub fn exif(&mut self, orientation: u8) -> Result<()> {
        use Mirror::{LeftRight, TopBottom};
        use Rotation::{Ccw180, Ccw270, Ccw90};
        match orientation {
            1 => {}
            2 => self.push(Transform::Mirror(LeftRight))?,
            3 => self.push(Transform::Rotate(Ccw180))?,
            4 => self.push(Transform::Mirror(TopBottom))?,
            5 => {
                self.push(Transform::Mirror(LeftRight))?;
                self.push(Transform::Rotate(Ccw90))?;
            }
            6 => self.push(Transform::Rotate(Ccw270))?,
            7 => {
                self.push(Transform::Mirror(LeftRight))?;
                self.push(Transform::Rotate(Ccw270))?;
            }
            8 => self.push(Transform::Rotate(Ccw90))?,
            _ => return Err(ImageError::Invalid),
        }
        Ok(())
    }

    fn source(&self, mut x: u32, mut y: u32) -> (u32, u32) {
        for step in self.steps.iter().rev() {
            (x, y) = match *step {
                Inverse::Crop(left, top) => (x + left, y + top),
                Inverse::Rotate(Rotation::Ccw90, width, _) => (width - 1 - y, x),
                Inverse::Rotate(Rotation::Ccw180, width, height) => (width - 1 - x, height - 1 - y),
                Inverse::Rotate(Rotation::Ccw270, _, height) => (y, height - 1 - x),
                Inverse::Rotate(Rotation::None, _, _) => (x, y),
                Inverse::Mirror(Mirror::LeftRight, width, _) => (width - 1 - x, y),
                Inverse::Mirror(Mirror::TopBottom, _, height) => (x, height - 1 - y),
            };
        }
        (x, y)
    }
}

/// Area downsample in upright coordinates. Source bytes stay in their decoded
/// layout and each source pixel is visited at most once for each attempt.
pub(crate) fn downsample(
    source: &[u8], source_width: u32, channels: usize, map: &Mapping, width: u32, height: u32,
) -> Result<Vec<u8>> {
    if !matches!(channels, 3 | 4) || width == 0 || height == 0 || width > map.width || height > map.height {
        return Err(ImageError::Invalid);
    }
    let size = (width as usize).checked_mul(height as usize)
        .and_then(|n| n.checked_mul(channels)).ok_or(ImageError::TooLarge)?;
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
                        .checked_mul(channels).ok_or(ImageError::Invalid)?;
                    let pixel = source.get(at..at + channels).ok_or(ImageError::Invalid)?;
                    let a = if channels == 4 { u64::from(pixel[3]) } else { 255 };
                    for c in 0..3 { rgb[c] += u64::from(pixel[c]) * a; }
                    alpha += a;
                    count += 1;
                }
            }
            if count == 0 { return Err(ImageError::Invalid); }
            let dest = ((y as usize * width as usize) + x as usize) * channels;
            if alpha != 0 {
                for c in 0..3 { output[dest + c] = (rgb[c] / alpha) as u8; }
            }
            if channels == 4 { output[dest + 3] = (alpha / count) as u8; }
        }
    }
    Ok(output)
}
