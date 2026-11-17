use std::io::Cursor;

use cadence_image::{prepare, Format};
use image::{ExtendedColorType, ImageDecoder, ImageEncoder};

const PROFILE: &[u8] = include_bytes!("../src/display-p3.icc");

fn chunk(output: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    output.extend_from_slice(kind);
    output.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    output.extend_from_slice(payload);
    if payload.len() % 2 != 0 {
        output.push(0);
    }
}

fn fixture() -> Vec<u8> {
    let (width, height) = (640u32, 320u32);
    let mut state = 0x1234_5678u32;
    let mut pixels = Vec::new();
    for _ in 0..height {
        for x in 0..width {
            let mut noise = [0u8; 3];
            for value in &mut noise {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *value = (state & 63) as u8;
            }
            if x < width / 2 {
                noise[0] += 192;
            } else {
                noise[2] += 192;
            }
            pixels.extend_from_slice(&noise);
        }
    }
    let mut simple = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut simple)
        .write_image(&pixels, width, height, ExtendedColorType::Rgb8)
        .expect("encode fixture WebP");

    // Orientation 6 rotates the red left half above the blue right half.
    let tiff: &[u8] = &[
        b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0,
        0, 0, 0,
    ];
    let mut webp = b"RIFF\0\0\0\0WEBP".to_vec();
    let canvas = [
        0x28,
        0,
        0,
        0,
        ((width - 1) & 255) as u8,
        (((width - 1) >> 8) & 255) as u8,
        0,
        ((height - 1) & 255) as u8,
        (((height - 1) >> 8) & 255) as u8,
        0,
    ];
    chunk(&mut webp, b"VP8X", &canvas);
    chunk(&mut webp, b"ICCP", PROFILE);
    webp.extend_from_slice(&simple[12..]);
    chunk(&mut webp, b"EXIF", tiff);
    let size = (webp.len() - 8) as u32;
    webp[4..8].copy_from_slice(&size.to_le_bytes());
    webp
}

#[test]
fn converted_webp_preserves_upright_pixels_and_color_profile() {
    let input = fixture();
    assert!(input.len() > 145_920, "fixture must require conversion");
    let prepared = prepare(&input, Format::Webp).expect("prepare valid WebP");
    assert_eq!(prepared.format, Format::Jpeg);
    assert_eq!(prepared.height, prepared.width * 2);
    let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(&prepared.bytes))
        .expect("decode prepared JPEG");
    assert_eq!(
        decoder.icc_profile().expect("read color profile").as_deref(),
        Some(PROFILE)
    );
    let image = image::DynamicImage::from_decoder(decoder)
        .expect("decode prepared pixels")
        .to_rgb8();
    let top = image.get_pixel(prepared.width / 2, prepared.height / 4).0;
    let bottom = image
        .get_pixel(prepared.width / 2, 3 * prepared.height / 4)
        .0;
    assert!(top[0] > top[2] + 80, "red marker must be above blue");
    assert!(bottom[2] > bottom[0] + 80, "blue marker must be below red");
}
