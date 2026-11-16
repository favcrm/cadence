use std::io::{Cursor, Write};

use cadence_image::{prepare, validate_native, Format};
use flate2::{write::ZlibEncoder, Compression};
use image::{ExtendedColorType, ImageDecoder, ImageEncoder};

const PROFILE: &[u8] = include_bytes!("../src/display-p3.icc");

fn png_with_profile(width: u32, height: u32) -> Vec<u8> {
    // Deterministic, poorly compressible pixels make the large fixture exceed
    // the model byte budget without requiring external files or randomness.
    let mut state = 0x1234_5678u32;
    let pixels: Vec<u8> = (0..width as usize * height as usize * 3)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&pixels, width, height, ExtendedColorType::Rgb8)
        .expect("encode fixture PNG");

    let mut compressor = ZlibEncoder::new(Vec::new(), Compression::default());
    compressor.write_all(PROFILE).expect("compress ICC profile");
    let compressed = compressor.finish().expect("finish ICC compression");
    let mut payload = b"Display P3\0\0".to_vec();
    payload.extend_from_slice(&compressed);
    let mut chunk = Vec::new();
    chunk.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    chunk.extend_from_slice(b"iCCP");
    chunk.extend_from_slice(&payload);
    let crc = crc32fast::hash(&chunk[4..]);
    chunk.extend_from_slice(&crc.to_be_bytes());
    // The encoder writes an eight-byte signature and a 25-byte IHDR first.
    png.splice(33..33, chunk);
    png
}

fn check_decoder<D: ImageDecoder>(mut decoder: D, expected: &[u8]) {
    let profile = decoder
        .icc_profile()
        .expect("read image ICC profile")
        .expect("image must retain its ICC profile");
    assert_eq!(profile, expected, "image ICC profile changed");
    let mut pixels = vec![0; decoder.total_bytes() as usize];
    decoder.read_image(&mut pixels).expect("decode image pixels");
}

fn check_png(bytes: &[u8]) {
    let decoder = image::codecs::png::PngDecoder::new(Cursor::new(bytes))
        .expect("open PNG decoder");
    check_decoder(decoder, PROFILE);
}

fn check_prepared(bytes: &[u8], format: Format) {
    match format {
        Format::Png => check_png(bytes),
        Format::Jpeg => {
            let decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(bytes))
                .expect("open JPEG decoder");
            check_decoder(decoder, PROFILE);
        }
        _ => panic!("prepared image is neither PNG nor JPEG"),
    }
}

#[test]
fn png_color_profile_survives_native_and_hosted_preparation() {
    let small = png_with_profile(8, 8);
    check_png(&small);
    let native = validate_native(&small, "image/png").expect("prepare native small PNG");
    check_png(native.as_ref());
    let hosted = prepare(&small, Format::Png).expect("prepare hosted small PNG");
    check_prepared(&hosted.bytes, hosted.format);

    let large = png_with_profile(1601, 512);
    assert!(large.len() > 145_920, "fixture must exceed the model byte budget");
    assert!(large.len() <= 4 * 1024 * 1024, "fixture must fit the native input cap");
    check_png(&large);
    let native = validate_native(&large, "image/png").expect("prepare native large PNG");
    check_png(native.as_ref());
    let hosted = prepare(&large, Format::Png).expect("prepare hosted large PNG");
    assert!(hosted.converted, "large PNG must be converted");
    assert!(hosted.width < 1601, "large PNG must be reduced");
    assert!(hosted.bytes.len() <= 145_920, "prepared output exceeds the model budget");
    check_prepared(&hosted.bytes, hosted.format);
}
